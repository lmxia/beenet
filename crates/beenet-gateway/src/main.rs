use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Result};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::header::HeaderValue;
use axum::http::{Response, StatusCode};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use beenet_common::config::{
    load_file, resolve_config_path_with_cli, resolve_gateway_settings_optional_file,
    GatewayCliOverrides,
};
use beenet_common::{BeenetCid, INVOKE_PROTOCOL};
use beenet_proto::{InvokeRequest, InvokeResponse, LoadStage, Status, TimeoutStage, Usage};
use clap::Parser;
use libp2p::futures::StreamExt;
use libp2p::core::multiaddr::Protocol;
use libp2p::request_response::{self, ProtocolSupport};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, ping, Multiaddr, PeerId, StreamProtocol, SwarmBuilder};
use serde::Deserialize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{info, warn};
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
            .send(Command { req, respond_to: tx })
            .await
            .map_err(|_| anyhow!("gateway event loop stopped"))?;
        rx.await.map_err(|_| anyhow!("gateway response channel closed"))
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
        .build();
    Ok(swarm)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cli = Args::parse();
    let path = resolve_config_path_with_cli(cli.config.clone(), &argv);
    let overrides = GatewayCliOverrides {
        http_addr: cli.http_addr.clone(),
        registry_url: cli.registry_url.clone(),
        registry_poll_ms: cli.registry_poll_ms,
        default_deadline_ms: cli.default_deadline_ms,
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

    let local_key = identity::Keypair::generate_ed25519();
    let swarm = build_swarm(local_key)?;
    let (tx, rx) = mpsc::channel(32);

    tokio::spawn(run_swarm_loop(worker_addrs.clone(), rx, swarm));

    let state = AppState {
        client: GatewayClient { tx },
        default_deadline_ms: settings.default_deadline_ms,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/run/ipfs/:cid", post(run_ipfs))
        .with_state(state);

    info!(
        http_addr = %settings.http_addr,
        registry_url = %settings.registry_url,
        poll_ms = settings.registry_poll_ms,
        "gateway started"
    );
    let listener = tokio::net::TcpListener::bind(settings.http_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

async fn run_ipfs(
    Path(cid): Path<String>,
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {
    let cid: BeenetCid = match cid.parse() {
        Ok(cid) => cid,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid cid: {err}"),
            )
                .into_response()
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

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct WorkerListEntry {
    peer_id: String,
    dial_multiaddr: String,
    #[serde(default)]
    supported_cids: Vec<String>,
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
                        .filter(|w| w.dial_multiaddr.parse::<Multiaddr>().is_ok())
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
                let mut candidates: Vec<WorkerListEntry> = workers
                    .iter()
                    .filter(|w| {
                        w.supported_cids.is_empty() || w.supported_cids.iter().any(|cid| cid == &cmd.req.cid.to_string())
                    })
                    .cloned()
                    .collect();
                if candidates.is_empty() {
                    candidates = workers;
                }
                if candidates.is_empty() {
                    let _ = cmd.respond_to.send(InvokeResponse {
                        request_id: cmd.req.request_id,
                        status: Status::LoadError {
                            stage: LoadStage::Fetch,
                            reason: "no workers with an active registry lease (empty list or registry unreachable)".into(),
                        },
                        body: Vec::new(),
                        stdout: String::new(),
                        stderr: String::new(),
                        usage: Usage::default(),
                    });
                    continue;
                }
                let idx = rr.fetch_add(1, Ordering::Relaxed) % candidates.len();
                let worker = candidates[idx].clone();
                let worker_addr: Multiaddr = match worker.dial_multiaddr.parse() {
                    Ok(addr) => addr,
                    Err(err) => {
                        let _ = cmd.respond_to.send(InvokeResponse {
                            request_id: cmd.req.request_id,
                            status: Status::RuntimeError {
                                reason: format!("invalid worker multiaddr in registry: {err}"),
                            },
                            body: Vec::new(),
                            stdout: String::new(),
                            stderr: String::new(),
                            usage: Usage::default(),
                        });
                        continue;
                    }
                };
                let worker_peer = match peer_id_from_multiaddr(&worker_addr) {
                    Ok(p) => p,
                    Err(err) => {
                        let _ = cmd.respond_to.send(InvokeResponse {
                            request_id: cmd.req.request_id,
                            status: Status::RuntimeError {
                                reason: format!("invalid worker multiaddr in registry: {err}"),
                            },
                            body: Vec::new(),
                            stdout: String::new(),
                            stderr: String::new(),
                            usage: Usage::default(),
                        });
                        continue;
                    }
                };
                if let Err(err) = swarm.dial(worker_addr.clone()) {
                    warn!("dial failed: {err}");
                }
                let id = swarm.behaviour_mut().request_response.send_request(&worker_peer, cmd.req);
                pending.insert(id, cmd.respond_to);
            }
            event = swarm.select_next_some() => match event {
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
                _ => {}
            }
        }
    }
}

fn peer_id_from_multiaddr(addr: &Multiaddr) -> Result<PeerId> {
    for proto in addr.iter() {
        if let Protocol::P2p(peer_id) = proto {
            return Ok(peer_id);
        }
    }
    Err(anyhow!("worker multiaddr is missing /p2p/<peer-id>"))
}
