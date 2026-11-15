# ItemForge API Reference

Complete HTTP REST API reference for the ItemForge platform.

---

## Base URL

```
http://localhost:8000
```

**Authorization:** `Bearer <token>` (from `<home>/control.json`)

---

## Endpoints

### 1. Ingest

#### `POST /v1/forge/ingest`

Upload photos or add a folder for processing.

**Request Body:**
```json
{
  "folder": "/path/to/photos",
  "mode": "auto",           // auto | manual
  "skip_duplicates": true,
  "tag_images": true
}
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

---

### 2. Items

#### `GET /v1/forge/items`

List all items with thumbnails.

**Response:**
```json
{
  "items": [
    {
      "id": "uuid",
      "title": "Vintage Stemmed Wine Glasses – Set of 6",
      "type": "glassware",
      "subtype": "stemmed_glass",
      "brand": "Unknown (vintage)",
      "colors": ["amber/orange", "purple", "green", "yellow", "clear"],
      "material": "glass",
      "condition": "used, good",
      "source_images": 3,
      "thumbnails": ["items/02_Vintage_Wine_Glasses/001_main.jpg"],
      "created_at": "2026-11-14T16:21:00Z"
    }
  ]
}
```

---

### 3. Research

#### `POST /v1/forge/items/{id}/research`

Run AI research on an item.

**Request Body:**
```json
{
  "depth": "deep",          // shallow | deep | exhaustive
  "include_web": false,
  "include_knowledge_modules": true
}
```

**Response:**
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
  "research_notes": "Vintage mid-20th century glassware...",
  "created_at": "2026-11-14T16:21:00Z"
}
```

---

### 4. Documentation

#### `POST /v1/forge/items/{id}/docs`

Generate documentation.

**Response:**
```markdown
# Vintage Stemmed Wine Glasses – Set of 6

## Overview
...

## Key Features
- ...

## Condition Details
...

## Specifications
| Property | Value |
|----------|-------|
| Type | glassware |
| Material | glass |
| Colors | amber, purple, green, yellow, clear |

## Research Notes
...

## Photo Captions
- 001_main.jpg: Main view of the set
- 002_detail.jpg: Detail of the bowl
- 003_detail_bowl.jpg: Detail of the bowl interior

## Suggested Price
$8–$15

## SEO Tags
vintage, glassware, wine glasses, set of 6, mid-century
```

---

### 5. Listings

#### `POST /v1/forge/items/{id}/listings`

Generate multi-platform listings.

**Request Body:**
```json
{
  "platforms": ["facebook", "instagram", "ebay", "etsy"],
  "title_override": null,
  "description_override": null
}
```

**Response:**
```json
{
  "id": "listing-uuid",
  "product_id": "product-uuid",
  "platforms": [
    {
      "platform": "facebook",
      "title": "Vintage Wine Glasses – Set of 6 | Mid-Century Glassware",
      "description": "Full description..."
    },
    {
      "platform": "instagram",
      "title": "Vintage Wine Glasses (Set of 6)",
      "description": "Full description..."
    }
  ],
  "created_at": "2026-11-14T16:21:00Z"
}
```

---

### 6. Listings (List)

#### `GET /v1/forge/listings`

List all generated listings.

**Response:**
```json
{
  "listings": [
    {
      "id": "listing-uuid",
      "product_id": "product-uuid",
      "platform": "facebook",
      "title": "Vintage Wine Glasses – Set of 6 | Mid-Century Glassware",
      "price": "$8–$15",
      "status": "draft",
      "created_at": "2026-11-14T16:21:00Z"
    }
  ]
}
```

---

### 7. Publish

#### `POST /v1/forge/listings/{id}/publish`

Approve and publish a listing.

**Request Body:**
```json
{
  "platform": "facebook",
  "title": "Optimized title",
  "description": "Optimized description",
  "price": "$12",
  "shipping": "Local pickup only"
}
```

