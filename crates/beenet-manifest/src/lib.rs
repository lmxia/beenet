//! `beenet:manifest/v1` custom section schema + embed/extract.
//!
//! See `readme.md §3.5`. The manifest is written as TOML into a Wasm custom
//! section. We intentionally avoid the `wasm-encoder` re-emit path and simply
//! **append** one custom section to the end of the module / component binary,
//! which is spec-valid for both.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use wasmparser::{Parser, Payload};

/// Custom-section name (`readme.md §3.5.2`).
pub const SECTION_NAME: &str = "beenet:manifest/v1";

/// Current schema version — bump on breaking changes.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub task: Task,
    #[serde(default)]
    pub runtime: Runtime,
    #[serde(default)]
    pub networking: Networking,
    #[serde(default)]
    pub ai: Ai,
    #[serde(default)]
    pub audit: Audit,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub name: String,
    pub version: String,
    /// Interface name, e.g. `wasi:http/incoming-handler@0.2` (gear 0) or
    /// `beenet:task/runner@0.1` (gear 1, M3+).
    pub interface: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Runtime {
    /// Wasm linear-memory cap requested by the author. Worker clamps to its own
    /// hard cap — see `readme.md §6.2.5`.
    pub max_memory_mb: Option<u32>,
    pub deadline_ms: Option<u32>,
    pub fuel_limit: Option<u64>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            max_memory_mb: None,
            deadline_ms: None,
            fuel_limit: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Networking {
    #[serde(default)]
    pub allowed_outbound_hosts: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Ai {
    #[serde(default)]
    pub allowed_models: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Audit {
    /// `"wall" | "cpu" | "fuel"` — M1 defaults to `wall`.
    #[serde(default = "default_cpu_reporting")]
    pub cpu_reporting: String,
}

fn default_cpu_reporting() -> String {
    "wall".to_string()
}

impl Default for Audit {
    fn default() -> Self {
        Self {
            cpu_reporting: default_cpu_reporting(),
        }
    }
}

impl Manifest {
    pub fn from_toml(s: &str) -> Result<Self> {
        let m: Manifest = toml::from_str(s).context("parse manifest toml")?;
        if m.schema_version != SCHEMA_VERSION {
            bail!(
                "unsupported manifest schema_version {} (expected {})",
                m.schema_version,
                SCHEMA_VERSION
            );
        }
        Ok(m)
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).context("serialize manifest toml")
    }
}

/// Extract the `beenet:manifest/v1` custom section from a wasm module or component.
///
/// Returns `Err` if the section is absent or the TOML is invalid.
pub fn extract(wasm: &[u8]) -> Result<Manifest> {
    let bytes =
        extract_raw(wasm)?.ok_or_else(|| anyhow!("custom section `{SECTION_NAME}` not found"))?;
    let s = std::str::from_utf8(&bytes).context("manifest section is not valid UTF-8")?;
    Manifest::from_toml(s)
}

/// Extract the raw bytes of the manifest custom section, if present.
pub fn extract_raw(wasm: &[u8]) -> Result<Option<Vec<u8>>> {
    for payload in Parser::new(0).parse_all(wasm) {
        let payload = payload.context("parse wasm")?;
        if let Payload::CustomSection(r) = payload {
            if r.name() == SECTION_NAME {
                return Ok(Some(r.data().to_vec()));
            }
        }
    }
    Ok(None)
}

/// Append a `beenet:manifest/v1` custom section to the wasm binary.
///
/// Errors if the input already carries one; re-pack by re-running `cargo build`
/// and calling `embed` on the fresh binary.
pub fn embed(wasm: &[u8], manifest: &Manifest) -> Result<Vec<u8>> {
    if extract_raw(wasm)?.is_some() {
        bail!("wasm already contains a `{SECTION_NAME}` section; re-build first");
    }
    let toml = manifest.to_toml()?;
    Ok(append_custom_section(wasm, SECTION_NAME, toml.as_bytes()))
}

/// Build a new wasm binary that is `wasm` with one extra custom section at the end.
fn append_custom_section(wasm: &[u8], name: &str, payload: &[u8]) -> Vec<u8> {
    // Custom section layout:
    //   u8           section_id = 0
    //   leb128(u32)  total_len   = leb_len(name_len) + name_len + payload_len
    //   leb128(u32)  name_len
    //   [u8; ..]     name
    //   [u8; ..]     payload
    let mut name_header = Vec::new();
    write_leb128_u32(&mut name_header, name.len() as u32);
    name_header.extend_from_slice(name.as_bytes());

    let body_len = name_header.len() + payload.len();
    let mut out = Vec::with_capacity(wasm.len() + 10 + body_len);
    out.extend_from_slice(wasm);
    out.push(0x00); // custom section id
    write_leb128_u32(&mut out, body_len as u32);
    out.extend_from_slice(&name_header);
    out.extend_from_slice(payload);
    out
}

fn write_leb128_u32(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if v == 0 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_WASM: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, // \0asm
        0x01, 0x00, 0x00, 0x00, // version 1
    ];

    fn sample() -> Manifest {
        Manifest {
            schema_version: 1,
            task: Task {
                name: "hello".into(),
                version: "0.1.0".into(),
                interface: "wasi:http/incoming-handler@0.2".into(),
            },
            runtime: Runtime {
                max_memory_mb: Some(64),
                deadline_ms: Some(5000),
                fuel_limit: None,
            },
            networking: Networking::default(),
            ai: Ai::default(),
            audit: Audit::default(),
        }
    }

    #[test]
    fn embed_then_extract() {
        let wasm = embed(MINIMAL_WASM, &sample()).unwrap();
        let back = extract(&wasm).unwrap();
        assert_eq!(back.task.name, "hello");
        assert_eq!(back.runtime.max_memory_mb, Some(64));
    }

    #[test]
    fn extract_missing() {
        assert!(extract(MINIMAL_WASM).is_err());
    }

    #[test]
    fn double_embed_errors() {
        let once = embed(MINIMAL_WASM, &sample()).unwrap();
        let err = embed(&once, &sample()).unwrap_err();
        assert!(err.to_string().contains("already contains"));
    }
}
