//! Beenet M1.5 worker: libp2p invoke + Spin [`FactorsExecutor`](spin_factors_executor::FactorsExecutor)
//! (flat [`BeenetFactors`](beenet_factors::BeenetFactors)) + wasi:http p2.

mod executor;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use beenet_common::config::{
    resolve_config_path_with_cli, WorkerCliOverrides, WorkerSettings,
};
use beenet_common::{BeenetCid, INVOKE_PROTOCOL};
use beenet_factors::BeenetFactors;
use beenet_manifest::Manifest;
use beenet_proto::{InvokeRequest, InvokeResponse, LoadStage, Status, TimeoutStage, Usage};
use clap::Parser;
use futures::StreamExt;
use libp2p::core::multiaddr::Protocol;
use libp2p::request_response::{self, ProtocolSupport};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, ping, Multiaddr, PeerId, StreamProtocol, SwarmBuilder};
use serde::Serialize;
use spin_app::AppComponent;
use spin_core::wasmtime::component::Component;
use spin_factors_executor::{ComponentLoader, FactorsExecutor};
use tokio::sync::{RwLock, Semaphore};
use tracing::{error, info, warn};

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
        let path = self
            .wasm_cache_dir
            .join(format!("{}.wasm", component.id()));
        // 我们这里不是composed，区别于spin 原生的，带有依赖管理的components。
        let wasm = fs::read(&path)
            .map_err(|e| anyhow::anyhow!("read wasm `{}`: {e}", path.display()))?;
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
            wasm_fetch_bearer: s
                .wasm_fetch_bearer
                .clone()
                .filter(|x| !x.trim().is_empty()),
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
        let max_memory_bytes = (chargeable_memory_mb as usize).saturating_mul(1024 * 1024).max(1);

        let invoke_fut = invoke_prepared(
            entry.factors_app.as_ref(),
            &entry.component_id,
            req,
            deadline_ms,
            max_memory_bytes,
        );

        let outcome = match tokio::time::timeout(
            Duration::from_millis(deadline_ms as u64),
            invoke_fut,
        )
        .await
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
        let factors_app = load_factors_app(
            self.factors_executor.clone(),
            cid,
            &manifest,
            &loader,
        )
        .await
        .context("load_factors_app failed")?;
        let component_id = cid.to_string();
        let entry = Arc::new(TaskEntry {
            manifest,
            factors_app,
            component_id,
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
            anyhow::bail!(
                "wasm fetch from {} returned HTTP {}",
                url,
                resp.status()
            );
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
            fs::create_dir_all(parent)
                .with_context(|| format!("create `{}`", parent.display()))?;
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

fn join_registry_heartbeat_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

#[derive(Serialize)]
struct RegistryHeartbeatBody<'a> {
    join_token: &'a str,
    peer_id: &'a str,
    dial_multiaddr: &'a str,
}

async fn registry_heartbeat_loop(
    http: reqwest::Client,
    heartbeat_url: String,
    join_token: String,
    peer_id: String,
    dial_multiaddr: String,
    period: Duration,
) {
    let mut interval = tokio::time::interval(period);
    loop {
        interval.tick().await;
        let body = RegistryHeartbeatBody {
            join_token: &join_token,
            peer_id: &peer_id,
            dial_multiaddr: &dial_multiaddr,
        };
        match http.post(&heartbeat_url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!(status = %resp.status(), "registry heartbeat ok");
            }
            Ok(resp) => {
                warn!(
                    status = %resp.status(),
                    "registry heartbeat rejected or failed"
                );
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

    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    let mut swarm = build_swarm(local_key)?;
    swarm.listen_on(listen_addr.clone())?;

    let dial_multiaddr = listen_addr
        .clone()
        .with(Protocol::P2p(local_peer_id.into()));
    let dial_str = dial_multiaddr.to_string();

    info!(
        peer_id = %local_peer_id,
        listen_addr = %listen_addr,
        dial_multiaddr = %dial_str,
        wasm_cache_dir = %settings.wasm_cache_dir.display(),
        max_concurrency = runtime.gate.available_permits(),
        max_instance_memory_mb = runtime.max_instance_memory_mb,
        "worker started"
    );

    let heartbeat_url =
        join_registry_heartbeat_url(&settings.registry_url, &settings.registry_heartbeat_path);
    let http = reqwest::Client::new();
    let peer_s = local_peer_id.to_string();
    let dial_owned = dial_str.clone();
    let period = Duration::from_secs(settings.registry_heartbeat_secs);
    let join_token = settings.join_token.clone();
    tokio::spawn(registry_heartbeat_loop(
        http,
        heartbeat_url,
        join_token,
        peer_s,
        dial_owned,
        period,
    ));

    while let Some(event) = swarm.next().await {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                let with_peer = address.with(Protocol::P2p(local_peer_id.into()));
                info!("worker listening at {with_peer} (dial multiaddr for heartbeat: {dial_str})");
            }
            SwarmEvent::Behaviour(WorkerBehaviourEvent::RequestResponse(
                request_response::Event::Message { peer, message, .. },
            )) => match message {
                request_response::Message::Request {
                    request, channel, ..
                } => {
                    info!(from = %peer, cid = %request.cid, request_id = %request.request_id, "invoke");
                    let runtime = runtime.clone();
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
            SwarmEvent::Behaviour(WorkerBehaviourEvent::Ping(_)) => {}
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

    error!("swarm ended unexpectedly");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::wasm_fetch_url;
    use beenet_common::BeenetCid;
    use std::str::FromStr;

    #[test]
    fn wasm_fetch_url_joins_base_and_cid() {
        let cid = BeenetCid::from_str(
            "bafkreigdvzf6jabcvbsyiqf27ew4zrqwxehehv7xg2tnfds4aq325jv4xu",
        )
        .unwrap();
        assert_eq!(
            wasm_fetch_url("https://example.com/wasm/", &cid),
            format!("https://example.com/wasm/{cid}")
        );
    }
}
