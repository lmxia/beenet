//! Beenet M1.5 worker: libp2p invoke + Spin [`FactorsExecutor`](spin_factors_executor::FactorsExecutor)
//! (flat [`BeenetFactors`](beenet_factors::BeenetFactors)) + wasi:http p2.

mod executor;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use beenet_common::config::{resolve_config_path_with_cli, WorkerCliOverrides, WorkerSettings};
use beenet_common::{BeenetCid, INVOKE_PROTOCOL};
use beenet_factors::BeenetFactors;
use beenet_manifest::Manifest;
use beenet_proto::{InvokeRequest, InvokeResponse, LoadStage, Status, TimeoutStage, Usage};
use clap::Parser;
use futures::StreamExt;
use libp2p::request_response::{self, ProtocolSupport};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, ping, Multiaddr, PeerId, StreamProtocol, SwarmBuilder};
use serde::{Deserialize, Serialize};
use spin_app::AppComponent;
use spin_core::wasmtime::component::Component;
use spin_factors_executor::{ComponentLoader, FactorsExecutor};
use tokio::sync::{watch, RwLock, Semaphore};
use tracing::{info, warn};

use crate::executor::{invoke_prepared, load_factors_app, ExecOutcome};

/// CLI overrides for fields also set in `config.toml` under `[worker]`.
#[derive(Parser, Debug, Clone)]
#[command(name = "beenet-worker", about = "Beenet worker (M1.5 factors)")]
struct Args {
    /// `config.toml` path (default: platform config dir `beenet/config.toml`).
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    #[arg(long)]
    listen_addr: Option<String>,

    #[arg(long)]
    wasm_cache_dir: Option<PathBuf>,

    #[arg(long)]
    default_deadline_ms: Option<u32>,

    #[arg(long)]
    default_memory_mb: Option<u32>,

    /// Worker-wide hard cap (L1) on per-instance linear memory (`target.md` D14).
    #[arg(long)]
    max_instance_memory_mb: Option<u32>,

    #[arg(long)]
    max_concurrency: Option<usize>,

    /// HTTP registry base URL; overrides `[worker].registry_url` in config.
    #[arg(long)]
    registry_url: Option<String>,

    /// Heartbeat `POST` path; overrides `[worker].registry_heartbeat_path`.
    #[arg(long)]
    registry_heartbeat_path: Option<String>,

    /// Join token; overrides `[worker].join_token` (must match `[registry].join_token`).
    #[arg(long)]
    join_token: Option<String>,

    #[arg(long)]
    registry_heartbeat_secs: Option<u64>,

    /// Optional `GET {base}/{cid}` base for wasm cache misses.
    #[arg(long)]
    wasm_fetch_base: Option<String>,

    #[arg(long)]
    wasm_fetch_bearer: Option<String>,

    #[arg(long)]
    wasm_fetch_timeout_secs: Option<u64>,
}

#[derive(NetworkBehaviour)]
struct WorkerBehaviour {
    request_response: request_response::cbor::Behaviour<InvokeRequest, InvokeResponse>,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
}

pub struct TaskEntry {
    pub manifest: Manifest,
    pub factors_app: Arc<spin_factors_executor::FactorsExecutorApp<BeenetFactors, ()>>,
    pub component_id: String,
    pub supported_cids: Vec<String>,
}

struct Runtime {
    factors_executor: Arc<FactorsExecutor<BeenetFactors, ()>>,
    wasm_cache_dir: PathBuf,
    wasm_fetch_base: Option<String>,
    wasm_fetch_bearer: Option<String>,
    wasm_fetch_timeout: Duration,
    default_deadline_ms: u32,
    default_memory_mb: u32,
    max_instance_memory_mb: u32,
    cache: RwLock<HashMap<BeenetCid, Arc<TaskEntry>>>,
    gate: Arc<Semaphore>,
}

struct BeenetComponentLoader {
    wasm_cache_dir: PathBuf,
}

#[async_trait]
impl ComponentLoader<BeenetFactors, ()> for BeenetComponentLoader {
    async fn load_component(
        &self,
        engine: &spin_core::wasmtime::Engine,
        component: &AppComponent,
    ) -> anyhow::Result<Component> {
        let path = self.wasm_cache_dir.join(format!("{}.wasm", component.id()));
        // 我们这里不是composed，区别于spin 原生的，带有依赖管理的components。
        let wasm =
            fs::read(&path).map_err(|e| anyhow::anyhow!("read wasm `{}`: {e}", path.display()))?;
        Component::new(engine, &wasm).map_err(|e| anyhow::anyhow!("compile component: {e}"))
    }
}

