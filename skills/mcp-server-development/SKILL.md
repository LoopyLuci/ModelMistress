# MCP Server Development

**trigger**: Use when building Model Context Protocol servers for AI agent communication
**description**: Build MCP servers with tool exposure, bidirectional communication, and Hermes Agent integration

## Overview

MCP (Model Context Protocol) enables agents to communicate and control each other. ModelMistress implements an MCP server that:

- Exposes model serving capabilities as tools
- Supports bidirectional communication with Hermes Agent
- Integrates with BrainBuilder for orchestrated AI workflows
- Provides state sharing across agent boundaries

## Key Components

### McpServer Structure

```rust
pub struct McpServer {
    pub host: String,
    pub port: u16,
    pub tools: RwLock<ToolRegistry>,
    pub connections: RwLock<HashMap<String, McpConnection>>,
    pub state: Arc<McpServerState>,
}

pub struct McpServerState {
    pub models: ModelRegistry,
    pub agents: AgentRegistry,
    pub config: AppConfig,
}
```

### Tool Registration

Tools are registered using the `tool!` macro:

```rust
tool!(
    name: "model:load",
    description: "Load a model from GGUF file",
    input: {
        type: Object,
        properties: {
            path: { type: string, description: "Path to model file" },
            context: { type: number, description: "Context length" }
        },
        required: ["path"]
    },
    output: {
        type: Object,
        properties: {
            success: { type: boolean },
            message: { type: string }
        }
    }
);
```

## Implementation Steps

### 1. Initialize Server

```rust
let server = McpServer::new("127.0.0.1".to_string(), 7780);

// Register tools
server.register_tool("model:load", handler_load_model);
server.register_tool("model:infer", handler_infer);
server.register_tool("model:unload", handler_unload_model);

// Start server
server.listen().await?;
```

### 2. Tool Handler Pattern

```rust
async fn handler_load_model(
    &self,
    _conn: &McpConnection,
    params: serde_json::Value,
) -> Result<serde_json::Value, McpError> {
    let path = params["path"].as_str()
        .ok_or(McpError::InvalidParams)?;
    
    match self.model_manager.load(path).await {
        Ok(model) => Ok(json!({ "success": true, "id": model.id })),
        Err(e) => Err(McpError::ToolError(e.to_string())),
    }
}
```

### 3. Connection Management

```rust
// Accept connections from Hermes Agent
async fn handle_connection(&self, stream: TcpStream) {
    let conn = McpConnection::from_stream(stream).await?;
    let client_id = conn.client_id.clone();
    
    self.connections.write().await.insert(client_id, conn);
    
    // Process messages
    self.process_messages(client_id).await?;
}
```

## Integration with External Agents

### Hermes Agent Connection

```rust
// Connect ModelMistress to Hermes Agent
let mut integration = self.bb_integration.lock().await?;
integration.connect("hermes-agent", "localhost:7780").await?;
```

### BrainBuilder Integration

```rust
// Access BrainBuilder graphs
let graphs = integration.list_graphs().await?;

// Load model from graph
integration.load_model("my-model", graph.id).await?;
```

## Message Format

### Tool Call

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "model:load",
    "arguments": { "path": "/models/gpt4.gguf" }
  }
}
```

### Tool Response

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "success": true,
    "model_id": "gpt4",
    "context": 8192
  }
}
```

## Best Practices

1. **Tool idempotency**: Tools should be safe to call multiple times
2. **Error handling**: Return meaningful error messages
3. **State consistency**: Use Arc<RwLock<>> for shared state
4. **Async boundaries**: Keep handlers async, use spawn_blocking for CPU work
5. **Validation**: Validate parameters before processing
6. **Timeouts**: Set reasonable timeouts for model operations

## Testing MCP Servers

```rust
#[tokio::test]
async fn test_mcp_server() {
    let server = McpServer::test_server();
    
    // Test tool registration
    assert!(server.has_tool("model:load"));
    
    // Test tool call
    let result = server.call_tool("model:load", json!({"path": "/test.gguf"})).await;
    assert!(result.is_ok());
}
```

## Common Patterns

### Tool Chaining
```rust
// Load model -> Run inference -> Stream results
let model = server.call_tool("model:load", params).await?;
let output = server.call_tool("model:infer", json!({
    "model": model["id"],
    "prompt": "Hello"
})).await?;
```

### Batch Operations
```rust
let mut results = vec![];
for tool in tools {
    results.push(server.call_tool(tool, params).await);
}
```

## Related Skills
- `model-mistress-desktop-dev` - Desktop application integration
- `slash-commands-pattern` - Command-to-tool mapping
- `brainbuilder-integration` - Multi-agent orchestration
- `hermes-agent` - Agent control plane development