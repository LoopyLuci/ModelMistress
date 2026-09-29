//! MCP Client for ModelMistress to control other agents

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Client handle for communicating with other agents via MCP
#[derive(Clone)]
pub struct McpClient {
    agent_id: String,
    connected_agents: Arc<RwLock<Vec<AgentEndpoint>>>,
}

impl Default for McpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl McpClient {
    pub fn new() -> Self {
        Self {
            agent_id: format!("model-mistress-{}", uuid::Uuid::new_v4().to_string().chars().take(8).collect::<String>()),
            connected_agents: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Connect to an agent by endpoint
    pub async fn connect(&mut self, agent: AgentEndpoint) -> Result<(), McpClientError> {
        self.connected_agents.write().await.push(agent);
        Ok(())
    }

    /// Call a tool on a connected agent
    pub async fn call_agent_tool(&self, agent_id: &str, tool_name: &str, params: serde_json::Value) -> Result<serde_json::Value, McpClientError> {
        // In a real implementation, this would make HTTP/WS request to the agent
        // For now, return a stub response
        Ok(serde_json::json!({
            "status": "called",
            "agent": agent_id,
            "tool": tool_name,
            "result": "stub"
        }))
    }

    /// Send a message to an agent
    pub async fn send_message(&self, agent_id: &str, message: McpMessage) -> Result<(), McpClientError> {
        // In a real implementation, this would send the message
        let _ = (agent_id, message);
        Ok(())
    }

    /// Get all connected agent IDs
    pub async fn list_connected_agents(&self) -> Vec<String> {
        self.connected_agents.read().await.iter().map(|a| a.id.clone()).collect()
    }
}

/// Handle wrapper for McpClient to be shared across the application
pub struct McpClientHandle {
    pub client: Arc<RwLock<McpClient>>,
    pub hermes_endpoint: Option<String>,
    pub other_agents: Vec<String>,
}

impl Default for McpClientHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl McpClientHandle {
    pub fn new() -> Self {
        Self {
            client: Arc::new(RwLock::new(McpClient::new())),
            hermes_endpoint: None,
            other_agents: Vec::new(),
        }
    }

    /// Connect to Hermes Agent
    pub async fn connect_hermes(&mut self, endpoint: String) {
        self.hermes_endpoint = Some(endpoint);
    }

    /// Add another agent to control
    pub fn add_agent(&mut self, agent_id: String) {
        if !self.other_agents.contains(&agent_id) {
            self.other_agents.push(agent_id);
        }
    }
}

/// An agent endpoint
#[derive(Debug, Clone)]
pub struct AgentEndpoint {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub capabilities: Vec<String>,
}

/// Message format for agent communication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpMessage {
    pub id: String,
    pub from: String,
    pub to: String,
    pub message_type: String,
    pub content: serde_json::Value,
    pub response_required: bool,
}

impl McpMessage {
    pub fn new(to: String, message_type: impl Into<String>, content: serde_json::Value) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            from: String::new(),
            to,
            message_type: message_type.into(),
            content,
            response_required: false,
        }
    }
}

/// MCP client errors
#[derive(Debug)]
pub enum McpClientError {
    ConnectionFailed(String),
    Timeout,
    InvalidResponse,
    AgentNotFound(String),
}

impl std::fmt::Display for McpClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpClientError::ConnectionFailed(endpoint) => write!(f, "Connection failed to: {}", endpoint),
            McpClientError::Timeout => write!(f, "Request timeout"),
            McpClientError::InvalidResponse => write!(f, "Invalid response from agent"),
            McpClientError::AgentNotFound(id) => write!(f, "Agent not found: {}", id),
        }
    }
}

impl std::error::Error for McpClientError {}