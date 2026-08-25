//! Beenet Registry — HTTP control plane.
//!
//! ## Authentication layers
//!
//! | Layer | Secret | Purpose |
//! |-------|--------|---------|
//! | Admin token | random UUID printed at startup | protect `/v1/admin/*` CRUD |
//! | Worker join token | short-lived admin-issued bearer token | bootstrap gate for worker registration |
//! | Gateway join token | short-lived admin-issued bearer token | bootstrap gate for gateway registration |
//! | Ed25519 keypair | worker/gateway-held private key | sign heartbeats/lookups; registry verifies with stored pubkey |
//!
//! ## Persistence
//! Worker registrations are stored in Redis (`beenet:registrations` Hash).
//! The registry is stateless otherwise — it can be restarted or scaled without losing
//! registered worker identities.
//!
//! ## Endpoints
//! | Method | Path | Auth | Description |
//! |--------|------|------|-------------|
//! | POST | `/v1/workers/join` | worker join token + sig | register a new worker |
//! | POST | `/v1/workers/heartbeat` | worker sig | renew worker lease |
//! | POST | `/v1/workers/lookup` | gateway sig | batch lookup by peer_ids (active lease only) |
//! | POST | `/v1/gateways/join` | gateway join token + sig | register a new gateway |
//! | POST | `/v1/gateways/heartbeat` | gateway sig | renew gateway lease |
//! | POST | `/v1/admin/tokens` | admin | create worker join token |
//! | GET  | `/v1/admin/tokens` | admin | list worker join tokens |
//! | DELETE | `/v1/admin/tokens/:id` | admin | revoke worker join token |
//! | POST | `/v1/admin/gateway-tokens` | admin | create gateway join token |
//! | GET  | `/v1/admin/gateway-tokens` | admin | list gateway join tokens |
//! | DELETE | `/v1/admin/gateway-tokens/:id` | admin | revoke gateway join token |
//! | GET  | `/v1/admin/registrations` | admin | list registered workers |
//! | DELETE | `/v1/admin/registrations/:peer_id` | admin | revoke worker registration |
//! | GET  | `/v1/admin/gateway-registrations` | admin | list registered gateways |
//! | DELETE | `/v1/admin/gateway-registrations/:peer_id` | admin | revoke gateway registration |
//! | GET  | `/v1/dashboard/status` | admin | full gateway + worker snapshot |
//! | GET  | `/health` | none | liveness probe |

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
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
use libp2p::{identity, PeerId};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

/// Workers whose heartbeat lease expired within this window are pruned from memory.
const STALE_AFTER: Duration = Duration::from_secs(60);
/// Tolerance for clock skew between worker and registry when validating signed timestamps.
const SIGNATURE_WINDOW_SECS: u64 = 60;
/// Default lifetime for reusable worker bootstrap tokens.
const DEFAULT_JOIN_TOKEN_TTL_SECS: u64 = 10 * 60;
/// Administrators cannot create bootstrap tokens that live longer than one hour.
const MAX_JOIN_TOKEN_TTL_SECS: u64 = 60 * 60;
/// Redis Hash key that stores all worker registrations.
const REDIS_REG_KEY: &str = "beenet:registrations";
/// Redis Hash key that stores all gateway registrations.
const REDIS_GW_REG_KEY: &str = "beenet:gateway_registrations";
/// Max gateways returned to a worker (primary + HA backups).
const GATEWAY_TIP_SIZE: usize = 3;
/// Modulus for sticky hash tie-break among equal-scoring gateways.
const STICKY_MOD: u64 = 1_000_000;
/// Max peer_ids accepted by `POST /v1/workers/lookup`.
const MAX_LOOKUP_PEER_IDS: usize = 256;
const MAX_GATEWAY_CONNECTIONS: usize = 10_000;

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

    /// Optional fixed admin token for local testing.
    /// If omitted, a random token is generated at startup and still printed.
    #[arg(long)]
    admin_token: Option<String>,

    /// Shared bearer token protecting Front Door route resolution.
    #[arg(long, env = "BEENET_INTERNAL_TOKEN")]
    internal_token: Option<String>,
}

// ── State ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    admin_token: String,
    internal_token: Option<String>,
    join_tokens: Arc<RwLock<HashMap<String, JoinTokenRecord>>>,
    gateway_join_tokens: Arc<RwLock<HashMap<String, JoinTokenRecord>>>,
    /// Workers that have successfully joined (public key stored here for sig verification).
    /// In-memory mirror of the Redis Hash — always kept in sync.
    registered: Arc<RwLock<HashMap<PeerId, RegistrationRecord>>>,
    /// Gateways that have successfully joined (public key stored for sig verification).
    registered_gateways: Arc<RwLock<HashMap<PeerId, GatewayRegistrationRecord>>>,
    /// Active workers with a live heartbeat lease (in-memory only; rebuilt from heartbeats).
    active: Arc<RwLock<HashMap<PeerId, ActiveRecord>>>,
    /// Active gateways with a live heartbeat lease (in-memory only; rebuilt from heartbeats).
    gateways: Arc<RwLock<HashMap<PeerId, ActiveGateway>>>,
    /// Async Redis connection (auto-reconnects; clone-safe).
    redis: redis::aio::ConnectionManager,
}

// ── Redis persistence ──────────────────────────────────────────────────────

