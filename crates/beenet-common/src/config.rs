//! Single TOML config file: platform `beenet/config.toml` or `--config path`.
//!
//! No `BEENET_*` process environment is read for application settings (see each binary’s `[section]`).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub struct BeenetConfigFile {
    #[serde(default)]
    pub gateway: Option<GatewaySection>,
    #[serde(default)]
    pub worker: Option<WorkerSection>,
    #[serde(default)]
    pub oss: Option<OssSection>,
}

#[derive(Debug, Deserialize, Default)]
pub struct GatewaySection {
    pub http_addr: Option<String>,
    pub registry_url: Option<String>,
    /// Interval for refreshing connected-peer metadata via `POST /v1/workers/lookup`.
    pub registry_poll_ms: Option<u64>,
    pub default_deadline_ms: Option<u32>,
    pub libp2p_listen_addr: Option<String>,
    /// Worker-visible gateway address to advertise to the registry.
    /// This can be private, LAN-local, or public as long as workers can dial it.
    pub public_addr: Option<String>,
    /// Persistent Ed25519 identity key path (stable PeerId for workers to dial).
    pub identity_key_path: Option<String>,
    /// Human-readable gateway id / display name (duplicates allowed; PeerId is identity).
    pub gateway_id: Option<String>,
    pub region: Option<String>,
    pub capacity: Option<u32>,
    /// HTTP URL advertised to the Registry for Front Door routing.
    pub http_url: Option<String>,
    /// Shared secret accepted from Front Door for targeted internal requests.
    pub frontdoor_token: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct WorkerSection {
    pub listen_addr: Option<String>,
    pub wasm_cache_dir: Option<String>,
    pub default_deadline_ms: Option<u32>,
    pub default_memory_mb: Option<u32>,
    pub max_instance_memory_mb: Option<u32>,
    pub max_concurrency: Option<usize>,
    pub registry_url: Option<String>,
    pub registry_heartbeat_path: Option<String>,
    /// Deprecated bootstrap token. New installations should provide it through
    /// the worker CLI, stdin, or a temporary secret file instead of persisting it.
    pub join_token: Option<String>,
    pub registry_heartbeat_secs: Option<u64>,
    pub wasm_fetch_base: Option<String>,
    pub wasm_fetch_bearer: Option<String>,
    pub wasm_fetch_timeout_secs: Option<u64>,
    /// Optional region label for Gateway affinity (e.g. `cn-hangzhou`).
    pub region: Option<String>,
    /// Human-readable display name (duplicates allowed; PeerId is the identity).
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct OssSection {
    pub endpoint: Option<String>,
    pub bucket: Option<String>,
    pub access_key_id: Option<String>,
    pub access_key_secret: Option<String>,
    pub region: Option<String>,
    pub key_prefix: Option<String>,
    pub force_path_style: Option<bool>,
}

// —— defaults (match former clap defaults) —— //

pub const DEFAULT_WORKER_LISTEN_ADDR: &str = "/ip4/0.0.0.0/tcp/0";
pub const DEFAULT_WASM_CACHE_DIR: &str = "./wasm_cache";
pub const DEFAULT_DEADLINE_MS: u32 = 10_000;
pub const DEFAULT_MEMORY_MB: u32 = 64;
pub const DEFAULT_MAX_INSTANCE_MEMORY_MB: u32 = 256;
pub const DEFAULT_REGISTRY_HEARTBEAT_PATH: &str = "/v1/workers/heartbeat";
pub const DEFAULT_REGISTRY_HEARTBEAT_SECS: u64 = 30;
pub const DEFAULT_WASM_FETCH_TIMEOUT_SECS: u64 = 60;
pub const DEFAULT_GATEWAY_HTTP_ADDR: &str = "127.0.0.1:8080";
pub const DEFAULT_GATEWAY_LIBP2P_LISTEN_ADDR: &str = "/ip4/0.0.0.0/tcp/4001";
pub const DEFAULT_GATEWAY_IDENTITY_KEY_PATH: &str = "gateway_cache/identity.key";
pub const DEFAULT_GATEWAY_CAPACITY: u32 = 1_000;
pub const DEFAULT_REGISTRY_POLL_MS: u64 = 2000;
pub const DEFAULT_REGISTRY_HTTP_ADDR: &str = "127.0.0.1:3030";
pub const DEFAULT_OSS_REGION: &str = "oss-cn-hangzhou";

fn trim_nonempty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn opt_merge(cli: Option<String>, file: Option<&String>) -> Option<String> {
    cli.or_else(|| file.cloned())
        .and_then(|s| trim_nonempty(&s))
}

fn pick_u32(cli: Option<u32>, file: Option<u32>, default: u32) -> u32 {
    cli.or(file).unwrap_or(default)
}

fn pick_u64(cli: Option<u64>, file: Option<u64>, default: u64) -> u64 {
    cli.or(file).unwrap_or(default)
}

fn pick_usize(cli: Option<usize>, file: Option<usize>) -> Option<usize> {
    cli.or(file)
}

fn pick_bool(cli: Option<bool>, file: Option<bool>, default: bool) -> bool {
    cli.or(file).unwrap_or(default)
}

/// Resolved settings for `beenet-worker`.
#[derive(Clone, Debug)]
pub struct WorkerSettings {
    pub listen_addr: String,
    pub wasm_cache_dir: PathBuf,
    pub default_deadline_ms: u32,
    pub default_memory_mb: u32,
    pub max_instance_memory_mb: u32,
    pub max_concurrency: Option<usize>,
    pub registry_url: String,
    pub registry_heartbeat_path: String,
    /// Deprecated persisted bootstrap token retained for config compatibility.
    pub join_token: Option<String>,
    pub registry_heartbeat_secs: u64,
    pub wasm_fetch_base: Option<String>,
    pub wasm_fetch_bearer: Option<String>,
    pub wasm_fetch_timeout_secs: u64,
    /// Optional region for Registry Gateway tip affinity.
    pub region: Option<String>,
    /// Human-readable display name (duplicates allowed; PeerId is the identity).
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct WorkerCliOverrides {
    pub listen_addr: Option<String>,
    pub wasm_cache_dir: Option<PathBuf>,
    pub default_deadline_ms: Option<u32>,
    pub default_memory_mb: Option<u32>,
    pub max_instance_memory_mb: Option<u32>,
    pub max_concurrency: Option<usize>,
    pub registry_url: Option<String>,
    pub registry_heartbeat_path: Option<String>,
    pub join_token: Option<String>,
    pub registry_heartbeat_secs: Option<u64>,
    pub wasm_fetch_base: Option<String>,
    pub wasm_fetch_bearer: Option<String>,
    pub wasm_fetch_timeout_secs: Option<u64>,
    pub region: Option<String>,
    pub name: Option<String>,
}

pub fn require_worker_section(cfg: &BeenetConfigFile) -> Result<&WorkerSection> {
    cfg.worker
        .as_ref()
        .ok_or_else(|| anyhow!("config must include a [worker] table"))
}

pub fn resolve_worker_settings(
    cfg: &BeenetConfigFile,
    cli: &WorkerCliOverrides,
) -> Result<WorkerSettings> {
    let w = require_worker_section(cfg)?;
    let registry_url = opt_merge(cli.registry_url.clone(), w.registry_url.as_ref())
        .ok_or_else(|| anyhow!("config [worker] must set registry_url (or pass --registry-url)"))?;
    let join_token = opt_merge(cli.join_token.clone(), w.join_token.as_ref());

    let listen_addr = opt_merge(cli.listen_addr.clone(), w.listen_addr.as_ref())
        .unwrap_or_else(|| DEFAULT_WORKER_LISTEN_ADDR.to_string());
    let wasm_cache_dir = cli
        .wasm_cache_dir
        .clone()
        .or_else(|| {
            w.wasm_cache_dir
                .as_ref()
                .and_then(|s| trim_nonempty(s))
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from(DEFAULT_WASM_CACHE_DIR));

    let registry_heartbeat_path = opt_merge(
        cli.registry_heartbeat_path.clone(),
        w.registry_heartbeat_path.as_ref(),
    )
    .unwrap_or_else(|| DEFAULT_REGISTRY_HEARTBEAT_PATH.to_string());

    Ok(WorkerSettings {
        listen_addr,
        wasm_cache_dir,
        default_deadline_ms: pick_u32(
            cli.default_deadline_ms,
            w.default_deadline_ms,
            DEFAULT_DEADLINE_MS,
        ),
        default_memory_mb: pick_u32(
            cli.default_memory_mb,
            w.default_memory_mb,
            DEFAULT_MEMORY_MB,
        ),
        max_instance_memory_mb: pick_u32(
            cli.max_instance_memory_mb,
            w.max_instance_memory_mb,
            DEFAULT_MAX_INSTANCE_MEMORY_MB,
        ),
        max_concurrency: pick_usize(cli.max_concurrency, w.max_concurrency),
        registry_url,
        registry_heartbeat_path,
        join_token,
        registry_heartbeat_secs: pick_u64(
            cli.registry_heartbeat_secs,
            w.registry_heartbeat_secs,
            DEFAULT_REGISTRY_HEARTBEAT_SECS,
        ),
        wasm_fetch_base: opt_merge(cli.wasm_fetch_base.clone(), w.wasm_fetch_base.as_ref()),
        wasm_fetch_bearer: opt_merge(cli.wasm_fetch_bearer.clone(), w.wasm_fetch_bearer.as_ref()),
        wasm_fetch_timeout_secs: pick_u64(
            cli.wasm_fetch_timeout_secs,
            w.wasm_fetch_timeout_secs,
            DEFAULT_WASM_FETCH_TIMEOUT_SECS,
        )
        .max(1),
        region: opt_merge(cli.region.clone(), w.region.as_ref()),
        name: opt_merge(cli.name.clone(), w.name.as_ref()),
    })
}

/// S3-compatible settings retained for hosted publishers and local compatibility.
#[derive(Clone, Debug)]
pub struct OssSettings {
    pub endpoint: String,
    pub bucket: String,
    pub access_key_id: String,
    pub access_key_secret: String,
    pub region: String,
    pub key_prefix: String,
    pub force_path_style: bool,
}

#[derive(Clone, Debug, Default)]
pub struct OssCliOverrides {
    pub endpoint: Option<String>,
    pub bucket: Option<String>,
    pub access_key_id: Option<String>,
    pub access_key_secret: Option<String>,
    pub region: Option<String>,
    pub key_prefix: Option<String>,
    pub force_path_style: Option<bool>,
}

pub fn resolve_oss_settings(cfg: &BeenetConfigFile, cli: &OssCliOverrides) -> Result<OssSettings> {
    let o = cfg
        .oss
        .as_ref()
        .ok_or_else(|| anyhow!("config must include an [oss] table for upload"))?;
    let endpoint = opt_merge(cli.endpoint.clone(), o.endpoint.as_ref())
        .ok_or_else(|| anyhow!("config [oss] must set endpoint"))?;
    let bucket = opt_merge(cli.bucket.clone(), o.bucket.as_ref())
        .ok_or_else(|| anyhow!("config [oss] must set bucket"))?;
    let access_key_id = opt_merge(cli.access_key_id.clone(), o.access_key_id.as_ref())
        .ok_or_else(|| anyhow!("config [oss] must set access_key_id"))?;
    let access_key_secret = opt_merge(cli.access_key_secret.clone(), o.access_key_secret.as_ref())
        .ok_or_else(|| anyhow!("config [oss] must set access_key_secret"))?;
    let region = opt_merge(cli.region.clone(), o.region.as_ref())
        .unwrap_or_else(|| DEFAULT_OSS_REGION.to_string());
    let key_prefix = opt_merge(cli.key_prefix.clone(), o.key_prefix.as_ref()).unwrap_or_default();
    let force_path_style = pick_bool(cli.force_path_style, o.force_path_style, false);

    Ok(OssSettings {
        endpoint,
        bucket,
        access_key_id,
        access_key_secret,
        region,
        key_prefix,
        force_path_style,
    })
}

#[derive(Clone, Debug)]
pub struct GatewaySettings {
    pub http_addr: SocketAddr,
    pub registry_url: String,
    /// Connected-peer metadata refresh interval (ms) via Registry lookup.
    pub registry_poll_ms: u64,
    pub default_deadline_ms: u32,
    pub libp2p_listen_addr: String,
    pub public_addr: Option<String>,
    pub identity_key_path: PathBuf,
    /// Display name sent to Registry (duplicates allowed).
    /// `None` means the binary should auto-generate a Docker-style name.
    pub gateway_id: Option<String>,
    pub region: Option<String>,
    pub capacity: u32,
    pub http_url: Option<String>,
    pub frontdoor_token: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct GatewayCliOverrides {
    pub http_addr: Option<String>,
    pub registry_url: Option<String>,
    pub registry_poll_ms: Option<u64>,
    pub default_deadline_ms: Option<u32>,
    pub libp2p_listen_addr: Option<String>,
    pub public_addr: Option<String>,
    pub identity_key_path: Option<PathBuf>,
    pub gateway_id: Option<String>,
    pub region: Option<String>,
    pub capacity: Option<u32>,
    pub http_url: Option<String>,
    pub frontdoor_token: Option<String>,
}

pub fn require_gateway_section(cfg: &BeenetConfigFile) -> Result<&GatewaySection> {
    cfg.gateway
        .as_ref()
        .ok_or_else(|| anyhow!("config must include a [gateway] table"))
}

pub fn resolve_gateway_settings(
    cfg: &BeenetConfigFile,
    cli: &GatewayCliOverrides,
) -> Result<GatewaySettings> {
    let g = require_gateway_section(cfg)?;
    resolve_gateway_settings_merged(Some(g), cli)
}

/// Resolve gateway settings from an optional config file plus CLI overrides.
/// When `cfg` is `None`, `--registry-url` must be supplied (container / K8s mode).
pub fn resolve_gateway_settings_optional_file(
    cfg: Option<&BeenetConfigFile>,
    cli: &GatewayCliOverrides,
) -> Result<GatewaySettings> {
    let section = cfg.and_then(|c| c.gateway.as_ref());
    resolve_gateway_settings_merged(section, cli)
}

fn resolve_gateway_settings_merged(
    g: Option<&GatewaySection>,
    cli: &GatewayCliOverrides,
) -> Result<GatewaySettings> {
    let registry_url = opt_merge(
        cli.registry_url.clone(),
        g.and_then(|s| s.registry_url.as_ref()),
    )
    .ok_or_else(|| {
        anyhow!("missing registry_url: pass --registry-url or set [gateway].registry_url in config")
    })?;
    let http_str = opt_merge(cli.http_addr.clone(), g.and_then(|s| s.http_addr.as_ref()))
        .unwrap_or_else(|| DEFAULT_GATEWAY_HTTP_ADDR.to_string());
    let http_addr: SocketAddr = http_str
        .parse()
        .with_context(|| format!("invalid gateway http_addr `{http_str}`"))?;
    let libp2p_listen_addr = opt_merge(
        cli.libp2p_listen_addr.clone(),
        g.and_then(|s| s.libp2p_listen_addr.as_ref()),
    )
    .unwrap_or_else(|| DEFAULT_GATEWAY_LIBP2P_LISTEN_ADDR.to_string());
    let public_addr = opt_merge(
        cli.public_addr.clone(),
        g.and_then(|s| s.public_addr.as_ref()),
    );
    let identity_key_path = cli
        .identity_key_path
        .clone()
        .or_else(|| {
            g.and_then(|s| s.identity_key_path.as_ref())
                .and_then(|s| trim_nonempty(s))
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from(DEFAULT_GATEWAY_IDENTITY_KEY_PATH));
    let gateway_id = opt_merge(
        cli.gateway_id.clone(),
        g.and_then(|s| s.gateway_id.as_ref()),
    )
    .and_then(|s| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.chars().take(64).collect::<String>())
    });
    let region = opt_merge(cli.region.clone(), g.and_then(|s| s.region.as_ref()));
    Ok(GatewaySettings {
        http_addr,
        registry_url,
        registry_poll_ms: pick_u64(
            cli.registry_poll_ms,
            g.and_then(|s| s.registry_poll_ms),
            DEFAULT_REGISTRY_POLL_MS,
        ),
        default_deadline_ms: pick_u32(
            cli.default_deadline_ms,
            g.and_then(|s| s.default_deadline_ms),
            DEFAULT_DEADLINE_MS,
        ),
        libp2p_listen_addr,
        public_addr,
        identity_key_path,
        gateway_id,
        region,
        capacity: pick_u32(
            cli.capacity,
            g.and_then(|s| s.capacity),
            DEFAULT_GATEWAY_CAPACITY,
        )
        .max(1),
        http_url: opt_merge(cli.http_url.clone(), g.and_then(|s| s.http_url.as_ref())),
        frontdoor_token: opt_merge(
            cli.frontdoor_token.clone(),
            g.and_then(|s| s.frontdoor_token.as_ref()),
        ),
    })
}

/// Platform config dir + `beenet/config.toml`.
pub fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("beenet")
        .join("config.toml")
}

fn parse_config_arg(args: &[String]) -> Option<PathBuf> {
    let mut i = 0usize;
    while i < args.len() {
        let a = &args[i];
        if a == "--config" {
            let path = args.get(i + 1)?;
            return Some(PathBuf::from(path));
        }
        if let Some(rest) = a.strip_prefix("--config=") {
            return Some(PathBuf::from(rest));
        }
        i += 1;
    }
    None
}

/// `argv` = `std::env::args().skip(1)`. Resolves `--config path` then [`default_config_path`].
pub fn resolve_config_path_from_argv(argv: &[String]) -> PathBuf {
    parse_config_arg(argv).unwrap_or_else(default_config_path)
}

pub fn resolve_config_path_with_cli(cli_config: Option<PathBuf>, argv: &[String]) -> PathBuf {
    cli_config
        .or_else(|| parse_config_arg(argv))
        .unwrap_or_else(default_config_path)
}

pub fn load_file(path: &Path) -> Result<BeenetConfigFile> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read config `{}`", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(BeenetConfigFile::default());
    }
    toml::from_str(&raw).with_context(|| format!("parse TOML `{}`", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_config_flag_before_subcommand() {
        let argv = vec!["--config".into(), "/a.toml".into(), "upload".into()];
        assert_eq!(
            resolve_config_path_from_argv(&argv),
            PathBuf::from("/a.toml")
        );
    }

    #[test]
    fn worker_merge_requires_registry_and_optional_gateway_addr() {
        let mut f = BeenetConfigFile {
            worker: Some(WorkerSection::default()),
            ..Default::default()
        };
        assert!(resolve_worker_settings(&f, &WorkerCliOverrides::default()).is_err());
        f.worker.as_mut().unwrap().registry_url = Some("http://localhost:3030".into());
        let s = resolve_worker_settings(&f, &WorkerCliOverrides::default()).unwrap();
        assert_eq!(s.listen_addr, DEFAULT_WORKER_LISTEN_ADDR);
        assert_eq!(s.wasm_cache_dir, PathBuf::from(DEFAULT_WASM_CACHE_DIR));
        assert!(s.join_token.is_none());
        f.worker.as_mut().unwrap().join_token = Some("t".into());
        let s = resolve_worker_settings(&f, &WorkerCliOverrides::default()).unwrap();
        assert_eq!(s.join_token.as_deref(), Some("t"));
        f.worker.as_mut().unwrap().region = Some("cn-hangzhou".into());
        let s = resolve_worker_settings(&f, &WorkerCliOverrides::default()).unwrap();
        assert_eq!(s.region.as_deref(), Some("cn-hangzhou"));
    }
}
