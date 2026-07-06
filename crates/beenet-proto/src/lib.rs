//! Wire types for `/beenet/invoke/1.0`.
//!
//! These mirror `readme.md §4.2`. Encoding is handled by `libp2p-request-response`'s
//! CBOR codec, so we only need `serde` derives here.

use beenet_common::BeenetCid;
use serde::{Deserialize, Serialize};

/// Request sent by a Gateway (or Agent) to a Worker over libp2p.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvokeRequest {
    pub request_id: String,
    pub cid: BeenetCid,
    #[serde(with = "serde_bytes")]
    pub input: Vec<u8>,
    pub deadline_ms: u32,
    pub caller_peer: Option<String>,
    pub trace_parent: Option<String>,
}

/// Response returned by the Worker.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvokeResponse {
    pub request_id: String,
    pub status: Status,
    #[serde(with = "serde_bytes", default, skip_serializing_if = "Vec::is_empty")]
    pub body: Vec<u8>,
    /// Guest stdout (UTF-8 lossy), truncated by the worker for wire safety.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    /// Guest stderr (UTF-8 lossy), truncated by the worker for wire safety.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    #[serde(default)]
    pub usage: Usage,
}

/// `readme.md §3.2.2` table A/B — status as a first-class result.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Status {
    Ok,
    BusinessError { http_status: u16, reason: String },
    RuntimeError { reason: String },
    LoadError { stage: LoadStage, reason: String },
    Timeout { stage: TimeoutStage },
    Rejected { reason: String },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LoadStage {
    Fetch,
    Compile,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TimeoutStage {
    Gateway,
    Exec,
}

/// Execution metrics, fed by `AuditFactor` (see `readme.md §3.6`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub wall_ns: u64,
    pub cpu_ns: u64,
    pub fuel_used: u64,
    pub mem_bytes: u64,
    pub chargeable_memory_mb: u32,
    pub fd_writes: u32,
    pub outbound_bytes: u64,
    pub ai_infer_calls: u32,
    pub ai_embedding_calls: u32,
    pub ai_prompt_tokens: u32,
    pub ai_generated_tokens: u32,
    pub billable: bool,
}

impl Status {
    /// `readme.md §3.6.3`: whether compute_fee should be charged.
    pub fn is_billable_compute(&self) -> bool {
        matches!(
            self,
            Status::Ok
                | Status::BusinessError { .. }
                | Status::RuntimeError { .. }
                | Status::Timeout {
                    stage: TimeoutStage::Exec
                }
        )
    }

    /// `readme.md §3.6.3`: whether base_fee (invocation) should be charged.
    pub fn is_billable_base(&self) -> bool {
        matches!(self, Status::Ok | Status::BusinessError { .. })
    }
}
