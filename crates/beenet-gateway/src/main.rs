use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
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

    /// Interval (ms) to refresh connected-peer metadata from Registry lookup.
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

    /// Display name in Registry/Dashboard (alias: `--name`). Duplicates allowed.
    #[arg(long = "gateway-id", visible_alias = "name")]
    gateway_id: Option<String>,

    #[arg(long)]
    region: Option<String>,

    #[arg(long)]
    capacity: Option<u32>,

    /// Bootstrap join token. Prefer --join-token-stdin or --join-token-file.
    #[arg(long)]
    join_token: Option<String>,

    /// Read the bootstrap join token from a temporary secret file.
    #[arg(long, value_name = "PATH")]
    join_token_file: Option<PathBuf>,

    /// Read the bootstrap join token from stdin.
    #[arg(long)]
    join_token_stdin: bool,
}

fn trimmed_token(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn bootstrap_token(cli: &Args) -> Result<Option<String>> {
    let explicit_sources = cli.join_token.is_some() as usize
        + cli.join_token_file.is_some() as usize
        + cli.join_token_stdin as usize;
    if explicit_sources > 1 {
        anyhow::bail!("use only one of --join-token, --join-token-file, or --join-token-stdin");
    }
    if let Some(token) = cli.join_token.clone().and_then(trimmed_token) {
        warn!("--join-token may expose the bootstrap token; prefer stdin or a secret file");
        return Ok(Some(token));
    }
    if let Some(path) = cli.join_token_file.as_ref() {
        let token = fs::read_to_string(path)
            .with_context(|| format!("read join token file `{}`", path.display()))?;
        return Ok(trimmed_token(token));
    }
    if cli.join_token_stdin {
        let mut token = String::new();
        io::stdin()
            .read_to_string(&mut token)
            .context("read join token from stdin")?;
        return Ok(trimmed_token(token));
    }
    Ok(None)
}

fn unix_secs_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn make_signature(keypair: &identity::Keypair, peer_id: &str, timestamp_secs: u64) -> Result<String> {
    let message = format!("{peer_id}\n{timestamp_secs}");
    let sig = keypair
        .sign(message.as_bytes())
        .map_err(|e| anyhow!("sign failed: {e}"))?;
    Ok(STANDARD.encode(sig))
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
        gateway_id: cli.gateway_id.clone(),
        region: cli.region.clone(),
        capacity: cli.capacity,
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

    let worker_cache: Arc<RwLock<HashMap<PeerId, WorkerListEntry>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let connected: Arc<RwLock<HashSet<PeerId>>> = Arc::new(RwLock::new(HashSet::new()));
    let http = reqwest::Client::new();
    let bootstrap = bootstrap_token(&cli)?;

    let local_key = load_or_create_keypair(&settings.identity_key_path)?;
    let local_peer_id = local_key.public().to_peer_id();
    let keypair = Arc::new(local_key);
    let identity_dir = settings
        .identity_key_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let gateway_id = beenet_common::display_name::resolve_persistent_display_name(
        &identity_dir,
        settings.gateway_id.as_deref(),
    )?;
    let mut swarm = build_swarm((*keypair).clone())?;
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

    ensure_gateway_registered(
        &http,
        &settings.registry_url,
        &keypair,
        &local_peer_id.to_string(),
        &gateway_id,
        settings.region.as_deref(),
        &dial_addr,
        bootstrap.as_deref(),
    )
    .await?;
    drop(bootstrap);

    tokio::spawn(gateway_heartbeat_loop(
        settings.registry_url.clone(),
        gateway_id.clone(),
        settings.region.clone(),
        settings.capacity,
        dial_addr,
        keypair.clone(),
        connected.clone(),
    ));

    tokio::spawn(registry_peer_refresh_loop(
        settings.registry_url.clone(),
        settings.registry_poll_ms,
        http.clone(),
        keypair.clone(),
        connected.clone(),
        worker_cache.clone(),
    ));

    tokio::spawn(run_swarm_loop(
        settings.registry_url.clone(),
        http,
        keypair,
        worker_cache,
        connected,
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
        .with_state(state)
        .layer(middleware::from_fn(cors_middleware));

    let listener = tokio::net::TcpListener::bind(settings.http_addr)
        .await
        .with_context(|| format!("bind gateway http `{}`", settings.http_addr))?;
    info!(
        peer_id = %local_peer_id,
        gateway_id = %gateway_id,
        http_addr = %settings.http_addr,
        registry_url = %settings.registry_url,
        peer_refresh_ms = settings.registry_poll_ms,
        libp2p_listen_addr = %settings.libp2p_listen_addr,
        public_addr = ?settings.public_addr,
        identity_key_path = %settings.identity_key_path.display(),
        "gateway started (workers dial in; invoke reuses inbound connections)"
    );
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
struct WorkersLookupRequest {
    peer_id: String,
    timestamp_secs: u64,
    signature: String,
    peer_ids: Vec<String>,
}

#[derive(Serialize)]
struct GatewayJoinBody {
    join_token: String,
    peer_id: String,
    public_key: String,
    timestamp_secs: u64,
    signature: String,
    gateway_id: String,
    region: Option<String>,
}

#[derive(Serialize)]
struct GatewayHeartbeat {
    gateway_id: String,
    peer_id: String,
    timestamp_secs: u64,
    signature: String,
    dial_addr: String,
    region: Option<String>,
    capacity: u32,
    connected_workers: u32,
}

const MAX_LOOKUP_PEER_IDS: usize = 256;

async fn do_gateway_join(
    http: &reqwest::Client,
    registry_url: &str,
    keypair: &identity::Keypair,
    peer_id: &str,
    gateway_id: &str,
    region: Option<&str>,
    join_token: &str,
) -> Result<()> {
    let ts = unix_secs_now();
    let sig = make_signature(keypair, peer_id, ts)?;
    let body = GatewayJoinBody {
        join_token: join_token.to_owned(),
        peer_id: peer_id.to_owned(),
        public_key: STANDARD.encode(keypair.public().encode_protobuf()),
        timestamp_secs: ts,
        signature: sig,
        gateway_id: gateway_id.to_owned(),
        region: region.map(str::to_owned),
    };
    let url = format!("{}/v1/gateways/join", registry_url.trim_end_matches('/'));
    let resp = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if status.is_success() {
        info!(%peer_id, "gateway registered with registry");
        return Ok(());
    }
    let text = resp.text().await.unwrap_or_default();
    anyhow::bail!("gateway join rejected (HTTP {status}): {text}")
}

/// Probe heartbeat; on 401 try join then succeed. Returns Err if enrollment impossible.
async fn ensure_gateway_registered(
    http: &reqwest::Client,
    registry_url: &str,
    keypair: &identity::Keypair,
    peer_id: &str,
    gateway_id: &str,
    region: Option<&str>,
    dial_addr: &str,
    join_token: Option<&str>,
) -> Result<()> {
    let url = format!(
        "{}/v1/gateways/heartbeat",
        registry_url.trim_end_matches('/')
    );
    let ts = unix_secs_now();
    let sig = make_signature(keypair, peer_id, ts)?;
    let probe = GatewayHeartbeat {
        gateway_id: gateway_id.to_owned(),
        peer_id: peer_id.to_owned(),
        timestamp_secs: ts,
        signature: sig,
        dial_addr: dial_addr.to_owned(),
        region: region.map(str::to_owned),
        capacity: 1,
        connected_workers: 0,
    };
    let resp = http
        .post(&url)
        .json(&probe)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if status.is_success() {
        info!(%peer_id, "gateway already registered with registry");
        return Ok(());
    }
    if status != reqwest::StatusCode::UNAUTHORIZED {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("gateway heartbeat probe failed (HTTP {status}): {text}");
    }
    let Some(token) = join_token else {
        anyhow::bail!(
            "gateway is not registered; provide --join-token-file / --join-token-stdin / --join-token"
        );
    };
    do_gateway_join(http, registry_url, keypair, peer_id, gateway_id, region, token).await
}

async fn gateway_heartbeat_loop(
    registry_url: String,
    gateway_id: String,
    region: Option<String>,
    capacity: u32,
    dial_addr: String,
    keypair: Arc<identity::Keypair>,
    connected: Arc<RwLock<HashSet<PeerId>>>,
) {
    let client = reqwest::Client::new();
    let url = format!(
        "{}/v1/gateways/heartbeat",
        registry_url.trim_end_matches('/')
    );
    let peer_id = keypair.public().to_peer_id().to_string();
    let mut interval = tokio::time::interval(Duration::from_secs(20));
    loop {
        interval.tick().await;
        let timestamp_secs = unix_secs_now();
        let signature = match make_signature(&keypair, &peer_id, timestamp_secs) {
            Ok(value) => value,
            Err(error) => {
                warn!(%error, "gateway heartbeat signing failed");
                continue;
            }
        };
        let body = GatewayHeartbeat {
            gateway_id: gateway_id.clone(),
            peer_id: peer_id.clone(),
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

async fn lookup_workers(
    client: &reqwest::Client,
    registry_base: &str,
    keypair: &identity::Keypair,
    peer_ids: &[PeerId],
) -> Result<HashMap<PeerId, WorkerListEntry>> {
    if peer_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let url = format!(
        "{}/v1/workers/lookup",
        registry_base.trim_end_matches('/')
    );
    let gateway_peer_id = keypair.public().to_peer_id().to_string();
    let mut out = HashMap::new();
    for chunk in peer_ids.chunks(MAX_LOOKUP_PEER_IDS) {
        let timestamp_secs = unix_secs_now();
        let signature = make_signature(keypair, &gateway_peer_id, timestamp_secs)?;
        let body = WorkersLookupRequest {
            peer_id: gateway_peer_id.clone(),
            timestamp_secs,
            signature,
            peer_ids: chunk.iter().map(|p| p.to_string()).collect(),
        };
        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("registry workers lookup request")?;
        if !resp.status().is_success() {
            anyhow::bail!("registry workers lookup HTTP {}", resp.status());
        }
        let parsed: WorkersListBody = resp
            .json()
            .await
            .context("registry workers lookup JSON decode")?;
        for entry in parsed.workers {
            if let Ok(peer) = PeerId::from_str(&entry.peer_id) {
                out.insert(peer, entry);
            }
        }
    }
    Ok(out)
}

async fn apply_lookup_to_cache(
    cache: &Arc<RwLock<HashMap<PeerId, WorkerListEntry>>>,
    connected: &Arc<RwLock<HashSet<PeerId>>>,
    requested: &[PeerId],
    found: HashMap<PeerId, WorkerListEntry>,
) {
    let still_connected = connected.read().await.clone();
    let mut guard = cache.write().await;
    for peer in requested {
        if !still_connected.contains(peer) {
            guard.remove(peer);
            continue;
        }
        match found.get(peer) {
            Some(entry) => {
                guard.insert(*peer, entry.clone());
            }
            None => {
                guard.remove(peer);
            }
        }
    }
    guard.retain(|peer, _| still_connected.contains(peer));
}

async fn registry_peer_refresh_loop(
    registry_base: String,
    period_ms: u64,
    client: reqwest::Client,
    keypair: Arc<identity::Keypair>,
    connected: Arc<RwLock<HashSet<PeerId>>>,
    cache: Arc<RwLock<HashMap<PeerId, WorkerListEntry>>>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(period_ms.max(500)));
    loop {
        tick.tick().await;
        let peers: Vec<PeerId> = connected.read().await.iter().copied().collect();
        if peers.is_empty() {
            let mut guard = cache.write().await;
            if !guard.is_empty() {
                guard.clear();
            }
            continue;
        }
        match lookup_workers(&client, &registry_base, &keypair, &peers).await {
            Ok(found) => {
                let before = cache.read().await.len();
                apply_lookup_to_cache(&cache, &connected, &peers, found).await;
                let after = cache.read().await.len();
                if before != after {
                    info!(
                        connected = peers.len(),
                        cached = after,
                        "registry connected-peer metadata refreshed"
                    );
                }
            }
            Err(e) => warn!(error = %e, "registry connected-peer lookup failed"),
        }
    }
}

fn spawn_peer_lookup(
    registry_base: String,
    client: reqwest::Client,
    keypair: Arc<identity::Keypair>,
    connected: Arc<RwLock<HashSet<PeerId>>>,
    cache: Arc<RwLock<HashMap<PeerId, WorkerListEntry>>>,
    peer_id: PeerId,
) {
    tokio::spawn(async move {
        match lookup_workers(&client, &registry_base, &keypair, &[peer_id]).await {
            Ok(found) => {
                apply_lookup_to_cache(&cache, &connected, &[peer_id], found).await;
            }
            Err(e) => warn!(%peer_id, error = %e, "registry peer lookup on connect failed"),
        }
    });
}

async fn run_swarm_loop(
    registry_base: String,
    http: reqwest::Client,
    keypair: Arc<identity::Keypair>,
    worker_cache: Arc<RwLock<HashMap<PeerId, WorkerListEntry>>>,
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
                let cache = worker_cache.read().await;
                let connected_snap = connected.read().await;
                let mut candidates: Vec<(PeerId, WorkerListEntry)> = connected_snap
                    .iter()
                    .filter_map(|peer| {
                        let entry = cache.get(peer)?;
                        let cid_ok = entry.supported_cids.is_empty()
                            || entry
                                .supported_cids
                                .iter()
                                .any(|cid| cid == &cmd.req.cid.to_string());
                        cid_ok.then(|| (*peer, entry.clone()))
                    })
                    .collect();
                if candidates.is_empty() {
                    // Fall back to any connected registered worker (ignore CID hint miss).
                    candidates = connected_snap
                        .iter()
                        .filter_map(|peer| cache.get(peer).map(|entry| (*peer, entry.clone())))
                        .collect();
                }
                if candidates.is_empty() {
                    let reason = if connected_snap.is_empty() {
                        "no connected workers".into()
                    } else if cache.is_empty() {
                        format!(
                            "no connected worker with an active registry lease (connected={})",
                            connected_snap.len()
                        )
                    } else {
                        format!(
                            "no eligible connected worker (connected={}, cached={})",
                            connected_snap.len(),
                            cache.len()
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
                    spawn_peer_lookup(
                        registry_base.clone(),
                        http.clone(),
                        keypair.clone(),
                        connected.clone(),
                        worker_cache.clone(),
                        peer_id,
                    );
                }
                SwarmEvent::ConnectionClosed { peer_id, .. } => {
                    let n = {
                        let mut guard = connected.write().await;
                        guard.remove(&peer_id);
                        guard.len()
                    };
                    worker_cache.write().await.remove(&peer_id);
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
