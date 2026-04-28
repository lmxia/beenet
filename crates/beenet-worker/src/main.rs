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
use beenet_common::{BeenetCid, INVOKE_PROTOCOL};
use beenet_factors::BeenetFactors;
use beenet_manifest::Manifest;
use beenet_proto::{InvokeRequest, InvokeResponse, Status, TimeoutStage, Usage};
use clap::Parser;
use futures::StreamExt;
use libp2p::core::multiaddr::Protocol;
use libp2p::request_response::{self, ProtocolSupport};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, ping, Multiaddr, PeerId, StreamProtocol, SwarmBuilder};
use spin_app::AppComponent;
use spin_core::wasmtime::component::Component;
use spin_factors_executor::{ComponentLoader, FactorsExecutor};
use tokio::sync::{RwLock, Semaphore};
use tracing::{error, info, warn};

use crate::executor::{invoke_prepared, load_factors_app, ExecOutcome};

#[derive(Parser, Debug, Clone)]
#[command(name = "beenet-worker", about = "Beenet worker (M1.5 factors)")]
struct Args {
    #[arg(long, env = "BEENET_LISTEN_ADDR", default_value = "/ip4/127.0.0.1/tcp/4001")]
    listen_addr: Multiaddr,

    #[arg(long, env = "BEENET_WASM_CACHE_DIR", default_value = "./wasm_cache")]
    wasm_cache_dir: PathBuf,

    #[arg(long, env = "BEENET_DEFAULT_DEADLINE_MS", default_value_t = 10_000)]
    default_deadline_ms: u32,

    #[arg(long, env = "BEENET_DEFAULT_MEMORY_MB", default_value_t = 64)]
    default_memory_mb: u32,

    /// Worker-wide hard cap (L1) on per-instance linear memory (`target.md` D14).
    #[arg(long, env = "BEENET_MAX_INSTANCE_MEMORY_MB", default_value_t = 256)]
    max_instance_memory_mb: u32,

    #[arg(long, env = "BEENET_MAX_CONCURRENCY")]
    max_concurrency: Option<usize>,
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
    fn new(factors_executor: Arc<FactorsExecutor<BeenetFactors, ()>>, args: &Args) -> Self {
        let max_concurrency = args.max_concurrency.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get() * 4)
                .unwrap_or(8)
        });
        Self {
            factors_executor,
            wasm_cache_dir: args.wasm_cache_dir.clone(),
            default_deadline_ms: args.default_deadline_ms,
            default_memory_mb: args.default_memory_mb,
            max_instance_memory_mb: args.max_instance_memory_mb,
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
        let entry = self
            .load_task(&req.cid)
            .await
            .with_context(|| format!("load task {}", req.cid))?;

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

    let args = Args::parse();
    fs::create_dir_all(&args.wasm_cache_dir)
        .with_context(|| format!("create `{}`", args.wasm_cache_dir.display()))?;

    let factors = BeenetFactors::new();
    let engine_builder = spin_core::Engine::builder(&spin_core::Config::default())?;
    let factors_executor = Arc::new(FactorsExecutor::new(engine_builder, factors)?);
    let runtime = Arc::new(Runtime::new(factors_executor, &args));

    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    let mut swarm = build_swarm(local_key)?;
    swarm.listen_on(args.listen_addr.clone())?;

    info!(
        peer_id = %local_peer_id,
        listen_addr = %args.listen_addr,
        wasm_cache_dir = %args.wasm_cache_dir.display(),
        max_concurrency = runtime.gate.available_permits(),
        max_instance_memory_mb = runtime.max_instance_memory_mb,
        "worker started"
    );

    while let Some(event) = swarm.next().await {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                let with_peer = address.with(Protocol::P2p(local_peer_id.into()));
                info!("worker reachable at {with_peer}");
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