/// JSON value stored per field in the Redis Hash.
#[derive(Serialize, Deserialize)]
struct RedisRegistration {
    /// base64(protobuf-encoded public key)
    public_key_b64: String,
    registered_at_unix_ms: u64,
    #[serde(default)]
    supported_cids: Vec<String>,
    #[serde(default)]
    loaded_cids: Vec<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// Upsert one registration into Redis (`HSET beenet:registrations <peer_id> <json>`).
async fn redis_put(
    redis: &mut redis::aio::ConnectionManager,
    peer_id: &PeerId,
    rec: &RegistrationRecord,
) {
    let value = RedisRegistration {
        public_key_b64: STANDARD.encode(rec.public_key.encode_protobuf()),
        registered_at_unix_ms: rec.registered_at_unix_ms,
        supported_cids: rec.supported_cids.clone(),
        loaded_cids: rec.loaded_cids.clone(),
        region: rec.region.clone(),
        name: rec.name.clone(),
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
                registered_at_unix_ms: r.registered_at_unix_ms,
                supported_cids: r.supported_cids,
                loaded_cids: r.loaded_cids,
                region: r.region,
                name: r.name,
            },
        );
    }
    info!(count = out.len(), "loaded worker registrations from Redis");
    out
}

#[derive(Serialize, Deserialize)]
struct RedisGatewayRegistration {
    public_key_b64: String,
    registered_at_unix_ms: u64,
    gateway_id: String,
    #[serde(default)]
    region: Option<String>,
}

async fn redis_gateway_put(
    redis: &mut redis::aio::ConnectionManager,
    peer_id: &PeerId,
    rec: &GatewayRegistrationRecord,
) {
    let value = RedisGatewayRegistration {
        public_key_b64: STANDARD.encode(rec.public_key.encode_protobuf()),
        registered_at_unix_ms: rec.registered_at_unix_ms,
        gateway_id: rec.gateway_id.clone(),
        region: rec.region.clone(),
    };
    let json = match serde_json::to_string(&value) {
        Ok(j) => j,
        Err(e) => {
            warn!(peer_id = %peer_id, error = %e, "failed to serialize gateway registration for Redis");
            return;
        }
    };
    if let Err(e) = redis
        .hset::<_, _, _, ()>(REDIS_GW_REG_KEY, peer_id.to_string(), json)
        .await
    {
        warn!(peer_id = %peer_id, error = %e, "Redis HSET gateway registration failed");
    }
}

async fn redis_gateway_del(redis: &mut redis::aio::ConnectionManager, peer_id: &PeerId) {
    if let Err(e) = redis
        .hdel::<_, _, ()>(REDIS_GW_REG_KEY, peer_id.to_string())
        .await
    {
        warn!(peer_id = %peer_id, error = %e, "Redis HDEL gateway registration failed");
    }
}

async fn redis_gateway_load_all(
    redis: &mut redis::aio::ConnectionManager,
) -> HashMap<PeerId, GatewayRegistrationRecord> {
    let raw: HashMap<String, String> = match redis.hgetall(REDIS_GW_REG_KEY).await {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "Redis HGETALL gateway registrations failed; starting empty");
            return HashMap::new();
        }
    };
    let mut out = HashMap::new();
    for (peer_id_str, json) in raw {
        let pid = match PeerId::from_str(&peer_id_str) {
            Ok(p) => p,
            Err(e) => {
                warn!(peer_id = %peer_id_str, error = %e, "skipping corrupt gateway registration");
                continue;
            }
        };
        let r: RedisGatewayRegistration = match serde_json::from_str(&json) {
            Ok(r) => r,
            Err(e) => {
                warn!(peer_id = %peer_id_str, error = %e, "skipping undeserializable gateway registration");
                continue;
            }
        };
        let pk_bytes = match STANDARD.decode(&r.public_key_b64) {
            Ok(b) => b,
            Err(e) => {
                warn!(peer_id = %peer_id_str, error = %e, "skipping gateway registration with invalid base64 key");
                continue;
            }
        };
        let public_key = match identity::PublicKey::try_decode_protobuf(&pk_bytes) {
            Ok(k) => k,
            Err(e) => {
                warn!(peer_id = %peer_id_str, error = %e, "skipping gateway registration with undecodable key");
                continue;
            }
        };
        out.insert(
            pid,
            GatewayRegistrationRecord {
                public_key,
                registered_at_unix_ms: r.registered_at_unix_ms,
                gateway_id: r.gateway_id,
                region: r.region,
            },
        );
    }
    info!(count = out.len(), "loaded gateway registrations from Redis");
    out
}

// ── Domain types ───────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct JoinTokenRecord {
    id: String,
    description: String,
    issued_by: Option<String>,
    token_hash: [u8; 32],
    created_at_unix_ms: u64,
    expires_at: Instant,
    expires_at_unix_ms: u64,
}

impl JoinTokenRecord {
    fn is_expired(&self) -> bool {
        Instant::now() > self.expires_at
    }

    fn matches(&self, token_value: &str) -> bool {
        constant_time_eq(&self.token_hash, &hash_join_token(token_value))
    }
}

#[derive(Clone, Debug)]
struct RegistrationRecord {
    public_key: identity::PublicKey,
    registered_at_unix_ms: u64,
    supported_cids: Vec<String>,
    loaded_cids: Vec<String>,
    region: Option<String>,
    name: Option<String>,
}

#[derive(Clone, Debug)]
struct ActiveRecord {
    last_seen: Instant,
    supported_cids: Vec<String>,
    loaded_cids: Vec<String>,
    name: Option<String>,
}

#[derive(Clone, Debug)]
struct GatewayRegistrationRecord {
    public_key: identity::PublicKey,
    registered_at_unix_ms: u64,
    gateway_id: String,
    region: Option<String>,
}

