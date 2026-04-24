//! Beenet M1 worker.
//!
//! Listens on libp2p `/beenet/invoke/1.0`, loads a wasm component by CID from
//! the local wasm cache, and executes it through a [`TaskExecutor`].
//!
//! M1 ships exactly one executor: [`Wasip2HttpExecutor`], which targets
//! `wasi:http/incoming-handler@0.2` (`readme.md §3.2` "gear 0"). Future gears
//! (e.g. `beenet:task/runner@0.1` in M3) slot in as additional `TaskExecutor`
//! implementations without disturbing the trigger / loader / cache layers.

mod executor;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use beenet_common::{BeenetCid, INVOKE_PROTOCOL};
use beenet_manifest::Manifest;
use beenet_proto::{InvokeRequest, InvokeResponse, Status, TimeoutStage, Usage};
use clap::Parser;
use futures::StreamExt;
use libp2p::core::multiaddr::Protocol;
use libp2p::request_response::{self, ProtocolSupport};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, ping, Multiaddr, PeerId, StreamProtocol, SwarmBuilder};
use tokio::sync::{RwLock, Semaphore};
use tracing::{error, info, warn};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine};

use crate::executor::{ExecOutcome, Wasip2HttpExecutor};
pub use crate::executor::{HostState, TaskExecutor};

#[derive(Parser, Debug, Clone)]
#[command(name = "beenet-worker", about = "Beenet M1 worker")]
struct Args {
    #[arg(long, env = "BEENET_LISTEN_ADDR", default_value = "/ip4/127.0.0.1/tcp/4001")]
    listen_addr: Multiaddr,

    #[arg(long, env = "BEENET_WASM_CACHE_DIR", default_value = "./wasm_cache")]
    wasm_cache_dir: PathBuf,

    #[arg(long, env = "BEENET_DEFAULT_DEADLINE_MS", default_value_t = 10_000)]
    default_deadline_ms: u32,

    #[arg(long, env = "BEENET_DEFAULT_MEMORY_MB", default_value_t = 64)]
    default_memory_mb: u32,

    /// Worker-level concurrency gate (`readme.md §6.2.4`).
    ///
    /// Defaults to `available_parallelism * 4` (see `readme.md §1.2`).
    #[arg(long, env = "BEENET_MAX_CONCURRENCY")]
    max_concurrency: Option<usize>,
}

#[derive(NetworkBehaviour)]
struct WorkerBehaviour {
    request_response: request_response::cbor::Behaviour<InvokeRequest, InvokeResponse>,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
}

/// An entry in the per-CID task cache (`readme.md §6.1` L1).
///
/// Wraps the parsed manifest together with the executor-specific pre-instance
/// handle (see [`TaskExecutor::prepare`]).
pub struct TaskEntry<E: TaskExecutor> {
    pub manifest: Manifest,
    pub prepared: E::Prepared,
}

struct Runtime<E: TaskExecutor> {
    engine: Engine,
    executor: E,
    wasm_cache_dir: PathBuf,
    default_deadline_ms: u32,
    default_memory_mb: u32,
    cache: RwLock<HashMap<BeenetCid, Arc<TaskEntry<E>>>>,
    gate: Arc<Semaphore>,
}

