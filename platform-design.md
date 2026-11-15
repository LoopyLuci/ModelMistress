# All-in-One Item Listing & Sales Platform
## With ModelMistress Integration
### Architecture, Design, Implementation Guide

**Target user:** Individual product seller/reseller with a catalog of physical items who wants to rapidly organize photos, research products, create listings, and automate multi-platform posting — using local AI only.

**Core AI engine:** ModelMistress (local model hub — OpenAI-compatible + Ollama-compatible API, vision, embeddings, speech, knowledge modules).

**Platform type:** Desktop-first (pywebview/WebView2 or Tauri shell + FastAPI backend). Not a browser dashboard.

**Guiding principles:**
- Local-first: all processing on your machine (RX 7900 XTTX GPU, Windows 10)
- Zero cloud costs: everything runs locally via ModelMistress
- Modular, composable architecture: swap backends, platforms, themes at runtime
- Production-grade: robust error handling, retry logic, logging, state persistence

---

## 1. Executive Summary

ModelMistress is the ideal AI backbone for this platform. It provides:
- **One local API endpoint** (`/v1`) — chat, completions, embeddings, rerank, vision, speech (STT + TTS)
- **Bundled llama.cpp** — no external dependencies, just the GPU driver
- **ONNX vision** — image classification, object detection, embeddings (for product identification)
- **Knowledge Modules (KM)** — versioned, signed, budget-managed domain knowledge (e-commerce, product categories)
- **Observability** — traces, tokens, resource usage
- **Fast, local, private** — everything stays on your machine

The platform architecture:
```
┌──────────────────────────────────────────────────────────────┐
│                    Desktop GUI (pywebview/WebView2)           │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐ ┌─────────┐│
│  │ Item Tree   │ │ Photo Preview│ │ AI Research │ │ Settings││
│  └─────────────┘ └─────────────┘ └─────────────┘ └─────────┘│
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
│  │ (ModelMistress│ │ (organized  │ │ (SQLite + Vector DB) │ │
│  │  router)     │ │  photos)     │ │                      │ │
│  └──────────────┘ └──────────────┘ └──────────────────────┘ │
└──────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        │                     │                     │
┌───────▼────────┐    ┌────────▼────────┐    ┌────────▼────────┐
│ Local Models   │    │ ModelMistress   │    │ Knowledge       │
│ (llama.cpp)    │    │ Hub (Rust)      │    │ Modules (KM)    │
│ Qwen3.5-9B     │    │ /v1 API         │    │ e-commerce,     │
│ Vision (ONNX)  │    │ /v1/vision/*    │    │ product research│
│ Embeddings     │    │ /v1/embeddings  │    │                 │
│ (nomic-embed)  │    │ /v1/rerank      │    │                 │
└────────────────┘    └────────────────┘    └─────────────────┘
```

---

## 2. Why ModelMistress is the Perfect AI Backend

### 2.1 ModelMistress Capabilities (Relevant to This Platform)

| Capability | ModelMistress Endpoint | Use in Platform |
|------------|------------------------|-----------------|
| **Image recognition** | `POST /v1/vision/classify` | Auto-identify items in photos (e.g., "wine glasses", "steel pail") |
| **Object detection** | `POST /v1/vision/detect` | Detect product regions for cropping/zooming |
| **Image embeddings** | `POST /v1/vision/embed` | RAG matching, duplicate detection, search |
| **Text chat** | `POST /v1/chat/completions` | Research items, generate descriptions, answer questions |
| **Text completions** | `POST /v1/completions` | Draft listing content, keywords |
| **Embeddings** | `POST /v1/embeddings` | Product catalog search, similarity matching |
| **Reranking** | `POST /v1/rerank` | Improve search relevance for similar products |
| **Speech-to-text** | `POST /v1/audio/transcriptions` | Voice input for quick notes, accessibility |
| **Text-to-speech** | `POST /v1/audio/speech` | Audio descriptions, accessibility output |
| **Knowledge Modules** | `/v1/km/...` | Domain expertise (e-commerce best practices, product categories) |
| **Observability** | `/v1/native/traces` | Debug AI responses, track costs, optimize prompts |

