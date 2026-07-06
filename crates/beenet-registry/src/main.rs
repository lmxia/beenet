//! Beenet Registry — HTTP control plane.
//!
//! ## Authentication layers
//!
//! | Layer | Secret | Purpose |
//! |-------|--------|---------|
//! | Admin token | random UUID printed at startup | protect `/v1/admin/*` CRUD |
//! | Join token | admin-issued UUID with optional TTL | one-time gate for worker registration |
//! | Ed25519 keypair | worker-held private key | sign every heartbeat; registry verifies with stored pubkey |
//!
//! ## Persistence
//! Worker registrations are stored in Redis (`beenet:registrations` Hash).
//! The registry is stateless otherwise — it can be restarted or scaled without losing
//! registered worker identities.
//!
//! ## Endpoints
//! | Method | Path | Auth | Description |
//! |--------|------|------|-------------|
//! | POST | `/v1/workers/join` | join token + sig | register a new worker |
//! | POST | `/v1/workers/heartbeat` | sig | renew lease |
//! | GET  | `/v1/workers` | none | gateway discovery |
//! | POST | `/v1/admin/tokens` | admin | create join token |
//! | GET  | `/v1/admin/tokens` | admin | list join tokens |
//! | DELETE | `/v1/admin/tokens/:id` | admin | revoke join token |
//! | GET  | `/v1/admin/registrations` | admin | list registered workers |
//! | DELETE | `/v1/admin/registrations/:peer_id` | admin | revoke worker registration |
//! | GET  | `/health` | none | liveness probe |

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD, Engine};
use beenet_common::config::DEFAULT_REGISTRY_HTTP_ADDR;
use clap::Parser;
use libp2p::multiaddr::Protocol;
use libp2p::{identity, Multiaddr, PeerId};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

/// Workers whose heartbeat lease expired within this window are pruned from memory.
const STALE_AFTER: Duration = Duration::from_secs(60);
/// Tolerance for clock skew between worker and registry when validating signed timestamps.
const SIGNATURE_WINDOW_SECS: u64 = 60;
/// Redis Hash key that stores all worker registrations.
const REDIS_REG_KEY: &str = "beenet:registrations";

// ── CLI ────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "beenet-registry", about = "Beenet HTTP registry")]
struct Args {
    /// HTTP listen address (default: 127.0.0.1:3030).
    #[arg(long)]
    http_addr: Option<String>,

    /// Redis URL for persisting worker registrations across restarts.
    /// Defaults to redis://127.0.0.1:6379.  In K8s, set to the Redis service URL.
    #[arg(long, default_value = "redis://127.0.0.1:6379")]
    redis_url: String,
}

// ── State ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    admin_token: String,
    join_tokens: Arc<RwLock<HashMap<String, JoinTokenRecord>>>,
    /// Workers that have successfully joined (public key stored here for sig verification).
    /// In-memory mirror of the Redis Hash — always kept in sync.
    registered: Arc<RwLock<HashMap<PeerId, RegistrationRecord>>>,
    /// Active workers with a live heartbeat lease (in-memory only; rebuilt from heartbeats).
    active: Arc<RwLock<HashMap<PeerId, ActiveRecord>>>,
    /// Async Redis connection (auto-reconnects; clone-safe).
    redis: redis::aio::ConnectionManager,
}

// ── Redis persistence ──────────────────────────────────────────────────────

/// JSON value stored per field in the Redis Hash.
#[derive(Serialize, Deserialize)]
struct RedisRegistration {
    /// base64(protobuf-encoded public key)
    public_key_b64: String,
    dial_multiaddr: String,
    registered_at_unix_ms: u64,
    #[serde(default)]
    supported_cids: Vec<String>,
}

/// Upsert one registration into Redis (`HSET beenet:registrations <peer_id> <json>`).
async fn redis_put(
    redis: &mut redis::aio::ConnectionManager,
    peer_id: &PeerId,
    rec: &RegistrationRecord,
) {
    let value = RedisRegistration {
        public_key_b64: STANDARD.encode(rec.public_key.encode_protobuf()),
        dial_multiaddr: rec.dial_multiaddr.clone(),
        registered_at_unix_ms: rec.registered_at_unix_ms,
        supported_cids: rec.supported_cids.clone(),
    };
    let json = match serde_json::to_string(&value) {
        Ok(j) => j,
        Err(e) => {
            warn!(peer_id = %peer_id, error = %e, "failed to serialize registration for Redis");
            return;
        }
    };
    if let Err(e) = redis
        .hset::<_, _, _, ()>(REDIS_REG_KEY, peer_id.to_string(), json)
        .await
    {
        warn!(peer_id = %peer_id, error = %e, "Redis HSET failed; registration not persisted");
    }
}

