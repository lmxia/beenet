#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spin_sdk::http::{IntoResponse, Request, Response};
#[cfg(target_arch = "wasm32")]
use spin_sdk::http_component;

const ALGORITHM: &str = "fair-red-packet/sha256-weighted-v1";
const MAX_PARTICIPANTS: usize = 100;
const MAX_TOTAL_CENTS: u64 = 20_000_000;

#[derive(Deserialize)]
struct DrawRequest {
    total_yuan: String,
    participants: Vec<String>,
    public_seed: String,
}

#[derive(Serialize)]
struct Allocation {
    name: String,
    amount_yuan: String,
}

#[derive(Serialize)]
struct DrawResult {
    total_yuan: String,
    allocations: Vec<Allocation>,
    lucky_winner: String,
    public_seed: String,
    algorithm: &'static str,
    draw_id: String,
}

#[derive(Serialize)]
struct ErrorResponse<'a> {
    error: &'a str,
}

fn parse_yuan(value: &str) -> Result<u64, &'static str> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('-') || value.starts_with('+') {
        return Err("total_yuan must be a positive decimal string");
    }
    let mut parts = value.split('.');
    let yuan = parts.next().unwrap_or_default();
    let fraction = parts.next();
    if parts.next().is_some() || yuan.is_empty() || !yuan.bytes().all(|b| b.is_ascii_digit()) {
        return Err("total_yuan must have at most two decimal places");
    }
    let cents = match fraction {
        None => 0,
        Some("") => return Err("total_yuan must have at most two decimal places"),
        Some(digits) if digits.len() <= 2 && digits.bytes().all(|b| b.is_ascii_digit()) => {
            let parsed = digits.parse::<u64>().map_err(|_| "total_yuan is too large")?;
            if digits.len() == 1 { parsed * 10 } else { parsed }
        }
        _ => return Err("total_yuan must have at most two decimal places"),
    };
    yuan.parse::<u64>()
        .map_err(|_| "total_yuan is too large")?
        .checked_mul(100)
        .and_then(|amount| amount.checked_add(cents))
        .ok_or("total_yuan is too large")
}

fn format_yuan(cents: u64) -> String { format!("{}.{:02}", cents / 100, cents % 100) }

fn validate(input: &DrawRequest) -> Result<u64, &'static str> {
    let total = parse_yuan(&input.total_yuan)?;
    if input.participants.is_empty() || input.participants.len() > MAX_PARTICIPANTS {
        return Err("participants must contain between 1 and 100 names");
    }
    if total < input.participants.len() as u64 { return Err("total_yuan must provide at least 0.01 yuan per participant"); }
    if total > MAX_TOTAL_CENTS { return Err("total_yuan must not exceed 200000.00"); }
    if input.public_seed.trim().is_empty() || input.public_seed.len() > 256 { return Err("public_seed must contain between 1 and 256 bytes"); }
    for (i, name) in input.participants.iter().enumerate() {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 40 { return Err("each participant name must contain between 1 and 40 characters"); }
        if input.participants[..i].iter().any(|previous| previous.trim() == name) { return Err("participant names must be unique"); }
    }
    Ok(total)
}

fn field(hash: &mut Sha256, value: &[u8]) { hash.update((value.len() as u64).to_be_bytes()); hash.update(value); }

fn draw(input: DrawRequest, total: u64) -> DrawResult {
    let mut hash = Sha256::new();
    field(&mut hash, ALGORITHM.as_bytes());
    hash.update(total.to_be_bytes());
    field(&mut hash, input.public_seed.as_bytes());
    hash.update((input.participants.len() as u64).to_be_bytes());
    for name in &input.participants { field(&mut hash, name.trim().as_bytes()); }
    let draw_hash: [u8; 32] = hash.finalize().into();
    let count = input.participants.len();
    let distributable = total - count as u64;
    let weights: Vec<u64> = (0..count).map(|i| {
        let mut h = Sha256::new(); h.update(draw_hash); h.update((i as u64).to_be_bytes());
        let d: [u8; 32] = h.finalize().into();
        u64::from_be_bytes(d[..8].try_into().unwrap()) % 1_000_000 + 1
    }).collect();
    let sum: u128 = weights.iter().map(|w| *w as u128).sum();
    let mut cents = vec![1_u64; count];
    let mut allocated = 0_u64;
    let mut remainders = Vec::with_capacity(count);
    for (i, weight) in weights.iter().enumerate() {
        let numerator = distributable as u128 * *weight as u128;
        let share = (numerator / sum) as u64;
        cents[i] += share; allocated += share; remainders.push((i, numerator % sum));
    }
    remainders.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    for (i, _) in remainders.into_iter().take((distributable - allocated) as usize) { cents[i] += 1; }
    let lucky = cents.iter().enumerate().max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(&a.0))).unwrap().0;
    let lucky_winner = input.participants[lucky].trim().to_string();
    let mut draw_id = String::with_capacity(64);
    for byte in draw_hash { draw_id.push_str(&format!("{:02x}", byte)); }
    let allocations = input.participants.into_iter().zip(cents).map(|(name, amount)| Allocation { name: name.trim().to_string(), amount_yuan: format_yuan(amount) }).collect();
    DrawResult { total_yuan: format_yuan(total), allocations, lucky_winner, public_seed: input.public_seed, algorithm: ALGORITHM, draw_id }
}

fn json_response<T: Serialize>(status: u16, value: &T) -> Response {
    Response::builder().status(status).header("content-type", "application/json").body(serde_json::to_vec(value).unwrap()).build()
}

#[cfg_attr(target_arch = "wasm32", http_component)]
fn handle(req: Request) -> anyhow::Result<impl IntoResponse> {
    let input = match serde_json::from_slice::<DrawRequest>(&req.into_body()) { Ok(v) => v, Err(_) => return Ok(json_response(400, &ErrorResponse { error: "invalid JSON request" })) };
    let total = match validate(&input) { Ok(v) => v, Err(error) => return Ok(json_response(422, &ErrorResponse { error })) };
    Ok(json_response(200, &draw(input, total)))
}
