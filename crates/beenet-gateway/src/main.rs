use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use axum::body::Bytes;
use axum::extract::{Path as AxumPath, State};
use axum::http::header::{HeaderValue, ACCESS_CONTROL_ALLOW_ORIGIN};
use axum::http::{Method, Request, Response, StatusCode};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use beenet_common::config::{
    load_file, resolve_config_path_with_cli, resolve_gateway_settings_optional_file,
    GatewayCliOverrides,
};
use beenet_common::{BeenetCid, INVOKE_PROTOCOL};
use beenet_proto::{InvokeRequest, InvokeResponse, LoadStage, Status, TimeoutStage, Usage};
use clap::Parser;
use libp2p::core::multiaddr::Protocol;
use libp2p::futures::StreamExt;
use libp2p::request_response::{self, ProtocolSupport};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, ping, Multiaddr, PeerId, StreamProtocol, SwarmBuilder};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

#[derive(Parser, Debug, Clone)]
#[command(name = "beenet-gateway", about = "Beenet M1 gateway")]
struct Args {
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    #[arg(long)]
    http_addr: Option<String>,

    #[arg(long)]
    registry_url: Option<String>,

    #[arg(long)]
    registry_poll_ms: Option<u64>,

    #[arg(long)]
    default_deadline_ms: Option<u32>,

    #[arg(long)]
    libp2p_listen_addr: Option<String>,

    #[arg(long)]
    public_addr: Option<String>,

    /// Persistent Ed25519 identity key (stable PeerId for workers to dial).
    #[arg(long, value_name = "PATH")]
    identity_key_path: Option<PathBuf>,

    #[arg(long, default_value = "gateway")]
    gateway_id: String,

    #[arg(long)]
    region: Option<String>,

    #[arg(long, default_value_t = 1000)]
    capacity: u32,
}

#[derive(NetworkBehaviour)]
struct GatewayBehaviour {
    request_response: request_response::cbor::Behaviour<InvokeRequest, InvokeResponse>,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
}

#[derive(Clone)]
struct AppState {
    client: GatewayClient,
    default_deadline_ms: u32,
}

#[derive(Clone)]
struct GatewayClient {
    tx: mpsc::Sender<Command>,
}

struct Command {
    req: InvokeRequest,
    respond_to: oneshot::Sender<InvokeResponse>,
}

impl GatewayClient {
    async fn invoke(&self, req: InvokeRequest) -> Result<InvokeResponse> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command {
                req,
                respond_to: tx,
            })
            .await
            .map_err(|_| anyhow!("gateway event loop stopped"))?;
        rx.await
            .map_err(|_| anyhow!("gateway response channel closed"))
    }
}

fn load_or_create_keypair(path: &Path) -> Result<identity::Keypair> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create identity dir `{}`", parent.display()))?;
        }
    }
    if path.exists() {
        let bytes =
            fs::read(path).with_context(|| format!("read identity key `{}`", path.display()))?;
        let keypair = identity::Keypair::from_protobuf_encoding(&bytes)
            .with_context(|| format!("decode identity key `{}`", path.display()))?;
        info!(path = %path.display(), "loaded persistent gateway identity");
        Ok(keypair)
    } else {
        let keypair = identity::Keypair::generate_ed25519();
        let bytes = keypair
            .to_protobuf_encoding()
            .context("encode new identity keypair")?;
        fs::write(path, &bytes)
            .with_context(|| format!("write identity key `{}`", path.display()))?;
        info!(path = %path.display(), "generated and saved new gateway identity");
        Ok(keypair)
    }
}

