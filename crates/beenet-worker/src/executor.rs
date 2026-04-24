//! Pluggable task executors.
//!
//! A `TaskExecutor` owns the per-interface translation between a Beenet
//! [`InvokeRequest`] and a wasm component invocation. M1 provides exactly one:
//! [`Wasip2HttpExecutor`] for `wasi:http/incoming-handler@0.2` components. A
//! future `beenet:task/runner@0.1` executor (gear 1, `readme.md §3.2`, M3) can
//! be added by implementing this trait without touching the trigger / cache /
//! gating layers.

use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use beenet_proto::{InvokeRequest, Status};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use wasmtime_wasi_http::p2::body::{HyperIncomingBody, HyperOutgoingBody};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::Store;
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::p2::bindings::http::types::{ErrorCode, Scheme};
use wasmtime_wasi_http::p2::bindings::ProxyPre;
use wasmtime_wasi_http::p2::{WasiHttpCtxView, WasiHttpView};
use wasmtime_wasi_http::WasiHttpCtx;

use crate::TaskEntry;

/// Per-store host state plumbed into every wasm instance.
///
/// `target.md §D16`: capability-not-granted default — we build `WasiCtx` with
/// only stdio wired and no env / no preopens / no `inherit_network`. M1.5 will
/// replace this with BeenetFactors.
pub struct HostState {
    wasi: WasiCtx,
    http: WasiHttpCtx,
    table: ResourceTable,
}

impl HostState {
    fn new() -> (Self, MemoryOutputPipe, MemoryOutputPipe) {
        let stdout = MemoryOutputPipe::new(64 * 1024);
        let stderr = MemoryOutputPipe::new(64 * 1024);
        let wasi = WasiCtxBuilder::new()
            .stdout(stdout.clone())
            .stderr(stderr.clone())
            .build();
        let state = Self {
            wasi,
            http: WasiHttpCtx::new(),
            table: ResourceTable::new(),
        };
        (state, stdout, stderr)
    }
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for HostState {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: Default::default(),
        }
    }
}

/// Result of a single task invocation as produced by a [`TaskExecutor`].
pub struct ExecOutcome {
    pub status: Status,
    pub body: Vec<u8>,
    pub stdout: String,
    pub stderr: String,
}

/// Contract all task interfaces implement.
///
/// `Prepared` is the executor-specific cached artefact produced at
/// [`prepare`](Self::prepare) time (typically a
/// [`wasmtime::component::InstancePre`] wrapped by a generated `Pre` struct).
/// It is stored once per CID in the L1 cache (`readme.md §6.1`).
#[async_trait]
pub trait TaskExecutor: Send + Sync + 'static {
    type Prepared: Send + Sync + 'static;

    /// Compile-time / instantiate-time preparation for a component. Called
    /// once per CID on first invocation.
    fn prepare(&self, component: &Component) -> Result<Self::Prepared>;

    /// Invoke the prepared component for a single request.
    async fn invoke(&self, entry: &TaskEntry<Self>, req: &InvokeRequest) -> Result<ExecOutcome>
    where
        Self: Sized;
}

/// `wasi:http/incoming-handler@0.2` executor (gear 0, M1).
///
/// Synthesises a `hyper::Request` from [`InvokeRequest`], routes it through
/// [`ProxyPre::instantiate_async`] + the generated `incoming_handler`, then
/// drains the outgoing body into [`ExecOutcome::body`]. HTTP status is mapped
/// to [`Status`] per `readme.md §3.2.2` (A/B table).
pub struct Wasip2HttpExecutor {
    linker: Arc<Linker<HostState>>,
}

impl Wasip2HttpExecutor {
    pub fn new(linker: Linker<HostState>) -> Self {
        Self {
            linker: Arc::new(linker),
        }
    }
}

#[async_trait]
impl TaskExecutor for Wasip2HttpExecutor {
    type Prepared = ProxyPre<HostState>;

    fn prepare(&self, component: &Component) -> Result<Self::Prepared> {
        let instance_pre = self
            .linker
            .instantiate_pre(component)
            .map_err(|e| anyhow!("instantiate_pre: {e}"))?;
        ProxyPre::new(instance_pre)
            .map_err(|e| anyhow!("component is not a wasi:http/proxy world: {e}"))
    }