/// Delete one registration from Redis (`HDEL beenet:registrations <peer_id>`).
async fn redis_del(redis: &mut redis::aio::ConnectionManager, peer_id: &PeerId) {
    if let Err(e) = redis
        .hdel::<_, _, ()>(REDIS_REG_KEY, peer_id.to_string())
        .await
    {
        warn!(peer_id = %peer_id, error = %e, "Redis HDEL failed");
    }
}

/// Load all registrations from Redis on startup (`HGETALL beenet:registrations`).
async fn redis_load_all(
    redis: &mut redis::aio::ConnectionManager,
) -> HashMap<PeerId, RegistrationRecord> {
    let raw: HashMap<String, String> = match redis.hgetall(REDIS_REG_KEY).await {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "Redis HGETALL failed; starting with empty registration table");
            return HashMap::new();
        }
    };
    let mut out = HashMap::new();
    for (peer_id_str, json) in raw {
        let pid = match PeerId::from_str(&peer_id_str) {
            Ok(p) => p,
            Err(e) => {
                warn!(peer_id = %peer_id_str, error = %e, "skipping corrupt Redis registration");
                continue;
            }
        };
        let r: RedisRegistration = match serde_json::from_str(&json) {
            Ok(r) => r,
            Err(e) => {
                warn!(peer_id = %peer_id_str, error = %e, "skipping undeserializable Redis registration");
                continue;
            }
        };
        let pk_bytes = match STANDARD.decode(&r.public_key_b64) {
            Ok(b) => b,
            Err(e) => {
                warn!(peer_id = %peer_id_str, error = %e, "skipping registration with invalid base64 public key");
                continue;
            }
        };
        let public_key = match identity::PublicKey::try_decode_protobuf(&pk_bytes) {
            Ok(k) => k,
            Err(e) => {
                warn!(peer_id = %peer_id_str, error = %e, "skipping registration with undecodable public key");
                continue;
            }
        };
        out.insert(
            pid,
            RegistrationRecord {
                public_key,
                dial_multiaddr: r.dial_multiaddr,
                registered_at_unix_ms: r.registered_at_unix_ms,
                supported_cids: r.supported_cids,
            },
        );
    }
    info!(count = out.len(), "loaded worker registrations from Redis");
    out
}

// ── Domain types ───────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct JoinTokenRecord {
    id: String,
    description: String,
    token_value: String,
    created_at_unix_ms: u64,
    expires_at: Option<Instant>,
    expires_at_unix_ms: Option<u64>,
}

impl JoinTokenRecord {
    fn is_expired(&self) -> bool {
        self.expires_at.map_or(false, |exp| Instant::now() > exp)
    }
}

#[derive(Clone, Debug)]
struct RegistrationRecord {
    public_key: identity::PublicKey,
    dial_multiaddr: String,
    registered_at_unix_ms: u64,
    supported_cids: Vec<String>,
}

#[derive(Clone, Debug)]
struct ActiveRecord {
    dial_multiaddr: String,
    last_seen: Instant,
}

// ── Admin API types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CreateTokenBody {
    #[serde(default)]
    description: String,
    ttl_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
struct TokenView {
    id: String,
    description: String,
    token_value: String,
    created_at_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at_unix_ms: Option<u64>,
    expired: bool,
}

impl From<&JoinTokenRecord> for TokenView {
    fn from(r: &JoinTokenRecord) -> Self {
        TokenView {
            id: r.id.clone(),
            description: r.description.clone(),
            token_value: r.token_value.clone(),
            created_at_unix_ms: r.created_at_unix_ms,
            expires_at_unix_ms: r.expires_at_unix_ms,
            expired: r.is_expired(),
        }
    }
}

#[derive(Debug, Serialize)]
struct TokenListResponse {
    tokens: Vec<TokenView>,
}