#[derive(Clone, Debug)]
struct ActiveGateway {
    gateway_id: String,
    dial_addr: String,
    region: Option<String>,
    capacity: u32,
    connected_workers: u32,
    connected_worker_peer_ids: Vec<String>,
    http_url: Option<String>,
    last_seen: Instant,
}

// ── Admin API types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CreateTokenBody {
    #[serde(default)]
    description: String,
    /// Cloud user id, `admin`, or other issuer label recorded with the token.
    #[serde(default)]
    issued_by: Option<String>,
    ttl_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
struct TokenView {
    id: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    issued_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_value: Option<String>,
    created_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    expired: bool,
}

impl From<&JoinTokenRecord> for TokenView {
    fn from(r: &JoinTokenRecord) -> Self {
        TokenView {
            id: r.id.clone(),
            description: r.description.clone(),
            issued_by: r.issued_by.clone(),
            token_value: None,
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
    public_key: String,
    registered_at_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default)]
    supported_cids: Vec<String>,
    #[serde(default)]
    loaded_cids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RegistrationListResponse {
    registrations: Vec<RegistrationView>,
}

#[derive(Debug, Serialize)]
struct GatewayRegistrationView {
    peer_id: String,
    gateway_id: String,
    registered_at_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<String>,
}

#[derive(Debug, Serialize)]
struct GatewayRegistrationListResponse {
    registrations: Vec<GatewayRegistrationView>,
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
    timestamp_secs: u64,
    /// Ed25519 signature over `"{peer_id}\n{timestamp_secs}"`, base64-encoded.
    signature: String,
    #[serde(default)]
    supported_cids: Vec<String>,
    #[serde(default)]
    loaded_cids: Vec<String>,
    #[serde(default)]
    region: Option<String>,
    /// Display name; duplicates allowed (PeerId is the unique identity).
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Serialize)]
struct JoinResponse {
    ok: bool,
    peer_id: String,
}

#[derive(Debug, Deserialize)]
struct HeartbeatBody {
    peer_id: String,
    timestamp_secs: u64,
    /// Ed25519 signature over `"{peer_id}\n{timestamp_secs}"`, base64-encoded.
    signature: String,
    #[serde(default)]
    supported_cids: Vec<String>,
    #[serde(default)]
    loaded_cids: Vec<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Serialize)]
struct HeartbeatOkResponse {
    ok: bool,
    gateways: Vec<GatewayView>,
}

#[derive(Debug, Deserialize)]
struct GatewayJoinBody {
    join_token: String,
    peer_id: String,
    public_key: String,
    timestamp_secs: u64,
    signature: String,
    gateway_id: String,
    #[serde(default)]
    region: Option<String>,
}

#[derive(Debug, Serialize)]
struct GatewayJoinResponse {
    ok: bool,
    peer_id: String,
}

#[derive(Debug, Deserialize)]
struct GatewayHeartbeatBody {
    gateway_id: String,
    peer_id: String,
    timestamp_secs: u64,
    signature: String,
    dial_addr: String,
    region: Option<String>,
    #[serde(default = "default_gateway_capacity")]
    capacity: u32,
    #[serde(default)]
    connected_worker_peer_ids: Vec<String>,
    #[serde(default)]
    http_url: Option<String>,
}

fn default_gateway_capacity() -> u32 {
    1_000
}

#[derive(Clone, Debug, Serialize)]
struct GatewayView {
    gateway_id: String,
    peer_id: String,
    dial_addr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<String>,
    capacity: u32,
    connected_workers: u32,
    last_seen_unix_ms: u64,
}

#[derive(Debug, Serialize)]
struct GatewaysResponse {
    gateways: Vec<GatewayView>,
}

#[derive(Debug, Serialize)]
struct ResolveRoute {
    gateway_id: String,
    gateway_peer_id: String,
    gateway_url: String,
    worker_peer_id: String,
    region: Option<String>,
    load: u32,
    preferred: bool,
}

#[derive(Debug, Serialize)]
struct ResolveResponse {
    cid: String,
    ttl_ms: u64,
    routes: Vec<ResolveRoute>,
}

#[derive(Debug, Serialize)]
struct WorkersResponse {
    workers: Vec<WorkerView>,
}

#[derive(Debug, Deserialize)]
struct WorkersLookupBody {
    /// Registered gateway peer id authenticating this lookup.
    peer_id: String,
    timestamp_secs: u64,
    signature: String,
    peer_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DashboardStatusResponse {
    gateways: Vec<GatewayView>,
    workers: Vec<WorkerView>,
    gateway_count: usize,
    worker_count: usize,
    generated_at_unix_ms: u64,
}

#[derive(Debug, Serialize)]
struct WorkerView {
    peer_id: String,
    connected: bool,
    last_seen_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default)]
    supported_cids: Vec<String>,
    #[serde(default)]
    loaded_cids: Vec<String>,
}

// ── Signature helpers ──────────────────────────────────────────────────────

fn signed_message(peer_id: &str, timestamp_secs: u64) -> Vec<u8> {
    format!("{peer_id}\n{timestamp_secs}").into_bytes()
}

fn verify_signature(
    pubkey: &identity::PublicKey,
    peer_id: &str,
    timestamp_secs: u64,
    sig_b64: &str,
) -> Result<()> {
    let sig = STANDARD
        .decode(sig_b64)
        .map_err(|_| anyhow!("signature is not valid base64"))?;
    let msg = signed_message(peer_id, timestamp_secs);
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
        .is_some_and(|t| t == state.admin_token);

    if !authorized {
        return (StatusCode::UNAUTHORIZED, "invalid or missing admin token").into_response();
    }
    next.run(request).await
}

