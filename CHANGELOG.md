# Changelog

## Unreleased

### Serving for agents
- **Serving profiles**: `[profiles."<model id>"]` and `[serving]` in `config.toml`, and the same fields in `model.load`:
  `ctx_per_slot`, `cache_type_k`, `cache_type_v`, `flash_attn`, `cache_prompt`, `cache_reuse`, `cache_ram_mib`,
  `kv_unified`, `batch_size`, `ubatch_size`, `jinja`, `chat_template_file` (plus the existing `ctx_size`, `gpu_layers`,
  `parallel`). Checked before a server starts. Unset options add no flag, so existing configs start models as before.
- `GET /v1/models` and `model.loaded` report `serving`: the real context window per slot, slots, KV types and the VRAM
  of each loaded model.
- **VRAM planner**: a load whose estimate does not fit the GPU unloads the least recently used idle models or is
  refused with `503 insufficient_vram` instead of spilling (`vram_guard`, `vram_margin_mib`, `skip_vram_check`).
  New operation `model.plan`. See `docs/serving.md`.
- A mistake in `[serving]` or a `[profiles]` entry (unknown key, wrong type, bad value) is now a warning, logged at start
  and shown by `service.status` and `config.get` as `config_warnings`; that field keeps its default and the hub starts.
- A draft model (`speculative`) is now actually used: MM passes `--spec-type draft-simple`, which the pinned llama-server
  needs. Measured slower on Qwen3-Coder-30B-A3B, so it stays off unless set (see `docs/STATUS.md`).

## 0.2.1 — 2026-10-11

The desktop app, ready for daily use.

### Desktop app
- **An installable Windows app** (NSIS per-user installer, and an MSI). It ships the hub beside it, starts it when
  the app opens (or uses a hub that already runs) and stops it on quit only if the app started it
  (`desktop.json` `{"stop_hub_on_quit": false}` or `MM_STOP_HUB_ON_QUIT=0` keeps it running). Tray: Open, Stop hub,
  Quit. A starting screen, and an error screen with Try again.
- **Chat**, the landing page: streaming replies with markdown, collapsible reasoning, Stop (or Esc), conversations,
  and a link from a reply to its trace.
- **Models**: load and unload with options, including experts on the CPU (`--cpu-moe`, `--n-cpu-moe`); pull by name;
  ModelMistress's own native models are listed (a GGUF with its `dusty.json` beside it), with experts on the CPU by
  default.
- **Status** (RAM, the loaded model's memory and buffers), **Settings**, **Speech**, **Images** and **Vision** pages.
  The old Swarm page is gone.
- Moved to Tauri 2. The window reaches the hub through a loopback relay that adds CORS headers for the app's own
  origins only (it adds no credentials). The content security policy allows only `http://127.0.0.1`.

### Hub
- `service.status` reports system RAM; a loaded model reports its resident memory and buffers.

### Also in this release
- **The Forge** (`docs/native/forge.md`): an optional Python/PyTorch environment the hub runs; Dusty-1.0 (Qwen3.5-4B
  as `qwen35moe`) converts and answers token for token like its base.
- **The Knowledge Module factory** (`docs/native/km-factory.md`): tasks from plan to publish, with review.

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
