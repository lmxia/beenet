//! Spin [`FactorsExecutor`](spin_factors_executor::FactorsExecutor) + wasi:http p2 invoke path (M1.5).

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use beenet_common::proto::{InvokeRequest, Status};
use beenet_factors::{ai_usage_snapshot, AiUsageSnapshot, BeenetFactors};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use spin_app::App;
use spin_factor_outbound_http::OutboundHttpFactor;
use spin_factor_wasi::WasiFactor;
use spin_factors_executor::{FactorsExecutor, FactorsExecutorApp};
use wasmtime::CallHook;
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi_http::p2::bindings::http::types::{ErrorCode, Scheme};
use wasmtime_wasi_http::p2::bindings::ProxyPre;
use wasmtime_wasi_http::p2::body::{HyperIncomingBody, HyperOutgoingBody};

/// Max stdout/stderr bytes shipped on `InvokeResponse` (M1.5 wire cap).
pub const WIRE_LOG_CAP_BYTES: usize = 16 * 1024;

/// Per-invoke guest CPU meter stored in Spin `executor_instance_state`.
///
/// Guest wall-clock slices only (host imports such as outbound HTTP are excluded).
#[derive(Debug, Default)]
pub struct CpuMeter {
    elapsed: Duration,
    last_entry: Option<Instant>,
}

impl CpuMeter {
    fn on_hook(&mut self, hook: CallHook) {
        match hook {
            CallHook::CallingWasm | CallHook::ReturningFromHost => {
                self.last_entry = Some(Instant::now());
            }
            CallHook::ReturningFromWasm | CallHook::CallingHost => {
                if let Some(entered) = self.last_entry.take() {
                    self.elapsed += entered.elapsed();
                }
            }
        }
    }

    /// Guest CPU-ish nanoseconds, including an in-progress guest slice (e.g. trap).
    pub fn cpu_ns(&self) -> u64 {
        let extra = self
            .last_entry
            .map(|entered| entered.elapsed())
            .unwrap_or_default();
        (self.elapsed + extra).as_nanos() as u64
    }
}

pub type BeenetExecutor = FactorsExecutor<BeenetFactors, CpuMeter>;
pub type BeenetExecutorApp = FactorsExecutorApp<BeenetFactors, CpuMeter>;

pub struct ExecOutcome {
    pub status: Status,
    pub body: Vec<u8>,
    pub stdout: String,
    pub stderr: String,
    pub cpu_ns: u64,
    pub mem_bytes: u64,
    pub ai_usage: AiUsageSnapshot,
}

