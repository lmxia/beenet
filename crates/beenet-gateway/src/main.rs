use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, Result};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use beenet_common::{BeenetCid, INVOKE_PROTOCOL};
use beenet_proto::{InvokeRequest, InvokeResponse, Status, TimeoutStage, Usage};
use clap::Parser;
use libp2p::futures::StreamExt;
use libp2p::core::multiaddr::Protocol;
use libp2p::request_response::{self, ProtocolSupport};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, ping, Multiaddr, PeerId, StreamProtocol, SwarmBuilder};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Parser, Debug, Clone)]
#[command(name = "beenet-gateway", about = "Beenet M1 gateway")]
struct Args {
    #[arg(long, env = "BEENET_GATEWAY_ADDR", default_value = "127.0.0.1:8080")]
    http_addr: std::net::SocketAddr,

    #[arg(long, env = "BEENET_WORKER_ADDR")]
    worker_addr: Multiaddr,

    #[arg(long, env = "BEENET_DEFAULT_DEADLINE_MS", default_value_t = 10_000)]
    default_deadline_ms: u32,
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

    let args = Args::parse();
    let local_key = identity::Keypair::generate_ed25519();
    let swarm = build_swarm(local_key)?;
    let (tx, rx) = mpsc::channel(32);

    tokio::spawn(run_swarm_loop(args.worker_addr.clone(), rx, swarm));

    let state = AppState {
        client: GatewayClient { tx },
        default_deadline_ms: args.default_deadline_ms,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/run/ipfs/:cid", post(run_ipfs))
        .with_state(state);

    info!(http_addr = %args.http_addr, worker_addr = %args.worker_addr, "gateway started");
    let listener = tokio::net::TcpListener::bind(args.http_addr).await?;
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

async fn run_swarm_loop(
    worker_addr: Multiaddr,
    mut cmd_rx: mpsc::Receiver<Command>,
    mut swarm: libp2p::Swarm<GatewayBehaviour>,
) {
    let worker_peer = match peer_id_from_multiaddr(&worker_addr) {
        Ok(p) => p,
        Err(err) => {
            warn!("invalid worker multiaddr `{worker_addr}`: {err}");
            return;
        }
    };

    let mut pending: HashMap<request_response::OutboundRequestId, oneshot::Sender<InvokeResponse>> =
        HashMap::new();

    loop {
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
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
