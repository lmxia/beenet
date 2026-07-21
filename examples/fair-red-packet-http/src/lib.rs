#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spin_sdk::http::{IntoResponse, Request, Response};
#[cfg(target_arch = "wasm32")]
use spin_sdk::http_component;

const ALGORITHM: &str = "fair-red-packet/sha256-weighted-v1";
const MAX_PARTICIPANTS: usize = 100;
const MAX_TOTAL_CENTS: u64 = 20_000_000;

#[derive(Debug, Deserialize)]
struct DrawRequest {
    total_yuan: String,
    participants: Vec<String>,
    public_seed: String,
}

#[derive(Debug, PartialEq, Serialize)]
struct Allocation {
    name: String,
    amount_yuan: String,
}

#[derive(Debug, PartialEq, Serialize)]
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
    if parts.next().is_some() || yuan.is_empty() || !yuan.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("total_yuan must have at most two decimal places");
    }
    let cents = match fraction {
        None => 0,
        Some("") => return Err("total_yuan must have at most two decimal places"),
        Some(digits) if digits.len() <= 2 && digits.bytes().all(|b| b.is_ascii_digit()) => {
            let parsed = digits
                .parse::<u64>()
                .map_err(|_| "total_yuan is too large")?;
            if digits.len() == 1 {
                parsed * 10
            } else {
                parsed
            }
        }
        _ => return Err("total_yuan must have at most two decimal places"),
    };
    yuan.parse::<u64>()
        .map_err(|_| "total_yuan is too large")?
        .checked_mul(100)
        .and_then(|amount| amount.checked_add(cents))
        .ok_or("total_yuan is too large")
}

fn format_yuan(cents: u64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

fn validate(request: &DrawRequest) -> Result<u64, &'static str> {
    let total_cents = parse_yuan(&request.total_yuan)?;
    if request.participants.is_empty() || request.participants.len() > MAX_PARTICIPANTS {
        return Err("participants must contain between 1 and 100 names");
    }
    if total_cents < request.participants.len() as u64 {
        return Err("total_yuan must provide at least 0.01 yuan per participant");
    }
    if total_cents > MAX_TOTAL_CENTS {
        return Err("total_yuan must not exceed 200000.00");
    }
    if request.public_seed.trim().is_empty() || request.public_seed.len() > 256 {
        return Err("public_seed must contain between 1 and 256 bytes");
    }
    for (index, name) in request.participants.iter().enumerate() {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 40 {
            return Err("each participant name must contain between 1 and 40 characters");
        }
        if request.participants[..index]
            .iter()
            .any(|previous| previous.trim() == name)
        {
            return Err("participant names must be unique");
        }
    }
    Ok(total_cents)
}

fn update_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn canonical_hash(request: &DrawRequest, total_cents: u64) -> [u8; 32] {
    let mut hasher = Sha256::new();
    update_field(&mut hasher, ALGORITHM.as_bytes());
    hasher.update(total_cents.to_be_bytes());
    update_field(&mut hasher, request.public_seed.as_bytes());
    hasher.update((request.participants.len() as u64).to_be_bytes());
    for name in &request.participants {
        update_field(&mut hasher, name.trim().as_bytes());
    }
    hasher.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn draw(request: DrawRequest, total_cents: u64) -> DrawResult {
    let draw_hash = canonical_hash(&request, total_cents);
    let count = request.participants.len();
    let distributable = total_cents - count as u64;

    let weights: Vec<u64> = (0..count)
        .map(|index| {
            let mut hasher = Sha256::new();
            hasher.update(draw_hash);
            hasher.update((index as u64).to_be_bytes());
            let digest: [u8; 32] = hasher.finalize().into();
            u64::from_be_bytes(digest[..8].try_into().expect("eight-byte slice")) % 1_000_000 + 1
        })
        .collect();
    let weight_sum: u128 = weights.iter().map(|weight| *weight as u128).sum();

    let mut cents = vec![1_u64; count];
    let mut remainders = Vec::with_capacity(count);
    let mut allocated = 0_u64;
    for (index, weight) in weights.iter().enumerate() {
        let numerator = distributable as u128 * *weight as u128;
        let share = (numerator / weight_sum) as u64;
        cents[index] += share;
        allocated += share;
        remainders.push((index, numerator % weight_sum));
    }
    remainders.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    for (index, _) in remainders
        .into_iter()
        .take((distributable - allocated) as usize)
    {
        cents[index] += 1;
    }

    let lucky_index = cents
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(&left.0)))
        .map(|(index, _)| index)
        .expect("validated non-empty participant list");
    let lucky_winner = request.participants[lucky_index].trim().to_string();
    let allocations = request
        .participants
        .into_iter()
        .zip(cents)
        .map(|(name, amount)| Allocation {
            name: name.trim().to_string(),
            amount_yuan: format_yuan(amount),
        })
        .collect();

    DrawResult {
        total_yuan: format_yuan(total_cents),
        allocations,
        lucky_winner,
        public_seed: request.public_seed,
        algorithm: ALGORITHM,
        draw_id: hex(&draw_hash),
    }
}