### 2.2 ModelMistress Advantages

| Attribute | Benefit |
|-----------|---------|
| **Zero cloud cost** | All models run locally on your GPU |
| **Privacy** | No images or data leave your machine |
| **Offline capable** | Works without internet after first model load |
| **Fast** | Qwen3.5-9B: 79 tokens/s on RX 7900 XTTX |
| **Single binary** | Bundled llama.cpp, nothing to install |
| **OpenAI-compatible** | Drop-in replacement for any OpenAI API client |

### 2.3 ModelMistress Limitations (and Mitigations)

| Limitation | Mitigation |
|------------|------------|
| No built-in web search | Use free web search API (DuckDuckGo/SerpAPI) as fallback |
| No built-in price API | Use LLM-based price estimation + marketplace data |
| Vision models are CPU-based | Use ONNX CPU for identification; use VLM for detailed analysis |
| No batch prompt jobs | Queue via Python asyncio; process sequentially or in parallel |
| Vision CPU-only | Mix: use vision for initial ID, VLM for detailed analysis |

---

## 3. System Architecture

### 3.1 High-Level Component Diagram

```
┌──────────────────────────────────────────────────────────────┐
│                    Desktop GUI (pywebview/WebView2)           │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐ ┌─────────┐│
│  │ Item Tree   │ │ Photo Preview│ │ AI Research │ │ Settings││
│  └─────────────┘ └─────────────┘ └─────────────┘ └─────────┘│
└──────────────────────────────────────────────────────────────┘
                              │  Local REST (JSON)
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

### 3.2 Component Breakdown

| Component | Technology | Role |
|-----------|------------|------|
| **GUI** | pywebview + WebView2 | Native desktop window, vanilla JS frontend |
| **Backend** | FastAPI + Uvicorn | REST API, async processing, admin UI |
| **Database** | SQLite → PostgreSQL | Product catalog, listings, AI logs |
| **Vector DB** | Qdrant or ChromaDB | RAG for product catalog and research |
| **AI Gateway** | ModelMistress API + Python client | Route to local models, fallback, caching |
| **Image Processor** | OpenCV + Pillow | Photo analysis, thumbnails, clustering |
| **OCR** | Tesseract | Read labels, product names from images |
| **Embedding Model** | nomic-embed-text (via ModelMistress) | Product catalog embeddings |
| **Vision Model** | ONNX (via ModelMistress vision) | Product identification |

---

## 4. AI Gateway: ModelMistress Integration

### 4.1 Connection Configuration

ModelMistress runs on your machine as a local hub. The platform connects to it:

```python
# config.yaml
modelmistress:
  url: "http://127.0.0.1:12345"  # From control.json or --port
  token: "secret-from-control.json"
  timeout: 60  # seconds (max for local)
  max_retries: 3
  backoff: 2.0  # seconds

# In config.toml of ModelMistress
[serving]
    max_loaded = 2  # Chat + embedder held locally
    idle_unload_s = 300  # Unload after 5 min idle
```

### 4.2 AI Gateway (Router) Design

```python
class AIGateway:
    """Routes requests between local models, with fallback and caching."""

    def __init__(self, config):
        self.config = config
        self.session = aiohttp.ClientSession()
        self.cache = LRUCache(maxsize=50, ttl=300)
        self.circuit_breaker = CircuitBreaker(failures=5, reset_timeout=60)

    async def identify_item(self, image_bytes: bytes) -> dict:
        """Identify an item from a photo using vision."""
        cache_key = self._make_cache_key(image_bytes, "identify_item")
        if cached := self.cache.get(cache_key):
            return cached

        async with self.circuit_breaker:
            # Try local vision model first
            result = await self._vision_classify(image_bytes)
            if result:
                self.cache.set(cache_key, result)
                return result

            # Fallback: use chat model to describe
            result = await self._chat_analyze(image_bytes)
            self.cache.set(cache_key, result)
            return result

    async def research_item(self, item_data: dict) -> dict:
        """Generate research using chat/completions."""
        prompt = self._build_research_prompt(item_data)
        result = await self._chat_complete(prompt)
        return result

    async def generate_documentation(self, item_data: dict) -> dict:
        """Generate markdown documentation."""
        prompt = self._build_doc_prompt(item_data)
        result = await self._chat_complete(prompt)
        return result

    async def generate_facebook_listing(self, item_data: dict) -> dict:
        """Generate Facebook Marketplace listing."""
        prompt = self._build_fb_listing_prompt(item_data)
        result = await self._chat_complete(prompt)
        return result