/// Run one HTTP invoke against a prepared Spin factors app (single `http` component).
pub async fn invoke_prepared(
    app: &BeenetExecutorApp,
    component_id: &str,
    req: &InvokeRequest,
    deadline_ms: u32,
    max_memory_bytes: usize,
) -> Result<ExecOutcome> {
    let ai_before = ai_usage_snapshot();
    let stdout_pipe = MemoryOutputPipe::new(64 * 1024);
    let stderr_pipe = MemoryOutputPipe::new(64 * 1024);

    let mut instance_builder = app.prepare(component_id)?;
    instance_builder
        .store_builder()
        .max_memory_size(max_memory_bytes.max(1));

    let wasi = instance_builder
        .factor_builder::<WasiFactor>()
        .context("missing WasiFactor")?;
    wasi.stdout(stdout_pipe.clone());
    wasi.stderr(stderr_pipe.clone());

    let instance_pre = app
        .get_instance_pre(component_id)
        .context("get_instance_pre")?
        .clone();
    let proxy_pre = ProxyPre::new(instance_pre)
        .map_err(|e| anyhow!("component is not a wasi:http/proxy world: {e}"))?;

    let (_instance, mut store) = instance_builder
        .instantiate(CpuMeter::default())
        .await?;
    store.as_mut().call_hook(|mut store, hook| {
        store.data_mut().executor_instance_state_mut().on_hook(hook);
        Ok(())
    });

    let deadline = Instant::now() + Duration::from_millis(deadline_ms as u64);
    store.set_deadline(deadline);

    let http_req = build_incoming_request(req)?;

    let mut wasi_http =
        OutboundHttpFactor::get_wasi_http_impl(store.data_mut().factors_instance_state_mut())
            .context("missing OutboundHttpFactor / wasi-http state")?;

    let (sender, receiver) = tokio::sync::oneshot::channel();
    let incoming = wasi_http
        .new_incoming_request(Scheme::Http, http_req)
        .map_err(|e| anyhow!("new_incoming_request failed: {e}"))?;
    let outparam = wasi_http
        .new_response_outparam(sender)
        .map_err(|e| anyhow!("new_response_outparam failed: {e}"))?;

    let proxy = proxy_pre.instantiate_async(&mut store).await?;
    let task = tokio::spawn(async move {
        proxy
            .wasi_http_incoming_handler()
            .call_handle(&mut store, incoming, outparam)
            .await?;
        Ok::<_, anyhow::Error>(store)
    });

    match receiver.await {
        Ok(Ok(resp)) => {
            let (status, body) = drain_response(resp).await?;
            let store = task.await.context("guest task join")??;
            let (cpu_ns, mem_bytes) = read_invoke_usage(&store);
            Ok(finalise(
                status,
                body,
                stdout_pipe,
                stderr_pipe,
                cpu_ns,
                mem_bytes,
                ai_before,
            ))
        }
        Ok(Err(err)) => {
            let (cpu_ns, mem_bytes) = match task.await {
                Ok(Ok(store)) => read_invoke_usage(&store),
                _ => (0, 0),
            };
            Ok(ExecOutcome {
                status: Status::RuntimeError {
                    reason: format!("guest rejected request: {err}"),
                },
                body: Vec::new(),
                stdout: drain_pipe_truncated(&stdout_pipe),
                stderr: drain_pipe_truncated(&stderr_pipe),
                cpu_ns,
                mem_bytes,
                ai_usage: ai_usage_snapshot().delta_since(ai_before),
            })
        }
        Err(_) => match task.await {
            Ok(Ok(store)) => {
                let (cpu_ns, mem_bytes) = read_invoke_usage(&store);
                Ok(ExecOutcome {
                    status: Status::RuntimeError {
                        reason: "guest finished without calling response-outparam::set".into(),
                    },
                    body: Vec::new(),
                    stdout: drain_pipe_truncated(&stdout_pipe),
                    stderr: drain_pipe_truncated(&stderr_pipe),
                    cpu_ns,
                    mem_bytes,
                    ai_usage: ai_usage_snapshot().delta_since(ai_before),
                })
            }
            Ok(Err(e)) => bail!("guest trapped before setting response: {e}"),
            Err(e) => bail!("guest task panicked: {e}"),
        },
    }
}

fn read_invoke_usage<T>(
    store: &spin_core::Store<spin_factors_executor::InstanceState<T, CpuMeter>>,
) -> (u64, u64) {
    let cpu_ns = store.data().executor_instance_state().cpu_ns();
    let mem_bytes = store.data().core_state().memory_consumed();
    (cpu_ns, mem_bytes)
}

fn build_incoming_request(req: &InvokeRequest) -> Result<hyper::Request<HyperIncomingBody>> {
    let body: HyperIncomingBody = Full::new(Bytes::from(req.input.clone()))
        .map_err(|never: std::convert::Infallible| -> ErrorCode { match never {} })
        .boxed_unsync();
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

async fn drain_response(resp: hyper::Response<HyperOutgoingBody>) -> Result<(u16, Vec<u8>)> {
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
    cpu_ns: u64,
    mem_bytes: u64,
    ai_before: AiUsageSnapshot,
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
        stdout: drain_pipe_truncated(&stdout_pipe),
        stderr: drain_pipe_truncated(&stderr_pipe),
        cpu_ns,
        mem_bytes,
        ai_usage: ai_usage_snapshot().delta_since(ai_before),
    }
}

fn drain_pipe_truncated(pipe: &MemoryOutputPipe) -> String {
    let bytes = pipe.contents();
    let s = String::from_utf8_lossy(&bytes).into_owned();
    truncate_utf8(s, WIRE_LOG_CAP_BYTES)
}

fn truncate_utf8(mut s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push_str("\n… [truncated]");
    s
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

/// Build [`spin_app::App`] + load through [`spin_factors_executor::FactorsExecutor::load_app`].
pub async fn load_factors_app(
    executor: Arc<BeenetExecutor>,
    cid: &beenet_common::BeenetCid,
    manifest: &beenet_artifact::Manifest,
    loader: &impl spin_factors_executor::ComponentLoader<BeenetFactors, CpuMeter>,
) -> Result<Arc<BeenetExecutorApp>> {
    let locked = beenet_factors::locked_app_single_http_component(
        &cid.to_string(),
        &manifest.networking.allowed_outbound_hosts,
        &manifest.ai.allowed_models,
    )?;
    let app = App::new("beenet-task", locked);
    let app_loaded = executor
        .load_app(app, Default::default(), loader, Some("http"))
        .await?;
    Ok(Arc::new(app_loaded))
}
