//! MCP (Model Context Protocol) integration for ModelMistress
//!
//! Enables bidirectional agent communication and control between
//! ModelMistress and Hermes Agent, as well as other agents.

pub mod client;
pub mod server;

pub use client::{McpClient, McpClientHandle};
pub use server::{init_default_tools, McpError, McpServer, McpTool, McpToolResponse};
pub use server::{AgentInfo, InferenceInfo, MemoryUsage, ModelInfo, ModelMistressState};