    async fn invoke(&self, entry: &TaskEntry<Self>, req: &InvokeRequest) -> Result<ExecOutcome> {
        let (state, stdout_pipe, stderr_pipe) = HostState::new();
        let mut store = Store::new(entry.prepared.engine(), state);

        let http_req = build_incoming_request(req)?;

        let (sender, receiver) = tokio::sync::oneshot::channel();
        let incoming = store
            .data_mut()
            .http()
            .new_incoming_request(Scheme::Http, http_req)
            .map_err(|e| anyhow!("new_incoming_request failed: {e}"))?;
        let outparam = store
            .data_mut()
            .http()
            .new_response_outparam(sender)
            .map_err(|e| anyhow!("new_response_outparam failed: {e}"))?;

        let proxy_pre = entry.prepared.clone();
        let task = tokio::spawn(async move {
            let proxy = proxy_pre.instantiate_async(&mut store).await?;
            proxy
                .wasi_http_incoming_handler()
                .call_handle(&mut store, incoming, outparam)
                .await?;
            Ok::<_, anyhow::Error>(store)
        });

        // Wait for the guest to either call `response-outparam::set` (success
        // or explicit failure) or drop the outparam (which closes the oneshot
        // sender; in that case we inspect the task result).
        let store = match receiver.await {
            Ok(Ok(resp)) => {
                let (status, body) = drain_response(resp).await?;
                let _ = task.await;
                return Ok(finalise(status, body, stdout_pipe, stderr_pipe));
            }
            Ok(Err(err)) => {
                // Guest called `response-outparam::set(err)` — surface as runtime.
                let _ = task.await;
                return Ok(ExecOutcome {
                    status: Status::RuntimeError {
                        reason: format!("guest rejected request: {err}"),
                    },
                    body: Vec::new(),
                    stdout: drain_pipe(&stdout_pipe),
                    stderr: drain_pipe(&stderr_pipe),
                });
            }
            Err(_) => {
                // Sender dropped without `set`; inspect the task.
                match task.await {
                    Ok(Ok(s)) => s,
                    Ok(Err(e)) => bail!("guest trapped before setting response: {e}"),
                    Err(e) => bail!("guest task panicked: {e}"),
                }
            }
        };
        // Unreachable in the success path; placeholder to keep the compiler happy
        // if this branch is taken (guest finished cleanly but never set outparam,
        // which we treat as a runtime error).
        let _ = store;
        bail!("guest finished without calling response-outparam::set");
    }
}

fn build_incoming_request(req: &InvokeRequest) -> Result<hyper::Request<HyperIncomingBody>> {
    // `HyperIncomingBody = UnsyncBoxBody<Bytes, ErrorCode>`, so we map the
    // infallible `Full` error into `ErrorCode` and use `.boxed_unsync()`.
    let body: HyperIncomingBody = Full::new(Bytes::from(req.input.clone()))
        .map_err(|never: std::convert::Infallible| -> ErrorCode { match never {} })
        .boxed_unsync();
    // `wasi:http/types.new_incoming_request` requires either the URI to carry an
    // authority or a `Host` header. Beenet invocations are always synthesised
    // with a fixed authority `beenet.local` — task code that cares about the
    // authority can read `x-beenet-cid` instead.
    let mut builder = hyper::Request::builder()
        .method(http::Method::POST)
        .uri("http://beenet.local/")
        .header(http::header::HOST, "beenet.local")
        .header("x-beenet-request-id", &req.request_id)
        .header("x-beenet-cid", req.cid.to_string())
        .header("x-beenet-deadline-ms", req.deadline_ms.to_string());
    if let Some(caller) = &req.caller_peer {
        builder = builder.header("x-beenet-caller-peer", caller);
    }
    builder.body(body).context("build hyper request")
}

async fn drain_response(
    resp: hyper::Response<HyperOutgoingBody>,
) -> Result<(u16, Vec<u8>)> {
    let status = resp.status().as_u16();
    let collected = resp
        .into_body()
        .collect()
        .await
        .context("collect response body")?;
    Ok((status, collected.to_bytes().to_vec()))
}

fn finalise(
    http_status: u16,
    body: Vec<u8>,
    stdout_pipe: MemoryOutputPipe,
    stderr_pipe: MemoryOutputPipe,
) -> ExecOutcome {
    let status = match http_status {
        200..=299 => Status::Ok,
        400..=499 => Status::BusinessError {
            http_status,
            reason: http_reason(http_status),
        },
        500..=599 => Status::RuntimeError {
            reason: format!("guest returned {http_status}"),
        },
        other => Status::RuntimeError {
            reason: format!("unexpected status {other}"),
        },
    };
    ExecOutcome {
        status,
        body,
        stdout: drain_pipe(&stdout_pipe),
        stderr: drain_pipe(&stderr_pipe),
    }
}

fn drain_pipe(pipe: &MemoryOutputPipe) -> String {
    let bytes = pipe.contents();
    String::from_utf8_lossy(&bytes).into_owned()
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
