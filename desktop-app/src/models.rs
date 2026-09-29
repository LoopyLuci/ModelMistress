//! Agent swarm and sub-agent configuration module
//! 
//! Provides enterprise-grade agent orchestration with fine-grained control

use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentConfig {
    pub name: String,
    pub role: SubAgentRole,
    pub reasoning_effort: ReasoningEffort,
    pub max_actions: usize,
    pub timeout_seconds: u64,
    pub memory_limit_mb: usize,
    pub cpu_limit: usize,
    pub tools_enabled: Vec<String>,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum SubAgentRole {
    Leaf,
    Orchestrator,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum ReasoningEffort {
    None,
    Low,
    Medium,
    High,
    Max,
}

impl Default for SubAgentRole {
    fn default() -> Self {
        Self::Leaf
    }
}

impl Default for ReasoningEffort {
    fn default() -> Self {
        Self::High
    }
}

impl SubAgentConfig {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            role: SubAgentRole::default(),
            reasoning_effort: ReasoningEffort::default(),
            max_actions: 50,
            timeout_seconds: 300,
            memory_limit_mb: 2048,
            cpu_limit: 4,
            tools_enabled: vec!["terminal".to_string(), "file".to_string()],
            provider: "nostria".to_string(),
            model: "lmf2.5:8b".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentSwarm {
    pub agents: Vec<SubAgentConfig>,
    pub max_concurrent: usize,
    pub shared_memory: bool,
    pub auto_scaling: bool,
}

impl AgentSwarm {
    pub fn new() -> Self {
        Self {
            agents: vec![SubAgentConfig::new("default-agent")],
            max_concurrent: 4,
            shared_memory: true,
            auto_scaling: true,
        }
    }

    pub fn add_agent(&mut self, config: SubAgentConfig) {
        self.agents.push(config);
    }

    pub fn remove_agent(&mut self, name: &str) {
        self.agents.retain(|a| a.name != name);
    }

    pub fn list_agents(&self) -> Vec<&SubAgentConfig> {
        self.agents.iter().collect()
    }
}

impl Default for AgentSwarm {
    fn default() -> Self {
        Self::new()
    }
}