impl Runtime {
    fn new(factors_executor: Arc<FactorsExecutor<BeenetFactors, ()>>, s: &WorkerSettings) -> Self {
        let max_concurrency = s.max_concurrency.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get() * 4)
                .unwrap_or(8)
        });
        Self {
            factors_executor,
            wasm_cache_dir: s.wasm_cache_dir.clone(),
            wasm_fetch_base: s
                .wasm_fetch_base
                .as_ref()
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty()),
            wasm_fetch_bearer: s.wasm_fetch_bearer.clone().filter(|x| !x.trim().is_empty()),
            wasm_fetch_timeout: Duration::from_secs(s.wasm_fetch_timeout_secs.max(1)),
            default_deadline_ms: s.default_deadline_ms,
            default_memory_mb: s.default_memory_mb,
            max_instance_memory_mb: s.max_instance_memory_mb,
            cache: RwLock::new(HashMap::new()),
            gate: Arc::new(Semaphore::new(max_concurrency)),
        }
    }

    async fn execute(&self, req: InvokeRequest) -> InvokeResponse {
        let started = Instant::now();

        let permit = match self.gate.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                return InvokeResponse {
                    request_id: req.request_id,
                    status: Status::Rejected {
                        reason: "worker concurrency gate exhausted".into(),
                    },
                    body: Vec::new(),
                    stdout: String::new(),
                    stderr: String::new(),
                    usage: Usage {
                        wall_ns: started.elapsed().as_nanos() as u64,
                        ai_infer_calls: 0,
                        ai_embedding_calls: 0,
                        ai_prompt_tokens: 0,
                        ai_generated_tokens: 0,
                        billable: false,
                        ..Usage::default()
                    },
                };
            }
        };

        let out = self.execute_inner(&req, started).await;
        drop(permit);

        match out {
            Ok(resp) => resp,
            Err(err) => {
                warn!(
                    cid = %req.cid,
                    request_id = %req.request_id,
                    error = ?err,
                    "task execution failed"
                );
                InvokeResponse {
                    request_id: req.request_id,
                    status: Status::RuntimeError {
                        reason: err.to_string(),
                    },
                    body: Vec::new(),
                    stdout: String::new(),
                    stderr: String::new(),
                    usage: Usage {
                        wall_ns: started.elapsed().as_nanos() as u64,
                        ai_infer_calls: 0,
                        ai_embedding_calls: 0,
                        ai_prompt_tokens: 0,
                        ai_generated_tokens: 0,
                        billable: true,
                        ..Usage::default()
                    },
                }
            }
        }
    }

    async fn execute_inner(&self, req: &InvokeRequest, started: Instant) -> Result<InvokeResponse> {
        let entry = match self.load_task(&req.cid).await {
            Ok(e) => e,
            Err(e) => {
                return Ok(InvokeResponse {
                    request_id: req.request_id.clone(),
                    status: Status::LoadError {
                        stage: LoadStage::Fetch,
                        reason: format!("{e:#}"),
                    },
                    body: Vec::new(),
                    stdout: String::new(),
                    stderr: String::new(),
                    usage: Usage {
                        wall_ns: started.elapsed().as_nanos() as u64,
                        ai_infer_calls: 0,
                        ai_embedding_calls: 0,
                        ai_prompt_tokens: 0,
                        ai_generated_tokens: 0,
                        billable: false,
                        ..Usage::default()
                    },
                });
            }
        };

        let deadline_ms = entry
            .manifest
            .runtime
            .deadline_ms
            .unwrap_or(self.default_deadline_ms)
            .min(req.deadline_ms.max(1));
        let chargeable_memory_mb = entry
            .manifest
            .runtime
            .max_memory_mb
            .unwrap_or(self.default_memory_mb)
            .min(self.max_instance_memory_mb);
        let max_memory_bytes = (chargeable_memory_mb as usize)
            .saturating_mul(1024 * 1024)
            .max(1);

        let invoke_fut = invoke_prepared(
            entry.factors_app.as_ref(),
            &entry.component_id,
            req,
            deadline_ms,
            max_memory_bytes,
        );

        let outcome =
            match tokio::time::timeout(Duration::from_millis(deadline_ms as u64), invoke_fut).await
            {
                Ok(Ok(v)) => v,
                Ok(Err(err)) => {
                    warn!(
                        cid = %req.cid,
                        request_id = %req.request_id,
                        error = %err,
                        "executor invoke returned error"
                    );
                    return Ok(InvokeResponse {
                        request_id: req.request_id.clone(),
                        status: Status::RuntimeError {
                            reason: err.to_string(),
                        },
                        body: Vec::new(),
                        stdout: String::new(),
                        stderr: String::new(),
                        usage: Usage {
                            wall_ns: started.elapsed().as_nanos() as u64,
                            chargeable_memory_mb,
                            ai_infer_calls: 0,
                            ai_embedding_calls: 0,
                            ai_prompt_tokens: 0,
                            ai_generated_tokens: 0,
                            billable: true,
                            ..Usage::default()
                        },
                    });
                }
                Err(_) => {
                    return Ok(InvokeResponse {
                        request_id: req.request_id.clone(),
                        status: Status::Timeout {
                            stage: TimeoutStage::Exec,
                        },
                        body: Vec::new(),
                        stdout: String::new(),
                        stderr: String::new(),
                        usage: Usage {
                            wall_ns: started.elapsed().as_nanos() as u64,
                            chargeable_memory_mb,
                            ai_infer_calls: 0,
                            ai_embedding_calls: 0,
                            ai_prompt_tokens: 0,
                            ai_generated_tokens: 0,
                            billable: true,
                            ..Usage::default()
                        },
                    });
                }
            };

        let ExecOutcome {
            status,
            body,
            stdout,
            stderr,
            mem_bytes,
            ai_usage,
        } = outcome;

        if !stdout.is_empty() {
            info!(cid = %req.cid, request_id = %req.request_id, stdout = %stdout);
        }
        if !stderr.is_empty() {
            warn!(cid = %req.cid, request_id = %req.request_id, stderr = %stderr);
        }

        let usage = Usage {
            wall_ns: started.elapsed().as_nanos() as u64,
            mem_bytes,
            chargeable_memory_mb,
            ai_infer_calls: ai_usage.infer_calls,
            ai_embedding_calls: ai_usage.embedding_calls,
            ai_prompt_tokens: ai_usage.prompt_tokens,
            ai_generated_tokens: ai_usage.generated_tokens,
            billable: status.is_billable_compute(),
            ..Usage::default()
        };

        Ok(InvokeResponse {
            request_id: req.request_id.clone(),
            status,
            body,
            stdout,
            stderr,
            usage,
        })
    }

    /// CIDs present on disk under `wasm_cache_dir` (servable / "cold or warm").
    fn supported_cids_on_disk(&self) -> Vec<String> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&self.wasm_cache_dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("wasm") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if stem.starts_with("bafy") || stem.starts_with("bafk") {
                    out.push(stem.to_string());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// CIDs currently loaded in the in-memory runtime cache ("hot").
    async fn loaded_cids(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .cache
            .read()
            .await
            .keys()
            .map(ToString::to_string)
            .collect();
        out.sort();
        out
    }

    async fn load_task(&self, cid: &BeenetCid) -> Result<Arc<TaskEntry>> {
        if let Some(entry) = self.cache.read().await.get(cid).cloned() {
            return Ok(entry);
        }

        self.ensure_wasm_cached(cid).await?;

        let wasm_path = self.wasm_path(cid);
        let wasm = fs::read(&wasm_path)
            .with_context(|| format!("read cached wasm `{}`", wasm_path.display()))?;
        let manifest = beenet_manifest::extract(&wasm).context("manifest extraction failed")?;
        let loader = BeenetComponentLoader {
            wasm_cache_dir: self.wasm_cache_dir.clone(),
        };
        let factors_app = load_factors_app(self.factors_executor.clone(), cid, &manifest, &loader)
            .await
            .context("load_factors_app failed")?;
        let component_id = cid.to_string();
        let entry = Arc::new(TaskEntry {
            manifest,
            factors_app,
            component_id,
            supported_cids: vec![cid.to_string()],
        });
        self.cache.write().await.insert(cid.clone(), entry.clone());
        Ok(entry)
    }

    fn wasm_path(&self, cid: &BeenetCid) -> PathBuf {
        self.wasm_cache_dir.join(format!("{cid}.wasm"))
    }

    async fn ensure_wasm_cached(&self, cid: &BeenetCid) -> Result<()> {
        let path = self.wasm_path(cid);
        if path.exists() {
            return Ok(());
        }
        let Some(base) = self.wasm_fetch_base.as_deref() else {
            anyhow::bail!(
                "wasm cache miss for {cid}: file `{}` missing and [worker].wasm_fetch_base is not set",
                path.display()
            );
        };
        let url = wasm_fetch_url(base, cid);
        info!(%url, %cid, "fetching wasm into cache");
        let client = reqwest::Client::builder()
            .timeout(self.wasm_fetch_timeout)
            .build()
            .context("build HTTP client for wasm fetch")?;
        let mut req = client.get(url.clone());
        if let Some(ref token) = self.wasm_fetch_bearer {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("wasm fetch from {} returned HTTP {}", url, resp.status());
        }
        let bytes = resp.bytes().await.context("wasm fetch read body")?;
        if bytes.is_empty() {
            anyhow::bail!("wasm fetch returned empty body from {url}");
        }
        let got = BeenetCid::from_bytes(&bytes);
        if &got != cid {
            anyhow::bail!("wasm content CID mismatch: expected {cid}, got {got} (check object key and corruption)");
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create `{}`", parent.display()))?;
        }
        fs::write(&path, &bytes).with_context(|| format!("write `{}`", path.display()))?;
        info!(path = %path.display(), %cid, "wasm stored in cache after fetch");
        Ok(())
    }
}

