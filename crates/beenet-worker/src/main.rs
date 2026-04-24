use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use beenet_common::{BeenetCid, INVOKE_PROTOCOL};
use beenet_manifest::Manifest;
use beenet_proto::{InvokeRequest, InvokeResponse, Status, TimeoutStage, Usage};
use clap::Parser;
use futures::StreamExt;
use libp2p::core::multiaddr::Protocol;
use libp2p::request_response::{self, ProtocolSupport};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, ping, Multiaddr, PeerId, StreamProtocol, SwarmBuilder};
use tokio::sync::RwLock;
use tracing::{error, info, warn};
use wasmtime::component::{Component, InstancePre, Linker};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "wit",
    world: "http-trigger",
});

use fermyon::spin::http_types::{Method, Request, Response};

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
}

#[derive(NetworkBehaviour)]
struct WorkerBehaviour {
    request_response: request_response::cbor::Behaviour<InvokeRequest, InvokeResponse>,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
}

#[derive(Clone)]
struct TaskEntry {
    manifest: Manifest,
    pre: InstancePre<HostState>,
}

struct Runtime {
    engine: Engine,
    linker: Linker<HostState>,
    wasm_cache_dir: PathBuf,
    default_deadline_ms: u32,
    default_memory_mb: u32,
    cache: RwLock<HashMap<BeenetCid, Arc<TaskEntry>>>,
}

struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    stdout: MemoryOutputPipe,
    stderr: MemoryOutputPipe,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl Runtime {
    fn new(args: &Args) -> Result<Self> {
        let config = Config::new();
        let engine = Engine::new(&config)?;

        let mut linker = Linker::<HostState>::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;

        Ok(Self {
            engine,
            linker,
            wasm_cache_dir: args.wasm_cache_dir.clone(),
            default_deadline_ms: args.default_deadline_ms,
            default_memory_mb: args.default_memory_mb,
            cache: RwLock::new(HashMap::new()),
        })
    }

    async fn execute(&self, req: InvokeRequest) -> InvokeResponse {
        let started = Instant::now();
        match self.execute_inner(&req).await {
            Ok((status, body, usage)) => InvokeResponse {
                request_id: req.request_id,
                status,
                body,
                usage,
            },
            Err(err) => InvokeResponse {
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
            },
        }
    }

    async fn execute_inner(&self, req: &InvokeRequest) -> Result<(Status, Vec<u8>, Usage)> {
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

        let started = Instant::now();
        let call = self.call_component(&entry, req);
        let (status, body, stdout, stderr) = match tokio::time::timeout(
            Duration::from_millis(deadline_ms as u64),
            call,
        )
        .await
        {
            Ok(Ok(v)) => v,
            Ok(Err(err)) => {
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

    async fn call_component(
        &self,
        entry: &TaskEntry,
        req: &InvokeRequest,
    ) -> Result<(Status, Vec<u8>, String, String)> {
        let stdout = MemoryOutputPipe::new(64 * 1024);
        let stderr = MemoryOutputPipe::new(64 * 1024);
        let wasi = WasiCtxBuilder::new()
            .stdout(stdout.clone())
            .stderr(stderr.clone())
            .build();
        let state = HostState {
            wasi,
            table: ResourceTable::new(),
            stdout,
            stderr,
        };

        let mut store = Store::new(&self.engine, state);
        let instance = entry.pre.instantiate_async(&mut store).await?;
        let func = instance
            .get_export_index(&mut store, None, "fermyon:spin/inbound-http")
            .and_then(|i| instance.get_export_index(&mut store, Some(&i), "handle-request"))
            .ok_or_else(|| anyhow!("component did not export fermyon:spin/inbound-http/handle-request"))?;
        let func = instance.get_typed_func::<(Request,), (Response,)>(&mut store, &func)?;

        let request = Request {
            method: Method::Post,
            uri: "/".to_string(),
            headers: vec![
                ("x-beenet-request-id".into(), req.request_id.clone()),
                ("x-beenet-cid".into(), req.cid.to_string()),
                ("x-beenet-deadline-ms".into(), req.deadline_ms.to_string()),
            ],
            params: vec![],
            body: Some(req.input.clone()),
        };

        let (response,) = func.call_async(&mut store, (request,)).await?;

        // Drop WASI resources holding the pipe refs before `try_into_inner`.
        store.data_mut().wasi = WasiCtxBuilder::new().build();
        *store.data_mut().ctx().table = ResourceTable::new();
        let stdout = std::mem::replace(&mut store.data_mut().stdout, MemoryOutputPipe::new(1));
        let stderr = std::mem::replace(&mut store.data_mut().stderr, MemoryOutputPipe::new(1));

        let stdout = String::from_utf8_lossy(&stdout.try_into_inner().unwrap_or_default()).into_owned();
        let stderr = String::from_utf8_lossy(&stderr.try_into_inner().unwrap_or_default()).into_owned();

        let body = response.body.unwrap_or_default();
        let status = match response.status {
            200..=299 => Status::Ok,
            400..=499 => Status::BusinessError {
                http_status: response.status,
                reason: http_reason(response.status),
            },
            500..=599 => Status::RuntimeError {
                reason: format!("guest returned {}", response.status),
            },
            other => Status::RuntimeError {
                reason: format!("unexpected status {other}"),
            },
        };

        Ok((status, body, stdout, stderr))
    }

    async fn load_task(&self, cid: &BeenetCid) -> Result<Arc<TaskEntry>> {
        if let Some(entry) = self.cache.read().await.get(cid).cloned() {
            return Ok(entry);
        }

        let wasm_path = self.wasm_path(cid);
        let wasm = fs::read(&wasm_path)
            .with_context(|| format!("read cached wasm `{}`", wasm_path.display()))?;
        let manifest = beenet_manifest::extract(&wasm)
            .map_err(|e| anyhow!("manifest extraction failed: {e}"))?;
        let component =
            Component::new(&self.engine, &wasm).map_err(|e| anyhow!("compile failed: {e}"))?;
        let pre = self
            .linker
            .instantiate_pre(&component)
            .map_err(|e| anyhow!("instantiate_pre failed: {e}"))?;
        let entry = Arc::new(TaskEntry { manifest, pre });
        self.cache.write().await.insert(cid.clone(), entry.clone());
        Ok(entry)
    }

    fn wasm_path(&self, cid: &BeenetCid) -> PathBuf {
        self.wasm_cache_dir.join(format!("{cid}.wasm"))
    }
}

fn http_reason(status: u16) -> String {
    match status {
        400 => "bad request",
        401 => "unauthorized",
        403 => "forbidden",
        404 => "not found",
        409 => "conflict",
        422 => "unprocessable entity",
        429 => "too many requests",
        500 => "internal server error",
        502 => "bad gateway",
        503 => "service unavailable",
        504 => "gateway timeout",
        _ => "http error",
    }
    .to_string()
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

    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    let mut swarm = build_swarm(local_key)?;
    swarm.listen_on(args.listen_addr.clone())?;

    let runtime = Arc::new(Runtime::new(&args)?);

    info!(
        peer_id = %local_peer_id,
        listen_addr = %args.listen_addr,
        wasm_cache_dir = %args.wasm_cache_dir.display(),
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