#[derive(Debug, Serialize)]
struct RegistrationView {
    peer_id: String,
    dial_multiaddr: String,
    registered_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    supported_cids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RegistrationListResponse {
    registrations: Vec<RegistrationView>,
}

#[derive(Debug, Serialize)]
struct DeleteResponse {
    deleted: bool,
}

// ── Worker API types ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct JoinBody {
    join_token: String,
    peer_id: String,
    /// Protobuf-encoded public key, base64-encoded.
    public_key: String,
    dial_multiaddr: String,
    timestamp_secs: u64,
    /// Ed25519 signature over `"{peer_id}\n{dial_multiaddr}\n{timestamp_secs}"`, base64-encoded.
    signature: String,
    #[serde(default)]
    supported_cids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct JoinResponse {
    ok: bool,
    peer_id: String,
}

#[derive(Debug, Deserialize)]
struct HeartbeatBody {
    peer_id: String,
    dial_multiaddr: String,
    timestamp_secs: u64,
    /// Ed25519 signature over `"{peer_id}\n{dial_multiaddr}\n{timestamp_secs}"`, base64-encoded.
    signature: String,
    #[serde(default)]
    supported_cids: Vec<String>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    supported_cids: Vec<String>,
}

// ── Signature helpers ──────────────────────────────────────────────────────

fn signed_message(peer_id: &str, dial_multiaddr: &str, timestamp_secs: u64) -> Vec<u8> {
    format!("{peer_id}\n{dial_multiaddr}\n{timestamp_secs}").into_bytes()
}

fn verify_signature(
    pubkey: &identity::PublicKey,
    peer_id: &str,
    dial_multiaddr: &str,
    timestamp_secs: u64,
    sig_b64: &str,
) -> Result<()> {
    let sig = STANDARD
        .decode(sig_b64)
        .map_err(|_| anyhow!("signature is not valid base64"))?;
    let msg = signed_message(peer_id, dial_multiaddr, timestamp_secs);
    if pubkey.verify(&msg, &sig) {
        Ok(())
    } else {
        Err(anyhow!("signature verification failed"))
    }
}

fn check_timestamp(timestamp_secs: u64) -> Result<()> {
    let now = unix_secs_now();
    let diff = now.max(timestamp_secs) - now.min(timestamp_secs);
    if diff > SIGNATURE_WINDOW_SECS {
        Err(anyhow!(
            "timestamp out of window (skew {diff}s, max {SIGNATURE_WINDOW_SECS}s)"
        ))
    } else {
        Ok(())
    }
}

// ── Admin auth middleware ──────────────────────────────────────────────────

async fn admin_auth_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> impl IntoResponse {
    let authorized = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map_or(false, |t| t == state.admin_token);

    if !authorized {
        return (StatusCode::UNAUTHORIZED, "invalid or missing admin token").into_response();
    }
    next.run(request).await
}

// ── Admin: join token CRUD ─────────────────────────────────────────────────

async fn create_token(
    State(state): State<AppState>,
    Json(body): Json<CreateTokenBody>,
) -> impl IntoResponse {
    let now_instant = Instant::now();
    let now_unix_ms = unix_ms_now();
    let ttl_secs = body.ttl_secs.filter(|&t| t > 0);

    let (expires_at, expires_at_unix_ms) = match ttl_secs {
        Some(ttl) => (
            Some(now_instant + Duration::from_secs(ttl)),
            Some(now_unix_ms + ttl * 1000),
        ),
        None => (None, None),
    };

    let record = JoinTokenRecord {
        id: Uuid::new_v4().to_string(),
        description: body.description,
        token_value: Uuid::new_v4().to_string(),
        created_at_unix_ms: now_unix_ms,
        expires_at,
        expires_at_unix_ms,
    };
    let view = TokenView::from(&record);
    let id = record.id.clone();
    state.join_tokens.write().await.insert(id.clone(), record);
    info!(%id, ?ttl_secs, "join token created");
    (StatusCode::CREATED, Json(view)).into_response()
}

async fn list_tokens(State(state): State<AppState>) -> impl IntoResponse {
    let map = state.join_tokens.read().await;
    let mut tokens: Vec<TokenView> = map.values().map(TokenView::from).collect();
    tokens.sort_by_key(|t| t.created_at_unix_ms);
    Json(TokenListResponse { tokens })
}

async fn delete_token(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let deleted = state.join_tokens.write().await.remove(&id).is_some();
    if deleted {
        info!(%id, "join token revoked");
    }
    Json(DeleteResponse { deleted })
}

// ── Admin: worker registration management ─────────────────────────────────

async fn list_registrations(State(state): State<AppState>) -> impl IntoResponse {
    let map = state.registered.read().await;
    let mut registrations: Vec<RegistrationView> = map
        .iter()
        .map(|(pid, rec)| RegistrationView {
            peer_id: pid.to_string(),
            dial_multiaddr: rec.dial_multiaddr.clone(),
            registered_at_unix_ms: rec.registered_at_unix_ms,
            supported_cids: rec.supported_cids.clone(),
        })
        .collect();
    registrations.sort_by_key(|r| r.registered_at_unix_ms);
    Json(RegistrationListResponse { registrations })
}

async fn delete_registration(
    State(mut state): State<AppState>,
    Path(peer_id_str): Path<String>,
) -> impl IntoResponse {
    let pid = match PeerId::from_str(&peer_id_str) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid peer_id").into_response(),
    };
    let deleted = state.registered.write().await.remove(&pid).is_some();
    if deleted {
        state.active.write().await.remove(&pid);
        redis_del(&mut state.redis, &pid).await;
        info!(peer_id = %pid, "worker registration revoked");
    }
    Json(DeleteResponse { deleted }).into_response()
}