```

### 4.3 Model Routing Table

| Task | Preferred Model | Fallback | Backup |
|------|-------------------|----------|--------|
| **Image identification** | Vision ONNX (ResNet-50, CLIP) | Chat Qwen3.5-9B | Chat Qwen3.5-7B |
| **Item research** | Qwen3.5-9B (Q4_K_M) | Qwen3.5-7B | Qwen3-3B |
| **Documentation generation** | Qwen3.5-9B | Qwen3.5-7B | Qwen3-3B |
| **Facebook listing generation** | Qwen3.5-9B | Qwen3.5-7B | Qwen3-3B |
| **Embeddings** | nomic-embed-text (via ModelMistress) | sentence-transformers | — |
| **OCR/labels** | Tesseract (local) | — | — |
| **Text-to-speech** | llama-tts (via ModelMistress) | — | — |
| **Web search** | DuckDuckGo (free) | SerpAPI (paid) | — |

### 4.4 Caching Strategy

| Cache Type | Capacity | TTL | Purpose |
|------------|----------|-----|---------|
| **LRU** | 50 entries | 5 min | Repeated image analysis, prompts |
| **Embedding** | 1000 entries | 24 hr | Product catalog search |
| **Vision embeddings** | 500 entries | 7 days | Similar product matching |

---

## 5. Core Modules (Integration with ModelMistress)

### 5.1 Photo Ingestion & Organization Module

**Responsibilities:**
- Accept photos (drag-drop, file dialog, folder watch)
- Auto-rename with timestamps: `YYYYMMDD_HHMMSS_<original>`
- Detect and skip duplicates (perceptual hash via CLIP embedding)
- Cluster similar images (same item) using CLIP embeddings
- Move photos into item-specific folders: `Sell/<Item Name>/images/`
- Generate thumbnails for the UI

**ModelMistress Integration:**
```python
async def extract_image_embedding(self, image_path: str) -> list[float]:
    """Extract CLIP embedding via ModelMistress vision embed."""
    data = {"image": f"data:image/jpeg;base64,{base64_encode(image_path)}"}
    async with self.session.post(
        f"{self.config['modelmistress']['url']}/v1/vision/embed",
        headers={"Authorization": f"Bearer {self.config['modelmistress']['token']}"},
        json=data
    ) as resp:
        result = await resp.json()
        return result["embedding"]
```

**Folder structure (post-ingestion):**
```
Sell/
├── Items/
│   ├── 01_Military_Pail/
│   │   ├── 001_main.jpg
│   │   └── 002_detail.jpg
│   ├── 02_Vintage_Wine_Glasses/
│   │   ├── 001_main.jpg
│   │   ├── 002_detail.jpg
│   │   └── 003_detail_bowl.jpg
│   └── ...
├── Documentation/
├── Platform_Posts/
│   ├── facebook/
│   └── instagram/
├── assets/
├── logs/
└── config.yaml
```

### 5.2 Product Research & Cataloging Module

**Responsibilities:**
- Send each product image to vision model for initial identification
- Use chat model to extract structured metadata
- Use RAG to search existing catalog for similar items
- Enrich with web search (optional)

**Entity schema (example):**
```json
{
  "item_id": "uuid",
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
  "research_notes": "Vintage mid-20th century glassware, no brand marking visible. Commonly found in amber, purple, and clear variants. Similar pieces sell $8–$15 on FB Marketplace.",
  "created_at": "...",
  "updated_at": "..."
}
```

### 5.3 Documentation Generator Module

**ModelMistress Integration:**
```python
async def generate_documentation(self, item_data: dict) -> str:
    prompt = f"""
    Generate a comprehensive markdown documentation for this product:

    Title: {item_data['title']}
    Type: {item_data['type']}
    Brand: {item_data['brand']}
    Colors: {', '.join(item_data['colors'])}
    Material: {item_data['material']}
    Condition: {item_data['condition']}
    Features: {', '.join(item_data['features'])}

    Include:
    - Overview (3-4 sentences)
    - Key Features (bullet list)
    - Condition Details
    - Specifications table
    - Research Notes
    - Photo Captions
    - Suggested Price
    - SEO Tags
    """
    response = await self._chat_complete(prompt)
    return response["choices"][0]["message"]["content"]
