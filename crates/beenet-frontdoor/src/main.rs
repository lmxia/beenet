use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use beenet_common::BeenetCid;
use clap::Parser;
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

const TARGET_WORKER: &str = "x-beenet-target-worker";
const FRONTDOOR_TOKEN: &str = "x-beenet-frontdoor-token";

#[derive(Debug, Parser)]
#[command(
    name = "beenet-frontdoor",
    about = "Beenet Registry-aware public HTTP entrypoint"
)]
struct Args {
    #[arg(long, default_value = "0.0.0.0:8080")]
    http_addr: SocketAddr,
    #[arg(long)]
    registry_url: String,
    #[arg(long, env = "BEENET_INTERNAL_TOKEN")]
    registry_token: String,
    #[arg(long, env = "BEENET_FRONTDOOR_TOKEN")]
    gateway_token: String,
    #[arg(long, default_value_t = 2_000)]
    cache_ttl_ms: u64,
    #[arg(long, default_value_t = 10_000)]
    upstream_timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct Route {
    gateway_id: String,
    gateway_peer_id: String,
    gateway_url: String,
    worker_peer_id: String,
    #[allow(dead_code)]
    region: Option<String>,
    load: u32,
    preferred: bool,
}

#[derive(Debug, Deserialize)]
struct ResolveResponse {
    #[allow(dead_code)]
    cid: String,
    ttl_ms: u64,
    routes: Vec<Route>,
}

#[derive(Clone)]
struct CacheEntry {
    expires_at: Instant,
    routes: Vec<Route>,
}

#[derive(Clone)]
struct AppState {
    registry_url: String,
    gateway_token: String,
    registry_token: String,
    cache_ttl: Duration,
    client: reqwest::Client,
    cache: Arc<RwLock<HashMap<String, CacheEntry>>>,
}

async fn health() -> &'static str {
    "ok"
}