/// `GET {trimmed_base}/{cid_string}` — same path segment `beenet-pack upload` uses for object key tail.
fn wasm_fetch_url(base: &str, cid: &BeenetCid) -> String {
    format!("{}/{}", base.trim_end_matches('/'), cid)
}

fn registry_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

// ── Identity persistence ───────────────────────────────────────────────────

/// Load the worker's Ed25519 keypair from `<wasm_cache_dir>/identity.key`.
/// If the file does not exist, generate a new keypair and persist it.
fn load_or_create_keypair(wasm_cache_dir: &PathBuf) -> Result<identity::Keypair> {
    let key_path = wasm_cache_dir.join("identity.key");
    if key_path.exists() {
        let bytes = fs::read(&key_path)
            .with_context(|| format!("read identity key `{}`", key_path.display()))?;
        let keypair = identity::Keypair::from_protobuf_encoding(&bytes)
            .with_context(|| format!("decode identity key `{}`", key_path.display()))?;
        info!(path = %key_path.display(), "loaded persistent identity keypair");
        Ok(keypair)
    } else {
        let keypair = identity::Keypair::generate_ed25519();
        let bytes = keypair
            .to_protobuf_encoding()
            .context("encode new identity keypair")?;
        fs::write(&key_path, &bytes)
            .with_context(|| format!("write identity key `{}`", key_path.display()))?;
        info!(
            path = %key_path.display(),
            peer_id = %PeerId::from(keypair.public()),
            "generated and saved new identity keypair"
        );
        Ok(keypair)
    }
}