// ── Admin: join token CRUD ─────────────────────────────────────────────────

fn mint_join_token(
    description: String,
    issued_by: Option<String>,
    ttl_secs: u64,
) -> (JoinTokenRecord, String) {
    let now_instant = Instant::now();
    let now_unix_ms = unix_ms_now();
    let token_value = format!("{}.{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let issued_by = issued_by.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    });
    let record = JoinTokenRecord {
        id: Uuid::new_v4().to_string(),
        description,
        issued_by,
        token_hash: hash_join_token(&token_value),
        created_at_unix_ms: now_unix_ms,
        expires_at: now_instant + Duration::from_secs(ttl_secs),
        expires_at_unix_ms: now_unix_ms + ttl_secs * 1000,
    };
    (record, token_value)
}

async fn create_token(
    State(state): State<AppState>,
    Json(body): Json<CreateTokenBody>,
) -> impl IntoResponse {
    let ttl_secs = match resolve_join_token_ttl(body.ttl_secs) {
        Ok(ttl_secs) => ttl_secs,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    let (record, token_value) = mint_join_token(body.description, body.issued_by, ttl_secs);
    let mut view = TokenView::from(&record);
    view.token_value = Some(token_value);
    let id = record.id.clone();
    state.join_tokens.write().await.insert(id.clone(), record);
    info!(%id, ttl_secs, issued_by = ?view.issued_by, "worker join token created");
    (StatusCode::CREATED, Json(view)).into_response()
}

async fn list_tokens(State(state): State<AppState>) -> impl IntoResponse {
    let map = state.join_tokens.read().await;
    let mut tokens: Vec<TokenView> = map.values().map(TokenView::from).collect();
    tokens.sort_by_key(|t| t.created_at_unix_ms);
    Json(TokenListResponse { tokens })
}

async fn delete_token(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let deleted = state.join_tokens.write().await.remove(&id).is_some();
    if deleted {
        info!(%id, "worker join token revoked");
    }
    Json(DeleteResponse { deleted })
}

async fn create_gateway_token(
    State(state): State<AppState>,
    Json(body): Json<CreateTokenBody>,
) -> impl IntoResponse {
    let ttl_secs = match resolve_join_token_ttl(body.ttl_secs) {
        Ok(ttl_secs) => ttl_secs,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    let (record, token_value) = mint_join_token(body.description, body.issued_by, ttl_secs);
    let mut view = TokenView::from(&record);
    view.token_value = Some(token_value);
    let id = record.id.clone();
    state
        .gateway_join_tokens
        .write()
        .await
        .insert(id.clone(), record);
    info!(%id, ttl_secs, "gateway join token created");
    (StatusCode::CREATED, Json(view)).into_response()
}

async fn list_gateway_tokens(State(state): State<AppState>) -> impl IntoResponse {
    let map = state.gateway_join_tokens.read().await;
    let mut tokens: Vec<TokenView> = map.values().map(TokenView::from).collect();
    tokens.sort_by_key(|t| t.created_at_unix_ms);
    Json(TokenListResponse { tokens })
}

async fn delete_gateway_token(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let deleted = state
        .gateway_join_tokens
        .write()
        .await
        .remove(&id)
        .is_some();
    if deleted {
        info!(%id, "gateway join token revoked");
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
            public_key: STANDARD.encode(rec.public_key.encode_protobuf()),
            registered_at_unix_ms: rec.registered_at_unix_ms,
            name: rec.name.clone(),
            supported_cids: rec.supported_cids.clone(),
            loaded_cids: rec.loaded_cids.clone(),
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

async fn list_gateway_registrations(State(state): State<AppState>) -> impl IntoResponse {
    let map = state.registered_gateways.read().await;
    let mut registrations: Vec<GatewayRegistrationView> = map
        .iter()
        .map(|(pid, rec)| GatewayRegistrationView {
            peer_id: pid.to_string(),
            gateway_id: rec.gateway_id.clone(),
            registered_at_unix_ms: rec.registered_at_unix_ms,
            region: rec.region.clone(),
        })
        .collect();
    registrations.sort_by_key(|r| r.registered_at_unix_ms);
    Json(GatewayRegistrationListResponse { registrations })
}

async fn delete_gateway_registration(
    State(mut state): State<AppState>,
    Path(peer_id_str): Path<String>,
) -> impl IntoResponse {
    let pid = match PeerId::from_str(&peer_id_str) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid peer_id").into_response(),
    };
    let deleted = state
        .registered_gateways
        .write()
        .await
        .remove(&pid)
        .is_some();
    if deleted {
        state.gateways.write().await.remove(&pid);
        redis_gateway_del(&mut state.redis, &pid).await;
        info!(peer_id = %pid, "gateway registration revoked");
    }
    Json(DeleteResponse { deleted }).into_response()
}

// ── Worker handlers ────────────────────────────────────────────────────────

async fn post_join(
    State(mut state): State<AppState>,
    Json(body): Json<JoinBody>,
) -> impl IntoResponse {
    // 1. Validate join token.
    let token_valid = {
        let map = state.join_tokens.read().await;
        map.values()
            .any(|t| !t.is_expired() && t.matches(&body.join_token))
    };
    if !token_valid {
        return (StatusCode::UNAUTHORIZED, "invalid or expired join_token").into_response();
    }

    // 2. Decode public key and verify it matches the claimed peer_id.
    let pubkey_bytes = match STANDARD.decode(&body.public_key) {
        Ok(b) => b,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "public_key is not valid base64").into_response()
        }
    };
    let pubkey = match identity::PublicKey::try_decode_protobuf(&pubkey_bytes) {
        Ok(k) => k,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid public_key: {e}")).into_response()
        }
    };
    let derived_peer_id = pubkey.to_peer_id();
    let claimed_peer_id = match PeerId::from_str(&body.peer_id) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid peer_id").into_response(),
    };
    if derived_peer_id != claimed_peer_id {
        return (StatusCode::BAD_REQUEST, "public_key does not match peer_id").into_response();
    }

    // 3. Check timestamp freshness.
    if let Err(e) = check_timestamp(body.timestamp_secs) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }

    // 4. Verify signature — proves worker owns the private key.
    if let Err(e) = verify_signature(&pubkey, &body.peer_id, body.timestamp_secs, &body.signature) {
        return (StatusCode::UNAUTHORIZED, e.to_string()).into_response();
    }

    // 5. Upsert registration record (in-memory + Redis).
    let region = normalize_region(body.region);
    let name = normalize_worker_name(body.name);
    let rec = RegistrationRecord {
        public_key: pubkey,
        registered_at_unix_ms: unix_ms_now(),
        supported_cids: body.supported_cids.clone(),
        loaded_cids: body.loaded_cids.clone(),
        region: region.clone(),
        name: name.clone(),
    };
    redis_put(&mut state.redis, &claimed_peer_id, &rec).await;
    state.registered.write().await.insert(claimed_peer_id, rec);
    state.active.write().await.insert(
        claimed_peer_id,
        ActiveRecord {
            last_seen: Instant::now(),
            supported_cids: body.supported_cids.clone(),
            loaded_cids: body.loaded_cids.clone(),
            name,
        },
    );
    info!(peer_id = %claimed_peer_id, "worker registered");
    (
        StatusCode::OK,
        Json(JoinResponse {
            ok: true,
            peer_id: body.peer_id,
        }),
    )
        .into_response()
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

    if let Err(e) = verify_signature(&pubkey, &body.peer_id, body.timestamp_secs, &body.signature) {
        return (StatusCode::UNAUTHORIZED, e.to_string()).into_response();
    }

    let region_from_body = normalize_region(body.region.clone());
    let name_from_body = normalize_worker_name(body.name.clone());
    let worker_region = {
        let mut map = state.registered.write().await;
        if let Some(rec) = map.get_mut(&pid) {
            rec.supported_cids = body.supported_cids.clone();
            rec.loaded_cids = body.loaded_cids.clone();
            if let Some(region) = region_from_body.clone() {
                rec.region = Some(region);
            }
            if let Some(name) = name_from_body.clone() {
                rec.name = Some(name);
            }
            let region = rec.region.clone();
            let name = rec.name.clone();
            let snapshot = rec.clone();
            drop(map);
            redis_put(&mut state.redis.clone(), &pid, &snapshot).await;
            (region, name)
        } else {
            (region_from_body, name_from_body)
        }
    };
    state.active.write().await.insert(
        pid,
        ActiveRecord {
            last_seen: Instant::now(),
            supported_cids: body.supported_cids.clone(),
            loaded_cids: body.loaded_cids.clone(),
            name: worker_region.1,
        },
    );
    info!(peer_id = %pid, "worker heartbeat ok (lease renewed)");
    let gateways = gateway_tip_for(&state, &body.peer_id, worker_region.0.as_deref()).await;
    (
        StatusCode::OK,
        Json(HeartbeatOkResponse { ok: true, gateways }),
    )
        .into_response()
}