```

### 5.4 Multi-Platform Listing Generator Module

**ModelMistress Integration:**
```python
async def generate_facebook_listing(self, item_data: dict) -> dict:
    prompt = f"""
    Generate a Facebook Marketplace listing for this product:

    Title: {item_data['title']}
    Brand: {item_data['brand']}
    Colors: {', '.join(item_data['colors'])}
    Material: {item_data['material']}
    Condition: {item_data['condition']}
    Features: {', '.join(item_data['features'])}

    Generate:
    1. Optimized title (65 chars max, SEO keywords)
    2. Detailed description (uses common buyer search terms)
    3. Product details table
    4. 3-5 photo captions
    5. Suggested price with rationale
    6. Shipping info
    """
    response = await self._chat_complete(prompt)
    return response["choices"][0]["message"]["content"]
```

### 5.5 Knowledge Modules (Domain Expertise)

**ModelMistress KMs** provide domain-specific knowledge:
- **E-commerce best practices** — listing optimization, pricing strategies
- **Product categories** — glassware, tools, furniture, etc.
- **Platform rules** — Facebook Marketplace, Etsy, eBay requirements

```python
# Load e-commerce KM for enhanced research
async def load_ecommerce_knowledge(self):
    await self._km_load("ecommerce-best-practices")

# Use KM as context in prompts
prompt = f"""
E-commerce expert knowledge (loaded from KM):
{await self._load_knowledge_module('ecommerce-best-practices')}

Product data:
{item_data}

Based on the e-commerce knowledge above, generate...
"""
```

---

## 6. Data Model

### 6.1 Database Schema

```sql
-- Products (catalog entities)
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
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);

-- Images (product photos)
CREATE TABLE images (
    id TEXT PRIMARY KEY,
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    role TEXT NOT NULL,  -- 'main', 'detail', 'alt'
    embedding BLOB,  -- serialized vector for similarity search
    perceptual_hash TEXT,
    uploaded_at TEXT DEFAULT (datetime('now'))
);

-- Listings (per platform)
CREATE TABLE listings (
    id TEXT PRIMARY KEY,
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE CASCADE,
    platform TEXT NOT NULL,  -- 'facebook', 'instagram', 'ebay', 'etsy'
    title TEXT,
    description TEXT,
    price TEXT,
    status TEXT DEFAULT 'draft',  -- 'draft', 'published', 'failed'
    published_at TEXT,
    error_log TEXT,
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);

-- AI call logs (for debugging & cost tracking)
CREATE TABLE ai_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    model_name TEXT NOT NULL,
    task_type TEXT NOT NULL,  -- 'identify', 'research', 'doc', 'listing', 'embedding'
    endpoint TEXT NOT NULL,  -- '/v1/chat/completions', '/v1/vision/classify', etc.
    prompt_hash TEXT,
    response_hash TEXT,
    latency_ms INTEGER,
    tokens_prompt INTEGER,
    tokens_completion INTEGER,
    error TEXT,
    timestamp TEXT DEFAULT (datetime('now'))
);

