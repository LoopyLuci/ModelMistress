# Status

Last checked 2026-10-07 on Windows 10 with an RX 7900 XTX, with the built-in llama.cpp (commit 7fee178, Vulkan).

## Ollama-compatible API

`src/ollama_api.rs` adds Ollama's server routes under `/api/*` on the hub (the `/v1` routes and CLI are unchanged):
`version`, `tags`, `show`, `chat`, `generate`, `embed`, `embeddings`, `ps`, `pull`, `delete`, `create`, `copy`.
Tested over a hub with stand-ins for llama-server (no real model): shapes, NDJSON streaming however the bytes are
cut, presets end to end, token required. Not yet run against a real Ollama client.

- Same token as `/v1`, loopback only for the owner's token; a tokenless Ollama client needs a header-adding proxy
  (decision for the owner: an opt-in tokenless-loopback setting).
- Modelfile `create` makes a *preset* (`<home>/presets.json`): FROM must already be an MM model (pull it first);
  ADAPTER files are attached to the FROM model when the preset is used (MM's LoRA is per model, so this is shared
  with other users of that model); TEMPLATE is used by `/api/generate` only (chat uses the model's own template).
- `digest` in `/api/tags` is a stable stand-in (id, size, mtime), not a content hash. `size_vram` is the VRAM the
  server reported, else 0. `keep_alive: 0` unloads after the request; other values are ignored (`idle_unload_s`
  is global). `num_ctx` and `num_gpu` options are load-time settings and are ignored per request.
- CLI: `show`, `ps`, `cp`, `create -f Modelfile`; operations `model.show`, `model.create`, `model.copy`.

## Works (tested end to end)

- **Nothing else to install.** Built with `--features bundled-engine`, the one `model-mistress` binary (114 MB)
  carries llama.cpp's `llama-server` and `llama-bench`, writes them to `<home>/engines/bundled/<hash>/` on first
  start, and uses them unless `llama_server` names another. They need only Windows itself and the GPU driver
  (`vulkan-1.dll`): the C++ runtime is linked in and OpenMP is off. The built-in server sees the RX 7900 XTX.
- **Downloads.** `model.pull` (CLI `pull`) from Hugging Face (`hf.co/org/repo[:quant]`, with the vision projector)
  and from the Ollama library (`name[:tag]`), as jobs with progress (`job.list`, `job.get`, `job.cancel`); a
  cancelled or broken download resumes, every file's SHA-256 is checked against the source, and a disk without
  room is refused up front. Tested: a 0.11 GB Hugging Face model and a 0.27 GB Ollama model at about 34 MB/s,
  both chatting on the GPU; a cancel at 63 % that resumed and passed the checksum. `model.search` and
  `model.files` find what to pull; `model.delete` removes only what ModelMistress downloaded (an Ollama model's
  blobs only when no other manifest uses them).
- The catalog: 13 models found across an Ollama store and a Hugging Face cache, with GGUF metadata; the vision
  projector attached to the Unsloth Qwen3.5 model.
- Loading on first use, explicit loads with options, LRU eviction at `max_loaded`, unload, and unload of all models.
- Chat (plain and streamed), completions, OpenAI model list; `ollama/<name>` passed through to a running Ollama.
- Measured: Qwen3.5-9B Q4_K_M loads in 17 s, and generates at 79 tokens/s in a chat.
  `bench.run` on llama3.2 1B: 15,363 tokens/s prompt processing, 434 tokens/s generation.
- `llama-server` processes die with the hub, even when the hub is killed with `taskkill /F`.
- The token check, errors (404 for unknown operations and models, 400 for bad input), metrics, logs, and
  `config.set` saving to `config.toml`.
- MCP over stdio (`model-mistress mcp`).

## The dashboard as an app (tested in headless Edge against a running hub)

- **Chat** is the landing page: model picker with MM's own (Forge) models in a group at the top, streamed answers
  (`/v1/chat/completions`, `stream: true`), conversations kept in the browser (new, rename, delete), system prompt,
  temperature, top-p, max tokens, Stop (button or Esc), Enter sends and Shift+Enter breaks a line, markdown with
  copyable code blocks (own renderer, everything escaped), reasoning (`reasoning_content` or `<think>`) collapsed
  apart from the answer, time to first token and tokens per second per reply, "trace replies" (sets `mm_trace` and
  links each reply to its trace), and a picture button for vision models. Stop cancels the task in llama-server.
- **Models**: installed models (size, quantisation, architecture, context, where it runs), load with options
  (context, GPU layers, experts on the CPU for MoE models), unload, loaded state with RAM and the buffers
  llama.cpp reports, details, delete with confirmation, Hugging Face search with per-file download and download by
  name (Ollama too), downloads with progress and stop.
- **Native models**: a GGUF under `<forge data dir>/**/gguf/` with a `dusty.json` beside `gguf/` is listed by
  `model.list` as source `native`, with `native: true`, its `family` (`dusty`, `misty`) and the expert counts.
  Loading a native MoE model passes `--cpu-moe` unless the load says `cpu_moe: false` (`n_cpu_moe: N` for the
  first N layers only), so Dusty Q8_0 (23.8 GB) runs on a 24 GB card: experts in RAM, the rest on the GPU.
- Measured with Dusty-1.0 Q8_0 (qwen35moe, 64 experts, top 4) in the Chat page, 80-token reply, a first request
  that also loaded it (file cached): 6.0 s to the first token, 4.9 tokens/s; a warm 40-token reply about 3.5
  tokens/s on a busy machine (free RAM was 7 GB of 64, the model server held 19 to 23 GB).
- **Settings** is a form made from `config.get` (a changed value is sent with `config.set`; tested by changing
  `max_loaded` and changing it back). **Status** (`/overview`) shows version, uptime, engine, RAM, loaded models
  and their memory (`service.status` gained `system`, and `model.loaded` gained `rss_mb` and `buffers`).
- **Speech, Images, Vision** pages: microphone to `/v1/audio/transcriptions`, text to `/v1/audio/speech`; prompt to
  an `image.generate` job with progress and a gallery; a dropped picture to `/v1/vision/*` with boxes drawn. Each
  shows "not available in this build" when the hub answers 501 or 404; with no speech, image or vision model on
  this machine they showed exactly that, the happy paths of these three were not run against real models.

## Hosting for peers

Per-peer keys (stored hashed in the home folder), quotas (requests per minute, tokens per day, concurrent requests,
`429` with `Retry-After`), model allow-lists, usage accounting, revocation that applies to the next request, and a hot
swap (`model.load` with `reload`, LoRA or draft changes, unload, eviction) that lets running requests, streams
included, finish on the old model ([hosting.md](hosting.md)). Listening beyond loopback needs `allow_network` and a key.
Tested over HTTP with stand-ins for `llama-server`: quotas, revocation, allow-lists, and a hot swap under concurrent
requests and a stream that drops nothing. Not tested: a hot swap of a real model under load.

## The desktop app

Talks to the hub only (list, pull, delete, load, unload, chat, speech, keys, usage); its Ollama client is gone
([desktop.md](desktop.md)). Its client code is tested against a hub in the test process; the Tauri window itself was not
built here.

## Speech to text (feature `whisper`)

`/v1/audio/transcriptions` and `/v1/audio/translations` run whisper.cpp inside the hub (see [speech.md](speech.md)).
Tested on Windows 10 (CPU) with `ggml-base.en.bin` and a synthesized WAV: transcript, verbose_json, srt, vtt and
the CLI. Not tested: MP3 input, multilingual models, GPU builds.

## llama.cpp's tools and text to speech

`quantize` (a job), `lora`, `speculative`, `embeddings.create`, `rerank.run`, `speech.synthesize` and
`POST /v1/audio/speech` ([tools.md](tools.md), [text-to-speech.md](text-to-speech.md)). Tested on Windows 10 (CPU
build of the built-in llama.cpp): quantize of a real GGUF, embeddings with nomic-embed-text, a LoRA adapter attached,
scaled and detached on a running model, and a draft model, all end to end. Not tested: reranking (no reranker model
on the machine) and speech (no model that the pinned `llama-tts` runs: it now takes a model plus an `mmproj`, such
as Qwen3-TTS, not OuteTTS). `model.convert` answers 501: Hugging Face conversion needs Python and PyTorch.

## Image generation

`POST /v1/images/generations`, the operation `image.generate` (a job: sampling steps as progress, cancel) and the
`image` command, through stable-diffusion.cpp's `sd-cli`, built from `engines/stable-diffusion.cpp` as a CMake project of
its own (own ggml; PNG only) and embedded with the `bundled-engine` feature ([images.md](images.md)). One process per
request, one at a time. Tested on Windows 10 (CPU build): a real Stable Diffusion 1.5 checkpoint at 256x256, 4 steps,
through the endpoint and the operation (valid PNG, step progress counted). Not tested: FLUX and SDXL (the SDXL GGUFs
on the machine are ComfyUI-style UNet-only files, which `sd-cli` cannot load), GPU backends, `response_format: url` with
a real image. `/v1/images/edits` answers 501 (image-to-image and inpainting exist in sd.cpp, but OpenAI's
transparency masks do not map cleanly).

## Image recognition (feature `vision`)

`POST /v1/vision/classify`, `/detect` and `/embed`, the operation `vision.analyze` and the `vision` command, through ONNX
Runtime (`ort` 2.0.0-rc.13, CPU) with PNG and JPEG decoding from the `image` crate; models are `.onnx` files in
`<home>/vision` with an optional `<name>.json` ([vision.md](vision.md)). Tested: everything that needs no model
(preprocessing, softmax and top-k, NMS, YOLO v5 and v8 decoding, kind guessing, sidecars, requests) in every build, and
with the feature a hand-written ONNX model through the real session and the hub routes on Windows 10. Not tested: GPU execution
providers, JPEG inputs through a model, float16 or multi-input models (refused
with a clear error).

Checked with real models (2026-10): ResNet-50, CLIP ViT-B/32 and YOLOv8n agree with ONNX Runtime's Python reference
after the preprocessing fix (letterboxed detectors, classifier crop_pct, antialiased resize): YOLOv8n dog 68.5% vs
68.3%, ResNet-50 Samoyed 97.3% vs 97.3%, CLIP cosine 0.999.

## Knowledge Modules

The runtime of `docs/native/km-spec.md` (104 tests): manifests for every kind, `.mmkm` packing that refuses links,
absolute paths, `..`, device names and unlisted files; SHA-256 of every payload checked on load; ed25519 signatures
with a per-source trust policy; a SQLite index with full-text search; selection with every score term reported and
every exclusion explained; disk/RAM/VRAM budgets with pins and in-flight protection; memory, data (with revision
history), skill and tool KMs; a controlled tag vocabulary with origin and confidence per tag. **Experts KMs** are
assembled into their core (the routed-expert slots they declare are byte-copied into a copy of the base GGUF, no
requantising; mismatches refused) and the model is hot-swapped onto it; evicting the last one returns to the base.
Each change copies the whole base (about 24 GB for Dusty Q8_0), and it has not yet run against the real Dusty.
**Specialist KMs** (speech to text, text to speech, images, vision) are callable with `km.specialist.call`: the
input is checked against the KM's schema and each call is traced. KM search can use a real embedder
(`factory.embedder.setup`; Qwen3-Embedding-0.6B tested). Not yet: attaching `projector` and `router` KMs, the learned
router, choosing KMs automatically for chat, using skill/tool KMs at inference, a core calling a specialist itself.

## The Knowledge Module factory

Tasks run plan → gather → generate → verify → tag → build → evaluate → publish (64 tests). Run for real with
Qwen3-1.7B as author and reviewer on the CPU: 20 records generated, 11 rejected (8 duplicates, 3 unsupported by their
sources), data and memory KMs built and imported, uplift measured, promotion held by the gate. A 0.6B reviewer accepted
everything, so reviewers should be about 1.7B or larger. Not yet: adapter training (the Forge now has LoRA; not wired into the factory), executable code
checks (the hook exists; tool-call schema checks are built in), learning the quality scorer from review decisions,
automatic resume after a hub restart (a task is marked `interrupted` and resumes on request).

## Observability and the dashboard

Chat requests can be traced (25 tests, and a real run with Qwen3-0.6B): tokens with probability, top-k alternatives and
entropy; reasoning apart from the answer; the templated prompt; stage timings; RAM. Rerun with changes and diff two
traces. The dashboard at `/ui` (embedded in the binary) works in any browser and in the desktop app: Thoughts, Traces,
Knowledge, residency, Data studio, Vocabulary, Factory. **Expert routing** is captured per token for MoE models (a small
patch to the bundled llama.cpp; tracing setting `routing` or `"mm_routing": true`): checked with Qwen3-Coder-30B-A3B
on Vulkan, no cost when off, about 4.5x slower generation while on. Not yet: tracing peers' requests. GPU memory per model is
in `model.loaded` (`serving.vram_mib`).

## Native models

Designed (`docs/native/architecture.md`). The Forge (`docs/native/forge.md`), an optional self-installed
Python/PyTorch environment run by the hub over a JSON-lines protocol, works on Windows with ROCm on an RX 7900 XTX.
Dusty-1.0 is built: Qwen3.5-4B converted to `qwen35moe` (the dense FFN is the shared expert, 64 empty routed slots
per layer for Knowledge Modules), and its Q8_0 GGUF answers token for token like the base (10 of 10 prompts, log
probabilities identical). Not yet: training Dusty's experts, consolidation, the base bake-off, anything of Misty.

## Serving for agents (LF1)

Serving profiles, `serving` in `/v1/models` and `model.loaded`, the VRAM planner and `model.plan`
([serving.md](serving.md)); 24 new unit tests (options, flag building, validation, the estimator on synthetic
GGUF metadata, the planner, TOML parsing) and a hub test; `cargo test` 352 passed. Measured on Windows 10, RX 7900 XTX
(24 GB, Vulkan), llama.cpp 7fee178, with the dev build of the hub and its own home folder:

| Check | Result |
|---|---|
| Qwen3-Coder-30B-A3B UD-Q4_K_XL, 32768 tokens in one shared KV (`kv_unified`), 4 slots, KV q8_0/q8_0, flash attention on | Loads in 42 s. The server reports `n_ctx` 32768, 4 slots. VRAM of the server process: 18,529 MiB (estimate 18,979 MiB, 2.4 % high). |
| Same, one stream (256 tokens, temperature 0) | 98 tokens/s on the first request, 149 tokens/s once the prompt was cached. |
| Same, 4 concurrent requests | 171 tokens/s aggregate (51 tokens/s each). |
| Tool call, Qwen3-Coder-30B-A3B (the GGUF's template, Jinja), CPU | A request with one tool returned a structured `tool_calls` entry with valid JSON arguments (`finish_reason: tool_calls`); feeding the result back gave a normal answer. |
| Tool call, Qwen3.5-9B Q4_K_M (Jinja), CPU | Same: structured `tool_calls`, valid arguments, round trip fine. |
| `--list-devices` free memory | Not reliable: with 5.8 GB held by other programs it said 18.7 GB free, and 5 GB fewer than the truth after an 18.5 GB model loaded. The planner reads Windows' `GPU Adapter Memory` counter instead (matches the 24,560 MiB total minus what is in use). |

Speculative decoding on the GPU (Qwen3-Coder-30B-A3B UD-Q4_K_XL as the target, `qwen3:1.7b` from the Ollama store as the
draft, KV q8_0/q8_0, flash attention, one slot, temperature 0, 400 tokens, three coding prompts; tokens/s are the
server's own `predicted_per_second`, the mean of the three after a warm-up request). This found a bug: the draft model
was loaded but never used, because the pinned llama-server needs `--spec-type draft-simple` (default `none`) and MM
did not pass it. Fixed. The rows below are with the flag:

| Context | Setup | tokens/s | Draft tokens accepted | VRAM of the server |
|---|---|---|---|---|
| 32768 | no draft | 147 (146-148) | - | 18,478 MiB |
| 32768 | draft, `n_max` 2 | 15-17 | 73-83 % | 19,210 MiB (card full: 3-5 GB spilled to shared memory) |
| 32768 | draft, `n_max` 4 | 14-15 | 60-71 % | same |
| 32768 | draft, `n_max` 8 / 16 | 8-11 / 6-8 | 48-53 % / 33-35 % | same |
| 8192 | no draft | 151 | - | 17,228 MiB |
| 8192 | draft, `n_max` 2 | 102 | 73 % | 18,687 MiB |
| 8192 | draft, `n_max` 4 | 103 | 60 % | 18,687 MiB |

Result: speculative decoding makes this model slower, 30 % even when everything fits (ctx 8192) and 10 times slower at ctx
32768 on a 24 GB card shared with a desktop, where the draft model and its KV cache push the load into shared memory. A
mixture-of-experts target with 3 B active parameters is already about as fast as a 1.7 B dense draft, and verifying
several tokens at once touches more experts. So it stays **off by default** (a model has no draft until `draft` or
`speculative` sets one; `--off` removes it); do not use it for this model. Quantising the draft cache
(`-ctkd q4_0 -ctvd q4_0`, through `extra_args`) saved about 500 MiB and did not change that. Not tried: a dense target.
Another finding: `speculative` on a loaded model reloads it while the old copy still runs, so the VRAM check counts
both and refuses a model that fills the card (unload first).

Not measured: 32,768 tokens **per slot** (131,072 in all: the estimate is 23.9 GB, over what the card has free next to
a desktop; use `model.plan` first), the tool-call check on the GPU, and the planner's unload path against real servers
(it is unit-tested with stand-ins). The tool-call checks ran on the CPU; the chat template is the same on the GPU.

## Not done yet

- A model is loaded for one use at a time: chat, embeddings or reranking (llama.cpp serves one of them per server).
- Other engines (vLLM, ExLlama); image edits and a resident image model (sd.cpp's server); GPU execution providers, float16 and
  multi-input models and segmentation or pose outputs for vision.
- Text to speech output other than WAV and raw PCM; converting Hugging Face weights (a Python tool).
- Training: LoRA and QLoRA fine-tuning, merge and GGUF export are in the Forge (QLoRA needs bitsandbytes, Linux/CUDA only in practice; untested on a real GPU run). Not yet: expert training, distillation, DPO.
- Hosting: TLS (use a reverse proxy), per-key rate limits by tokens per minute.

## Legacy code

The first design's modules (`backends`, `config`, `mcp`, `models`, `observability`, `plugins`, `protocol`,
`router`, `runtime`, `server`) and the `cli` crate are still in the tree and still compile, but nothing in the
working binary uses them. The old CPU engine (`backends::cpu`) returns placeholder text instead of running a model,
the old server hard-codes port 8000, and the CLI's `tui` and `chat` commands print "not yet implemented". They will
either be rebuilt on the new core or removed.
