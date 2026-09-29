//! MCP (Model Context Protocol) integration for ModelMistress
//! 
//! Enables bidirectional agent communication and control between
//! ModelMistress and Hermes Agent, as well as other agents.

pub mod server;
pub mod client;

pub use server::{McpServer, McpTool, McpToolResponse, McpError, init_default_tools};
pub use client::{McpClient, McpClientHandle};
pub use server::{ModelMistressState, ModelInfo, AgentInfo, InferenceInfo, MemoryUsage};