async fn post_gateway_join(
    State(mut state): State<AppState>,
    Json(body): Json<GatewayJoinBody>,
) -> impl IntoResponse {
    let token_valid = {
        let map = state.gateway_join_tokens.read().await;
        map.values()
            .any(|t| !t.is_expired() && t.matches(&body.join_token))
    };
    if !token_valid {
        return (StatusCode::UNAUTHORIZED, "invalid or expired join_token").into_response();
    }
    if body.gateway_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "gateway_id is required").into_response();
    }

    let pubkey_bytes = match STANDARD.decode(&body.public_key) {
        Ok(b) => b,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "public_key is not valid base64").into_response()
        }
    };
    let pubkey = match identity::PublicKey::try_decode_protobuf(&pubkey_bytes) {
        Ok(k) => k,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid public_key: {e}")).into_response()
        }
    };
    let claimed_peer_id = match PeerId::from_str(&body.peer_id) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid peer_id").into_response(),
    };
    if pubkey.to_peer_id() != claimed_peer_id {
        return (StatusCode::BAD_REQUEST, "public_key does not match peer_id").into_response();
    }
    if let Err(e) = check_timestamp(body.timestamp_secs) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }
    if let Err(e) = verify_signature(&pubkey, &body.peer_id, body.timestamp_secs, &body.signature) {
        return (StatusCode::UNAUTHORIZED, e.to_string()).into_response();
    }

    let region = normalize_region(body.region);
    let rec = GatewayRegistrationRecord {
        public_key: pubkey,
        registered_at_unix_ms: unix_ms_now(),
        gateway_id: body.gateway_id.trim().to_string(),
        region,
    };
    redis_gateway_put(&mut state.redis, &claimed_peer_id, &rec).await;
    state
        .registered_gateways
        .write()
        .await
        .insert(claimed_peer_id, rec);
    info!(peer_id = %claimed_peer_id, "gateway registered");
    (
        StatusCode::OK,
        Json(GatewayJoinResponse {
            ok: true,
            peer_id: body.peer_id,
        }),
    )
        .into_response()
}

