# ModelMistress ItemForge
## All-in-One Item Listing & Sales Platform

**Clever unique name:** **ItemForge** — "Forging items into listings" (create + document + publish in one place)

**Full project name:** ItemForge (built on ModelMistress, the local model hub)

**Sub-project name:** ModelMistress-ItemForge

---

## What This Is

ItemForge is an all-in-one desktop platform for resellers and sellers that:
1. **Ingests photos** — drag-drop or folder watch, auto-organizes into item folders
2. **Identifies items** — AI vision + LLM analysis for product identification
3. **Researches products** — deep research on each item (features, condition, provenance)
4. **Generates documentation** — per-item markdown documentation
5. **Creates platform listings** — Facebook Marketplace, Instagram, eBay, Etsy optimized content
6. **Automates posting** — schedule and publish listings

**Backend:** [ModelMistress](README.md) (local model hub — OpenAI-compatible API + Ollama-compatible API + MCP + ACP)

**Frontend:** pywebview/WebView2 (native desktop GUI)

**Architecture:** Desktop-first, not a browser dashboard. Local-first, zero cloud costs.

---

## Project Name: ItemForge

| Attribute | Choice |
|-----------|--------|
| **Project name** | ItemForge |
| **Repository** | Z:\Projects\ModelMistress (sub-project) |
| **Binary** | `itemforge` |
| **API base** | `/v1/forge` |
| **Philosophy** | "Forge items into listings" — creating, documenting, and publishing in one place |

## Architecture Overview

```
┌──────────────────────────────────────────────────────────────┐
│                    Desktop GUI (pywebview/WebView2)           │
│  Item Tree │ Photo Preview │ AI Research │ Docs │ Listings   │
└──────────────────────────────────────────────────────────────┘
                              │  Local REST API (JSON)
┌──────────────────────────────────────────────────────────────┐
│              FastAPI Backend (Python)                         │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────────────┐ │
│  │ Ingestion    │ │ Research     │ │ Documentation      │ │
│  │ Service      │ │ Service      │ │ Generator          │ │
│  └──────────────┘ └──────────────┘ └──────────────────────┘ │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────────────┐ │
│  │ Listing      │ │ Schedule/Auto│ │ Platform Integrations│ │
│  │ Writer       │ │ Engine       │ │ (FB, IG, eBay)       │ │
│  └──────────────┘ └──────────────┘ └──────────────────────┘ │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────────────┐ │
│  │ AI Gateway   │ │ Media Repo   │ │ Storage / DB         │ │
│  │ (ModelMistress│ │ (organized   │ │ (SQLite + Vector DB) │ │
│  │  router)     │ │  photos)     │ │                      │ │
│  └──────────────┘ └──────────────┘ └──────────────────────┘ │
└──────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        │                     │                     │
┌───────▼────────┐    ┌────────▼────────┐    ┌────────▼────────┐
│ ModelMistress  │    │ Local Models    │    │ Web Search      │
│ Hub (Rust)     │    │ (llama.cpp)     │    │ (DuckDuckGo)    │
│ /v1 chat, vision│   │ Qwen3.5-9B      │    │ SerpAPI/DDG     │
│ /v1/vision/*   │    │ Image gen, TTS  │    │                 │
│ /v1/vision/*   │    │ Embeddings,     │    │                 │
│ /v1/km/*       │    │ reranking       │    │                 │
└────────────────┘    └────────────────┘    └─────────────────┘
```

---

## Four Interface Types (API + CLI + MCP + ACP)

