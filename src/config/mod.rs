pub mod hardware;

use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use tracing::info;

pub use hardware::{
    CpuInfo, CpuInfoDetected, GpuInfo, GpuInfoDetected, HardwareConfigBuilder,
    HardwareInfo, Microarchitecture, MemoryBudget, PerformanceConfig,
    PerformanceProfile, SimdFeatures, ThreadPoolConfig,
};

// ============================================================================
// Configuration System
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMistressConfig {
    pub server: ServerConfig,
    pub router: RouterConfig,
    pub plugins: Vec<PluginConfig>,
    pub observability: ObservabilityConfig,
    pub security: SecurityConfig,
    /// Hardware-specific configuration (auto-detected if not set)
    #[serde(default)]
    pub hardware: HardwareConfigSection,
}

/// Top-level hardware configuration section in model-mistress.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareConfigSection {
    /// Performance profile: low_latency, throughput, balanced
    #[serde(default = "default_profile")]
    pub profile: PerformanceProfile,
    /// Override auto-detected thread counts
    #[serde(default)]
    pub thread_override: Option<ThreadPoolConfig>,
    /// CPU-only mode (no GPU offloading)
    #[serde(default)]
    pub cpu_only: bool,
}

fn default_profile() -> PerformanceProfile {
    PerformanceProfile::Balanced
}

impl Default for HardwareConfigSection {
    fn default() -> Self {
        Self {
            profile: PerformanceProfile::Balanced,
            thread_override: None,
            cpu_only: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub listen_addr: String,
    pub worker_threads: Option<usize>,
    pub max_connections: Option<u32>,
    pub request_timeout_ms: u64,
    pub cors_origins: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterConfig {
    pub default_model: Option<String>,
    pub fallback_model: Option<String>,
    pub enable_canary: bool,
    pub canary_percentage: f64,
    pub cloud_burst_enabled: bool,
    pub cloud_burst_threshold: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub enabled: bool,
    pub settings: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    pub enable_tracing: bool,
    pub enable_metrics: bool,
    pub metrics_port: u16,
    pub log_level: String,
    pub log_format: LogFormat,
    pub tracing_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogFormat {
    Json,
    Pretty,
    Compact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub enable_mtls: bool,
    pub tls_cert_path: Option<String>,
    pub tls_key_path: Option<String>,
    pub api_keys: Vec<String>,
    pub jwt_secret: Option<String>,
    pub allowed_origins: Vec<String>,
}

impl Default for ModelMistressConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                listen_addr: "0.0.0.0:8000".to_string(),
                worker_threads: None,
                max_connections: Some(10000),
                request_timeout_ms: 30000,
                cors_origins: vec!["*".to_string()],
            },
            router: RouterConfig {
                default_model: Some("llama-3-8b".to_string()),
                fallback_model: Some("llama-3-8b".to_string()),
                enable_canary: false,
                canary_percentage: 0.05,
                cloud_burst_enabled: false,
                cloud_burst_threshold: 0.85,
            },
            plugins: vec![],
            observability: ObservabilityConfig {
                enable_tracing: true,
                enable_metrics: true,
                metrics_port: 9090,
                log_level: "info".to_string(),
                log_format: LogFormat::Json,
                tracing_endpoint: None,
            },
            security: SecurityConfig {
                enable_mtls: false,
                tls_cert_path: None,
                tls_key_path: None,
                api_keys: vec![],
                jwt_secret: None,
                allowed_origins: vec!["*".to_string()],
            },
            hardware: HardwareConfigSection::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Failed to read config file: {0}")]
    ReadError(String),
    
    #[error("Failed to parse config: {0}")]
    ParseError(String),
}

impl ModelMistressConfig {
    /// Resolve the effective hardware/performance configuration.
    /// Uses the config's hardware section + auto-detection.
    pub fn resolve_performance(&self) -> PerformanceConfig {
        let mut builder = HardwareConfigBuilder::new()
            .with_profile(self.hardware.profile);

        if self.hardware.cpu_only {
            builder = builder.cpu_only();
        }
        if let Some(ref threads) = self.hardware.thread_override {
            builder = builder.with_threads(threads.clone());
        }

        builder.build()
    }

    pub fn load() -> Result<Self, ConfigError> {
        // Try to load from file, fall back to defaults
        let config_path = std::env::var("MODEL_MISTRESS_CONFIG")
            .unwrap_or_else(|_| "model-mistress.toml".to_string());
        
        if std::path::Path::new(&config_path).exists() {
            let content = std::fs::read_to_string(&config_path)
                .map_err(|e| ConfigError::ReadError(e.to_string()))?;
            let config: ModelMistressConfig = toml::from_str(&content)
                .map_err(|e| ConfigError::ParseError(e.to_string()))?;
            info!(path = %config_path, "Loaded configuration");
            Ok(config)
        } else {
            info!("No config file found, using defaults");
            Ok(Self::default())
        }
    }
}