fn build_swarm(local_key: identity::Keypair) -> Result<libp2p::Swarm<GatewayBehaviour>> {
    let cfg = request_response::Config::default()
        .with_request_timeout(Duration::from_secs(30))
        .with_max_concurrent_streams(32);

    let swarm = SwarmBuilder::with_existing_identity(local_key)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )?
        .with_behaviour(|key| GatewayBehaviour {
            request_response: request_response::cbor::Behaviour::new(
                [(StreamProtocol::new(INVOKE_PROTOCOL), ProtocolSupport::Full)],
                cfg,
            ),
            identify: identify::Behaviour::new(identify::Config::new(
                "/beenet/0.1".to_string(),
                key.public(),
            )),
            ping: ping::Behaviour::default(),
        })?
        // Keep reverse long connections from workers; default idle timeout is ~10s.
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(3600)))
        .build();
    Ok(swarm)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(false)
        .init();

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cli = Args::parse();
    let path = resolve_config_path_with_cli(cli.config.clone(), &argv);
    let overrides = GatewayCliOverrides {
        http_addr: cli.http_addr.clone(),
        registry_url: cli.registry_url.clone(),
        registry_poll_ms: cli.registry_poll_ms,
        default_deadline_ms: cli.default_deadline_ms,
        libp2p_listen_addr: cli.libp2p_listen_addr.clone(),
        public_addr: cli.public_addr.clone(),
        identity_key_path: cli.identity_key_path.clone(),
    };
    let settings = if path.exists() {
        let file_cfg = load_file(&path)?;
        resolve_gateway_settings_optional_file(Some(&file_cfg), &overrides)?
    } else if cli.config.is_some() {
        anyhow::bail!(
            "missing config file `{}` (add [gateway] or pass --config)",
            path.display()
        );
    } else if cli.registry_url.is_some() {
        resolve_gateway_settings_optional_file(None, &overrides)?
    } else {
        anyhow::bail!(
            "missing config file `{}` (add [gateway], pass --config, or set --registry-url for container mode)",
            path.display()
        );
    };

    let worker_addrs: Arc<RwLock<Vec<WorkerListEntry>>> = Arc::new(RwLock::new(Vec::new()));
    tokio::spawn(registry_poll_loop(
        settings.registry_url.clone(),
        settings.registry_poll_ms,
        worker_addrs.clone(),
    ));

    let local_key = load_or_create_keypair(&settings.identity_key_path)?;
    let local_peer_id = local_key.public().to_peer_id();
    let mut swarm = build_swarm(local_key.clone())?;
    let libp2p_listen_addr: Multiaddr = settings.libp2p_listen_addr.parse().with_context(|| {
        format!(
            "invalid gateway libp2p_listen_addr `{}`",
            settings.libp2p_listen_addr
        )
    })?;
    swarm.listen_on(libp2p_listen_addr.clone())?;
    if let Some(public_addr) = settings.public_addr.as_ref() {
        let public_addr: Multiaddr = public_addr
            .parse()
            .with_context(|| format!("invalid gateway public_addr `{public_addr}`"))?;
        let announced = public_addr.with(Protocol::P2p(local_peer_id.into()));
        swarm.add_external_address(announced.clone());
        info!(%announced, "gateway public address announced");
    }
    let (tx, rx) = mpsc::channel(32);
    let connected: Arc<RwLock<HashSet<PeerId>>> = Arc::new(RwLock::new(HashSet::new()));

    let dial_addr = if let Some(public_addr) = settings.public_addr.as_ref() {
        public_addr
            .parse::<Multiaddr>()?
            .with(Protocol::P2p(local_peer_id.into()))
            .to_string()
    } else {
        warn!("gateway public_addr is not set; using libp2p_listen_addr for registry heartbeat in local/test mode");
        settings
            .libp2p_listen_addr
            .parse::<Multiaddr>()?
            .with(Protocol::P2p(local_peer_id.into()))
            .to_string()
    };

    tokio::spawn(gateway_heartbeat_loop(
        settings.registry_url.clone(),
        cli.gateway_id.clone(),
        cli.region.clone(),
        cli.capacity,
        dial_addr,
        local_key.clone(),
        connected.clone(),
    ));

    tokio::spawn(run_swarm_loop(
        worker_addrs.clone(),
        connected.clone(),
        rx,
        swarm,
    ));

    let state = AppState {
        client: GatewayClient { tx },
        default_deadline_ms: settings.default_deadline_ms,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/run/ipfs/:cid", post(run_ipfs))
        .layer(middleware::from_fn(cors_middleware))
        .with_state(state);

    info!(
        peer_id = %local_peer_id,
        http_addr = %settings.http_addr,
        registry_url = %settings.registry_url,
        poll_ms = settings.registry_poll_ms,
        libp2p_listen_addr = %settings.libp2p_listen_addr,
        public_addr = %settings.public_addr.as_deref().unwrap_or("<none>"),
        identity_key_path = %settings.identity_key_path.display(),
        "gateway started (workers dial in; invoke reuses inbound connections)"
    );
    let listener = tokio::net::TcpListener::bind(settings.http_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

async fn cors_middleware(req: Request<axum::body::Body>, next: Next) -> impl IntoResponse {
    if req.method() == Method::OPTIONS {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header("access-control-allow-methods", "GET, POST, OPTIONS")
            .header("access-control-allow-headers", "*")
            .body(axum::body::Body::empty())
            .unwrap()
            .into_response();
    }
    let mut resp = next.run(req).await;
    resp.headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    resp.into_response()
}

/// Whether `addr` looks like an Alibaba Cloud SLB health-check probe (not a real peer).
fn is_lb_health_probe_addr(addr: &Multiaddr) -> bool {
    for proto in addr.iter() {
        if let Protocol::Ip4(ip) = proto {
            // Aliyun SLB probes commonly come from 100.64/10 (CGNAT) / 100.127/16.
            if ip.octets()[0] == 100 && (ip.octets()[1] >= 64 && ip.octets()[1] <= 127) {
                return true;
            }
        }
    }
    false
}

async fn run_ipfs(
    AxumPath(cid): AxumPath<String>,
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {
    let cid: BeenetCid = match cid.parse() {
        Ok(cid) => cid,
        Err(err) => {
            return (StatusCode::BAD_REQUEST, format!("invalid cid: {err}")).into_response()
        }
    };

    let req = InvokeRequest {
        request_id: Uuid::new_v4().to_string(),
        cid,
        input: body.to_vec(),
        deadline_ms: state.default_deadline_ms,
        caller_peer: None,
        trace_parent: None,
    };

    match state.client.invoke(req).await {
        Ok(resp) => into_http_response(resp),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            format!("gateway invoke failed: {err}"),
        )
            .into_response(),
    }
}

fn into_http_response(resp: InvokeResponse) -> axum::response::Response {
    let status = match &resp.status {
        Status::Ok => StatusCode::OK,
        Status::BusinessError { http_status, .. } => {
            StatusCode::from_u16(*http_status).unwrap_or(StatusCode::BAD_REQUEST)
        }
        Status::RuntimeError { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        Status::LoadError { .. } => StatusCode::BAD_GATEWAY,
        Status::Timeout {
            stage: TimeoutStage::Gateway,
        } => StatusCode::GATEWAY_TIMEOUT,
        Status::Timeout {
            stage: TimeoutStage::Exec,
        } => StatusCode::REQUEST_TIMEOUT,
        Status::Rejected { .. } => StatusCode::TOO_MANY_REQUESTS,
    };

    let mut builder = Response::builder().status(status);
    builder = builder.header("x-beenet-request-id", &resp.request_id);
    builder = builder.header("x-beenet-status", status_label(&resp.status));
    if !resp.stdout.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&STANDARD.encode(&resp.stdout)) {
            builder = builder.header("x-beenet-stdout-b64", v);
        }
    }
    if !resp.stderr.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&STANDARD.encode(&resp.stderr)) {
            builder = builder.header("x-beenet-stderr-b64", v);
        }
    }
    builder
        .body(axum::body::Body::from(resp.body))
        .unwrap()
        .into_response()
}