// ── Worker handlers ────────────────────────────────────────────────────────

fn validate_multiaddr(peer_id: &str, dial: &str) -> Result<(PeerId, Multiaddr)> {
    let pid = PeerId::from_str(peer_id).map_err(|e| anyhow!("invalid peer_id: {e}"))?;
    let ma: Multiaddr = dial.parse().map_err(|e| anyhow!("invalid dial_multiaddr: {e}"))?;
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

async fn post_join(
    State(mut state): State<AppState>,
    Json(body): Json<JoinBody>,
) -> impl IntoResponse {
    // 1. Validate join token.
    let token_valid = {
        let map = state.join_tokens.read().await;
        map.values()
            .any(|t| t.token_value == body.join_token && !t.is_expired())
    };
    if !token_valid {
        return (StatusCode::UNAUTHORIZED, "invalid or expired join_token").into_response();
    }

    // 2. Decode public key and verify it matches the claimed peer_id.
    let pubkey_bytes = match STANDARD.decode(&body.public_key) {
        Ok(b) => b,
        Err(_) => return (StatusCode::BAD_REQUEST, "public_key is not valid base64").into_response(),
    };
    let pubkey = match identity::PublicKey::try_decode_protobuf(&pubkey_bytes) {
        Ok(k) => k,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid public_key: {e}")).into_response(),
    };
    let derived_peer_id = pubkey.to_peer_id();
    let claimed_peer_id = match PeerId::from_str(&body.peer_id) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid peer_id").into_response(),
    };
    if derived_peer_id != claimed_peer_id {
        return (StatusCode::BAD_REQUEST, "public_key does not match peer_id").into_response();
    }

    // 3. Validate multiaddr format.
    if let Err(e) = validate_multiaddr(&body.peer_id, &body.dial_multiaddr) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }

    // 4. Check timestamp freshness.
    if let Err(e) = check_timestamp(body.timestamp_secs) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }

    // 5. Verify signature — proves worker owns the private key.
    if let Err(e) = verify_signature(
        &pubkey,
        &body.peer_id,
        &body.dial_multiaddr,
        body.timestamp_secs,
        &body.signature,
    ) {
        return (StatusCode::UNAUTHORIZED, e.to_string()).into_response();
    }

    // 6. Upsert registration record (in-memory + Redis).
    let rec = RegistrationRecord {
        public_key: pubkey,
        dial_multiaddr: body.dial_multiaddr.clone(),
        registered_at_unix_ms: unix_ms_now(),
        supported_cids: body.supported_cids.clone(),
    };
    redis_put(&mut state.redis, &claimed_peer_id, &rec).await;
    state.registered.write().await.insert(claimed_peer_id, rec);
    state.active.write().await.insert(
        claimed_peer_id,
        ActiveRecord {
            dial_multiaddr: body.dial_multiaddr,
            last_seen: Instant::now(),
        },
    );
    info!(peer_id = %claimed_peer_id, "worker registered");
    (StatusCode::OK, Json(JoinResponse { ok: true, peer_id: body.peer_id })).into_response()
}