async fn post_gateway_heartbeat(
    State(state): State<AppState>,
    Json(body): Json<GatewayHeartbeatBody>,
) -> impl IntoResponse {
    if body.connected_worker_peer_ids.len() > MAX_GATEWAY_CONNECTIONS {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "connected_worker_peer_ids must contain at most {MAX_GATEWAY_CONNECTIONS} entries"
            ),
        )
            .into_response();
    }
    let peer_id = match PeerId::from_str(&body.peer_id) {
        Ok(value) => value,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid peer_id").into_response(),
    };
    if body.gateway_id.trim().is_empty() || body.dial_addr.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "gateway_id and dial_addr are required",
        )
            .into_response();
    }
    let pubkey = {
        let map = state.registered_gateways.read().await;
        match map.get(&peer_id) {
            Some(r) => r.public_key.clone(),
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    "gateway not registered; call /v1/gateways/join first",
                )
                    .into_response()
            }
        }
    };
    if let Err(error) = check_timestamp(body.timestamp_secs).and_then(|_| {
        verify_signature(&pubkey, &body.peer_id, body.timestamp_secs, &body.signature)
    }) {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
    let region = normalize_region(body.region);
    let gateway_id = {
        let t = body.gateway_id.trim();
        if t.is_empty() {
            return (StatusCode::BAD_REQUEST, "gateway_id is required").into_response();
        }
        t.chars().take(64).collect::<String>()
    };
    {
        let mut map = state.registered_gateways.write().await;
        if let Some(rec) = map.get_mut(&peer_id) {
            rec.gateway_id = gateway_id.clone();
            if let Some(region) = region.clone() {
                rec.region = Some(region);
            }
            let snapshot = rec.clone();
            drop(map);
            redis_gateway_put(&mut state.redis.clone(), &peer_id, &snapshot).await;
        }
    }
    let mut connected_worker_peer_ids: Vec<String> = body
        .connected_worker_peer_ids
        .iter()
        .filter_map(|peer| PeerId::from_str(peer.trim()).ok())
        .map(|peer| peer.to_string())
        .collect();
    connected_worker_peer_ids.sort();
    connected_worker_peer_ids.dedup();
    let http_url = match body.http_url {
        Some(url) => {
            let url = url.trim().trim_end_matches('/');
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return (StatusCode::BAD_REQUEST, "http_url must use http or https")
                    .into_response();
            }
            Some(url.to_string())
        }
        None => None,
    };
    state.gateways.write().await.insert(
        peer_id,
        ActiveGateway {
            gateway_id,
            dial_addr: body.dial_addr,
            region,
            capacity: body.capacity.max(1),
            connected_workers: connected_worker_peer_ids.len() as u32,
            connected_worker_peer_ids,
            http_url,
            last_seen: Instant::now(),
        },
    );
    Json(GatewaysResponse {
        gateways: gateway_views(&state).await,
    })
    .into_response()
}

async fn gateway_views(state: &AppState) -> Vec<GatewayView> {
    let now = Instant::now();
    let now_ms = unix_ms_now();
    let mut values: Vec<_> = state
        .gateways
        .read()
        .await
        .iter()
        .filter_map(|(peer_id, gateway)| {
            let age = now.duration_since(gateway.last_seen);
            (age <= STALE_AFTER && gateway.capacity > 0).then(|| GatewayView {
                gateway_id: gateway.gateway_id.clone(),
                peer_id: peer_id.to_string(),
                dial_addr: gateway.dial_addr.clone(),
                region: gateway.region.clone(),
                capacity: gateway.capacity,
                connected_workers: gateway.connected_workers,
                last_seen_unix_ms: now_ms.saturating_sub(age.as_millis() as u64),
            })
        })
        .collect();
    values.sort_by_key(gateway_load);
    values
}

fn gateway_load(gateway: &GatewayView) -> u32 {
    gateway.connected_workers.saturating_mul(10_000) / gateway.capacity.max(1)
}

fn normalize_region(region: Option<String>) -> Option<String> {
    region.and_then(|r| {
        let t = r.trim();
        (!t.is_empty()).then(|| t.to_string())
    })
}

/// Trim and cap display names (duplicates allowed across workers).
fn normalize_worker_name(name: Option<String>) -> Option<String> {
    name.and_then(|n| {
        let t = n.trim();
        if t.is_empty() {
            return None;
        }
        let capped: String = t.chars().take(64).collect();
        Some(capped)
    })
}

fn affinity_penalty(worker_region: Option<&str>, gateway_region: &Option<String>) -> u8 {
    let wr = worker_region.map(str::trim).filter(|s| !s.is_empty());
    let gr = gateway_region
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (wr, gr) {
        (Some(wr), Some(gr)) if wr.eq_ignore_ascii_case(gr) => 0,
        (Some(_), _) => 2,
        (None, _) => 1,
    }
}

fn sticky_key(worker_peer_id: &str, gateway_peer_id: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    worker_peer_id.hash(&mut hasher);
    gateway_peer_id.hash(&mut hasher);
    hasher.finish() % STICKY_MOD
}