async fn resolve(state: &AppState, cid: &str) -> Result<Vec<Route>, String> {
    if let Some(entry) = state.cache.read().await.get(cid) {
        if entry.expires_at > Instant::now() {
            return Ok(entry.routes.clone());
        }
    }
    let url = format!(
        "{}/v1/internal/routes/resolve/{cid}",
        state.registry_url.trim_end_matches('/')
    );
    let response = state
        .client
        .get(url)
        .bearer_auth(&state.registry_token)
        .send()
        .await
        .map_err(|error| format!("registry resolve failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("registry resolve HTTP {}", response.status()));
    }
    let resolved: ResolveResponse = response
        .json()
        .await
        .map_err(|error| format!("invalid registry resolve response: {error}"))?;
    let ttl = state
        .cache_ttl
        .min(Duration::from_millis(resolved.ttl_ms.max(1)));
    state.cache.write().await.insert(
        cid.to_string(),
        CacheEntry {
            expires_at: Instant::now() + ttl,
            routes: resolved.routes.clone(),
        },
    );
    Ok(resolved.routes)
}

async fn run_ipfs(
    Path(cid): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if cid.parse::<BeenetCid>().is_err() {
        return (StatusCode::BAD_REQUEST, "invalid cid").into_response();
    }
    let routes = match resolve(&state, &cid).await {
        Ok(routes) if !routes.is_empty() => routes,
        Ok(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "no active route for cid").into_response()
        }
        Err(error) => return (StatusCode::BAD_GATEWAY, error).into_response(),
    };

    // Stable selection spreads different requests while preserving affinity for a request id.
    let request_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let preferred_routes: Vec<&Route> = routes.iter().filter(|route| route.preferred).collect();
    let fallback_routes: Vec<&Route> = routes.iter().filter(|route| !route.preferred).collect();
    let ranked = if preferred_routes.is_empty() {
        fallback_routes.clone()
    } else {
        preferred_routes.clone()
    };
    let best_load = ranked.iter().map(|route| route.load).min().unwrap_or(0);
    let eligible: Vec<&Route> = ranked
        .iter()
        .filter(|route| route.load <= best_load.saturating_add(500))
        .copied()
        .collect();
    let index = request_id.bytes().fold(0usize, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte as usize)
    }) % eligible.len();
    let mut ordered_routes = Vec::with_capacity(eligible.len());
    ordered_routes.extend_from_slice(&eligible[index..]);
    ordered_routes.extend_from_slice(&eligible[..index]);
    if !preferred_routes.is_empty() {
        ordered_routes.extend(fallback_routes.into_iter());
    }
    let mut last_failure = None;
    for route in ordered_routes {
        let url = format!("{}/run/ipfs/{cid}", route.gateway_url.trim_end_matches('/'));
        let mut request = state.client.post(url).body(body.clone());
        for (name, value) in &headers {
            if name.as_str().eq_ignore_ascii_case(TARGET_WORKER)
                || name.as_str().eq_ignore_ascii_case(FRONTDOOR_TOKEN)
                || name.as_str().eq_ignore_ascii_case("host")
                || name.as_str().eq_ignore_ascii_case("content-length")
            {
                continue;
            }
            request = request.header(name, value);
        }
        request = request
            .header(TARGET_WORKER, &route.worker_peer_id)
            .header(FRONTDOOR_TOKEN, &state.gateway_token)
            .header("x-beenet-route-gateway", &route.gateway_peer_id)
            .header("x-request-id", &request_id);
        let upstream = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                warn!(gateway = %route.gateway_id, %error, "gateway proxy failed; trying next route");
                last_failure = Some("gateway unavailable".to_string());
                continue;
            }
        };
        if matches!(
            upstream.status(),
            StatusCode::BAD_GATEWAY
                | StatusCode::SERVICE_UNAVAILABLE
                | StatusCode::TOO_MANY_REQUESTS
                | StatusCode::GATEWAY_TIMEOUT
        ) {
            last_failure = Some(format!(
                "gateway route failed with HTTP {}",
                upstream.status()
            ));
            continue;
        }
        return proxy_response(upstream, route.gateway_id.clone()).await;
    }
    (
        StatusCode::BAD_GATEWAY,
        last_failure.unwrap_or_else(|| "gateway unavailable".to_string()),
    )
        .into_response()
}

async fn proxy_response(upstream: reqwest::Response, gateway_id: String) -> Response {
    let status = upstream.status();
    let upstream_headers = upstream.headers().clone();
    let bytes = match upstream.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("gateway response failed: {error}"),
            )
                .into_response()
        }
    };
    let mut response = (status, bytes).into_response();
    for (name, value) in upstream_headers {
        let Some(name) = name else { continue };
        if name == HeaderName::from_static("content-length")
            || name == HeaderName::from_static("connection")
        {
            continue;
        }
        response.headers_mut().insert(name, value);
    }
    response.headers_mut().insert(
        HeaderName::from_static("x-beenet-gateway"),
        HeaderValue::from_str(&gateway_id).unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );
    response
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    if args.gateway_token.trim().is_empty() {
        anyhow::bail!("--gateway-token must not be empty");
    }
    if args.registry_token.trim().is_empty() {
        anyhow::bail!("--registry-token must not be empty");
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(args.upstream_timeout_ms.max(1)))
        .build()
        .context("build HTTP client")?;
    let state = AppState {
        registry_url: args.registry_url,
        gateway_token: args.gateway_token,
        registry_token: args.registry_token,
        cache_ttl: Duration::from_millis(args.cache_ttl_ms.max(1)),
        client,
        cache: Arc::new(RwLock::new(HashMap::new())),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/run/ipfs/:cid", post(run_ipfs))
        .with_state(state);
    info!(http_addr = %args.http_addr, "beenet-frontdoor listening");
    let listener = tokio::net::TcpListener::bind(args.http_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
