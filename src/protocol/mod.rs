use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Internal Protocol - High-performance binary protocol for internal communication
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalRequest {
    pub request_id: String,
    pub model: String,
    pub prompt: String,
    pub parameters: InferenceParameters,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceParameters {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<i32>,
    pub stop: Option<Vec<String>>,
    pub stream: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalResponse {
    pub request_id: String,
    pub tokens: Vec<String>,
    pub finish_reason: FinishReason,
    pub usage: TokenUsage,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FinishReason {
    Stop,
    Length,
    ContentFilter,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

// ============================================================================
// Capability Negotiation Protocol
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityNegotiation {
    pub protocol_version: ProtocolVersion,
    pub client_capabilities: Vec<Capability>,
    pub server_capabilities: Vec<Capability>,
    pub negotiated: Vec<Capability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Capability {
    Streaming,
    ToolCalling,
    Vision,
    Audio,
    Embeddings,
    BatchInference,
    Custom(String),
}

impl ProtocolVersion {
    pub fn current() -> Self {
        Self {
            major: 1,
            minor: 0,
            patch: 0,
        }
    }

    pub fn is_compatible(&self, other: &ProtocolVersion) -> bool {
        self.major == other.major && self.minor >= other.minor
    }
}

// ============================================================================
// Health Check Protocol
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    pub status: HealthStatus,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub uptime_seconds: u64,
    pub active_requests: u32,
    pub queue_depth: u32,
    pub gpu_utilization: Option<f64>,
    pub memory_utilization: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

// ============================================================================
// Model Registry Protocol
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub format: ModelFormat,
    pub quantization: QuantizationType,
    pub size_bytes: u64,
    pub checksum: String,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelFormat {
    GGUF,
    SafeTensors,
    ONNX,
    PyTorch,
    JAX,
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum QuantizationType {
    FP32,
    FP16,
    BF16,
    INT8,
    INT4,
    GPTQ,
    AWQ,
    NF4,
    FP8,
    MX,
    Custom(String),
}