// ── Signed payload ─────────────────────────────────────────────────────────

/// Build the canonical signed message and return its base64-encoded Ed25519 signature.
fn make_signature(
    keypair: &identity::Keypair,
    peer_id: &str,
    timestamp_secs: u64,
) -> Result<String> {
    let msg = format!("{peer_id}\n{timestamp_secs}");
    let sig = keypair
        .sign(msg.as_bytes())
        .map_err(|e| anyhow::anyhow!("signing failed: {e}"))?;
    Ok(STANDARD.encode(sig))
}

fn unix_secs_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Registry HTTP types ────────────────────────────────────────────────────

#[derive(Serialize)]
struct JoinBody {
    join_token: String,
    peer_id: String,
    public_key: String,
    timestamp_secs: u64,
    signature: String,
    supported_cids: Vec<String>,
    loaded_cids: Vec<String>,
}

#[derive(Deserialize)]
struct JoinResponse {
    ok: bool,
}

#[derive(Serialize)]
struct HeartbeatBody {
    peer_id: String,
    timestamp_secs: u64,
    signature: String,
    supported_cids: Vec<String>,
    loaded_cids: Vec<String>,
}

// ── Registration & heartbeat logic ────────────────────────────────────────

/// Call `POST /v1/workers/join`. Returns `Ok(())` on success.
async fn do_join(
    http: &reqwest::Client,
    join_url: &str,
    keypair: &identity::Keypair,
    peer_id: &str,
    supported_cids: Vec<String>,
    loaded_cids: Vec<String>,
    join_token: &str,
) -> Result<()> {
    let ts = unix_secs_now();
    let sig = make_signature(keypair, peer_id, ts)?;
    let pubkey_bytes = keypair.public().encode_protobuf();
    let body = JoinBody {
        join_token: join_token.to_owned(),
        peer_id: peer_id.to_owned(),
        public_key: STANDARD.encode(&pubkey_bytes),
        timestamp_secs: ts,
        signature: sig,
        supported_cids,
        loaded_cids,
    };
    let resp = http
        .post(join_url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {join_url}"))?;
    let status = resp.status();
    if status.is_success() {
        let jr: JoinResponse = resp.json().await.context("parse join response")?;
        if jr.ok {
            info!(%peer_id, "worker registered with registry");
            return Ok(());
        }
        anyhow::bail!("join returned ok=false");
    }
    let text = resp.text().await.unwrap_or_default();
    anyhow::bail!("join rejected (HTTP {status}): {text}")
}

/// Send one heartbeat. Returns `true` if accepted (200), `false` if 401 (unregistered).
async fn do_heartbeat(
    http: &reqwest::Client,
    heartbeat_url: &str,
    keypair: &identity::Keypair,
    peer_id: &str,
    supported_cids: Vec<String>,
    loaded_cids: Vec<String>,
) -> Result<bool> {
    let ts = unix_secs_now();
    let sig = make_signature(keypair, peer_id, ts)?;
    let body = HeartbeatBody {
        peer_id: peer_id.to_owned(),
        timestamp_secs: ts,
        signature: sig,
        supported_cids,
        loaded_cids,
    };
    let resp = http
        .post(heartbeat_url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {heartbeat_url}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(false);
    }
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("heartbeat failed (HTTP {status}): {text}");
    }
    Ok(true)
}