fn json_response<T: Serialize>(status: u16, value: &T) -> Response {
    let body = serde_json::to_vec(value).expect("serializing a response cannot fail");
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("x-red-packet-algorithm", ALGORITHM)
        .body(body)
        .build()
}

#[cfg_attr(target_arch = "wasm32", http_component)]
fn handle(req: Request) -> anyhow::Result<impl IntoResponse> {
    let request = match serde_json::from_slice::<DrawRequest>(&req.into_body()) {
        Ok(request) => request,
        Err(_) => {
            return Ok(json_response(
                400,
                &ErrorResponse {
                    error: "invalid JSON request",
                },
            ))
        }
    };
    let total_cents = match validate(&request) {
        Ok(total_cents) => total_cents,
        Err(error) => return Ok(json_response(422, &ErrorResponse { error })),
    };
    Ok(json_response(200, &draw(request, total_cents)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> DrawRequest {
        DrawRequest {
            total_yuan: "100.00".to_string(),
            participants: ["小明", "小红", "老王", "翠花"]
                .map(str::to_string)
                .to_vec(),
            public_seed: "2026春晚-第一个节目".to_string(),
        }
    }

    fn allocation_cents(result: &DrawResult) -> Vec<u64> {
        result
            .allocations
            .iter()
            .map(|allocation| parse_yuan(&allocation.amount_yuan).unwrap())
            .collect()
    }

    #[test]
    fn parses_money_without_floating_point() {
        assert_eq!(parse_yuan("100"), Ok(10_000));
        assert_eq!(parse_yuan("0.1"), Ok(10));
        assert_eq!(parse_yuan("0.01"), Ok(1));
        assert!(parse_yuan("1.001").is_err());
        assert!(parse_yuan("-1.00").is_err());
    }

    #[test]
    fn draw_is_deterministic_and_conserves_every_cent() {
        let first = draw(request(), 10_000);
        let second = draw(request(), 10_000);
        let cents = allocation_cents(&first);

        assert_eq!(first, second);
        assert_eq!(cents.iter().sum::<u64>(), 10_000);
        assert!(cents.iter().all(|amount| *amount >= 1));
    }

    #[test]
    fn changing_public_seed_changes_the_draw() {
        let first = draw(request(), 10_000);
        let mut changed = request();
        changed.public_seed = "2026春晚-第二个节目".to_string();
        let second = draw(changed, 10_000);

        assert_ne!(first.draw_id, second.draw_id);
        assert_ne!(first.allocations, second.allocations);
    }

    #[test]
    fn supports_the_minimum_one_cent_per_person() {
        let mut input = request();
        input.total_yuan = "0.04".to_string();
        let result = draw(input, 4);

        assert_eq!(allocation_cents(&result), [1, 1, 1, 1]);
    }

    #[test]
    fn rejects_duplicate_names_and_insufficient_money() {
        let mut duplicate = request();
        duplicate.participants[1] = "小明".to_string();
        assert_eq!(
            validate(&duplicate),
            Err("participant names must be unique")
        );

        let mut insufficient = request();
        insufficient.total_yuan = "0.03".to_string();
        assert_eq!(
            validate(&insufficient),
            Err("total_yuan must provide at least 0.01 yuan per participant")
        );
    }
}
