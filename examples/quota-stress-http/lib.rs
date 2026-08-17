use serde::{Deserialize, Serialize};
use spin_sdk::http::{IntoResponse, Request, Response};
use spin_sdk::http_component;
use std::time::Instant;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
struct Input {
    mode: String,
    iterations: Option<u64>,
    memory_mb: Option<usize>,
}

#[derive(Serialize)]
struct Output {
    mode: String,
    elapsed_ms: u128,
    iterations: u64,
    memory_mb: usize,
    checksum: u64,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

#[http_component]
fn handle(req: Request) -> anyhow::Result<impl IntoResponse> {
    let input: Input = match serde_json::from_slice(&req.into_body()) {
        Ok(input) => input,
        Err(error) => return json_error(400, format!("invalid JSON: {error}")),
    };

    let started = Instant::now();
    let mut checksum = 0u64;
    let mut iterations = 0u64;
    let mut memory_mb = 0usize;

    match input.mode.as_str() {
        "cpu" => {
            iterations = input.iterations.unwrap_or(50_000_000).min(2_000_000_000);
            checksum = cpu_burn(iterations);
        }
        "memory" => {
            memory_mb = input.memory_mb.unwrap_or(32).min(1024);
            checksum = memory_burn(memory_mb);
        }
        other => return json_error(400, format!("unsupported mode `{other}`")),
    }

    let body = serde_json::to_vec(&Output {
        mode: input.mode,
        elapsed_ms: started.elapsed().as_millis(),
        iterations,
        memory_mb,
        checksum,
    })?;
    Ok(Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(body)
        .build())
}

fn cpu_burn(iterations: u64) -> u64 {
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    for i in 0..iterations {
        x ^= i.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = x.rotate_left(13).wrapping_mul(0x94d0_49bb_1331_11eb);
    }
    x
}

fn memory_burn(memory_mb: usize) -> u64 {
    let bytes = memory_mb.saturating_mul(1024 * 1024);
    let mut data = vec![0u8; bytes];
    let mut checksum = 0u64;
    for (idx, byte) in data.iter_mut().enumerate().step_by(4096) {
        let value = (idx as u64).wrapping_mul(31).rotate_left(7) as u8;
        *byte = value;
        checksum = checksum.wrapping_add(value as u64);
    }
    checksum
}

fn json_error(status: u16, error: String) -> anyhow::Result<Response> {
    let body = serde_json::to_vec(&ErrorBody { error })?;
    Ok(Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(body)
        .build())
}
