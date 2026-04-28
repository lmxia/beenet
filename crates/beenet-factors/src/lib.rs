//! Flat [`RuntimeFactors`](spin_factors::RuntimeFactors) for Beenet M1.5.
//!
//! Mirrors `target.md` §7 `beenet-factors`: WASI (no file mounts), variables,
//! outbound networking allowlists, and outbound HTTP — without pulling in the
//! full `spin-runtime-factors` graph (KV/SQLite/LLM/…).

use anyhow::Result;
use serde_json::json;
use spin_factor_outbound_http::OutboundHttpFactor;
use spin_factor_outbound_networking::OutboundNetworkingFactor;
use spin_factor_variables::VariablesFactor;
use spin_factor_wasi::{DummyFilesMounter, WasiFactor};
use spin_factors::RuntimeFactors;
use spin_locked_app::locked::{
    ContentRef, LockedApp, LockedComponent, LockedComponentSource, LockedTrigger,
};
use spin_locked_app::values::ValuesMap;

/// Beenet worker factors: same layering order as Spin HTTP triggers
/// (`wasi` → `variables` → `outbound_networking` → `outbound_http`).
#[derive(RuntimeFactors)]
pub struct BeenetFactors {
    pub wasi: WasiFactor,
    pub variables: VariablesFactor,
    pub outbound_networking: OutboundNetworkingFactor,
    pub outbound_http: OutboundHttpFactor,
}

fn log_disallowed_outbound(scheme: &str, authority: &str) {
    tracing::warn!(
        "beenet: outbound request blocked by allowed_outbound_hosts policy ({scheme}://{authority})"
    );
}

impl BeenetFactors {
    pub fn new() -> Self {
        let mut outbound_networking = OutboundNetworkingFactor::new();
        outbound_networking.set_disallowed_host_handler(log_disallowed_outbound);
        Self {
            wasi: WasiFactor::new(DummyFilesMounter),
            variables: VariablesFactor::new(),
            outbound_networking,
            outbound_http: OutboundHttpFactor::default(),
        }
    }
}

impl Default for BeenetFactors {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a minimal [`LockedApp`] for a single `http` trigger + one component.
///
/// `component_id` must match the CID string used as the wasm cache filename
/// stem. `allowed_outbound_hosts` is written to locked metadata consumed by
/// [`OutboundNetworkingFactor`](spin_factor_outbound_networking::OutboundNetworkingFactor).
pub fn locked_app_single_http_component(
    component_id: &str,
    allowed_outbound_hosts: &[String],
) -> Result<LockedApp> {
    let mut metadata = ValuesMap::new();
    if !allowed_outbound_hosts.is_empty() {
        metadata.insert(
            "allowed_outbound_hosts".to_string(),
            serde_json::to_value(allowed_outbound_hosts)?,
        );
    }

    let component = LockedComponent {
        id: component_id.to_string(),
        metadata,
        source: LockedComponentSource {
            content_type: "application/wasm".to_string(),
            content: ContentRef {
                // Real bytes are loaded by `beenet-worker`'s `ComponentLoader` from disk.
                source: Some(format!("beenet://{component_id}")),
                inline: None,
                digest: None,
            },
        },
        env: Default::default(),
        files: vec![],
        config: Default::default(),
        dependencies: Default::default(),
        host_requirements: Default::default(),
    };

    let trigger = LockedTrigger {
        id: "http".to_string(),
        trigger_type: "http".to_string(),
        trigger_config: json!({ "component": component_id }),
    };

    Ok(LockedApp {
        spin_lock_version: Default::default(),
        must_understand: vec![],
        metadata: Default::default(),
        host_requirements: Default::default(),
        variables: Default::default(),
        triggers: vec![trigger],
        components: vec![component],
    })
}