-- Knowledge modules
CREATE TABLE knowledge_modules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    version TEXT,
    source TEXT,  -- 'local', 'github', 'manual'
    loaded_at TEXT,
    last_used_at TEXT
);
```

---

## 7. API Design (Backend Endpoints)

| Method | Endpoint | Description |
|--------|----------|-------------|
| `POST` | `/api/ingest` | Upload photos / add folder |
| `GET` | `/api/items` | List all items with thumbnails |
| `GET` | `/api/items/{id}` | Get product entity |
| `POST` | `/api/items/{id}/research` | Run AI research (uses ModelMistress chat) |
| `GET` | `/api/items/{id}/docs` | Get generated documentation |
| `POST` | `/api/items/{id}/docs` | Generate documentation (uses ModelMistress) |
| `POST` | `/api/items/{id}/listings` | Generate multi-platform listings |
| `GET` | `/api/listings` | List generated listings |
| `POST` | `/api/listings/{id}/publish` | Approve & publish to platform |
| `GET` | `/api/status` | Health check (ModelMistress endpoint) |
| `POST` | `/api/import` | Bulk import from folder |
| `GET` | `/api/ai/status` | ModelMistress connection status, loaded models |

---

## 8. Desktop GUI (Vanilla JS + HTML/CSS)

### 8.1 Layout

```
+--------------------------------------------------------------------+
|  Header: App name | AI status (ModelMistress: connected) | Settings|
+--------+-----------------------------------------------------------+
|        |  Sidebar: Item tree (folder items)  |  Preview pane       |
|  Nav   |  - 01 Military Pail (1)            |  [main image]       |
|  tabs  |  - 02 Vintage Wine Glasses (3)     |  - detail 1         |
|        |  - 03 ...                        |  - detail 2         |
|        |                                  |  AI Research panel  |
|        |                                  |  [Generate research] |
|        |                                  |  → "This is a..."  |
|        |                                  |  AI Documentation   |
|        |                                  |  [Generate docs]   |
|        |                                  |  FB Listing         |
|        |                                  |  [Generate FB]     |
|        |                                  |  AI Chat (input)    |
|        |                                  |  "Tell me more..."  |
+--------+----------------------------------+---------------------+
|  Footer: status bar, logs, ModelMistress connected | publish queue |
+--------------------------------------------------------------------+
```

### 8.2 Accessibility
- ARIA labels on all interactive elements
- Keyboard navigation (Tab, Enter, Escape, Ctrl+1-4)
- Screen reader-friendly labels
- High-contrast theme toggle
- Screen reader-friendly error messages

---

## 9. Project Directory Structure

```
item-listing-platform/
├── backend/
│   ├── app/
│   │   ├── __init__.py
│   │   ├── main.py                 # FastAPI app entry
│   │   ├── config.py               # Settings (modelmistress URL, token)
│   │   ├── models.py               # Pydantic schemas
│   │   ├── database.py             # SQLite/PostgreSQL
│   │   ├── services/
│   │   │   ├── __init__.py
│   │   │   ├── ingestion.py        # Photo organizer (OpenCV, PIL)
│   │   │   ├── research.py         # AI research (ModelMistress client)
│   │   │   ├── docs.py             # Documentation generator
│   │   │   ├── listings.py         # Listing writer (FB, IG, eBay)
│   │   │   └── ai_gateway.py       # ModelMistress router + caching + CB
│   │   ├── routes/
│   │   │   ├── __init__.py
│   │   │   ├── items.py
│   │   │   ├── listings.py
│   │   │   └── status.py
│   │   └── ai/
│   │       ├── client.py           # ModelMistress HTTP client
│   │       ├── rag.py              # Embedding + vector search
│   │       └── km.py               # Knowledge Module loader
│   ├── requirements.txt
│   └── README.md
├── frontend/
│   ├── index.html
│   ├── app.js
│   ├── styles.css
│   └── icons/ (SVG icons)
├── modelmistress/
│   ├── README.md                   # ModelMistress setup guide
│   ├── config.example.toml         # ModelMistress config
│   └── models/                     # Pulled models (Qwen3.5-9B, CLIP, nomic-embed)
├── docs/
│   ├── architecture.md
│   └── configuration.md
├── config.example.yaml
└── README.md
```

---

## 10. Implementation Roadmap

### Phase 1: Core Scaffolding (2-3 weeks)
- [ ] FastAPI backend skeleton
- [ ] ModelMistress client connection
- [ ] pywebview desktop app shell
- [ ] Basic folder ingestion (copy photos to `Items/`)
- [ ] SQLite database + CRUD
- [ ] Simple UI (item list, preview)

### Phase 2: AI Integration (3-4 weeks)
- [ ] ModelMistress connection (chat, vision, embeddings)
- [ ] Image analysis service (vision classify + chat)
- [ ] Entity generation (LLM)
- [ ] Documentation generator (LLM)
- [ ] Listing generator (FB template + LLM)

### Phase 3: Automation & Polish (2-3 weeks)
- [ ] Scheduling engine
- [ ] Circuit breaker + retry (for flaky local models)
- [ ] Multi-platform export (CSV for manual upload)
- [ ] Batch processing (bulk import)
- [ ] Theme/toggle (dark/light)
- [ ] Knowledge Modules (e-commerce best practices)

### Phase 4: Advanced (future)
- [ ] Real platform APIs (FB Marketplace, eBay via OAuth)
- [ ] Speech-to-text input (whisper via ModelMistress)
- [ ] Text-to-speech output (accessibility)
- [ ] Browser automation for FB/IG posting
- [ ] Cost tracking & optimization
- [ ] Multi-user support & authentication

---

## 11. ModelMistress Setup & Configuration

### 11.1 Quick Start

```bash
# 1. Clone and build
git submodule update --init --recursive engines/llama.cpp
cargo build --release -p model-mistress --features bundled-engine

