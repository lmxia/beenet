use std::fs;
use std::path::{Path, PathBuf};

pub struct QuotaPreset {
    pub label: &'static str,
    pub cpu_percent: u32,
    pub memory_mb: u32,
    pub pids_max: u32,
}

#[derive(Clone)]
pub struct WorkerSnapshot {
    pub name: String,
    pub region: String,
    pub wasm_cache_dir: String,
    pub cpu_percent: u32,
    pub memory_mb: u32,
    pub pids_max: u32,
}

impl WorkerSnapshot {
    pub const PRESETS: [QuotaPreset; 4] = [
        QuotaPreset {
            label: "轻量",
            cpu_percent: 10,
            memory_mb: 256,
            pids_max: 64,
        },
        QuotaPreset {
            label: "均衡",
            cpu_percent: 25,
            memory_mb: 512,
            pids_max: 128,
        },
        QuotaPreset {
            label: "更多",
            cpu_percent: 50,
            memory_mb: 1024,
            pids_max: 128,
        },
        QuotaPreset {
            label: "高性能",
            cpu_percent: 150,
            memory_mb: 2048,
            pids_max: 256,
        },
    ];

    pub fn load_or_create() -> (PathBuf, Self) {
        let path = default_config_path();
        if let Ok(text) = fs::read_to_string(&path) {
            return (path, Self::parse(&text));
        }
        let snapshot = Self::fresh();
        let _ = snapshot.save(&path);
        (path, snapshot)
    }

    pub fn fresh() -> Self {
        Self {
            name: String::new(),
            region: String::new(),
            wasm_cache_dir: default_cache_dir().display().to_string(),
            cpu_percent: 25,
            memory_mb: 512,
            pids_max: 128,
        }
    }

    pub fn has_identity(&self) -> bool {
        Path::new(&self.wasm_cache_dir).join("identity.key").exists()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        fs::create_dir_all(&self.wasm_cache_dir)
            .map_err(|error| format!("create cache {}: {error}", self.wasm_cache_dir))?;
        fs::write(path, self.toml()).map_err(|error| format!("write {}: {error}", path.display()))
    }

    fn toml(&self) -> String {
        let mut out = format!(
            "[worker]\n\
             backend = \"native\"\n\
             listen_addr = \"/ip4/0.0.0.0/tcp/0\"\n\
             registry_url = \"https://registry.hyperos.com.cn\"\n\
             wasm_fetch_base = \"https://cloud.hyperos.com.cn/api/v1/artifacts\"\n\
             wasm_fetch_timeout_secs = 60\n\
             registry_heartbeat_secs = 30\n\
             wasm_cache_dir = {}\n",
            toml_string(&self.wasm_cache_dir)
        );
        if !self.name.trim().is_empty() {
            out.push_str(&format!("name = {}\n", toml_string(self.name.trim())));
        }
        if !self.region.trim().is_empty() {
            out.push_str(&format!("region = {}\n", toml_string(self.region.trim())));
        }
        out.push_str(&format!(
            "\n[worker.quota]\ncpu_percent = {}\nmemory_mb = {}\npids_max = {}\n",
            self.cpu_percent, self.memory_mb, self.pids_max
        ));
        out
    }

    fn parse(toml: &str) -> Self {
        let mut snapshot = Self::fresh();
        if let Some(value) = toml_string_value(toml, "name") {
            snapshot.name = value;
        }
        if let Some(value) = toml_string_value(toml, "region") {
            snapshot.region = value;
        }
        if let Some(value) = toml_string_value(toml, "wasm_cache_dir") {
            snapshot.wasm_cache_dir = value;
        }
        if let Some(value) = toml_u32(toml, "cpu_percent") {
            snapshot.cpu_percent = value;
        }
        if let Some(value) = toml_u32(toml, "memory_mb") {
            snapshot.memory_mb = value;
        }
        if let Some(value) = toml_u32(toml, "pids_max") {
            snapshot.pids_max = value;
        }
        snapshot
    }
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UiState {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub email: String,
    #[serde(skip)]
    pub points: Option<i64>,
}

impl UiState {
    pub fn load() -> Self {
        let path = ui_state_path();
        fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = ui_state_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string(self) {
            let _ = fs::write(path, text);
        }
    }

    pub fn clear(&mut self) {
        self.token.clear();
        self.email.clear();
        self.points = None;
        self.save();
    }
}

fn default_config_path() -> PathBuf {
    appdata_dir().join("config.toml")
}

fn ui_state_path() -> PathBuf {
    appdata_dir().join("ui-state.json")
}

fn appdata_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("beenet")
}

fn default_cache_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("beenet")
        .join("wasm_cache")
}

fn toml_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

fn toml_string_value(toml: &str, key: &str) -> Option<String> {
    for line in toml.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(key) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim();
        if let Some(inner) = rest.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            return Some(inner.replace("\\\\", "\\").replace("\\\"", "\""));
        }
    }
    None
}

fn toml_u32(toml: &str, key: &str) -> Option<u32> {
    for line in toml.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(key) else {
            continue;
        };
        let rest = rest.trim_start().strip_prefix('=')?;
        return rest.trim().parse().ok();
    }
    None
}