async fn post_heartbeat(
    State(state): State<AppState>,
    Json(body): Json<HeartbeatBody>,
) -> impl IntoResponse {
    let pid = match PeerId::from_str(&body.peer_id) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid peer_id").into_response(),
    };

    let pubkey = {
        let map = state.registered.read().await;
        match map.get(&pid) {
            Some(r) => r.public_key.clone(),
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    "worker not registered; call /v1/workers/join first",
                )
                    .into_response()
            }
        }
    };

    if let Err(e) = check_timestamp(body.timestamp_secs) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }

    if let Err(e) = verify_signature(
        &pubkey,
        &body.peer_id,
        &body.dial_multiaddr,
        body.timestamp_secs,
        &body.signature,
    ) {
        return (StatusCode::UNAUTHORIZED, e.to_string()).into_response();
    }

    state.active.write().await.insert(
        pid,
        ActiveRecord {
            dial_multiaddr: body.dial_multiaddr,
            last_seen: Instant::now(),
        },
    );
    if let Some(rec) = state.registered.write().await.get_mut(&pid) {
        rec.supported_cids = body.supported_cids.clone();
    }
    info!(peer_id = %pid, "worker heartbeat ok (lease renewed)");
    (StatusCode::OK, Json(HeartbeatOkResponse { ok: true })).into_response()
}

async fn get_workers(State(state): State<AppState>) -> impl IntoResponse {
    let map = state.active.read().await;
    let now = Instant::now();
    let mut workers = Vec::new();
    for (pid, rec) in map.iter() {
        if now.duration_since(rec.last_seen) <= STALE_AFTER {
            let elapsed_ms = now.duration_since(rec.last_seen).as_millis();
            let last_seen_unix_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis().saturating_sub(elapsed_ms))
                .unwrap_or(0);
            workers.push(WorkerView {
                peer_id: pid.to_string(),
                dial_multiaddr: rec.dial_multiaddr.clone(),
                last_seen_unix_ms,
                supported_cids: state
                    .registered
                    .read()
                    .await
                    .get(pid)
                    .map(|r| r.supported_cids.clone())
                    .unwrap_or_default(),
            });
        }
    }
    Json(WorkersResponse { workers })
}

async fn health() -> &'static str {
    "ok"
}

// ── Background task ────────────────────────────────────────────────────────

async fn purge_loop(state: AppState) {
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    loop {
        tick.tick().await;
        let now = Instant::now();

        {
            let mut map = state.active.write().await;
            let before = map.len();
            map.retain(|_, rec| now.duration_since(rec.last_seen) <= STALE_AFTER);
            let removed = before.saturating_sub(map.len());
            if removed > 0 {
                warn!(removed, "purged workers with expired heartbeat lease");
            }
        }

        {
            let mut map = state.join_tokens.write().await;
            let before = map.len();
            map.retain(|_, t| !t.is_expired());
            let removed = before.saturating_sub(map.len());
            if removed > 0 {
                warn!(removed, "purged expired join tokens");
            }
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn unix_secs_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Main ───────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Args::parse();

    let http_str = cli
        .http_addr
        .unwrap_or_else(|| DEFAULT_REGISTRY_HTTP_ADDR.to_string());
    let http_addr: std::net::SocketAddr = http_str
        .parse()
        .with_context(|| format!("invalid --http-addr `{http_str}`"))?;

    // Connect to Redis and load persisted registrations.
    let redis_client =
        redis::Client::open(cli.redis_url.as_str()).context("invalid --redis-url")?;
    let mut redis_conn = redis::aio::ConnectionManager::new(redis_client)
        .await
        .with_context(|| format!("connect to Redis at `{}`", cli.redis_url))?;

    let registered_map = redis_load_all(&mut redis_conn).await;

    let admin_token = Uuid::new_v4().to_string();

    let state = AppState {
        admin_token: admin_token.clone(),
        join_tokens: Arc::new(RwLock::new(HashMap::new())),
        registered: Arc::new(RwLock::new(registered_map)),
        active: Arc::new(RwLock::new(HashMap::new())),
        redis: redis_conn,
    };

    tokio::spawn(purge_loop(state.clone()));

    let admin_routes = Router::new()
        .route("/v1/admin/tokens", post(create_token).get(list_tokens))
        .route("/v1/admin/tokens/:id", delete(delete_token))
        .route("/v1/admin/registrations", get(list_registrations))
        .route("/v1/admin/registrations/:peer_id", delete(delete_registration))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ));

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/workers/join", post(post_join))
        .route("/v1/workers/heartbeat", post(post_heartbeat))
        .route("/v1/workers", get(get_workers))
        .merge(admin_routes)
        .with_state(state);

    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  beenet-registry ADMIN TOKEN (changes on every restart)");
    println!("  {admin_token}");
    println!("  Authorization: Bearer <token>  →  /v1/admin/*");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    info!(
        %http_addr,
        redis_url = %cli.redis_url,
        "beenet-registry listening"
    );

    let listener = tokio::net::TcpListener::bind(http_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