async fn registry_heartbeat_loop(
    http: reqwest::Client,
    heartbeat_url: String,
    join_url: String,
    keypair: Arc<identity::Keypair>,
    peer_id: String,
    runtime: Arc<Runtime>,
    period: Duration,
    join_token: Option<String>,
) {
    let mut interval = tokio::time::interval(period);
    loop {
        interval.tick().await;
        let supported_cids = runtime.supported_cids_on_disk();
        let loaded_cids = runtime.loaded_cids().await;
        match do_heartbeat(
            &http,
            &heartbeat_url,
            &keypair,
            &peer_id,
            supported_cids.clone(),
            loaded_cids.clone(),
        )
        .await
        {
            Ok(true) => {
                info!("registry heartbeat ok");
            }
            Ok(false) => {
                // Registry restarted or registration was revoked — attempt re-join.
                warn!("heartbeat rejected (unregistered); attempting re-join");
                if let Some(ref token) = join_token {
                    if let Err(e) = do_join(
                        &http,
                        &join_url,
                        &keypair,
                        &peer_id,
                        supported_cids,
                        loaded_cids,
                        token,
                    )
                    .await
                    {
                        warn!(error = %e, "re-join failed");
                    }
                } else {
                    warn!(
                        "worker is not registered and no join_token is configured; \
                        worker will be invisible to the gateway until manually re-registered"
                    );
                }
            }
            Err(e) => {
                warn!(error = %e, "registry heartbeat request error");
            }
        }
    }
}

