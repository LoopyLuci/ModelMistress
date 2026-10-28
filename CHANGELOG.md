# Changelog

## Unreleased

- **Agents and Claude**: `model-mistress mcp --start` starts a hub in the background when none answers (it keeps
  running after the session), and `mcp --peer` serves only `models_list` and `chat_complete`, with a peer key from
  `MODELMISTRESS_KEY`. The MCP server now tells the client what its tools are for. A Claude Code plugin is in
  `integrations/claude/model-mistress` (slash commands, a skill, the MCP server), listed by `.claude-plugin/marketplace.json`.
  See [docs/claude.md](docs/claude.md), [docs/agents.md](docs/agents.md) and [docs/integrations.md](docs/integrations.md).
- **Finding the hub**: `MODELMISTRESS_URL` with `MODELMISTRESS_TOKEN` names a hub without a control file. Errors name
  the URL, the file and the variable that would fix them. An address from a control file that does not answer says the
  file may be a left-over copy, and names `MODELMISTRESS_URL` and `MM_HOME`. `--home` names the hub for that command, so `MODELMISTRESS_URL` does not
  override it. Hub errors reach MCP and CLI callers as `code: message`.
- **Peer keys**: `keys create` and `keys revoke` refuse to write `keys.json` while a hub runs in that home but does not
  answer. The running hub reads the file only at start, so such a write would not reach it (and a revoke would not stop
  its keys), and the command used to report success.
- **Key audit trail**: every peer key created or revoked is recorded in `keys.json` with its time, key, peer and who
  made it (`hub`, or `offline` for a command with no hub answering). `keys log` shows the record. A key change reads
  `keys.json` first, so a hub never undoes a change made by another process, and a damaged `keys.json` refuses key
  changes instead of being replaced. See [docs/hosting.md](docs/hosting.md).
- **Error codes**: chat, completions, embeddings and rerank answer llama-server's own errors under stable codes. A prompt
  over the context window is `context_exceeded` (400, with `n_prompt_tokens` and `n_ctx`); a server still loading is
  `loading`, and one that reports itself unavailable is `busy` (503, with `retry_after_s` and a `Retry-After` header).
  The `embeddings.create` and `rerank.run` operations and the Ollama shim's chat, generate and embed routes answer the
  same codes; the shim's `{"error"}` body gains a `code` field. See [docs/agents.md](docs/agents.md).
- **Fail-fast `busy`** (`fail_fast_busy`, off by default): with it on, a request that finds no idle slot in a model's
  llama-server gets `busy` (503, `Retry-After: 1`) at once, instead of waiting in the server's queue. The slot is checked
  with llama-server's `GET /slots?fail_on_no_slot=1` before each request on `/v1`. The dashboard's Settings page has the
  switch; `config.set` takes `{"settings": {"fail_fast_busy": true}}`.
- **Fix**: a hub started from `mcp --start` no longer keeps the MCP client's stdio pipes open (Windows passed them on).
- **A Python desktop window** (`gui/`, PySide6 6.12.0): Status, Models, Chat and Settings. It is a client of the hub's
  HTTP API (standard library only on the Python side), so the hub keeps no Python dependency. Run `python -m mm_gui`
  from `gui/`; see [gui/README.md](gui/README.md).
- **Window look**: Settings has an Appearance group (theme: follow the system, light or dark; style, Fusion by default;
  text size), kept per user and never sent to the hub. Ctrl+1 to Ctrl+4 switch pages, Ctrl+Enter sends in Chat, and the
  window remembers its size. [docs/gap-analysis.md](docs/gap-analysis.md) lists what is still missing in the hub,
  the dashboard and the window.
- **Dashboard gates**: `npm run check` in `frontend/` runs a vue-tsc type-check, Vitest unit tests (26) and the build.
  `ci/pipeline.py` has a `dashboard` stage (the same checks, from the lock file) and a `gui` stage (the window's
  tests, with the pinned PySide6 and pytest). `src/build.rs` rebuilds the hub when `frontend/dist` changes.
- **`MM_HOME` is the home folder everywhere**: `default_home()` (used by every client that finds the hub without a
  folder), the desktop app and the CI pipeline now use `MM_HOME` when it is set (blank values are ignored), else the
  platform folder as before. `--home` still wins for one command. The CLI already read `MM_HOME`; it now goes
  through the same rule.

- **`ollama_discovery` setting** (default `true`, so nothing changes): `false` stops using Ollama models entirely.
  No Ollama store is scanned (the detected ones, `ollama_dirs`, the download folder's `ollama` store), and
  `ollama/<name>` is refused with an error, and `model.pull` from the Ollama library is refused too. `backend.list`
  reports Ollama as turned off by config. Also in the desktop settings form.

## 0.3.0 — 2026-10-15

ModelMistress serves coding agents, and the native-model system gains experts, specialists and routing traces.

### Serving for agents
- **Per-model serving profiles**: context per slot, KV cache type, flash attention, prompt cache and reuse, Jinja
  tool-call templates and batch sizes, set per model in `[profiles]`.
- **VRAM planner**: estimates what a model needs before loading it and refuses, or evicts idle models, instead of
  spilling into shared memory (on by default). Serving details are reported on `/v1/models`.
- A typo in `[serving]` or `[profiles]` is a warning (logged, and listed as `config_warnings` in
  `service.status` and `config.get`); that field falls back to its default instead of stopping the hub.
- Speculative decoding now really uses its draft model (`--spec-type`). Measured on a 24 GB GPU it was slower for
  Qwen3-Coder-30B-A3B at 8k and 32k, so it stays off unless set per model.

### Native models and Knowledge Modules
- **Experts KMs** are assembled into their core model and hot-swapped in.
- **Specialist KMs are callable** (`km.specialist.call`): the input is checked, the engine runs the KM's model, and
  the call is recorded.
- **Expert-routing capture**: a llama-server patch reports which experts each MoE layer chose per token, and traces
  record it (no cost when off; about 4.5x slower generation while on).

### Fixes
- Tests delete their own temp folders (about 2,000 had piled up).
- Engine patches are stored byte for byte, so a CRLF checkout no longer makes `git apply` refuse them.

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
