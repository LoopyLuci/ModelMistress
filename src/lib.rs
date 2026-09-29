//! ModelMistress: loads and serves local models (GGUF through llama.cpp) behind one OpenAI-compatible API and a
//! control API (ABP's module contract) for programs and agents.
//!
//! The working core is `catalog` (what models are on this machine), `engine` (llama.cpp processes, Ollama
//! pass-through), `hub` (the HTTP server) and `client` (the CLI's and MCP's way to a running hub).

pub mod catalog;
pub mod client;
pub mod engine;
pub mod hub;

// ---- legacy -----------------------------------------------------------------------------------------------------
// The first design's scaffolding, kept until it is either rebuilt on the core above or removed. Its CPU engine
// (`backends::cpu`) returns placeholder text and `server` is superseded by `hub`; see docs/STATUS.md. Its lints
// are allowed here so that `clippy -D warnings` still holds the core to the strict bar.
#[allow(dead_code, unused, non_camel_case_types, clippy::all)]
pub mod backends;
#[allow(dead_code, unused, clippy::all)]
pub mod config;
#[allow(dead_code, unused, clippy::all)]
pub mod mcp;
#[allow(dead_code, unused, clippy::all)]
pub mod models;
#[allow(dead_code, unused, clippy::all)]
pub mod observability;
#[allow(dead_code, unused, clippy::all)]
pub mod plugins;
#[allow(dead_code, unused, clippy::all)]
pub mod protocol;
#[allow(dead_code, unused, clippy::all)]
pub mod router;
#[allow(dead_code, unused, clippy::all)]
pub mod runtime;
#[allow(dead_code, unused, clippy::all)]
pub mod server;

pub use mcp::{init_default_tools, ModelMistressState};
pub use mcp::{AgentInfo, InferenceInfo, McpError, McpServer, McpToolResponse, MemoryUsage, ModelInfo};