### 1️⃣ API (HTTP REST — OpenAI-compatible)

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/forge/ingest` | POST | Upload photos / add folder |
| `/v1/forge/items` | GET | List all items with thumbnails |
| `/v1/forge/items/{id}` | GET | Get product entity |
| `/v1/forge/items/{id}/research` | POST | Run AI research (ModelMistress chat) |
| `/v1/forge/items/{id}/docs` | POST | Generate documentation |
| `/v1/forge/items/{id}/listings` | POST | Generate multi-platform listings |
| `/v1/forge/listings` | GET | List generated listings |
| `/v1/forge/listings/{id}/publish` | POST | Approve & publish to platform |
| `/v1/forge/status` | GET | Health check (ModelMistress endpoint) |
| `/v1/forge/ai/status` | GET | ModelMistress connection status |

**Usage:**
```bash
# Import photos
curl -X POST http://127.0.0.1:8000/v1/forge/ingest \
  -H "Authorization: Bearer $TOKEN" \
  -F "folder=C:/Users/limpi/OneDrive/Documents/Sell"

# List items
curl http://127.0.0.1:8000/v1/forge/items \
  -H "Authorization: Bearer $TOKEN"
```

### 2️⃣ CLI (Command-Line Interface)

```bash
# Install the CLI
cargo build --release -p model-mistress --features bundle-engine
# The CLI is at: target/release/model-mistress

# Server (headless mode)
model-mistress serve --port 8000

# Chat with a model
model-mistress chat qwen3.5:9b "Research this product: {description}"

# List models
model-mistress models

# Pull a model
model-mistress pull hf.co/unsloth/Qwen3-8B-GGUF:Q4_K_M

# MCP over stdio
model-mistress mcp

# Vision (image recognition)
model-mistress vision resnet50 cat.jpg

# Speech-to-text
model-mistress transcribe audio.wav

# Text-to-speech
model-mistress speak "Hello" --out hello.wav

# Knowledge Modules
model-mistress km load ecommerce-best-practices
```

### 3️⃣ MCP (Model Context Protocol)

ModelMistress already implements MCP. The platform uses it for:
- Bidirectional agent communication
- Tool discovery and invocation
- Agent-to-agent control

**MCP tools provided:**
| Tool | Description |
|------|-------------|
| `list_models` | List available models |
| `chat` | Run a chat completion |
| `health` | Health check |
| `identify_item` | Identify item from photo |
| `research_item` | Generate research notes |
| `generate_documentation` | Generate markdown docs |
| `generate_facebook_listing` | Generate FB listing |
| `generate_platform_post` | Generate platform post |
| `list_products` | List products in catalog |
| `search_products` | Search products (RAG) |
| `get_product` | Get product by ID |

**MCP connection (from Hermes Agent):**
```python
from model_mistress.mcp import McpClient

client = McpClient()
client.connect_hermes("http://localhost:8000/mcp")
# ... use tools
```

### 4️⃣ ACP (Agent Client Protocol)

ACP is the protocol for agent-to-agent communication. ModelMistress implements a capability negotiation protocol for interoperable agents.

**ACP protocol flow:**
1. **Capability Negotiation** — Client and server exchange capabilities
2. **Health Check** — Server reports status, uptime, queue depth, GPU/memory utilization
3. **Model Registry** — Client can query available models, formats, quantization
4. **Tool Invocation** — Server invokes tools on client
5. **Message Routing** — Bidirectional message passing

**ACP endpoint:**
```
http://localhost:8000/api/acp
```

**ACP message format:**
```json
{
  "protocol_version": { "major": 1, "minor": 0, "patch": 0 },
  "client_capabilities": ["streaming", "tool_calling", "vision", "audio"],
  "server_capabilities": ["chat", "vision", "embeddings", "rerank", "knowledge_modules"],
  "negotiated": ["chat", "vision", "embeddings", "knowledge_modules"]
}
```

**ACP usage:**
```python
import requests

# Health check
resp = requests.get("http://localhost:8000/api/acp/health")
print(resp.json())

