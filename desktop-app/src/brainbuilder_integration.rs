//! BrainBuilder Integration for ModelMistress

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct BrainBuilderIntegration {
    pub connected: bool,
    pub bb_endpoint: Option<String>,
    pub registry: Arc<RwLock<HashMap<String, ModelInfo>>>,
    pub graphs: Arc<RwLock<HashMap<String, GraphInfo>>>,
}

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub name: String,
    pub path: String,
    pub params: u64,
    pub context: usize,
    pub loaded: bool,
}

#[derive(Debug, Clone)]
pub struct GraphInfo {
    pub id: String,
    pub name: String,
    pub trainable_params: u64,
    pub status: String,
}

impl Default for BrainBuilderIntegration {
    fn default() -> Self {
        Self {
            connected: false,
            bb_endpoint: None,
            registry: Arc::new(RwLock::new(HashMap::new())),
            graphs: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl BrainBuilderIntegration {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn connect(&mut self, endpoint: String) -> Result<String, String> {
        let msg = format!("Connected to BrainBuilder at {}", endpoint);
        self.bb_endpoint = Some(endpoint);
        self.connected = true;
        Ok(msg)
    }

    pub async fn list_graphs(&self) -> Result<Vec<GraphInfo>, String> {
        if !self.connected {
            return Err("Not connected to BrainBuilder".to_string());
        }
        let graphs = self.graphs.read().await;
        Ok(graphs.values().cloned().collect())
    }

    pub async fn load_model(&self, model_name: &str, graph_id: &str) -> Result<String, String> {
        if !self.connected {
            return Err("Not connected to BrainBuilder".to_string());
        }
        let mut registry = self.registry.write().await;
        registry.insert(
            model_name.to_string(),
            ModelInfo {
                name: model_name.to_string(),
                path: format!("brainbuilder://{graph_id}"),
                params: 8_000_000_000,
                context: 8192,
                loaded: true,
            },
        );
        Ok(format!("Model '{}' loaded from graph '{}'", model_name, graph_id))
    }

    pub async fn infer_from_graph(&self, model_name: &str, input: &str) -> Result<String, String> {
        if !self.connected {
            return Err("Not connected to BrainBuilder".to_string());
        }
        let registry = self.registry.read().await;
        if let Some(model) = registry.get(model_name) {
            if !model.loaded {
                return Err(format!("Model '{}' not loaded", model_name));
            }
        } else {
            return Err(format!("Model '{}' not found", model_name));
        }
        drop(registry);
        Ok(format!("BrainBuilder inference: {}", input.chars().take(50).collect::<String>()))
    }

    pub async fn unload_model(&self, model_name: &str) -> Result<String, String> {
        let mut registry = self.registry.write().await;
        if registry.remove(model_name).is_some() {
            Ok(format!("Model '{}' unloaded", model_name))
        } else {
            Err(format!("Model '{}' not found", model_name))
        }
    }
}

pub mod bridge {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct BrainBuilderRequest {
        pub action: String,
        pub graph_id: Option<String>,
        pub params: serde_json::Value,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct BrainBuilderResponse {
        pub success: bool,
        pub result: serde_json::Value,
        pub error: Option<String>,
    }

    impl BrainBuilderResponse {
        pub fn success(data: impl Into<serde_json::Value>) -> Self {
            Self { success: true, result: data.into(), error: None }
        }
        pub fn error(msg: impl Into<String>) -> Self {
            Self { success: false, result: serde_json::json!({}), error: Some(msg.into()) }
        }
    }
}