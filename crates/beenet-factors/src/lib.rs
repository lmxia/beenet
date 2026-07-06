//! Flat [`RuntimeFactors`](spin_factors::RuntimeFactors) for Beenet M1.5.
//!
//! Mirrors `target.md` §7 `beenet-factors`: WASI (no file mounts), variables,
//! outbound networking allowlists, outbound HTTP, and a lightweight AI factor.

use anyhow::Result;
use serde_json::json;
use spin_factor_outbound_http::OutboundHttpFactor;
use spin_factor_outbound_networking::OutboundNetworkingFactor;
use spin_factor_variables::VariablesFactor;
use spin_factor_wasi::{DummyFilesMounter, WasiFactor};
use spin_factors::{
    ConfigureAppContext, Factor, FactorData, InitContext, PrepareContext, RuntimeFactors,
    SelfInstanceBuilder,
};
use spin_locked_app::locked::{
    ContentRef, LockedApp, LockedComponent, LockedComponentSource, LockedTrigger,
};
use spin_locked_app::values::ValuesMap;
use spin_locked_app::MetadataKey;
use spin_world::v1::llm as v1;
use spin_world::v2::llm as v2;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};

pub const ALLOWED_MODELS_KEY: MetadataKey<Vec<String>> = MetadataKey::new("ai_models");

static AI_INFER_CALLS: AtomicU32 = AtomicU32::new(0);
static AI_EMBEDDING_CALLS: AtomicU32 = AtomicU32::new(0);
static AI_PROMPT_TOKENS: AtomicU32 = AtomicU32::new(0);
static AI_GENERATED_TOKENS: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, Debug, Default)]
pub struct AiUsageSnapshot {
    pub infer_calls: u32,
    pub embedding_calls: u32,
    pub prompt_tokens: u32,
    pub generated_tokens: u32,
}

impl AiUsageSnapshot {
    pub fn delta_since(self, before: Self) -> Self {
        Self {
            infer_calls: self.infer_calls.saturating_sub(before.infer_calls),
            embedding_calls: self.embedding_calls.saturating_sub(before.embedding_calls),
            prompt_tokens: self.prompt_tokens.saturating_sub(before.prompt_tokens),
            generated_tokens: self.generated_tokens.saturating_sub(before.generated_tokens),
        }
    }
}

pub fn ai_usage_snapshot() -> AiUsageSnapshot {
    AiUsageSnapshot {
        infer_calls: AI_INFER_CALLS.load(Ordering::Relaxed),
        embedding_calls: AI_EMBEDDING_CALLS.load(Ordering::Relaxed),
        prompt_tokens: AI_PROMPT_TOKENS.load(Ordering::Relaxed),
        generated_tokens: AI_GENERATED_TOKENS.load(Ordering::Relaxed),
    }
}

/// Lightweight, native Beenet AI factor.
#[derive(Default)]
pub struct AiFactor;

impl AiFactor {
    pub fn new() -> Self {
        Self
    }
}

impl Factor for AiFactor {
    type RuntimeConfig = ();
    type AppState = AppState;
    type InstanceBuilder = InstanceState;

    fn init(&mut self, ctx: &mut impl InitContext<Self>) -> anyhow::Result<()> {
        ctx.link_bindings(spin_world::v1::llm::add_to_linker::<_, FactorData<Self>>)?;
        ctx.link_bindings(spin_world::v2::llm::add_to_linker::<_, FactorData<Self>>)?;
        Ok(())
    }

    fn configure_app<T: RuntimeFactors>(
        &self,
        ctx: ConfigureAppContext<T, Self>,
    ) -> anyhow::Result<Self::AppState> {
        let component_allowed_models = ctx
            .app()
            .components()
            .map(|component| {
                Ok((
                    component.id().to_string(),
                    component
                        .get_metadata(ALLOWED_MODELS_KEY)?
                        .unwrap_or_default()
                        .into_iter()
                        .collect::<HashSet<_>>(),
                ))
            })
            .collect::<anyhow::Result<HashMap<_, _>>>()?;
        Ok(AppState {
            component_allowed_models,
        })
    }

    fn prepare<T: RuntimeFactors>(
        &self,
        ctx: PrepareContext<T, Self>,
    ) -> anyhow::Result<Self::InstanceBuilder> {
        let allowed_models = ctx
            .app_state()
            .component_allowed_models
            .get(ctx.app_component().id())
            .cloned()
            .unwrap_or_default();
        Ok(InstanceState { allowed_models })
    }
}

pub struct AppState {
    component_allowed_models: HashMap<String, HashSet<String>>,
}

pub struct InstanceState {
    allowed_models: HashSet<String>,
}

impl SelfInstanceBuilder for InstanceState {}

