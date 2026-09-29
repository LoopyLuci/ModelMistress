# Hermes Agent Integration

**trigger**: Use when connecting ModelMistress with Hermes Agent control plane
**description**: Bidirectional agent communication via MCP protocol and Hermes control plane

## Overview

ModelMistress integrates with Hermes Agent through multiple communication channels:

1. **MCP Server** (Port 7780) - Bidirectional tool exposure
2. **tm-mcp Registered Tools** - TaskManager integration
3. **SSE Proxy** - OpenAI-compatible API forwarding
4. **Control Plane API** - State management and commands

## Integration Architecture

```
┌─────────────────┐
│   ModelMistress │
│   MCP Server    │◄─── TCP ───┬──► Hermes Agent
│   (Port 7780)   │           │
└─────────────────┘           │
                              │
┌─────────────────┐           │
│  tm-mcp         │◄─ HTTP ───┘
│  MCP Bridge     │
└─────────────────┘
```

## MCP Server Setup

### Server Initialization

```rust
let server = McpServer::new("127.0.0.1", 7780);

// Register core tools
server.register_tool("model:load", handler_load_model)?;
server.register_tool("model:infer", handler_infer)?;
server.register_tool("model:unload", handler_unload)?;
server.register_tool("agent:swarm", handler_swarm)?;

// Start listening
server.listen().await?;
```

### Tool Handler Template

```rust
async fn handler_load_model(
    &self,
    _conn: &McpConnection,
    params: serde_json::Value,
) -> Result<serde_json::Value, McpError> {
    let path = params["path"].as_str()
        .ok_or(McpError::InvalidParams)?;
    
    let result = self.model_manager.load(&path).await?;
    
    Ok(json!({
        "success": true,
        "model_id": result.id,
        "context": result.context_length
    }))
}
```

## tm-mcp Integration

### Registered Tools

ModelMistress registers with tm-mcp for:

```toml
# ~/.hermes/profiles/default/tools/model-mistress.toml
[[tools]]
name = "ModelMistress Load Model"
description = "Load a model from GGUF file"
input_schema = { type = "object", properties = { path = { type = "string" } } }

[[tools]]
name = "ModelMistress Inference"
description = "Run inference with loaded model"
input_schema = { type = "object", properties = { model = { type = "string" }, prompt = { type = "string" } } }
```

### TaskManager Communication

```rust
// Communicate via TaskManager API
let client = tm_mcp::Client::new("http://localhost:8383");

// Execute model load task
let task = TaskSpec {
    name: "model-load".to_string(),
    agent: "model-mistress".to_string(),
    intent: Intent::LoadModel {
        path: "/models/gpt4.gguf".to_string(),
    },
    ..Default::default()
};

let result = client.execute(task).await?;
```

## SSE Proxy Integration

### OpenAI-Compatible API

```rust
// Forward requests through SSE proxy
let sse_client = reqwest::Client::new();
let response = sse_client
    .post("http://localhost:7780/v1/chat/completions")
    .json(&request_body)
    .send()
    .await?;
```

### Request Routing

```rust
pub struct SseProxy {
    pub target: String,  // e.g., "ollama://localhost:11434"
    pub model: String,   // Default model
    pub headers: Vec<Header>,
}

impl SseProxy {
    pub async fn forward(&self, request: OpenAiRequest) -> Result<OpenAiResponse, Error> {
        // Transform OpenAI format to target format
        let target_request = self.transform(request).await?;
        
        // Forward to model server
        let response = reqwest::Client::new()
            .post(&self.target)
            .json(&target_request)
            .send()
            .await?;
        
        // Transform back to OpenAI format
        self.transform_response(response).await
    }
}
```

## Control Plane Commands

### CLI Commands via Hermes

The following slash commands are exposed to Hermes Agent:

| Command | Description |
|---------|-------------|
| `/models` | List loaded models |
| `/load <path>` | Load model from GGUF |
| `/unload <name>` | Unload model |
| `/infer <model> <prompt>` | Run inference |
| `/status` | Show system status |
| `/health` | Health check endpoint |
| `/swarm <action>` | Manage agent swarms |

### Programmatic Access

