# ACP (Agent Client Protocol) for ItemForge

## Overview

ACP is the protocol for agent-to-agent communication in the ItemForge ecosystem. It enables:
- Bidirectional communication between agents (Hermes Agent, model serving agents, etc.)
- Capability negotiation — determine what each agent can do
- Tool invocation and result routing
- Health monitoring and status reporting
- Message routing and queuing

ItemForge uses ModelMistress as its AI backend, which implements MCP for the same purpose. ACP complements MCP by providing a higher-level protocol for agent coordination.

---

## ACP Protocol Stack

```
┌──────────────────────────────────────────────────────────────┐
│                    Desktop GUI (pywebview)                    │
└──────────────────────────────────────────────────────────────┘
                              │
┌────────────────────────────┴────────────────────────────┐
│              ItemForge Backend (FastAPI)                  │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────────┐ │
│  │ Ingestion │ │ Research │ │ Docs     │ │ Listings     │ │
│  └──────────┘ └──────────┘ └──────────┘ └──────────────┘ │
│  ┌──────────┐ ┌──────────┐ ┌──────────────┐              │
│  │ AI Gateway│ │ Media Repo│ │ Platform APIs│             │
│  └──────────┘ └──────────┘ └──────────────┘              │
└────────────────────────────┬──────────────────────────────┘
                             │
┌────────────────────────────┴──────────────────────────────┐
│              ModelMistress Hub (Rust)                     │
│  /v1/chat/completions  → Qwen3.5-9B                       │
│  /v1/vision/classify   → ONNX vision                      │
│  /v1/embeddings        → nomic-embed                      │
│  /v1/km/*              → Knowledge Modules                │
└──────────────────────────────────────────────────────────────┘
```

---

## ACP Message Format

```json
{
  "protocol_version": { "major": 1, "minor": 0, "patch": 0 },
  "id": "request-123",
  "from": "hermes-agent",
  "to": "model-mistress",
  "message_type": "tool_call",
  "content": {
    "tool": "chat",
    "params": {
      "model": "qwen3.5:9b",
      "messages": [{"role": "user", "content": "Research this item..."}]
    }
  },
  "response_required": true
}
```

---

## Capability Negotiation

### Client Capabilities

| Capability | Description |
|------------|-------------|
| `streaming` | Support for streaming responses |
| `tool_calling` | Support for tool invocation |
| `vision` | Support for image processing |
| `audio` | Support for speech (STT/TTS) |
| `embeddings` | Support for embedding generation |
| `batch_inference` | Support for batch processing |
| `knowledge_modules` | Support for Knowledge Modules |

### Server Capabilities

| Capability | Description |
|------------|-------------|
| `chat` | Chat completion |
| `vision` | Image recognition |
| `embeddings` | Embedding generation |
| `rerank` | Document reranking |
| `knowledge_modules` | Knowledge Module management |
| `speech` | Speech-to-text and text-to-speech |
| `schedule` | Scheduling and automation |
| `platforms` | Multi-platform listing support |

### Negotiation Example

```json
{
  "protocol_version": { "major": 1, "minor": 0, "patch": 0 },
  "client_capabilities": ["streaming", "tool_calling", "vision", "audio"],
  "server_capabilities": ["chat", "vision", "embeddings", "knowledge_modules"],
  "negotiated": ["streaming", "tool_calling", "vision", "embeddings"]
}
```

---

## Health Check Protocol

### Health Check Request

```json
{
  "protocol_version": { "major": 1, "minor": 0, "patch": 0 },
  "id": "health-check",
  "message_type": "health_check",
  "content": {}
}
```

### Health Check Response

```json
{
  "status": "healthy",
  "timestamp": "2026-11-14T16:21:00Z",
  "uptime_seconds": 3600,
  "active_requests": 3,
  "queue_depth": 0,
  "gpu_utilization": 45.2,
  "memory_utilization": 62.8
}
```

### Health Status Values

| Status | Description |
|--------|-------------|
| `healthy` | All systems operational |
| `degraded` | Some features unavailable but service works |
| `unhealthy` | Service not operational |

---

## Model Registry Protocol

### List Models

```json
{
  "protocol_version": { "major": 1, "minor": 0, "patch": 0 },
  "id": "list-models",
  "message_type": "list_models",
  "content": {}
}
```

### Response

```json
{
  "models": [
    {
      "id": "qwen3.5:9b",
      "name": "Qwen3.5-9B",
      "version": "Q4_K_M",
      "format": "GGUF",
      "quantization": "INT4",
      "size_bytes": 5368709120,
      "checksum": "sha256:abc123...",
      "metadata": {
        "architecture": "Qwen3.5",
        "context_length": 32768,
        "vision": true
      }
    }
  ],
  "active_connections": 2
}
```

---

## Tool Invocation

### Call Tool

```json
{
  "protocol_version": { "major": 1, "minor": 0, "patch": 0 },
  "id": "call-chat",
  "message_type": "tool_call",
  "content": {
    "tool": "chat",
    "params": {
      "model": "qwen3.5:9b",
      "messages": [
        {"role": "user", "content": "How do I research this product?"}
      ]
    }
  },
  "response_required": true
}
```

### Response

```json
{
  "id": "call-chat",
  "result": {
    "response": "Here's how to research your product...",
    "tokens_used": 152,
    "latency_ms": 2450
  },
  "error": null
}
```

---

## ACP Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/acp/negotiate` | POST | Capability negotiation |
| `/api/acp/health` | GET | Health check |
| `/api/acp/models` | GET | List available models |
| `/api/acp/tools` | GET | List available tools |
| `/api/acp/tool/{name}` | POST | Call a tool |
| `/api/acp/message` | POST | Send message to agent |

---

## ACP vs MCP

| Aspect | MCP | ACP |
|--------|-----|-----|
| **Primary purpose** | Tool exposure to LLMs/agents | Agent-to-agent communication |
| **Transport** | stdio, HTTP, SSE | HTTP/SSE, WebSockets |
| **Scope** | Single agent, multiple tools | Multi-agent coordination |
| **Use case** | Agent exposes tools to LLM | Agents coordinate tasks |
| **Protocol version** | MCP 1.0 | ACP 1.0 |

---

## Integration with Hermes Agent

To integrate ItemForge with Hermes Agent:

1. **Install the Hermes Agent skill** (in `skills/`) — see `hermes-agent-integration` skill
2. **Connect to the ACP endpoint** — `http://localhost:8000/api/acp`
3. **Negotiate capabilities** — exchange capability lists
4. **Use tools** — call `chat`, `vision`, `embeddings`, `knowledge_modules` tools
5. **Monitor health** — `GET /api/acp/health`

### Hermes Agent Connection

```python
from model_mistress.mcp import McpClientHandle

handle = McpClientHandle()
handle.connect_hermes("http://localhost:8000/mcp")
```

---

## Troubleshooting

| Issue | Solution |
|-------|----------|
| Cannot connect to ACP | Check `http://localhost:8000/api/acp/health` |
| Capability negotiation fails | Ensure both sides support protocol v1.0+ |
| Tool call fails | Verify the tool exists and is registered |
| Health check shows unhealthy | Check ModelMistress server logs |
| Unable to list models | Ensure models are loaded (`model-mistress models`) |

---

## References

- `../README.md` — ItemForge overview
- `../docs/api.md` — API reference
- `../docs/claude.md` — Claude integration
- `../src/mcp/` — MCP server implementation
- `../src/protocol/` — Internal protocol and capability negotiation
- `../src/hub/` — HTTP server implementation