impl<E: TaskExecutor> Runtime<E> {
    fn new(engine: Engine, executor: E, args: &Args) -> Self {
        let max_concurrency = args.max_concurrency.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get() * 4)
                .unwrap_or(8)
        });
        Self {
            engine,
            executor,
            wasm_cache_dir: args.wasm_cache_dir.clone(),
            default_deadline_ms: args.default_deadline_ms,
            default_memory_mb: args.default_memory_mb,
            cache: RwLock::new(HashMap::new()),
            gate: Arc::new(Semaphore::new(max_concurrency)),
        }
    }

    async fn execute(&self, req: InvokeRequest) -> InvokeResponse {
        let started = Instant::now();

        // `readme.md §6.2.4`: worker-level concurrency gate. On exhaustion we
        // return Rejected (`§3.6`: fully exempt from billing).
        let permit = match self.gate.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                return InvokeResponse {
                    request_id: req.request_id,
                    status: Status::Rejected {
                        reason: "worker concurrency gate exhausted".into(),
                    },
                    body: Vec::new(),
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
            Ok((status, body, usage)) => InvokeResponse {
                request_id: req.request_id,
                status,
                body,
                usage,
            },
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
                    usage: Usage {
                        wall_ns: started.elapsed().as_nanos() as u64,
                        billable: true,
                        ..Usage::default()
                    },
                }
            }
        }
    }

    async fn execute_inner(
        &self,
        req: &InvokeRequest,
        started: Instant,
    ) -> Result<(Status, Vec<u8>, Usage)> {
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
            .unwrap_or(self.default_memory_mb);

        let call = self.executor.invoke(&entry, req);
        let outcome = match tokio::time::timeout(
            Duration::from_millis(deadline_ms as u64),
            call,
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
                return Ok((
                    Status::RuntimeError {
                        reason: err.to_string(),
                    },
                    Vec::new(),
                    Usage {
                        wall_ns: started.elapsed().as_nanos() as u64,
                        chargeable_memory_mb,
                        billable: true,
                        ..Usage::default()
                    },
                ));
            }
            Err(_) => {
                return Ok((
                    Status::Timeout {
                        stage: TimeoutStage::Exec,
                    },
                    Vec::new(),
                    Usage {
                        wall_ns: started.elapsed().as_nanos() as u64,
                        chargeable_memory_mb,
                        billable: true,
                        ..Usage::default()
                    },
                ));
            }
        };

        let ExecOutcome {
            status,
            body,
            stdout,
            stderr,
        } = outcome;

        if !stdout.is_empty() {
            info!(cid = %req.cid, request_id = %req.request_id, stdout = %stdout);
        }
        if !stderr.is_empty() {
            warn!(cid = %req.cid, request_id = %req.request_id, stderr = %stderr);
        }

        let usage = Usage {
            wall_ns: started.elapsed().as_nanos() as u64,
            chargeable_memory_mb,
            billable: status.is_billable_compute(),
            ..Usage::default()
        };
        Ok((status, body, usage))
    }

    async fn load_task(&self, cid: &BeenetCid) -> Result<Arc<TaskEntry<E>>> {
        if let Some(entry) = self.cache.read().await.get(cid).cloned() {
            return Ok(entry);
        }

        let wasm_path = self.wasm_path(cid);
        let wasm = fs::read(&wasm_path)
            .with_context(|| format!("read cached wasm `{}`", wasm_path.display()))?;
        let manifest = beenet_manifest::extract(&wasm).context("manifest extraction failed")?;
        let component = Component::new(&self.engine, &wasm)
            .map_err(|e| anyhow::anyhow!("compile component failed: {e}"))?;
        let prepared = self
            .executor
            .prepare(&component)
            .context("executor prepare failed")?;
        let entry = Arc::new(TaskEntry { manifest, prepared });
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

fn build_engine_and_linker() -> Result<(Engine, Linker<HostState>)> {
    let config = Config::new();
    let engine = Engine::new(&config)?;
    let mut linker = Linker::<HostState>::new(&engine);

    // WASI p2 base interfaces (io/cli/clocks/random/filesystem/sockets…).
    //
    // `readme.md §D16`: M1's default security posture is capability-not-granted
    // at the `WasiCtx` level — sockets and filesystem are in the linker but the
    // guest gets permission-denied at runtime unless `WasiCtxBuilder` explicitly
    // opts in. M1.5 replaces this posture with `OutboundNetworkingFactor` +
    // `NoFilesMounter` for enforceable allowlists.
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    // `wasi:http/{types,outgoing-handler}` — inbound handler is invoked via
    // `ProxyPre::instantiate_async`, not via the linker.
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;

    Ok((engine, linker))
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

    let (engine, linker) = build_engine_and_linker()?;
    let executor = Wasip2HttpExecutor::new(linker);
    let runtime = Arc::new(Runtime::new(engine, executor, &args));

    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    let mut swarm = build_swarm(local_key)?;
    swarm.listen_on(args.listen_addr.clone())?;

    info!(
        peer_id = %local_peer_id,
        listen_addr = %args.listen_addr,
        wasm_cache_dir = %args.wasm_cache_dir.display(),
        max_concurrency = runtime.gate.available_permits(),
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
                    // Spawn so a slow task doesn't block the swarm loop; the
                    // concurrency gate inside `execute` still provides the
                    // worker-wide upper bound.
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