fn status_label(status: &Status) -> &'static str {
    match status {
        Status::Ok => "ok",
        Status::BusinessError { .. } => "business-error",
        Status::RuntimeError { .. } => "runtime-error",
        Status::LoadError { .. } => "load-error",
        Status::Timeout { .. } => "timeout",
        Status::Rejected { .. } => "rejected",
    }
}

#[derive(Debug, Deserialize)]
struct WorkersListBody {
    workers: Vec<WorkerListEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct WorkerListEntry {
    peer_id: String,
    #[serde(default)]
    last_seen_unix_ms: u64,
    #[serde(default)]
    supported_cids: Vec<String>,
    #[serde(default)]
    loaded_cids: Vec<String>,
}

#[derive(Serialize)]
struct GatewayHeartbeat {
    gateway_id: String,
    peer_id: String,
    public_key: String,
    timestamp_secs: u64,
    signature: String,
    dial_addr: String,
    region: Option<String>,
    capacity: u32,
    connected_workers: u32,
}

async fn gateway_heartbeat_loop(
    registry_url: String,
    gateway_id: String,
    region: Option<String>,
    capacity: u32,
    dial_addr: String,
    keypair: identity::Keypair,
    connected: Arc<RwLock<HashSet<PeerId>>>,
) {
    let client = reqwest::Client::new();
    let url = format!(
        "{}/v1/gateways/heartbeat",
        registry_url.trim_end_matches('/')
    );
    let peer_id = keypair.public().to_peer_id().to_string();
    let public_key = STANDARD.encode(keypair.public().encode_protobuf());
    let mut interval = tokio::time::interval(Duration::from_secs(20));
    loop {
        interval.tick().await;
        let timestamp_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or(0);
        let message = format!("{peer_id}\n{timestamp_secs}");
        let signature = match keypair.sign(message.as_bytes()) {
            Ok(value) => STANDARD.encode(value),
            Err(error) => {
                warn!(%error, "gateway heartbeat signing failed");
                continue;
            }
        };
        let body = GatewayHeartbeat {
            gateway_id: gateway_id.clone(),
            peer_id: peer_id.clone(),
            public_key: public_key.clone(),
            timestamp_secs,
            signature,
            dial_addr: dial_addr.clone(),
            region: region.clone(),
            capacity,
            connected_workers: connected.read().await.len() as u32,
        };
        match client.post(&url).json(&body).send().await {
            Ok(response) if response.status().is_success() => {
                info!(%gateway_id, "gateway lease renewed")
            }
            Ok(response) => warn!(status = %response.status(), "gateway heartbeat rejected"),
            Err(error) => warn!(%error, "gateway heartbeat failed"),
        }
    }
}

async fn registry_poll_loop(
    registry_base: String,
    period_ms: u64,
    out: Arc<RwLock<Vec<WorkerListEntry>>>,
) {
    let client = reqwest::Client::new();
    let url = format!("{}/v1/workers", registry_base.trim_end_matches('/'));
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(period_ms.max(500)));
    loop {
        tick.tick().await;
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<WorkersListBody>().await {
                Ok(body) => {
                    let parsed: Vec<WorkerListEntry> = body
                        .workers
                        .into_iter()
                        .filter(|w| PeerId::from_str(&w.peer_id).is_ok())
                        .collect();
                    let mut guard = out.write().await;
                    if guard.len() != parsed.len()
                        || guard.iter().zip(parsed.iter()).any(|(a, b)| a != b)
                    {
                        info!(count = parsed.len(), "registry worker list updated");
                    }
                    *guard = parsed;
                }
                Err(e) => warn!(error = %e, "registry JSON decode failed"),
            },
            Ok(resp) => warn!(status = %resp.status(), "registry GET /v1/workers failed"),
            Err(e) => warn!(error = %e, "registry poll request error"),
        }
    }
}