/// Pick a small, affinity-aware, diversified gateway tip for one worker.
fn select_gateway_tip(
    candidates: Vec<GatewayView>,
    worker_peer_id: &str,
    worker_region: Option<&str>,
) -> Vec<GatewayView> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(u8, u32, u64, GatewayView)> = candidates
        .into_iter()
        .map(|gateway| {
            let aff = affinity_penalty(worker_region, &gateway.region);
            let load = gateway_load(&gateway);
            let sticky = sticky_key(worker_peer_id, &gateway.peer_id);
            (aff, load, sticky, gateway)
        })
        .collect();
    scored.sort_by_key(|(aff, load, sticky, _)| (*aff, *load, *sticky));

    let mut selected = Vec::with_capacity(GATEWAY_TIP_SIZE);
    selected.push(scored.remove(0).3);

    while selected.len() < GATEWAY_TIP_SIZE && !scored.is_empty() {
        let selected_regions: Vec<String> = selected
            .iter()
            .filter_map(|g| normalize_region(g.region.clone()).map(|r| r.to_ascii_lowercase()))
            .collect();
        let selected_peers: std::collections::HashSet<&str> =
            selected.iter().map(|g| g.peer_id.as_str()).collect();

        let mut best_idx = None;
        let mut best_key: Option<(bool, u32, u64)> = None;
        for (idx, (_aff, load, sticky, gateway)) in scored.iter().enumerate() {
            if selected_peers.contains(gateway.peer_id.as_str()) {
                continue;
            }
            let region_diverse = gateway
                .region
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|r| {
                    let lower = r.to_ascii_lowercase();
                    !selected_regions.iter().any(|sr| sr == &lower)
                })
                .unwrap_or(false);
            // Prefer region-diverse first, then lower load, then sticky.
            let key = (!region_diverse, *load, *sticky);
            if best_key.is_none_or(|bk| key < bk) {
                best_key = Some(key);
                best_idx = Some(idx);
            }
        }
        match best_idx {
            Some(idx) => selected.push(scored.remove(idx).3),
            None => break,
        }
    }
    selected
}

async fn gateway_tip_for(
    state: &AppState,
    worker_peer_id: &str,
    worker_region: Option<&str>,
) -> Vec<GatewayView> {
    let all = gateway_views(state).await;
    select_gateway_tip(all, worker_peer_id, worker_region)
}

async fn worker_views(state: &AppState) -> Vec<WorkerView> {
    let map = state.active.read().await;
    let now = Instant::now();
    map.iter()
        .filter_map(|(pid, rec)| worker_view_if_active(pid, rec, now))
        .collect()
}

/// Point-lookup active workers by peer id. Unknown / stale peers are omitted.
fn select_active_workers_by_peers(
    active: &HashMap<PeerId, ActiveRecord>,
    peer_ids: &[PeerId],
    now: Instant,
) -> Vec<WorkerView> {
    peer_ids
        .iter()
        .filter_map(|pid| {
            active
                .get(pid)
                .and_then(|rec| worker_view_if_active(pid, rec, now))
        })
        .collect()
}

fn worker_view_if_active(pid: &PeerId, rec: &ActiveRecord, now: Instant) -> Option<WorkerView> {
    if now.duration_since(rec.last_seen) > STALE_AFTER {
        return None;
    }
    let elapsed_ms = now.duration_since(rec.last_seen).as_millis() as u64;
    let last_seen_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
        .saturating_sub(elapsed_ms);
    Some(WorkerView {
        peer_id: pid.to_string(),
        connected: true,
        last_seen_unix_ms,
        name: rec.name.clone(),
        supported_cids: rec.supported_cids.clone(),
        loaded_cids: rec.loaded_cids.clone(),
    })
}

async fn post_workers_lookup(
    State(state): State<AppState>,
    Json(body): Json<WorkersLookupBody>,
) -> impl IntoResponse {
    if body.peer_ids.len() > MAX_LOOKUP_PEER_IDS {
        return (
            StatusCode::BAD_REQUEST,
            format!("peer_ids must contain at most {MAX_LOOKUP_PEER_IDS} entries"),
        )
            .into_response();
    }
    let gateway_pid = match PeerId::from_str(body.peer_id.trim()) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid peer_id").into_response(),
    };
    let pubkey = {
        let map = state.registered_gateways.read().await;
        match map.get(&gateway_pid) {
            Some(r) => r.public_key.clone(),
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    "gateway not registered; call /v1/gateways/join first",
                )
                    .into_response()
            }
        }
    };
    if let Err(e) = check_timestamp(body.timestamp_secs) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }
    if let Err(e) = verify_signature(&pubkey, &body.peer_id, body.timestamp_secs, &body.signature) {
        return (StatusCode::UNAUTHORIZED, e.to_string()).into_response();
    }

    let peer_ids: Vec<PeerId> = body
        .peer_ids
        .iter()
        .filter_map(|s| PeerId::from_str(s.trim()).ok())
        .collect();
    let map = state.active.read().await;
    let workers = select_active_workers_by_peers(&map, &peer_ids, Instant::now());
    Json(WorkersResponse { workers }).into_response()
}

async fn get_dashboard_status(State(state): State<AppState>) -> impl IntoResponse {
    let gateways = gateway_views(&state).await;
    let workers = worker_views(&state).await;
    Json(DashboardStatusResponse {
        gateway_count: gateways.len(),
        worker_count: workers.len(),
        generated_at_unix_ms: unix_ms_now(),
        gateways,
        workers,
    })
}

