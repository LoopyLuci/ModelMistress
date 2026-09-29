# BrainBuilder Integration

**trigger**: Use when connecting ModelMistress with BrainBuilder project for AI orchestration
**description**: Bidirectional integration between ModelMistress model serving and BrainBuilder's orchestration engine

## Overview

BrainBuilder is a polyglot AI orchestration engine that manages computational graphs, models, and distributed training. ModelMistress integrates with BrainBuilder to:

- Share models across agent boundaries
- Execute inference on BrainBuilder graphs
- Manage model lifecycles centrally
- Enable cross-project agent collaboration

## Integration Architecture

```
┌─────────────────┐         ┌──────────────────┐
│   ModelMistress │◄───────►│   BrainBuilder   │
│                 │  MCP    │                  │
│  ┌───────────┐  │         │  ┌────────────┐  │
│  │ Model     │  │         │  │ Graph      │  │
│  │ Registry  │──┼─────────┼─►│ Registry   │  │
│  │           │  │         │  │            │  │
│  │ Inference │  │         │  │ Training   │  │
│  │ Engine    │  │         │  │ Engine     │  │
│  └───────────┘  │         │  └────────────┘  │
└─────────────────┘         └──────────────────┘
         ▲                          ▲
         │                          │
         ▼                          ▼
    ┌──────────┐              ┌──────────┐
    │  MCP     │              │  REST    │
    │  Server  │              │  API     │
    └──────────┘              └──────────┘
```

## Connection Setup

### Initial Connection

```rust
let mut integration = BrainBuilderIntegration::new();

// Connect to BrainBuilder instance
let result = integration.connect(
    "brainbuilder-worker-1",  // agent_id
    "http://localhost:8080"   // endpoint
).await?;

println!("Connected: {}", result);
```

### Health Check

```rust
pub async fn health_check(&self) -> Result<HealthStatus, String> {
    if !self.connected {
        return Err("Not connected to BrainBuilder".to_string());
    }
    
    let response = reqwest::get(&format!("{}/api/health", self.endpoint))
        .await?
        .json::<HealthStatus>()
        .await?;
    
    Ok(response)
}
```

## Working with Models

### Loading Models from BrainBuilder

```rust
// Load model from a BrainBuilder graph
let result = integration.load_model(
    "gpt4-custom",
    "graph-production-001"
).await?;

// Model is now accessible via MCP tools
// /model:load, /model:infer, /model:unload
```

### Inference Pipeline

```rust
// Run inference using BrainBuilder graph
let result = integration.infer_from_graph(
    "gpt4-custom",
    "User query: What is the weather?"
).await?;

let response = serde_json::from_str(&result)?;
```

### Model Registry Synchronization

```rust
// List all models in BrainBuilder registry
let models = integration.list_models().await?;

for model in models {
    println!("Model: {} ({})", model.name, model.path);
}
```

## Graph Operations

### List Available Graphs

```rust
pub async fn list_graphs(&self) -> Result<Vec<GraphInfo>, String> {
    let response = reqwest::get(&format!("{}/api/graphs", self.endpoint))
        .await?
        .json::<Vec<GraphInfo>>()
        .await?;
    
    Ok(response)
}

pub struct GraphInfo {
    pub id: String,
    pub name: String,
    pub params: u64,
    pub status: GraphStatus,
    pub created_at: DateTime<Utc>,
}
```

### Execute Graph

```rust
// Start graph execution
let execution = integration.execute_graph(
    "graph-training-001",
    vec!["--epochs=10", "--lr=0.001"]
).await?;

println!("Started execution: {}", execution.id);
```

## Cross-Agent Communication

### Message Passing

```rust
// Send message to another agent via BrainBuilder
let result = integration.send_message(
    "agent-hermes",
    Message {
        type: "model_request".to_string(),
        payload: json!({ "model": "gpt4" }),
    }
).await?;
```

### Request-Response Pattern

```rust
let response = integration.request::<InferenceRequest, InferenceResponse>(
    "agent-worker-gpu",
    InferenceRequest {
        model: "llama-3-70b".to_string(),
        prompt: "Explain quantum computing".to_string(),
    }
).await?;
```

## MCP Tool Integration

### Expose BrainBuilder Tools

```rust
// Register MCP tools that route to BrainBuilder
mcp_server.register_tool(
    "brainbuilder:load_graph",
    Box::new(|params| {
        let graph_id = params["graph_id"].as_str()?;
        let integration = get_integration();
        integration.load_graph(graph_id)
    })
)?;

mcp_server.register_tool(
    "brainbuilder:execute_model",
    Box::new(|params| {
        let model = params["model"].as_str()?;
        let prompt = params["prompt"].as_str()?;
        let integration = get_integration();
        integration.run_inference(model, prompt)
    })
)?;
```

### Available Slash Commands

- `/bb-connect <endpoint>` - Connect to BrainBuilder instance
- `/bb-graphs` - List available graphs
- `/bb-load <graph_id>` - Load model from graph
- `/bb-exec <graph_id>` - Execute graph
- `/bb-status` - Check integration status

## Error Handling

### Connection Errors

```rust
pub enum BrainBuilderError {
    ConnectionFailed(String),
    Timeout(String),
    InvalidResponse(String),
    ResourceNotFound(String),
}

impl From<reqwest::Error> for BrainBuilderError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            BrainBuilderError::Timeout(e.to_string())
        } else {
            BrainBuilderError::ConnectionFailed(e.to_string())
        }
    }
}
```

### Retry Logic

```rust
pub async fn with_retry<F, T>(mut f: F, max_retries: usize) -> Result<T, String>
where
    F: FnMut() -> impl Future<Output = Result<T, String>>,
{
    let mut last_error = None;
    
    for _ in 0..max_retries {
        match f().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = Some(e);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    
    Err(last_error.unwrap_or("Unknown error".to_string()))
}
```

## Testing Integration

```rust
#[tokio::test]
async fn test_brainbuilder_connection() {
    let mut integration = BrainBuilderIntegration::new();
    
    // Test connection
    let result = integration.connect("test-agent", "http://localhost:8080").await;
    assert!(result.is_ok());
    
    // Test health check
    if integration.connected {
        let health = integration.health_check().await;
        println!("Health: {:?}", health);
    }
}

#[tokio::test]
async fn test_model_loading() {
    let integration = BrainBuilderIntegration::new();
    
    // Mock test
    let result = integration.load_model("test-model", "graph-001").await;
    assert!(result.is_ok());
}
```

## Best Practices

1. **Connection pooling**: Reuse HTTP clients for better performance
2. **Timeouts**: Set reasonable timeouts for all external calls
3. **Circuit breaker**: Implement failure detection and recovery
4. **Async everywhere**: Use async/await for non-blocking operations
5. **Type safety**: Use strong typing for requests/responses
6. **Logging**: Log all integration events for debugging
7. **Fallbacks**: Have graceful degradation when BrainBuilder is unavailable

## Troubleshooting

### Connection Issues
- Verify BrainBuilder is running: `curl http://localhost:8080/api/health`
- Check firewall rules
- Validate authentication tokens

### Model Loading Failures
- Ensure model path is correct
- Check GGUF file integrity
- Verify model is registered in BrainBuilder

### Synchronization Problems
- Check state consistency between ModelMistress and BrainBuilder
- Use version/timestamp checks
- Implement reconciliation logic

## Related Skills
- `mcp-server-development` - Protocol-level communication
- `model-mistress-desktop-dev` - Application integration
- `slash-commands-pattern` - User-facing commands
- `hermes-agent` - Agent orchestration