# Capability negotiation
resp = requests.post("http://localhost:8000/api/acp/negotiate", json={
    "client_capabilities": ["streaming", "tool_calling", "vision"]
})
print(resp.json())
```

---

## ModelMistress Integration Points

| Platform Feature | ModelMistress Endpoint |
|------------------|------------------------|
| **Photo ID** | `POST /v1/vision/classify` |
| **Item research** | `POST /v1/chat/completions` |
| **Documentation** | `POST /v1/chat/completions` |
| **Facebook listing** | `POST /v1/chat/completions` |
| **Product search** | `POST /v1/embeddings` |
| **Smart search** | `POST /v1/rerank` |
| **Voice input** | `POST /v1/audio/transcriptions` |
| **Voice output** | `POST /v1/audio/speech` |
| **Knowledge base** | `/v1/km/...` |
| **Observability** | `/v1/native/traces` |

---

## Getting Started

### Prerequisites
- Rust toolchain (cargo)
- Windows 10+ (with RX 7900 XTTX or similar GPU)
- RAM: 16GB+
- NVRAM: 8GB+ (for Qwen3.5-9B)

### Quick Start

```bash
# 1. Build ModelMistress
cargo build --release -p model-mistress --features bundled-engine

# 2. Serve
model-mistress serve

# 3. Pull models
model-mistress pull hf.co/unsloth/Qwen3-8B-GGUF:Q4_K_M
model-mistress pull hf.co/nomic-ai/nomic-embed-text-v1.5-Q5_0

# 4. Install vision models
# Copy .onnx + .json sidecar into <home>/vision/

# 5. Start the ItemForge platform (FastAPI)
cd backend
uvicorn app.main:app --reload
```

### Desktop App

```bash
# The desktop window (PySide6)
python -m gui.app
```

---

## API Reference

### Ingest

**POST** `/v1/forge/ingest`

Upload photos or add a folder.

```bash
curl -X POST http://127.0.0.1:8000/v1/forge/ingest \
  -H "Authorization: Bearer $TOKEN" \
  -F "folder=C:/Users/limpi/OneDrive/Documents/Sell"
```

**Response:**
```json
{
  "folders_created": 12,
  "images_processed": 63,
  "duplicates_removed": 3,
  "items_created": 2,
  "errors": []
}
```

### Items

**GET** `/v1/forge/items`

List all items with thumbnails.

```bash
curl http://127.0.0.1:8000/v1/forge/items \
  -H "Authorization: Bearer $TOKEN"
```

### Research

**POST** `/v1/forge/items/{id}/research`

Run AI research on an item.

```bash
curl -X POST http://127.0.0.1:8000/v1/forge/items/{id}/research \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"depth": "deep", "include_web": true}'
```

### Documentation

**POST** `/v1/forge/items/{id}/docs`

Generate documentation.

```bash
curl -X POST http://127.0.0.1:8000/v1/forge/items/{id}/docs \
  -H "Authorization: Bearer $TOKEN"
```

### Listings

**POST** `/v1/forge/items/{id}/listings`

Generate multi-platform listings.

```bash
curl -X POST http://127.0.0.1:8000/v1/forge/items/{id}/listings \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"platforms": ["facebook", "instagram", "ebay", "etsy"]}'
```

### Listings Publish

**POST** `/v1/forge/listings/{id}/publish`

Approve and publish a listing.

```bash
curl -X POST http://127.0.0.1:8000/v1/forge/listings/{id}/publish \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"platform": "facebook", "title": "Optimized title", "description": "Optimized description"}'
```

### AI Status

**GET** `/v1/forge/ai/status`

Check ModelMistress connection and loaded models.

```bash
curl http://127.0.0.1:8000/v1/forge/ai/status \
  -H "Authorization: Bearer $TOKEN"
