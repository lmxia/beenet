//! Protocol-level Beenet Wasm artifact packaging and inspection.
//!
//! This crate deliberately contains no storage, cloud, or CLI concerns. It is
//! the shared contract used by local tooling and Beenet Cloud builders.

mod manifest;

use anyhow::{Context, Result};
use beenet_common::BeenetCid;

pub use manifest::{
    embed, extract, extract_raw, Ai, Audit, Manifest, Networking, Runtime, Task, SCHEMA_VERSION,
    SECTION_NAME,
};

#[derive(Clone, Debug)]
pub struct ArtifactInfo {
    pub cid: BeenetCid,
    pub size: usize,
    pub manifest: Manifest,
}

/// Embed a validated `beenet.toml` manifest into a freshly compiled Wasm.
pub fn package(wasm: &[u8], manifest_toml: &str) -> Result<Vec<u8>> {
    let manifest = Manifest::from_toml(manifest_toml).context("validate Beenet manifest")?;
    embed(wasm, &manifest).context("embed Beenet manifest")
}

/// Inspect a packaged artifact and return its content CID and manifest.
pub fn inspect(wasm: &[u8]) -> Result<ArtifactInfo> {
    let manifest = extract(wasm).context("extract Beenet manifest")?;
    Ok(ArtifactInfo {
        cid: BeenetCid::from_bytes(wasm),
        size: wasm.len(),
        manifest,
    })
}

/// Verify that an artifact's bytes match an expected CID.
pub fn verify_cid(wasm: &[u8], expected: &BeenetCid) -> Result<()> {
    let actual = BeenetCid::from_bytes(wasm);
    if &actual != expected {
        anyhow::bail!("artifact CID mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_WASM: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, // \0asm
        0x01, 0x00, 0x00, 0x00, // version 1
    ];

    const MANIFEST: &str = r#"
schema_version = 1

[task]
name = "artifact-test"
version = "0.1.0"
interface = "wasi:http/incoming-handler@0.2"
"#;

    #[test]
    fn package_inspect_and_verify() {
        let artifact = package(MINIMAL_WASM, MANIFEST).unwrap();
        let info = inspect(&artifact).unwrap();
        assert_eq!(info.size, artifact.len());
        assert_eq!(info.manifest.task.name, "artifact-test");
        verify_cid(&artifact, &info.cid).unwrap();
    }

    #[test]
    fn rejects_wrong_cid() {
        let artifact = package(MINIMAL_WASM, MANIFEST).unwrap();
        let other = BeenetCid::from_bytes(b"other");
        assert!(verify_cid(&artifact, &other).is_err());
    }
}