# 2. Serve (foreground)
model-mistress serve

# 3. Pull a chat model
model-mistress pull hf.co/unsloth/Qwen3-8B-GGUF:Q4_K_M

# 4. Pull a vision model (for image recognition)
mkdir -p <home>/vision
copy resnet50.onnx <home>/vision/
copy resnet50.json <home>/vision/

# 5. Pull an embedding model
model-mistress pull hf.co/nomic-ai/nomic-embed-text-v1.5-Q5_0
```

### 11.2 Recommended Models for This Platform

| Model | Size | Purpose | VRAM |
|-------|------|---------|------|
| Qwen3.5-9B (Q4_K_M) | 5.1 GB | Chat, research, docs, listings | 8 GB |
| Qwen3.5-7B (Q4_K_M) | 4.6 GB | Fallback chat | 8 GB |
| Qwen3-3B (Q4_K_M) | 2.0 GB | Emergency fallback | 4 GB |
| nomic-embed-text-v1.5-Q5_0 | 0.6 GB | Embeddings | 1 GB |
| CLIP-ViT-B-32 (ONNX) | 620 MB | Vision embeddings | 1 GB |
| ResNet-50 (ONNX) | 100 MB | Image classification | 1 GB |

### 11.3 ModelMistress Config (config.toml)

```toml
[serving]
    max_loaded = 2
    idle_unload_s = 300
    flash_attn = "on"
    fail_fast_busy = true

[profiles."qwen3.5:9b"]
    parallel = 1
    ctx_size = 32768
    kv_unified = true
    cache_type_k = "q8_0"
    cache_type_v = "q8_0"
    flash_attn = "on"
    cache_prompt = true

[profiles."nomic-embed-text-v1.5"]
    parallel = 1
    ctx_size = 512
    gpu_layers = 0

[vision]
    vision_dir = "<home>/vision"
