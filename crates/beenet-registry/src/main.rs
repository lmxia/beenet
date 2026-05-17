//! Beenet Registry — HTTP control plane: workers **heartbeat** here to join and renew a dial-address lease.
//!
//! - `POST /v1/workers/heartbeat` — same payload on every tick: `{ join_token, peer_id, dial_multiaddr }` (upsert + bump `last_seen`)
//! - `GET /v1/workers` — JSON list of workers whose lease is still fresh (Gateway polling)

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use beenet_common::config::{
    load_file, resolve_config_path_with_cli, resolve_registry_settings, RegistryCliOverrides,
};
use clap::Parser;
use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Workers whose heartbeat lease expired within this window are omitted from `GET /v1/workers` and purged from memory.
const STALE_AFTER: Duration = Duration::from_secs(60);

#[derive(Parser, Debug)]
#[command(name = "beenet-registry", about = "Beenet HTTP registry (worker heartbeats + gateway discovery)")]
struct Args {
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    #[arg(long)]
    http_addr: Option<String>,

    #[arg(long)]
    join_token: Option<String>,
}

#[derive(Clone)]
struct AppState {
    join_token: String,
    workers: Arc<RwLock<HashMap<PeerId, WorkerRecord>>>,
}

#[derive(Clone, Debug)]
struct WorkerRecord {
    dial_multiaddr: String,
    last_seen: Instant,
}

#[derive(Debug, Deserialize)]
struct HeartbeatBody {
    join_token: String,
    peer_id: String,
    dial_multiaddr: String,
}

#[derive(Debug, Serialize)]
struct HeartbeatOkResponse {
    ok: bool,
}

#[derive(Debug, Serialize)]
struct WorkersResponse {
    workers: Vec<WorkerView>,
}

#[derive(Debug, Serialize)]
struct WorkerView {
    peer_id: String,
    dial_multiaddr: String,
    last_seen_unix_ms: u128,
}

fn validate_registration(peer_id: &str, dial: &str) -> Result<(PeerId, Multiaddr)> {
    let pid = PeerId::from_str(peer_id).map_err(|e| anyhow!("invalid peer_id: {e}"))?;
    let ma: Multiaddr = dial
        .parse()
        .map_err(|e| anyhow!("invalid dial_multiaddr: {e}"))?;
    let mut found = None;
    for p in ma.iter() {
        if let Protocol::P2p(tail) = p {
            found = Some(tail);
            break;
        }
    }
    let tail = found.ok_or_else(|| anyhow!("dial_multiaddr missing /p2p/<peer-id>"))?;
    if tail != pid {
        return Err(anyhow!("peer_id does not match /p2p record in dial_multiaddr"));
    }
    Ok((pid, ma))
}

async fn post_heartbeat(
    State(state): State<AppState>,
    Json(body): Json<HeartbeatBody>,
) -> impl IntoResponse {
    if body.join_token != state.join_token {
        return (StatusCode::UNAUTHORIZED, "invalid join_token").into_response();
    }
    match validate_registration(&body.peer_id, &body.dial_multiaddr) {
        Ok((pid, _)) => {
            let now = Instant::now();
            let mut map = state.workers.write().await;
            map.insert(
                pid,
                WorkerRecord {
                    dial_multiaddr: body.dial_multiaddr,
                    last_seen: now,
                },
            );
            info!(peer_id = %pid, "worker heartbeat ok (lease renewed)");
            (StatusCode::OK, Json(HeartbeatOkResponse { ok: true })).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

async fn get_workers(State(state): State<AppState>) -> impl IntoResponse {
    let map = state.workers.read().await;
    let now = Instant::now();
    let mut workers = Vec::new();
    for (pid, rec) in map.iter() {
        if now.duration_since(rec.last_seen) <= STALE_AFTER {
            let elapsed_ms = now
                .duration_since(rec.last_seen)
                .as_millis();
            // Approximate last seen as (now - elapsed) for clients that want a wall clock hint.
            let last_seen_unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis().saturating_sub(elapsed_ms))
                .unwrap_or(0);
            workers.push(WorkerView {
                peer_id: pid.to_string(),
                dial_multiaddr: rec.dial_multiaddr.clone(),
                last_seen_unix_ms,
            });
        }
    }
    Json(WorkersResponse { workers })
}

async fn health() -> &'static str {
    "ok"
}

async fn purge_loop(state: AppState) {
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    loop {
        tick.tick().await;
        let now = Instant::now();
        let mut map = state.workers.write().await;
        let before = map.len();
        map.retain(|_, rec| now.duration_since(rec.last_seen) <= STALE_AFTER);
        let removed = before.saturating_sub(map.len());
        if removed > 0 {
            warn!(removed, "purged workers with expired heartbeat lease");
        }
    }
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
    if !path.exists() {
        return Err(anyhow!(
            "missing config file `{}` (add [registry] or pass --config)",
            path.display()
        ));
    }
    let file_cfg = load_file(&path)?;
    let overrides = RegistryCliOverrides {
        http_addr: cli.http_addr.clone(),
        join_token: cli.join_token.clone(),
    };
    let settings = resolve_registry_settings(&file_cfg, &overrides)?;
    if settings.join_token.trim().is_empty() {
        return Err(anyhow!("config [registry].join_token must be non-empty"));
    }

    let state = AppState {
        join_token: settings.join_token.clone(),
        workers: Arc::new(RwLock::new(HashMap::new())),
    };

    tokio::spawn(purge_loop(state.clone()));

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/workers/heartbeat", post(post_heartbeat))
        .route("/v1/workers", get(get_workers))
        .with_state(state);

    info!(
        http_addr = %settings.http_addr,
        "beenet-registry listening (worker heartbeats: POST /v1/workers/heartbeat; gateway: GET /v1/workers)"
    );

    let listener = tokio::net::TcpListener::bind(settings.http_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