```rust
// Execute commands from Rust
let registry = commands::get_command_registry();

// Execute with arguments
let result = registry.execute("/load /models/gpt4.gguf");

if let Ok(response) = result {
    if response.success {
        println!("Model loaded: {}", response.content);
    }
}
```

## State Synchronization

### Shared Model Registry

```rust
pub struct ModelRegistry {
    pub models: RwLock<HashMap<String, LoadedModel>>,
    pub events: RwLock<VecDeque<ModelEvent>>,
}

impl ModelRegistry {
    pub fn sync_with_hermes(&self) -> Result<(), SyncError> {
        let hermes_state = self.hermes_client.get_state().await?;
        let mut models = self.models.write().await?;
        
        // Reconcile model states
        for (id, model) in hermes_state.models {
            models.entry(id).or_insert(model);
        }
        
        Ok(())
    }
}
```

### Event Streaming

```rust
// Stream events to Hermes Agent
pub async fn stream_events(&self, mut tx: Sender<ModelEvent>) {
    loop {
        let event = self.event_bus.recv().await;
        if let Err(_) = tx.send(event).await {
            break; // Client disconnected
        }
    }
}
```

## Authentication & Security

### API Keys

```rust
use axum::{
    middleware::from_fn,
    http::Request,
    response::Response,
};

async fn auth_middleware<B>(
    req: Request<B>,
    next: impl FnOnce(Request<B>) -> axum::response::Response,
) -> Response {
    let auth = req.headers().get("Authorization");
    if validate_auth(auth).await {
        next(req)
    } else {
        axum::response::Html("<h1>401 Unauthorized</h1>").into_response()
    }
}
```

### Session Management

```rust
#[derive(Clone)]
pub struct SessionStore {
    sessions: DashMap<String, Session>,
}

pub struct Session {
    pub user_id: String,
    pub model: Option<String>,
    pub expires_at: DateTime<Utc>,
}
```

## Monitoring & Telemetry

### Health Endpoint

```rust
#[axum::debug_handler]
async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "healthy",
        "uptime": metrics::uptime(),
        "models_loaded": metrics::model_count(),
        "memory_usage": metrics::memory_usage(),
        "last_error": metrics::last_error().await
    }))
}
```

### Prometheus Metrics

```rust
// Model loading counter
pub static MODEL_LOAD_COUNT: AtomicU64 = AtomicU64::new(0);

// Inference latency histogram
lazy_static::lazy_static! {
    pub static ref INFERENCE_LATENCY: Histogram = Histogram::new();
}

// Export metrics
async fn prometheus_handler() -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let mf = prometheus::gather();
    let mut output = vec![];
    encoder.encode(&mf, &mut output).unwrap();
    String::from_utf8(output).unwrap()
}
```

## Testing Integration

### Integration Tests

```rust
#[tokio::test]
async fn test_mcp_integration() {
    let server = spawn_test_server();
    
    let result = client.call_tool("model:load", json!({
        "path": "/test/model.gguf"
    })).await?;
    
    assert!(result["success"].as_bool().unwrap());
}

#[tokio::test]
async fn test_hermes_bridge() {
    let bridge = HermesBridge::new("test-agent");
    
    let state = bridge.sync_state().await?;
    assert!(!state.is_empty());
}
```

## Common Integration Patterns

### Tool Chaining

```rust
// Load model -> Get inference -> Stream results
let model = mcp.call("model:load", { path: "/model.gguf" }).await?;
let result = mcp.call("model:infer", { 
    model: model.id, 
    prompt: "Hello" 
}).await?;
stream.emit(result);
```

### Batch Operations

```rust
// Load multiple models
let models = ["gpt4", "llama", "mixtral"];
let results = futures::future::join_all(
    models.iter().map(|m| load_model(m))
).await;
```

## Troubleshooting

### Connection Issues
- Verify MCP server running: `netstat -tlnp | grep 7780`
- Check firewall settings
- Test with `curl http://localhost:7780/health`

### Tool Errors
- Check tool registration logs
- Validate input parameters
- Review error handling in handlers

### Performance
- Monitor with Prometheus metrics
- Check SSE proxy latency
- Review model loading times

## Related Skills
- `mcp-server-development` - Protocol implementation
- `brainbuilder-integration` - Multi-agent orchestration
- `model-mistress-desktop-dev` - Application integration
- `slash-commands-pattern` - User interface design