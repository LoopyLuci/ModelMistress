# Changelog

## 0.2.0 — 2026-10-10

The first release of ModelMistress as a standalone model hub.

### Models, built in
- **llama.cpp built into the binary** (`--features bundled-engine`): `llama-server`, `llama-quantize`, `llama-tts`
  and stable-diffusion.cpp's `sd-cli`, Vulkan by default; nothing else to install but the GPU driver.
- **Downloads** from Hugging Face (`hf.co/<org>/<repo>:<quant>`) and the Ollama library, with progress, resume and
  SHA-256 checks.
- **Speech to text** (`--features whisper`): OpenAI's `/v1/audio/transcriptions` and `/translations` through
  whisper.cpp; `transcribe`.
- **Text to speech:** `/v1/audio/speech` and `speak` through llama.cpp's `llama-tts` (for example Qwen3-TTS).
- **Image generation:** `/v1/images/generations`, the `image.generate` job and `image` (SD 1.x/2.x, SDXL, FLUX).
- **Image recognition** (`--features vision`): classify, detect and embed with ONNX models through ONNX Runtime;
  preprocessing matches the reference implementations (letterboxed detectors, crop_pct, antialiased resize).
- **llama.cpp's tools as operations:** quantize, LoRA adapters, a draft model for speculative decoding, embeddings
  and reranking beside a chat model.

### Knowledge Modules and the factory
- **Knowledge Modules (`mmkm/1`):** versioned, tagged, optionally signed packages of knowledge, skills and tools that
  work beside a model's weights; safe packing, hash checks on every load, explained search, disk/RAM/VRAM residency
  budgets, training data with revision history, a controlled tag vocabulary.
- **The factory:** tasks that plan, gather, generate cited records, verify them with a second model, tag them, build
  and evaluate KMs, and publish them under a promotion policy; resumable, budgeted and traced.

### Observability and the dashboard
- **Traces of the model's thoughts:** tokens with probabilities, alternatives and entropy, reasoning apart from the
  answer, stage timings and resources; rerun with changes and compare.
- **A dashboard at `/ui`** in any browser and in the desktop app: Thoughts, Traces, Knowledge, residency, Data studio,
  Vocabulary, Factory.

### Hosting and control
- **Peers** use this hub's models with their own keys, quotas, allow-lists and usage records; the owner's token stays
  local.
- **Control API and MCP:** every operation has a JSON Schema and is also an MCP tool (`model-mistress mcp`).
- **The desktop app** runs on the hub (no Ollama needed).

### Designed, not in this release
- MM's own models (Dusty and Misty), the training system and consolidation: `docs/native/architecture.md`.