**Response:**
```json
{
  "id": "listing-uuid",
  "platform": "facebook",
  "title": "Optimized title",
  "description": "Optimized description",
  "price": "$12",
  "status": "published",
  "published_at": "2026-11-14T16:21:00Z"
}
```

---

### 8. Status

#### `GET /v1/forge/status`

Health check.

**Response:**
```json
{
  "status": "healthy",
  "uptime_seconds": 3600,
  "modelmistress": {
    "connected": true,
    "url": "http://127.0.0.1:12345",
    "models": ["qwen3.5:9b", "nomic-embed-text-v1.5"]
  }
}
```

---

### 9. AI Status

#### `GET /v1/forge/ai/status`

Check ModelMistress connection and loaded models.

**Response:**
```json
{
  "connected": true,
  "url": "http://127.0.0.1:12345",
  "token_provided": true,
  "models": [
    {
      "id": "qwen3.5:9b",
      "name": "Qwen3.5-9B",
      "status": "loaded"
    },
    {
      "id": "nomic-embed-text-v1.5",
      "name": "Nomic Embed Text",
      "status": "loaded"
    }
  ]
}
```

---

## Error Format

All errors follow this format:

```json
{
  "detail": {
    "error": {
      "code": 400,
      "message": "Description of the error",
      "type": "validation_error"
    }
  }
}
```

---

## Rate Limits

- **Per minute:** 60 requests (configurable via `config.toml`)
- **Per day:** 1000 requests (configurable via `config.toml`)
- **ModelMistress quota:** Model-specific limits apply

---

## Webhooks

ItemForge supports webhooks for event notifications.

### Events

| Event | Description |
|-------|-------------|
| `item.ingested` | New item created |
| `item.research.complete` | Research finished |
| `item.documentation.generated` | Documentation generated |
| `listing.published` | Listing published |

### Webhook Payload

```json
{
  "event": "item.ingested",
  "timestamp": "2026-11-14T16:21:00Z",
  "data": {
    "item_id": "uuid",
    "images": 3,
    "folders_created": 1
  }
}
```

---

## Example: Full Workflow

```bash
#!/bin/bash
# 1. Ingest photos
curl -X POST http://localhost:8000/v1/forge/ingest \
  -H "Authorization: Bearer $TOKEN" \
  -F "folder=/path/to/photos"

# 2. List items
curl http://localhost:8000/v1/forge/items \
  -H "Authorization: Bearer $TOKEN"

# 3. Research item 1
curl -X POST http://localhost:8000/v1/forge/items/{id}/research \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"depth": "deep"}'

# 4. Generate documentation
curl -X POST http://localhost:8000/v1/forge/items/{id}/docs \
  -H "Authorization: Bearer $TOKEN"

# 5. Generate listings
curl -X POST http://localhost:8000/v1/forge/items/{id}/listings \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"platforms": ["facebook", "instagram"]}'

# 6. Publish to Facebook
curl -X POST http://localhost:8000/v1/forge/listings/{listings_id}/publish \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "platform": "facebook",
    "title": "Optimized title",
    "description": "Optimized description",
    "price": "$12",
    "shipping": "Local pickup only"
  }'
```

---

## Windows-Specific Notes

- **Paths:** Always use native Windows paths (`C:\Users\...`), NOT WSL-style paths (`/c/Users/...`)
- **File uploads:** Use multipart/form-data with `-F` in curl
- **Line endings:** Windows CRLF (`\r\n`) for text files
- **Path separators:** Forward slashes `/` work in Python; backslashes `\` work in PowerShell

---

## Troubleshooting

| Issue | Solution |
|-------|----------|
| Connection refused | Ensure server is running: `uvicorn app.main:app --reload` |
| 401 Unauthorized | Check `Authorization: Bearer <token>` header |
| 404 Not Found | Check endpoint path and version |
| 429 Too Many Requests | Increase rate limits or wait |
| ModelMistress not responding | Check ModelMistress server is running |
| Timeout | Increase timeout in `config.toml` |

---

## References

- `acp.md` — ACP protocol documentation
- `claude.md` — Claude integration
- `platform-design.md` — Full architecture