async fn run_swarm_loop(
    worker_addrs: Arc<RwLock<Vec<WorkerListEntry>>>,
    connected: Arc<RwLock<HashSet<PeerId>>>,
    mut cmd_rx: mpsc::Receiver<Command>,
    mut swarm: libp2p::Swarm<GatewayBehaviour>,
) {
    let rr = AtomicUsize::new(0);
    let mut pending: HashMap<request_response::OutboundRequestId, oneshot::Sender<InvokeResponse>> =
        HashMap::new();

    loop {
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                let workers = worker_addrs.read().await.clone();
                let connected_snap = connected.read().await.clone();
                let mut candidates: Vec<(PeerId, WorkerListEntry)> = workers
                    .iter()
                    .filter_map(|w| {
                        let peer = PeerId::from_str(&w.peer_id).ok()?;
                        if !connected_snap.contains(&peer) {
                            return None;
                        }
                        let cid_ok = w.supported_cids.is_empty()
                            || w.supported_cids.iter().any(|cid| cid == &cmd.req.cid.to_string());
                        cid_ok.then(|| (peer, w.clone()))
                    })
                    .collect();
                if candidates.is_empty() {
                    // Fall back to any connected registered worker (ignore CID hint miss).
                    candidates = workers
                        .iter()
                        .filter_map(|w| {
                            let peer = PeerId::from_str(&w.peer_id).ok()?;
                            connected_snap.contains(&peer).then(|| (peer, w.clone()))
                        })
                        .collect();
                }
                if candidates.is_empty() {
                    let reason = if workers.is_empty() {
                        "no workers with an active registry lease (empty list or registry unreachable)".into()
                    } else {
                        format!(
                            "no connected worker (registry has {}, connected={})",
                            workers.len(),
                            connected_snap.len()
                        )
                    };
                    let _ = cmd.respond_to.send(InvokeResponse {
                        request_id: cmd.req.request_id,
                        status: Status::LoadError {
                            stage: LoadStage::Fetch,
                            reason,
                        },
                        body: Vec::new(),
                        stdout: String::new(),
                        stderr: String::new(),
                        usage: Usage::default(),
                    });
                    continue;
                }
                let idx = rr.fetch_add(1, Ordering::Relaxed) % candidates.len();
                let (worker_peer, _) = candidates[idx].clone();
                info!(%worker_peer, "invoke on existing reverse connection");
                let id = swarm
                    .behaviour_mut()
                    .request_response
                    .send_request(&worker_peer, cmd.req);
                pending.insert(id, cmd.respond_to);
            }
            event = swarm.select_next_some() => match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    info!(%address, "gateway listening");
                }
                SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                    let n = {
                        let mut guard = connected.write().await;
                        guard.insert(peer_id);
                        guard.len()
                    };
                    info!(%peer_id, ?endpoint, connected = n, "worker connected");
                }
                SwarmEvent::ConnectionClosed { peer_id, .. } => {
                    let n = {
                        let mut guard = connected.write().await;
                        guard.remove(&peer_id);
                        guard.len()
                    };
                    info!(%peer_id, connected = n, "worker disconnected");
                }
                SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                    warn!(?peer_id, ?error, "outgoing connection error");
                }
                SwarmEvent::IncomingConnectionError { local_addr, send_back_addr, error, .. } => {
                    if is_lb_health_probe_addr(&send_back_addr) {
                        debug!(%local_addr, %send_back_addr, ?error, "lb health-check probe (ignored)");
                    } else {
                        warn!(%local_addr, %send_back_addr, ?error, "incoming connection error");
                    }
                }
                SwarmEvent::Behaviour(GatewayBehaviourEvent::RequestResponse(
                    request_response::Event::Message { message, .. }
                )) => {
                    if let request_response::Message::Response { request_id, response } = message {
                        if let Some(tx) = pending.remove(&request_id) {
                            let _ = tx.send(response);
                        }
                    }
                }
                SwarmEvent::Behaviour(GatewayBehaviourEvent::RequestResponse(
                    request_response::Event::OutboundFailure { request_id, error, .. }
                )) => {
                    if let Some(tx) = pending.remove(&request_id) {
                        let _ = tx.send(InvokeResponse {
                            request_id: "unknown".into(),
                            status: Status::RuntimeError {
                                reason: format!("outbound failure: {error}"),
                            },
                            body: Vec::new(),
                            stdout: String::new(),
                            stderr: String::new(),
                            usage: Usage::default(),
                        });
                    }
                }
                SwarmEvent::Behaviour(GatewayBehaviourEvent::RequestResponse(
                    request_response::Event::InboundFailure { error, .. }
                )) => {
                    warn!("inbound failure: {error}");
                }
                SwarmEvent::Behaviour(GatewayBehaviourEvent::Identify(event)) => {
                    info!("identify: {:?}", event);
                }
                SwarmEvent::Behaviour(GatewayBehaviourEvent::Ping(event)) => {
                    info!("ping: {:?}", event);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_from_str_ok() {
        let key = identity::Keypair::generate_ed25519();
        let peer = PeerId::from(key.public());
        assert_eq!(PeerId::from_str(&peer.to_string()).unwrap(), peer);
    }
}