fn build_swarm(local_key: identity::Keypair) -> Result<libp2p::Swarm<WorkerBehaviour>> {
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
        .with_dns()?
        .with_behaviour(|key| WorkerBehaviour {
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
        // Keep the reverse long connection to the gateway; default idle timeout is ~10s.
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(3600)))
        .build();
    Ok(swarm)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct GatewayCandidate {
    peer_id: String,
    dial_addr: String,
}

#[derive(Deserialize)]
struct GatewaysBody {
    gateways: Vec<GatewayCandidate>,
}

fn merge_gateway_candidates(
    discovered: Vec<GatewayCandidate>,
    fallback: &[GatewayCandidate],
) -> Vec<GatewayCandidate> {
    let mut merged = Vec::new();
    for candidate in discovered.into_iter() {
        if merged.len() >= 2 {
            break;
        }
        if candidate.peer_id.trim().is_empty() || candidate.dial_addr.trim().is_empty() {
            continue;
        }
        if PeerId::from_str(&candidate.peer_id).is_err() {
            continue;
        }
        if candidate.dial_addr.parse::<Multiaddr>().is_err() {
            continue;
        }
        if merged
            .iter()
            .any(|existing: &GatewayCandidate| existing.peer_id == candidate.peer_id)
        {
            continue;
        }
        merged.push(candidate);
    }

    for candidate in fallback {
        if merged.len() >= 2 {
            break;
        }
        if !merged
            .iter()
            .any(|existing| existing.peer_id == candidate.peer_id)
        {
            merged.push(candidate.clone());
        }
    }
    merged
}

fn gateway_peer_set(candidates: &[GatewayCandidate]) -> std::collections::HashSet<PeerId> {
    candidates
        .iter()
        .filter_map(|candidate| PeerId::from_str(&candidate.peer_id).ok())
        .collect()
}

fn reconnect_jitter() -> Duration {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| (value.subsec_millis() % 750) as u64)
        .unwrap_or(0);
    Duration::from_millis(millis)
}

async fn gateway_discovery_loop(
    registry_url: String,
    fallback: Vec<GatewayCandidate>,
    tx: watch::Sender<Vec<GatewayCandidate>>,
) {
    let client = reqwest::Client::new();
    let url = format!("{}/v1/gateways", registry_url.trim_end_matches('/'));
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    loop {
        interval.tick().await;
        match client.get(&url).send().await {
            Ok(response) if response.status().is_success() => {
                match response.json::<GatewaysBody>().await {
                    Ok(body) => {
                        let merged = merge_gateway_candidates(body.gateways, &fallback);
                        if *tx.borrow() != merged {
                            let _ = tx.send(merged);
                        }
                    }
                    Err(error) => {
                        warn!(%error, "gateway discovery JSON decode failed; retaining cached candidates")
                    }
                }
            }
            Ok(response) => warn!(status = %response.status(), "gateway discovery rejected"),
            Err(error) => warn!(%error, "gateway discovery failed; retaining cached candidates"),
        }
    }
}

