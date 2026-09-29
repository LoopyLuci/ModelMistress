//! MCP server implementation for ModelMistress

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResponse {
    pub id: String,
    pub tool: String,
    pub result: serde_json::Value,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpError {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMistressState {
    pub models: Vec<String>,
    pub active_connections: usize,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceInfo {
    pub request_id: String,
    pub model: String,
    pub tokens_generated: usize,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryUsage {
    pub model_size_mb: f64,
    pub kv_cache_mb: f64,
    pub total_mb: f64,
}

#[derive(Debug, Clone)]
pub struct McpConnection {
    pub id: String,
    pub endpoint: String,
    pub connected: bool,
}

#[derive(Debug)]
pub struct McpServer {
    pub tools: HashMap<String, McpTool>,
    pub connections: Arc<RwLock<HashMap<String, McpConnection>>>,
    pub state: Arc<RwLock<ModelMistressState>>,
}

impl McpServer {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            connections: Arc::new(RwLock::new(HashMap::new())),
            state: Arc::new(RwLock::new(ModelMistressState {
                models: Vec::new(),
                active_connections: 0,
                status: "initialized".into(),
            })),
        }
    }

    pub async fn register_tool(&mut self, tool: McpTool) {
        self.tools.insert(tool.name.clone(), tool);
    }

    pub async fn connect_agent(&self, agent_id: String, endpoint: String) {
        let conn = McpConnection {
            id: agent_id.clone(),
            endpoint,
            connected: true,
        };
        self.connections.write().await.insert(agent_id, conn);
    }

    pub async fn list_tools(&self) -> Vec<McpTool> {
        self.tools.values().cloned().collect()
    }

    pub async fn call_tool(&self, name: &str, params: serde_json::Value) -> McpToolResponse {
        let id = Uuid::new_v4().to_string();
        match self.tools.get(name) {
            Some(_) => McpToolResponse {
                id,
                tool: name.into(),
                result: serde_json::json!({"status": "ok", "params": params}),
                error: None,
            },
            None => McpToolResponse {
                id,
                tool: name.into(),
                result: serde_json::json!({}),
                error: Some(format!("tool not found: {name}")),
            },
        }
    }
}

pub fn init_default_tools() -> HashMap<String, McpTool> {
    let mut tools = HashMap::new();
    tools.insert("list_models".into(), McpTool {
        name: "list_models".into(),
        description: "List available models".into(),
        parameters: serde_json::json!({}),
    });
    tools.insert("chat".into(), McpTool {
        name: "chat".into(),
        description: "Run a chat completion".into(),
        parameters: serde_json::json!({"model": "string", "messages": "array"}),
    });
    tools.insert("health".into(), McpTool {
        name: "health".into(),
        description: "Health check".into(),
        parameters: serde_json::json!({}),
    });
    tools
}