```

### 11.4 Platform Connection

```python
# The platform connects to ModelMistress via config
modelmistress = {
    "url": "http://127.0.0.1:12345",  # From control.json
    "token": "...",  # From control.json
    "timeout": 60,  # seconds
}
```

---

## 12. Performance & Scalability

| Component | Strategy |
|-----------|----------|
| **Image processing** | OpenCV + Pillow; batch processing; GPU acceleration |
| **AI inference** | ModelMistress serves local models via llama.cpp; GPU if available |
| **Caching** | LRU (50 entries, 5-min TTL); embedding cache (1000 entries, 24-hr) |
| **Circuit breaker** | 5 failures → 60s open → retry |
| **Rate limiting** | Per-model concurrency limits (ModelMistress handles this) |
| **Vector DB** | Qdrant/ChromaDB; HNSW index for fast similarity search |
| **Database** | SQLite for dev; PostgreSQL for prod (connection pooling) |

---

## 13. Security & Privacy

- **Local-first:** All images and data stay on your machine
- **No external calls:** All AI processing happens on your local ModelMistress
- **HTTPS:** Use local HTTPS for the web server (self-signed cert for dev)
- **Secrets:** Store ModelMistress token in environment variables, not in code
- **Audit logging:** Log all AI calls (model, tokens, latency) for debugging and cost tracking
- **Data minimization:** Only store what's needed for selling; delete sensitive metadata

---

## 14. Integration Points (ModelMistress → Platform)

| Platform Feature | ModelMistress Endpoint | Data Flow |
|------------------|------------------------|-----------|
| **Photo ID** | `POST /v1/vision/classify` | Photo → Vision model → Item type |
| **Item research** | `POST /v1/chat/completions` | Item metadata → Chat model → Research notes |
| **Documentation** | `POST /v1/chat/completions` | Item metadata → Chat model → Markdown |
| **Facebook listing** | `POST /v1/chat/completions` | Item metadata → Chat model → FB listing |
| **Product search** | `POST /v1/embeddings` | Embedding → Vector DB → Similarity |
| **Smart search** | `POST /v1/rerank` | Query → Reranker → Best matches |
| **Voice input** | `POST /v1/audio/transcriptions` | Audio → Whisper → Text |
| **Voice output** | `POST /v1/audio/speech` | Text → llama-tts → WAV/MP3 |
| **Knowledge base** | `/v1/km/...` | Load KM → Use in prompts |
| **Observability** | `/v1/native/traces` | Debug AI responses, track tokens |

---

## 15. Recommended Implementation Steps

### Pre-Development (Week 0)
1. Install ModelMistress: `cargo build --release -p model-mistress --features bundled-engine`
2. Pull models: `model-mistress pull hf.co/unsloth/Qwen3-8B-GGUF:Q4_K_M`
3. Set up vision: Copy ONNX models to `<home>/vision/`
4. Set up embedding: `model-mistress pull nomic-embed-text-v1.5`
5. Test connections: `curl http://127.0.0.1:<port>/v1/health`

### Development (Weeks 1-8)
1. **Phase 1:** Scaffold FastAPI + pywebview
2. **Phase 2:** Connect ModelMistress client
3. **Phase 3:** Build ingestion + photo organizer
4. **Phase 4:** Build AI research + documentation
5. **Phase 5:** Build multi-platform listing generator
6. **Phase 6:** Test, polish, and iterate

### Production (Week 8+)
1. Package as installer (pyinstaller + ModelMistress binary)
2. Create user documentation
3. Provide model download instructions
4. Set up backup/restore (config.toml, models/)

---

## 16. Gap Analysis: What's Missing (and How to Fill It)

| Gap | Impact | Mitigation |
|-----|--------|------------|
| No web search in ModelMistress | Item research limited to local knowledge | Use DuckDuckGo/SerpAPI for web enrichment |
| No price API in ModelMistress | Price suggestions based on LLM estimation | Use LLM knowledge + marketplace data mining |
| Vision CPU-only | Slower image identification | Use vision for quick ID, VLM for detailed analysis |
| No batch prompt jobs | Single-item processing only | Queue via Python asyncio; process sequentially |
| No model aliases | Long model names in prompts | Use presets (`model.create`) to create stable aliases |

---

## 17. Cost Analysis

| Component | Cost |
|-----------|------|
| ModelMistress hub | $0 (open source) |
| Qwen3.5-9B (GGUF) | $0 (download from HF) |
| Vision models (ONNX) | $0 (download from HF) |
| Embedding model | $0 |
| Database (SQLite) | $0 |
| Desktop GUI (pywebview) | $0 |
| Your GPU (RX 7900 XTTX) | $0 (already owned) |
| **Total** | **$0/month** |

---

## 18. Next Steps for the User

1. **Install ModelMistress:** Build from source, pull Qwen3.5-9B + vision models
2. **Start the hub:** `model-mistress serve`
3. **Run the platform:** `uvicorn backend.app.main:app --reload`
4. **Import Sell folder:** Use the ingestion module to organize photos
5. **Run AI research:** Use the research service to generate titles and descriptions
6. **Generate listings:** Create FB/IG content and review before publishing
7. **Automate:** Set up scheduling for future import + posting

---

*This document is a comprehensive architecture guide. Implementation is done in phases, starting with Phase 1 (scaffolding) and expanding with each phase.*
