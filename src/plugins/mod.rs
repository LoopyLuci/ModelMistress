use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

// ============================================================================
// Plugin System - Designed for 100-year extensibility
// ============================================================================

/// Core Plugin trait that all plugins must implement.
/// This is the stable ABI for the plugin system.
pub trait Plugin: Send + Sync + 'static {
    /// Unique identifier for this plugin
    fn id(&self) -> &str;
    
    /// Human-readable name
    fn name(&self) -> &str;
    
    /// Version of this plugin
    fn version(&self) -> &str;
    
    /// Capabilities this plugin provides
    fn capabilities(&self) -> Vec<Capability>;
    
    /// Initialize the plugin with configuration
    fn init(&mut self, config: &PluginConfig) -> Result<(), PluginError>;
    
    /// Handle a request if this plugin can
    /// Returns Some(response) if handled, None to pass to next handler
    fn handle_request(&self, request: &PluginRequest) -> Option<PluginResponse>;
    
    /// Cleanup when plugin is unloaded
    fn shutdown(&mut self) -> Result<(), PluginError>;
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginConfig {
    pub enabled: bool,
    pub settings: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginRequest {
    pub request_id: String,
    pub model: String,
    pub payload: serde_json::Value,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginResponse {
    pub status: u32,
    pub body: serde_json::Value,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Capability {
    Authentication,
    Authorization,
    RateLimiting,
    RequestTransform,
    ResponseTransform,
    Logging,
    Metrics,
    Caching,
    LoadBalancing,
    HealthCheck,
    Custom(String),
}

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("Plugin initialization failed: {0}")]
    InitFailed(String),
    
    #[error("Plugin execution error: {0}")]
    ExecutionError(String),
    
    #[error("Plugin not found: {0}")]
    NotFound(String),
    
    #[error("Plugin version mismatch: {0}")]
    VersionMismatch(String),
}

// ============================================================================
// Plugin Registry
// ============================================================================

pub struct PluginRegistry {
    plugins: tokio::sync::RwLock<Vec<Box<dyn Plugin>>>,
}

impl PluginRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            plugins: tokio::sync::RwLock::new(Vec::new()),
        })
    }

    pub async fn register(&self, mut plugin: Box<dyn Plugin>, config: &PluginConfig) -> Result<(), PluginError> {
        plugin.init(config)?;
        let id = plugin.id().to_string();
        let name = plugin.name().to_string();
        info!(plugin_id = %id, plugin_name = %name, "Registering plugin");
        self.plugins.write().await.push(plugin);
        Ok(())
    }

    pub async fn unregister(&self, plugin_id: &str) -> Result<(), PluginError> {
        let mut plugins = self.plugins.write().await;
        let initial_len = plugins.len();
        plugins.retain(|p| p.id() != plugin_id);
        
        if plugins.len() == initial_len {
            return Err(PluginError::NotFound(plugin_id.to_string()));
        }
        
        info!(plugin_id = %plugin_id, "Unregistered plugin");
        Ok(())
    }

    pub async fn process_request(&self, request: &PluginRequest) -> Option<PluginResponse> {
        let plugins = self.plugins.read().await;
        for plugin in plugins.iter() {
            if let Some(response) = plugin.handle_request(request) {
                return Some(response);
            }
        }
        None
    }

    pub async fn shutdown_all(&self) {
        let mut plugins = self.plugins.write().await;
        for plugin in plugins.iter_mut() {
            if let Err(e) = plugin.shutdown() {
                warn!(plugin_id = %plugin.id(), error = %e, "Error shutting down plugin");
            }
        }
        plugins.clear();
    }
}

// ============================================================================
// Built-in Plugin: API Key Authentication
// ============================================================================

pub struct ApiKeyAuthPlugin {
    valid_keys: Vec<String>,
}

impl ApiKeyAuthPlugin {
    pub fn new(valid_keys: Vec<String>) -> Self {
        Self { valid_keys }
    }
}

impl Plugin for ApiKeyAuthPlugin {
    fn id(&self) -> &str {
        "api-key-auth"
    }

    fn name(&self) -> &str {
        "API Key Authentication"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Authentication]
    }

    fn init(&mut self, _config: &PluginConfig) -> Result<(), PluginError> {
        Ok(())
    }

    fn handle_request(&self, request: &PluginRequest) -> Option<PluginResponse> {
        if let Some(api_key) = request.metadata.get("x-api-key") {
            if self.valid_keys.contains(api_key) {
                return None; // Allow request to continue
            }
        }
        
        Some(PluginResponse {
            status: 401,
            body: serde_json::json!({
                "error": {
                    "message": "Invalid or missing API key",
                    "type": "authentication_error"
                }
            }),
            headers: HashMap::new(),
        })
    }

    fn shutdown(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
}

// ============================================================================
// Built-in Plugin: Rate Limiter
// ============================================================================

pub struct RateLimiterPlugin {
    max_requests_per_minute: u32,
    request_counts: tokio::sync::RwLock<HashMap<String, (u32, std::time::Instant)>>,
}

impl RateLimiterPlugin {
    pub fn new(max_requests_per_minute: u32) -> Self {
        Self {
            max_requests_per_minute,
            request_counts: tokio::sync::RwLock::new(HashMap::new()),
        }
    }
}

impl Plugin for RateLimiterPlugin {
    fn id(&self) -> &str {
        "rate-limiter"
    }

    fn name(&self) -> &str {
        "Rate Limiter"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::RateLimiting]
    }

    fn init(&mut self, _config: &PluginConfig) -> Result<(), PluginError> {
        Ok(())
    }

    fn handle_request(&self, request: &PluginRequest) -> Option<PluginResponse> {
        let _client_id = request.metadata.get("x-client-id")
            .unwrap_or(&"anonymous".to_string())
            .clone();
        
        // In production, this would use a sliding window
        None
    }

    fn shutdown(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
}
