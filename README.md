# ModelMistress

Loads and serves local models behind one OpenAI-compatible API, with a control API and MCP tools for programs
and agents. It is a module of [AgenticBotPlatform](https://github.com/LoopyLuci/AgenticBotPlatform) (ABP) and also
runs on its own.

- **Finds your models.** It scans Ollama stores, Hugging Face caches (where Unsloth keeps its downloads), LM
  Studio's folder and any folders you add, and reads each GGUF's metadata (architecture, context length,
  quantization, vision projector).
- **Serves them through llama.cpp.** Each loaded model runs in its own `llama-server` process, on a free loopback
  port with its own random key, so only the hub can use it. Models load on first use. The least recently used one is
  unloaded when `max_loaded` would be exceeded, and idle models can be unloaded after a time you set. The processes
  belong to the hub: a Windows job object or Linux's parent-death signal stops them even if the hub is killed.
- **Downloads models** from Hugging Face (`hf.co/<org>/<repo>:<quant>`) and from the Ollama library
  (`qwen3:8b`), with progress, resume and a SHA-256 check of every file. No Ollama or Python needed.
- **Brings its own llama.cpp.** Built with `--features bundled-engine`, the binary carries `llama-server`, `llama-quantize` and `llama-tts` (Vulkan by
  default) and needs nothing else installed but the GPU driver.
- **Speech to text, built in.** With `--features whisper` it runs whisper.cpp itself: OpenAI's
  `/v1/audio/transcriptions` and `/v1/audio/translations` (WAV or MP3 in; json, text, verbose_json, srt or vtt out)
  and `model-mistress transcribe talk.wav`. A whisper model is a `ggml-*.bin` file in a model folder
  ([docs/speech.md](docs/speech.md)).
- **Text to speech, built in.** `POST /v1/audio/speech` (OpenAI's route; WAV or raw PCM out) and
  `model-mistress speak "Hello" --out hello.wav` run llama.cpp's own `llama-tts`, built from the vendored llama.cpp
  ([docs/text-to-speech.md](docs/text-to-speech.md)).
- **llama.cpp's tools as operations.** `quantize` (a job with progress), LoRA adapters (`lora`), a draft model for
  speculative decoding (`speculative`), embeddings and reranking served beside a chat model, all built from the
  vendored llama.cpp; Hugging Face conversion needs Python and says so ([docs/tools.md](docs/tools.md)).
- **Image generation, built in.** Built with `--features bundled-engine`, the binary also carries
  stable-diffusion.cpp's `sd-cli`: OpenAI's `/v1/images/generations` (PNG as `b64_json` or a url), the operation
  `image.generate` (a job with sampling progress and cancel) and `model-mistress image "a red fox" --out fox.png`.
  Stable Diffusion, SDXL and FLUX models ([docs/images.md](docs/images.md)).
- **Image recognition, built in.** With `--features vision` it runs ONNX models through ONNX Runtime: classify a
  picture, find objects with boxes, or get an embedding vector, through `/v1/vision/classify`, `/detect` and `/embed`, the
  operation `vision.analyze` and `model-mistress vision resnet50 cat.jpg`. Models are `.onnx` files in
  `<home>/vision` ([docs/vision.md](docs/vision.md)).
- **Knowledge Modules.** Versioned, tagged, optionally signed packages of knowledge, skills and tools that work beside
  a model's weights: found by search with an explained score, loaded and unloaded within disk/RAM/VRAM budgets, and
  verified on every load. Experts KMs are assembled into a mixture-of-experts model and hot-swapped in; specialist KMs
  (speech, images, vision) are callable and traced. A KM never contains code ([docs/native/km-spec.md](docs/native/km-spec.md),
  [docs/native/km-runtime.md](docs/native/km-runtime.md)).
- **A Knowledge Module factory.** Give it a goal and sources (files, URLs, skills, tools, other KMs, traces); any model
  the hub serves plans, writes cited training records, a second model checks them, tags are proposed against a
  controlled vocabulary with a review queue, and data and memory KMs are built, evaluated and published under a
  promotion policy. Every step is resumable and observable
  ([docs/native/km-factory.md](docs/native/km-factory.md), [docs/native/km-factory-runtime.md](docs/native/km-factory-runtime.md)).
- **The model's thoughts, visible.** Any chat can be traced: every token with its probability, the alternatives the
  model weighed and its entropy, the experts each layer chose (MoE models), the reasoning kept apart from the answer,
  stage timings and resources. Traces can be
  rerun with changes (ablation) and compared. A dashboard at `/ui` (any browser, and the desktop app) is also where you chat with the models (the Chat page
  streams, keeps conversations locally and shows reasoning apart), manage them (load options such as experts on the
  CPU, downloads) and change settings; it shows traces,
  Knowledge Modules, residency, training data and factory tasks ([docs/native/observability.md](docs/native/observability.md)).
- **Its own models, in progress.** Dusty (Qwen3.5-4B turned into a mixture of experts whose experts come from Knowledge
  Modules) and Misty (trained from scratch), with a training system ("the Forge"), consolidation of what a KM does
  well into the weights, and gated self-training ([docs/native/architecture.md](docs/native/architecture.md)).
- **Passes `ollama/<name>` through** to a running Ollama, if you still have one, so both show up in one model list.
- **One token.** The hub's token is also the OpenAI API key.

## Quick start

```bash
git submodule update --init --recursive engines/llama.cpp engines/stable-diffusion.cpp
cargo build --release -p model-mistress --features bundled-engine   # builds llama.cpp in (a few minutes once)
model-mistress serve            # foreground; writes <home>/control.json = {url, token, pid, version, api}
# in another terminal:
model-mistress pull hf.co/unsloth/Qwen3-8B-GGUF:Q4_K_M    # or: model-mistress pull qwen3:8b
model-mistress models           # what is on this machine
model-mistress chat qwen3 "Hello"
model-mistress mcp              # MCP over stdio, for agents
```

The bundled build needs CMake, a C++ compiler (Visual Studio's on Windows) and, for the default Vulkan backend,
the Vulkan SDK; `MM_GGML_BACKEND=cuda|hip|metal|cpu` picks another (see `src/build.rs`). Without the feature,
ModelMistress uses a `llama-server` you name with `llama_server`, put in `<home>/engines/llama.cpp/`, or have on
PATH. Downloads go to `download_dir` (default `<home>`); gated Hugging Face repos need `hf_token` or HF_TOKEN.

`<home>` is `%LOCALAPPDATA%\ModelMistress` on Windows, `~/.local/share/ModelMistress` on Linux, or `--home` /
`MM_HOME`. Settings live in `<home>/config.toml` (every field optional; `config.get` shows them all).

Build llama.cpp from source with the GPU backend you have (for example `-DGGML_VULKAN=ON` for AMD or Intel
GPUs, `-DGGML_CUDA=ON` for NVIDIA).

## Desktop app

On Windows, `ModelMistress_<version>_x64-setup.exe` installs the app and the hub together. The app uses a hub that
already runs, else starts the one it ships with (and stops it again on quit). See [docs/desktop.md](docs/desktop.md).

## API

Everything but `/v1/health` needs `Authorization: Bearer <token>`.

| Route | What |
|---|---|
| `GET /v1/health` | `{ok, pid, version, uptime_s}` |
| `GET /v1/operations` | every operation with a JSON Schema for its input |
| `POST /v1/call/{op}` | run one; the answer is `{"result": ...}` |
| `POST /v1/service/stop` | unload everything and stop |
| `GET /v1/models` | OpenAI model list |
| `POST /v1/chat/completions`, `/v1/completions`, `/v1/embeddings` | OpenAI, streaming or not |
| `POST /v1/rerank`, `/rerank` | reranking with a reranker model (llama-server's format) |
| `POST /v1/audio/transcriptions`, `/v1/audio/translations` | OpenAI speech to text (feature `whisper`) |
| `POST /v1/audio/speech` | OpenAI text to speech (JSON in; WAV or PCM out) |
| `POST /v1/images/generations` | OpenAI image generation (PNG as `b64_json` or `url`); `/v1/images/edits` answers 501 |
| `POST /v1/vision/classify`, `/v1/vision/detect`, `/v1/vision/embed` | image recognition (feature `vision`; JSON with a base64 `image`) |
| `/v1/km/...` | Knowledge Modules: list, search, import/export, load/unload, pin/ban, channels, residency, data records, vocabulary (owner only) |
| `/v1/factory/...` | factory tasks, plans, review queue, settings (owner only) |
| `GET /v1/native/traces`, `/traces/{id}`, `/traces/{id}/tokens`, `/stream` (SSE); `POST /traces/{id}/rerun` | traces of the model's thoughts (owner only) |
| `GET /ui` | the dashboard |

Operations: `service.status`, `backend.list`, `model.list`, `model.info`, `model.loaded`, `model.load`,
`model.plan`, `model.unload`, `model.pull`, `model.search`, `model.files`, `model.delete`, `model.show`, `model.create`, `model.copy`, `job.list`, `job.get`, `job.cancel`,
`chat.complete`, `speech.transcribe`, `speech.synthesize`, `image.generate`, `vision.analyze`, `quantize`, `lora`, `speculative`, `embeddings.create`,
`rerank.run`, `model.convert` (answers 501), `bench.run` (llama-bench), `metrics.get`, `logs.tail`, `config.get`,
`config.set`, and the families `km.*`, `factory.*` and `trace.*` (`GET /v1/operations` lists every one with its
schema). Every operation is also an MCP tool.

### Ollama-compatible API

The hub also speaks Ollama's server API on the same host, port and token, so an Ollama client or tool can point
at ModelMistress (`OLLAMA_HOST=http://127.0.0.1:<port>`). The token is required, as for `/v1`: send it as
`Authorization: Bearer <token>` (from `control.json`, or `MM_TOKEN`). A client that cannot send a header needs a
local proxy that adds it, or the opt-in `allow_loopback_no_token = true` in `config.toml` (default `false`): then
requests whose connecting address is `127.0.0.1` or `::1` need no token on `/api/*` and `/v1`. The check is on the
socket's peer address, never a header, and other addresses still need the token. The tradeoff: every program and
user on this machine (and any local reverse proxy that forwards to the hub, which then looks local) gets full
owner access with no secret, so turn it on only on a machine you trust. The owner's token only; a peer key is
refused on `/api/*`.

`keep_alive` on `/api/chat`, `/api/generate` and `/api/embed` is per model, as in Ollama: seconds or `"5m"`; `0`
unloads the model after the request, a negative value keeps it loaded, and a request without it falls back to
`idle_unload_s`. A chat model and an embedding model stay loaded together: chat models count against
`max_loaded`, embedding and rerank models against `max_loaded_embedding` (default 1), least recently used first.

| Route | Maps to |
| --- | --- |
| `GET /api/version`, `/api/tags`, `/api/ps` | the version; the catalog (plus presets); the loaded models |
| `POST /api/show` | `model.info` metadata, the preset's Modelfile, template, parameters |
| `POST /api/chat` | the chat path of `/v1/chat/completions`; NDJSON when streaming (the default) |
| `POST /api/generate` | a one-turn chat, or `/v1/completions` with `raw` or a `template`; `system`, `options`, `keep_alive`, `stream` |
| `POST /api/embed`, `/api/embeddings` | `/v1/embeddings` |
| `POST /api/pull`, `DELETE /api/delete` | `model.pull` (status lines), `model.delete` (only what MM downloaded) or a preset |
| `POST /api/create`, `POST /api/copy` | a Modelfile preset; a copy of one |

`create NAME -f Modelfile` (or `POST /api/create`) reads FROM, SYSTEM, TEMPLATE, PARAMETER, ADAPTER, MESSAGE and
LICENSE and keeps a preset in `<home>/presets.json`; using its name runs the FROM model with those defaults. CLI:
`show`, `ps`, `cp`, `create`. See `docs/STATUS.md`.

## Serving agents

Per-model serving profiles (`[profiles."<model id>"]` in `config.toml`: context per slot, parallel slots, KV-cache
types, flash attention, prompt caching, batch sizes, chat template), the real context window and VRAM of each loaded
model in `GET /v1/models` (`serving`) and `model.loaded`, and a VRAM planner that refuses or unloads the least recently
used model instead of letting a load spill: [docs/serving.md](docs/serving.md).

## Development

`python ci/pipeline.py` runs everything a change must pass (rustfmt, clippy `-D warnings`, tests, a release build,
and a smoke test against a real hub; with llama-server and a small model on the machine, a real chat too).
`python ci/pipeline.py --install-hook` runs it before every push. What works and what does not is in
[docs/STATUS.md](docs/STATUS.md); the original design is in [DESIGN.md](DESIGN.md).

## License

Apache-2.0