async fn resolve_cid(
    State(state): State<AppState>,
    axum::extract::Path(cid): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let Some(expected) = state.internal_token.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "route resolver is disabled",
        )
            .into_response();
    };
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if supplied != Some(expected) {
        return (StatusCode::UNAUTHORIZED, "invalid resolver credentials").into_response();
    }
    if cid.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "cid is required").into_response();
    }
    let now = Instant::now();
    let active = state.active.read().await;
    let gateways = state.gateways.read().await;
    let mut routes = Vec::new();
    for (gateway_peer_id, gateway) in gateways.iter() {
        if now.duration_since(gateway.last_seen) > STALE_AFTER {
            continue;
        }
        let Some(gateway_url) = gateway.http_url.clone() else {
            continue;
        };
        for worker_peer_id in &gateway.connected_worker_peer_ids {
            let Ok(worker_peer_id_parsed) = PeerId::from_str(worker_peer_id) else {
                continue;
            };
            let Some(worker) = active.get(&worker_peer_id_parsed) else {
                continue;
            };
            if now.duration_since(worker.last_seen) > STALE_AFTER {
                continue;
            }
            let preferred = worker.supported_cids.is_empty()
                || worker
                    .supported_cids
                    .iter()
                    .any(|supported| supported == &cid);
            {
                routes.push(ResolveRoute {
                    gateway_id: gateway.gateway_id.clone(),
                    gateway_peer_id: gateway_peer_id.to_string(),
                    gateway_url: gateway_url.clone(),
                    worker_peer_id: worker_peer_id.clone(),
                    region: gateway.region.clone(),
                    load: gateway_load(&GatewayView {
                        gateway_id: gateway.gateway_id.clone(),
                        peer_id: gateway_peer_id.to_string(),
                        dial_addr: gateway.dial_addr.clone(),
                        region: gateway.region.clone(),
                        capacity: gateway.capacity,
                        connected_workers: gateway.connected_workers,
                        last_seen_unix_ms: 0,
                    }),
                    preferred,
                });
            }
        }
    }
    routes.sort_by_key(|route| (!route.preferred, route.load));
    Json(ResolveResponse {
        cid,
        ttl_ms: 2_000,
        routes,
    })
    .into_response()
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
                warn!(removed, "purged expired worker join tokens");
            }
        }
        {
            let mut map = state.gateway_join_tokens.write().await;
            let before = map.len();
            map.retain(|_, t| !t.is_expired());
            let removed = before.saturating_sub(map.len());
            if removed > 0 {
                warn!(removed, "purged expired gateway join tokens");
            }
        }
        {
            let mut map = state.gateways.write().await;
            let before = map.len();
            map.retain(|_, gateway| now.duration_since(gateway.last_seen) <= STALE_AFTER);
            let removed = before.saturating_sub(map.len());
            if removed > 0 {
                warn!(removed, "purged gateways with expired heartbeat lease");
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

fn hash_join_token(token_value: &str) -> [u8; 32] {
    Sha256::digest(token_value.as_bytes()).into()
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn resolve_join_token_ttl(ttl_secs: Option<u64>) -> std::result::Result<u64, String> {
    let ttl_secs = ttl_secs.unwrap_or(DEFAULT_JOIN_TOKEN_TTL_SECS);
    if ttl_secs == 0 || ttl_secs > MAX_JOIN_TOKEN_TTL_SECS {
        return Err(format!(
            "ttl_secs must be between 1 and {MAX_JOIN_TOKEN_TTL_SECS}"
        ));
    }
    Ok(ttl_secs)
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
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
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
    let registered_gateways = redis_gateway_load_all(&mut redis_conn).await;

    let (admin_token, admin_token_source) = cli
        .admin_token
        .filter(|s| !s.trim().is_empty())
        .map(|token| (token, "configured"))
        .unwrap_or_else(|| (Uuid::new_v4().to_string(), "generated"));

    let state = AppState {
        admin_token: admin_token.clone(),
        internal_token: cli.internal_token.filter(|token| !token.trim().is_empty()),
        join_tokens: Arc::new(RwLock::new(HashMap::new())),
        gateway_join_tokens: Arc::new(RwLock::new(HashMap::new())),
        registered: Arc::new(RwLock::new(registered_map)),
        registered_gateways: Arc::new(RwLock::new(registered_gateways)),
        active: Arc::new(RwLock::new(HashMap::new())),
        gateways: Arc::new(RwLock::new(HashMap::new())),
        redis: redis_conn,
    };

    tokio::spawn(purge_loop(state.clone()));

    let admin_routes = Router::new()
        .route("/v1/admin/tokens", post(create_token).get(list_tokens))
        .route("/v1/admin/tokens/:id", delete(delete_token))
        .route(
            "/v1/admin/gateway-tokens",
            post(create_gateway_token).get(list_gateway_tokens),
        )
        .route("/v1/admin/gateway-tokens/:id", delete(delete_gateway_token))
        .route("/v1/admin/registrations", get(list_registrations))
        .route(
            "/v1/admin/registrations/:peer_id",
            delete(delete_registration),
        )
        .route(
            "/v1/admin/gateway-registrations",
            get(list_gateway_registrations),
        )
        .route(
            "/v1/admin/gateway-registrations/:peer_id",
            delete(delete_gateway_registration),
        )
        .route("/v1/dashboard/status", get(get_dashboard_status))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ));

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/workers/join", post(post_join))
        .route("/v1/workers/heartbeat", post(post_heartbeat))
        .route("/v1/workers/lookup", post(post_workers_lookup))
        .route("/v1/gateways/join", post(post_gateway_join))
        .route("/v1/gateways/heartbeat", post(post_gateway_heartbeat))
        .route("/v1/internal/routes/resolve/:cid", get(resolve_cid))
        .merge(admin_routes)
        .with_state(state);

    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  beenet-registry ADMIN TOKEN ({admin_token_source})");
    println!("  {admin_token}");
    println!("  Authorization: Bearer <token>  →  /v1/admin/*  /v1/dashboard/*");
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

#[cfg(test)]
mod tests;