```

---

## Directory Structure

```
Z:\Projects\ModelMistress\
├── itemforge/                    # New: ItemForge platform
│   ├── app/                      # FastAPI backend
│   │   ├── __init__.py
│   │   ├── main.py               # FastAPI app entry
│   │   ├── config.py             # Settings
│   │   ├── models.py             # Pydantic schemas
│   │   ├── database.py           # SQLite/PostgreSQL
│   │   ├── services/
│   │   │   ├── __init__.py
│   │   │   ├── ingestion.py      # Photo organizer
│   │   │   ├── research.py       # AI research
│   │   │   ├── docs.py           # Documentation generator
│   │   │   ├── listings.py       # Listing writer
│   │   │   └── ai_gateway.py     # ModelMistress router
│   │   ├── routes/
│   │   │   ├── __init__.py
│   │   │   ├── items.py
│   │   │   ├── listings.py
│   │   │   └── status.py
│   │   └── ai/
│   │       ├── client.py         # ModelMistress HTTP client
│   │       ├── rag.py            # Embedding + vector search
│   │       └── km.py             # Knowledge Module loader
│   ├── backend/                  # Existing FastAPI backend
│   ├── gui/                      # PySide6 desktop window
│   ├── cli/                      # CLI tool
│   ├── frontend/                 # Vue.js dashboard
│   ├── docs/
│   │   ├── acp.md               # ACP protocol documentation
│   │   ├── api.md               # API reference
│   │   ├── claude.md            # Claude integration
│   │   └── platform-design.md   # Full architecture
│   └── README.md                # This file
├── src/                          # ModelMistress core
├── engines/                      # llama.cpp, etc.
├── gui/                          # Python desktop window
├── cli/                          # CLI tool
├── frontend/                     # Vue.js dashboard
├── docs/                         # ModelMistress docs
└── skills/                       # Skills directory (includes e-commerce-photo-workflow)
```

---

## Product Schema

```json
{
  "id": "uuid",
  "title": "Vintage Stemmed Wine Glasses – Set of 6",
  "type": "glassware",
  "subtype": "stemmed_glass",
  "brand": "Unknown (vintage)",
  "colors": ["amber/orange", "purple", "green", "yellow", "clear"],
  "material": "glass",
  "condition": "used, good",
  "features": ["wide bowl", "slender stem", "multicolor set of 6"],
  "dimensions_cm": null,
  "price_range": "$8–$15",
  "source_images": ["images/001_main.jpg"],
  "research_notes": "Vintage mid-20th century glassware...",
  "documentation": "...",
  "facebook_listing": "...",
  "created_at": "...",
  "updated_at": "..."
}
```

---

## Database Schema

```sql
CREATE TABLE products (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    type TEXT,
    subtype TEXT,
    brand TEXT,
    colors TEXT,
    material TEXT,
    condition TEXT,
    features TEXT,
    dimensions_cm TEXT,
    price_range TEXT,
    research_notes TEXT,
    documentation TEXT,
    facebook_listing TEXT,
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);

CREATE TABLE images (
    id TEXT PRIMARY KEY,
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    role TEXT NOT NULL,
    embedding BLOB,
    perceptual_hash TEXT,
    uploaded_at TEXT DEFAULT (datetime('now'))
);

CREATE TABLE listings (
    id TEXT PRIMARY KEY,
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE CASCADE,
    platform TEXT NOT NULL,
    title TEXT,
    description TEXT,
    price TEXT,
    status TEXT DEFAULT 'draft',
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);

CREATE TABLE ai_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    model_name TEXT NOT NULL,
    task_type TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    latency_ms INTEGER,
    tokens_prompt INTEGER,
    tokens_completion INTEGER,
    error TEXT,
    timestamp TEXT DEFAULT (datetime('now'))
);
```

---

## Accessibility

- ARIA labels on all interactive elements
- Keyboard navigation (Tab, Enter, Escape, Ctrl+1-4)
- Screen reader-friendly labels
- High-contrast theme toggle
- Screen reader-friendly error messages
- Voice input (via ModelMistress speech STT)
- Voice output (via ModelMistress text-to-speech)

---

## Troubleshooting

| Issue | Solution |
|-------|----------|
| ModelMistress not connecting | Check `<home>/control.json` for URL and token |
| Vision not working | Install ONNX models in `<home>/vision/` |
| Embeddings not working | Pull `nomic-embed-text-v1.5` |
| Quota exceeded | Reset via `model-mistress keys rotate` |
| Rate limited | Use `model-mistress schedule` to queue requests |
| CLI not found | Run `cargo build --release -p model-mistress --features bundle-engine` |

---

## License

Apache 2.0 — Same as ModelMistress