async fn run_swarm_loop(
    runtime: Arc<Runtime>,
    mut gateways: watch::Receiver<Vec<GatewayCandidate>>,
    mut swarm: libp2p::Swarm<WorkerBehaviour>,
) {
    let mut connected = std::collections::HashSet::new();
    let mut dial_in_flight = std::collections::HashSet::new();
    let mut desired_gateways = gateway_peer_set(&gateways.borrow());
    let mut retry = tokio::time::interval(Duration::from_secs(5));

    loop {
        tokio::select! {
            _ = retry.tick() => {
                tokio::time::sleep(reconnect_jitter()).await;
                for candidate in gateways.borrow().clone() {
                    let Ok(peer_id) = PeerId::from_str(&candidate.peer_id) else { continue };
                    if !desired_gateways.contains(&peer_id) || connected.contains(&peer_id) || dial_in_flight.contains(&peer_id) { continue; }
                    let Ok(address) = candidate.dial_addr.parse::<Multiaddr>() else { warn!(%peer_id, "invalid gateway dial address"); continue; };
                    match swarm.dial(address) { Ok(()) => { dial_in_flight.insert(peer_id); }, Err(error) => warn!(%peer_id, %error, "gateway dial failed") }
                }
            }
            _ = gateways.changed() => {
                desired_gateways = gateway_peer_set(&gateways.borrow());
                info!(count = desired_gateways.len(), "gateway candidates updated");
                for peer_id in connected.clone() {
                    if !desired_gateways.contains(&peer_id) {
                        if swarm.disconnect_peer_id(peer_id).is_err() {
                            warn!(%peer_id, "failed to disconnect stale gateway");
                        }
                    }
                }
            }
            event = swarm.select_next_some() => match event {
                SwarmEvent::ConnectionEstablished {
                    peer_id, endpoint, ..
                } => {
                    info!(%peer_id, ?endpoint, "connection established");
                    dial_in_flight.remove(&peer_id);
                    if desired_gateways.contains(&peer_id) {
                        connected.insert(peer_id);
                        info!(%peer_id, "reverse long connection to gateway ready");
                    } else {
                        warn!(%peer_id, "disconnecting unselected gateway");
                        let _ = swarm.disconnect_peer_id(peer_id);
                    }
                }
                SwarmEvent::ConnectionClosed { peer_id, num_established, .. } => {
                    info!(%peer_id, remaining = num_established, "connection closed");
                    if num_established == 0 { connected.remove(&peer_id); dial_in_flight.remove(&peer_id); }
                }
                SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                    warn!(?peer_id, ?error, "outgoing connection error");
                    if let Some(peer_id) = peer_id { dial_in_flight.remove(&peer_id); }
                }
                SwarmEvent::IncomingConnectionError {
                    local_addr,
                    send_back_addr,
                    error,
                    ..
                } => {
                    warn!(%local_addr, %send_back_addr, ?error, "incoming connection error");
                }
                SwarmEvent::NewListenAddr { address, .. } => {
                    info!("worker listening at {address}");
                }
                SwarmEvent::Behaviour(WorkerBehaviourEvent::RequestResponse(
                    request_response::Event::Message { peer, message, .. },
                )) => match message {
                    request_response::Message::Request {
                        request, channel, ..
                    } => {
                        if !desired_gateways.contains(&peer) { warn!(%peer, "invoke rejected from unselected gateway"); continue; }
                        info!(from = %peer, cid = %request.cid, request_id = %request.request_id, "invoke");
                        let response = runtime.execute(request).await;
                        if let Err(resp) = swarm
                            .behaviour_mut()
                            .request_response
                            .send_response(channel, response)
                        {
                            warn!("failed to send response: {:?}", resp);
                        }
                    }
                    request_response::Message::Response { .. } => {}
                },
                SwarmEvent::Behaviour(WorkerBehaviourEvent::Identify(event)) => {
                    info!("identify: {:?}", event);
                }
                SwarmEvent::Behaviour(WorkerBehaviourEvent::Ping(event)) => {
                    info!("ping: {:?}", event);
                }
                SwarmEvent::Behaviour(WorkerBehaviourEvent::RequestResponse(
                    request_response::Event::InboundFailure { peer, error, .. },
                )) => {
                    warn!(%peer, ?error, "inbound failure");
                }
                SwarmEvent::Behaviour(WorkerBehaviourEvent::RequestResponse(
                    request_response::Event::OutboundFailure { peer, error, .. },
                )) => {
                    warn!(%peer, ?error, "outbound failure");
                }
                SwarmEvent::Behaviour(WorkerBehaviourEvent::RequestResponse(
                    request_response::Event::ResponseSent { peer, .. },
                )) => {
                    info!(%peer, "response sent");
                }
                other => {
                    info!("swarm event: {:?}", other);
                }
            }
        }
    }
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
    if !path.exists() {
        anyhow::bail!(
            "missing config file `{}` (add a [worker] table or pass --config)",
            path.display()
        );
    }
    let file_cfg = beenet_common::config::load_file(&path)?;
    let overrides = WorkerCliOverrides {
        listen_addr: cli.listen_addr.clone(),
        wasm_cache_dir: cli.wasm_cache_dir.clone(),
        default_deadline_ms: cli.default_deadline_ms,
        default_memory_mb: cli.default_memory_mb,
        max_instance_memory_mb: cli.max_instance_memory_mb,
        max_concurrency: cli.max_concurrency,
        registry_url: cli.registry_url.clone(),
        registry_heartbeat_path: cli.registry_heartbeat_path.clone(),
        join_token: cli.join_token.clone(),
        registry_heartbeat_secs: cli.registry_heartbeat_secs,
        wasm_fetch_base: cli.wasm_fetch_base.clone(),
        wasm_fetch_bearer: cli.wasm_fetch_bearer.clone(),
        wasm_fetch_timeout_secs: cli.wasm_fetch_timeout_secs,
    };
    let settings = beenet_common::config::resolve_worker_settings(&file_cfg, &overrides)?;
    let listen_addr: Multiaddr = settings
        .listen_addr
        .parse()
        .with_context(|| format!("invalid listen multiaddr `{}`", settings.listen_addr))?;
    fs::create_dir_all(&settings.wasm_cache_dir)
        .with_context(|| format!("create `{}`", settings.wasm_cache_dir.display()))?;

    let factors = BeenetFactors::new();
    let engine_builder = spin_core::Engine::builder(&spin_core::Config::default())?;
    let factors_executor = Arc::new(FactorsExecutor::new(engine_builder, factors)?);
    let runtime = Arc::new(Runtime::new(factors_executor, &settings));

    // Load or generate a persistent Ed25519 keypair from wasm_cache_dir/identity.key.
    let local_key = load_or_create_keypair(&settings.wasm_cache_dir)?;
    let local_peer_id = PeerId::from(local_key.public());
    let mut swarm = build_swarm(local_key.clone())?;
    swarm.listen_on(listen_addr.clone())?;
    let fallback_gateways: Vec<GatewayCandidate> = Vec::new();
    let (gateway_tx, gateway_rx) = watch::channel(fallback_gateways.clone());
    tokio::spawn(gateway_discovery_loop(
        settings.registry_url.clone(),
        fallback_gateways,
        gateway_tx,
    ));
    tokio::spawn(run_swarm_loop(runtime.clone(), gateway_rx, swarm));

    info!(
        peer_id = %local_peer_id,
        listen_addr = %listen_addr,
        wasm_cache_dir = %settings.wasm_cache_dir.display(),
        max_concurrency = runtime.gate.available_permits(),
        max_instance_memory_mb = runtime.max_instance_memory_mb,
        "worker started (dials gateway; keeps reverse long connection)"
    );

    let heartbeat_url = registry_url(&settings.registry_url, &settings.registry_heartbeat_path);
    let join_url = registry_url(&settings.registry_url, "/v1/workers/join");
    let http = reqwest::Client::new();
    let keypair_arc = Arc::new(local_key);
    let peer_s = local_peer_id.to_string();

    // Initial registration: try heartbeat first; if unregistered, attempt join.
    let initial_supported_cids = runtime.supported_cids_on_disk();
    let initial_loaded_cids = runtime.loaded_cids().await;
    match do_heartbeat(
        &http,
        &heartbeat_url,
        &keypair_arc,
        &peer_s,
        initial_supported_cids.clone(),
        initial_loaded_cids.clone(),
    )
    .await
    {
        Ok(true) => {
            info!(peer_id = %local_peer_id, "worker already registered with registry");
        }
        Ok(false) => {
            // Not yet registered — join now.
            if let Some(ref token) = settings.join_token {
                do_join(
                    &http,
                    &join_url,
                    &keypair_arc,
                    &peer_s,
                    initial_supported_cids,
                    initial_loaded_cids,
                    token,
                )
                .await
                .context("initial worker registration failed")?;
            } else {
                anyhow::bail!(
                    "worker is not registered with the registry and no join_token is configured; \
                    set [worker].join_token in config or pass --join-token"
                );
            }
        }
        Err(e) => {
            // Registry not reachable yet — log a warning and let the heartbeat loop retry.
            warn!(error = %e, "registry not reachable at startup; will retry in heartbeat loop");
        }
    }

    let period = Duration::from_secs(settings.registry_heartbeat_secs);
    let join_token = settings.join_token.clone();
    tokio::spawn(registry_heartbeat_loop(
        http,
        heartbeat_url,
        join_url,
        keypair_arc,
        peer_s,
        runtime.clone(),
        period,
        join_token,
    ));
    tokio::signal::ctrl_c()
        .await
        .context("wait for worker shutdown signal")?;
    info!("worker shutdown requested");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::wasm_fetch_url;
    use super::*;
    use beenet_common::BeenetCid;
    use std::str::FromStr;

    #[test]
    fn wasm_fetch_url_joins_base_and_cid() {
        let cid =
            BeenetCid::from_str("bafkreigdvzf6jabcvbsyiqf27ew4zrqwxehehv7xg2tnfds4aq325jv4xu")
                .unwrap();
        assert_eq!(
            wasm_fetch_url("https://example.com/wasm/", &cid),
            format!("https://example.com/wasm/{cid}")
        );
    }
}