impl v2::Host for InstanceState {
    async fn infer(
        &mut self,
        model: v2::InferencingModel,
        prompt: String,
        _params: Option<v2::InferencingParams>,
    ) -> std::result::Result<v2::InferencingResult, v2::Error> {
        if !self.allowed_models.is_empty() && !self.allowed_models.contains(&model) {
            return Err(access_denied_error(&model));
        }
        let prompt_tokens = token_count(&prompt);
        let text = classify_prompt(&model, &prompt);
        let generated_tokens = token_count(&text).max(4);
        AI_INFER_CALLS.fetch_add(1, Ordering::Relaxed);
        AI_PROMPT_TOKENS.fetch_add(prompt_tokens, Ordering::Relaxed);
        AI_GENERATED_TOKENS.fetch_add(generated_tokens, Ordering::Relaxed);
        Ok(v2::InferencingResult {
            text,
            usage: v2::InferencingUsage {
                prompt_token_count: prompt_tokens,
                generated_token_count: generated_tokens,
            },
        })
    }

    async fn generate_embeddings(
        &mut self,
        model: v2::EmbeddingModel,
        data: Vec<String>,
    ) -> std::result::Result<v2::EmbeddingsResult, v2::Error> {
        if !self.allowed_models.is_empty() && !self.allowed_models.contains(&model) {
            return Err(access_denied_error(&model));
        }
        let prompt_token_count: u32 = data.iter().map(|s| token_count(s)).sum();
        let embeddings = data.iter().map(|s| deterministic_embedding(s)).collect();
        AI_EMBEDDING_CALLS.fetch_add(1, Ordering::Relaxed);
        AI_PROMPT_TOKENS.fetch_add(prompt_token_count, Ordering::Relaxed);
        Ok(v2::EmbeddingsResult {
            embeddings,
            usage: v2::EmbeddingsUsage { prompt_token_count },
        })
    }

    fn convert_error(&mut self, error: v2::Error) -> anyhow::Result<v2::Error> {
        Ok(error)
    }
}

impl v1::Host for InstanceState {
    async fn infer(
        &mut self,
        model: v1::InferencingModel,
        prompt: String,
        params: Option<v1::InferencingParams>,
    ) -> std::result::Result<v1::InferencingResult, v1::Error> {
        <Self as v2::Host>::infer(self, model, prompt, params.map(Into::into))
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    async fn generate_embeddings(
        &mut self,
        model: v1::EmbeddingModel,
        data: Vec<String>,
    ) -> std::result::Result<v1::EmbeddingsResult, v1::Error> {
        <Self as v2::Host>::generate_embeddings(self, model, data)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    fn convert_error(&mut self, error: v1::Error) -> anyhow::Result<v1::Error> {
        Ok(error)
    }
}

/// Beenet worker factors: same layering order as Spin HTTP triggers
/// (`wasi` → `variables` → `outbound_networking` → `outbound_http`).
#[derive(RuntimeFactors)]
pub struct BeenetFactors {
    pub wasi: WasiFactor,
    pub variables: VariablesFactor,
    pub outbound_networking: OutboundNetworkingFactor,
    pub outbound_http: OutboundHttpFactor,
    pub ai: AiFactor,
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
            ai: AiFactor::new(),
        }
    }
}

impl Default for BeenetFactors {
    fn default() -> Self {
        Self::new()
    }
}

fn access_denied_error(model: &str) -> v2::Error {
    v2::Error::InvalidInput(format!(
        "The component does not have access to use '{model}'. To give the component access, add '{model}' to the 'ai_models' key for the component in your manifest"
    ))
}

pub fn locked_app_single_http_component(
    component_id: &str,
    allowed_outbound_hosts: &[String],
    allowed_ai_models: &[String],
) -> Result<LockedApp> {
    let mut metadata = ValuesMap::new();
    if !allowed_outbound_hosts.is_empty() {
        metadata.insert(
            "allowed_outbound_hosts".to_string(),
            serde_json::to_value(allowed_outbound_hosts)?,
        );
    }
    if !allowed_ai_models.is_empty() {
        metadata.insert(
            "ai_models".to_string(),
            serde_json::to_value(allowed_ai_models)?,
        );
    }

    let component = LockedComponent {
        id: component_id.to_string(),
        metadata,
        source: LockedComponentSource {
            content_type: "application/wasm".to_string(),
            content: ContentRef {
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

fn token_count(s: &str) -> u32 {
    s.split_whitespace().count().max(1) as u32
}

fn classify_prompt(model: &str, prompt: &str) -> String {
    let lower = prompt.to_ascii_lowercase();
    let label = if lower.contains("refund") || lower.contains("billing") || lower.contains("invoice") {
        "billing"
    } else if lower.contains("urgent") || lower.contains("down") || lower.contains("outage") {
        "incident"
    } else if lower.contains("bug") || lower.contains("error") || lower.contains("crash") {
        "bug"
    } else if lower.contains("feature") || lower.contains("request") {
        "feature"
    } else {
        "general"
    };
    format!("{model}:{label}")
}

fn deterministic_embedding(s: &str) -> Vec<f32> {
    let mut buckets = [0f32; 8];
    for (i, b) in s.bytes().enumerate() {
        let sign = if b % 2 == 0 { 1.0 } else { -1.0 };
        buckets[i % buckets.len()] += sign * ((b as f32) / 255.0);
    }
    let norm = buckets.iter().map(|v| v * v).sum::<f32>().sqrt().max(1.0);
    buckets.into_iter().map(|v| v / norm).collect()
}
