# Status

Updated 2026-10-28 at repository commit 8409cd6. The measurements below were taken on 2026-10-07 on Windows 10 with an
RX 7900 XTX and the built-in llama.cpp (commit 7fee178, Vulkan), unless a section says otherwise. On 2026-10-28 the
pipeline's test stage passed (398 tests), and the pipeline's smoke test passed against the built binary.

## Hub wedge, watchdog and placement (2026-11-22)

A llama-server that exited while loading (GPU out of memory, a driver crash) deadlocked the load thread and silenced the
whole hub; fixed in `src/engine.rs`, with a self-restart watchdog (`src/watchdog.rs`, exit 70) and automatic placement
(`--fit on`, fallback to more free room and then the CPU, crashes remembered in `placements.json`). See
[serving.md](serving.md) and the changelog entry for what was run on the RX 7900 XTX (crash to CPU in 5.8 s, a small
model on the GPU, a partial offload, an oversized request refused) and what was not (the watchdog on a real hung hub, the
Scheduled Task's relaunch time, a model larger than VRAM, the dashboard fields in a browser, non-Windows crash
detection). 539 library tests pass.

## Hardware-aware loading and labels (2026-11-23)

Model loads hold to the memory the machine really has. The GPU keeps `vram_reserve_mib` (default 2048) free for the
system, so models use at most 22 GB of a 24 GB card. On Windows a GPU load is also charged against the system's commit
(measured about 1.2 times its size: 25 GB of headroom fell to 11 GB for an 11.8 GB model), which made 20 GB loads fail
with `ErrorOutOfDeviceMemory` and 23 GB of VRAM free; loads are now held to what the commit allows, an out-of-memory
failure is retried as up to three lower splits before the CPU, and the limit the card showed is kept in
`gpu_limits.json`. Every model in `model.list` and `/v1/models` carries `quant`, `label` and `hardware` (what it can run
on, where it runs or would go, VRAM and RAM); Misty's roster counts the card's room, reports a `device` per role and
keeps the embedder and reranker on the CPU. See [serving.md](serving.md) and [HANDOVER.md](HANDOVER.md).

Run: the 27B Q5_K_XL (20.2 GB) loaded on the RX 7900 XTX twice with the new build, and `--fit-target` held a load to
free memory minus the target. 577 library tests, clippy and the frontend tests pass. Not run: the retry ladder on the
real card (the failures did not come back), the Models page chips in a browser, and any measurement of a CPU model and a
GPU model at the same time. Not built: load progress for slow disks, a thread budget for a CPU and a GPU model together,
a CPU draft model, AirLLM on a model bigger than memory (`docs/airllm-spike-results.md`), the PrismML GPU build on Linux (open items in [HANDOVER.md](HANDOVER.md)).

Ternary Bonsai files on the card (2026-11-23): `misty setup --install prism-gpu` installs the PrismML build with the Vulkan
backend (29 MB, pinned). A scratch hub on the RX 7900 XTX loaded the 9B `PQ2_0` file on it (2.3 GB of VRAM, 136 tokens/s)
while the 2B ran on the CPU build (48 tokens/s), both at once, and both answered correctly. `llama-bench` on the 2B: prompt
3,205 against 211 tokens/s on the CPU, generation 100.7 against 46.8.

## System awareness and data-broker opt-outs (2026-11-27)

The hub reads the PC every two seconds and yields to it: workers the hub started run below normal, go to idle priority with
efficiency mode when the machine is strained and pause when it stays critical; heavy jobs wait for it to quiet; idle models
are unloaded when memory is short. SystemWatchdog, when it runs, adds GPU load, temperatures, drive activity, video memory and
the foreground program. See [system-awareness.md](system-awareness.md). Run on this machine in a scratch hub with real Python
children and Watchdog's real feed class on a scratch port (priorities lowered, paused, stepped back and restored to normal,
a heavy job held and released, a stale feed ignored); not run: a real model server or model shed, the installed Watchdog
feeding live readings, Linux and macOS.

`model-mistress privacy` drives the pinned f*ck_data_brokers tool (letters, sending only on the owner's typed count, replies,
chasing); see [privacy.md](privacy.md). Run in a scratch home with placeholder details: the install gates, tamper
refusal, drafting and the send refusals; nothing downloaded or sent. Not run: the registry import, a real send, real replies.

## Ollama-compatible API

`src/ollama_api.rs` adds Ollama's server routes under `/api/*` on the hub (the `/v1` routes and CLI are unchanged):
`version`, `tags`, `show`, `chat`, `generate`, `embed`, `embeddings`, `ps`, `pull`, `delete`, `create`, `copy`.
Tested over a hub with stand-ins for llama-server (no real model): shapes, NDJSON streaming however the bytes are
cut, presets end to end, token required. Not yet run against a real Ollama client. Errors carry the hub's stable codes (`context_exceeded`, `loading`, `busy`
with `Retry-After`, `not_found`, `backend_error`), and the body adds `code` to Ollama's `{"error"}` shape; the tests are
in `src/tests_upstream.rs`.

- Same token as `/v1`, loopback only for the owner's token; a tokenless Ollama client needs a header-adding proxy
  (decision for the owner: an opt-in tokenless-loopback setting).
- Modelfile `create` makes a *preset* (`<home>/presets.json`): FROM must already be an MM model (pull it first);
  ADAPTER files are attached to the FROM model when the preset is used (MM's LoRA is per model, so this is shared
  with other users of that model); TEMPLATE is used by `/api/generate` only (chat uses the model's own template).
- `digest` in `/api/tags` is a stable stand-in (id, size, mtime), not a content hash. `size_vram` is the VRAM the
  server reported, else 0. `keep_alive` is per model: 0 unloads after the request, negative keeps it loaded, seconds or "5m" set its
  idle deadline; omitted falls back to the global `idle_unload_s`. `num_ctx` and `num_gpu` options are load-time settings and are ignored per request.
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
  temperature, top-p, max tokens, Stop (button or Esc), Enter sends and Shift+Enter breaks a line, Up and Down walk
  through earlier messages, markdown with
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

Every key change is recorded in `keys.json` (`events`: time, key id, peer, and `via`, which is `hub` or `offline`), and
`model-mistress keys log` shows them. A key change reads `keys.json` first, so a change made by another process is not
undone by the hub; a `keys.json` that cannot be read refuses key changes. A running hub re-reads `keys.json` only when
it starts serving and when it writes a key, so an offline revoke reaches it at its next key change or restart. The
offline commands still refuse to write while a hub that does not answer is running. Tested with unit tests on the file
and with an offline run of the built binary.

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

## Rigel (feature `rigel-engine`, engine = "rigel")

Tested on 2026-11-15 on Windows 10 with an RX 7900 XTX and HIP SDK 7.2, using Qwen3-Coder-30B-A3B UD-Q4_K_XL with
paged experts (`--moe-direct-io --moe-slots 1536`). The library tests cover choosing the engine, the one-slot rule,
the flags and the PATH. End to end on the same day, a hub built with `--features rigel-engine` did the following:
- `backend.list` showed the built-in Rigel (`a6b46ade3 (hip gfx1100)`);
- `model.plan` showed `engine: rigel` with one slot and the `--moe-*` flags;
- a chat answered correctly, with the 17.7 GB model using 6.1 GB of VRAM (3.5 tok/s on a cold start).

The speed measurements are in [rigel.md](rigel.md).

Rebuilt and re-run on 2026-11-24 (HIP SDK 7.2, MSVC toolset pinned to 14.44 with `MM_RIGEL_MSVC_TOOLSET`; 14.51 does not compile): the same
30B model loaded in 10 s on 5.75 GB of VRAM and answered at 2.2 tokens/s cold and 12.0 tokens/s on a longer answer. Releases 0.4.1 and
0.4.2 do **not** contain Rigel (built without `rigel-engine`). Plan for owning the source and the AirLLM-style hybrid:
[rigel-hybrid-plan.md](rigel-hybrid-plan.md).

## Misty (`src/misty`, `engine = "prism"`)

Written and first run on 2026-11-21. Two kinds of evidence.

Unit and integration tests with scripted stand-ins for the language models and the Cactus runtime:
`cargo test --lib -p model-mistress misty::` (95 tests; the whole library suite, 502 tests, passes), clippy with
warnings denied and `cargo fmt --check`. They cover the router, the agent loop (tools, approvals, retries, citations, cancellation),
retrieval, the file, secret and web guards, pipelines, the operations, the REST, SSE and OpenAI surfaces, the peer rules,
the setup planner, the ternary-engine guard and the CLI. The ABP client has 17 tests in its own repository.

A real run on Windows 10 x64, Ryzen 9 5900X, with a scratch home and a second hub on its own port (the owner's running hub
was not touched): `misty setup --install default` downloaded the Prism engine (build 12128), the Cactus library, Needle3,
Whistle, Mirage and Bonsai 2 2B and 9B (`PQ2_0`), each checked against its pinned size and SHA-256; `misty doctor` passed;
`misty bench`, retrieval with a correct citation, tool calls (Needle chose, the policy held, a write was refused),
a spoken question (Whistle), a question about a picture (Mirage), the `voice-note` pipeline, the OpenAI route (plain and
streamed, `tools` refused) and ABP's client module all worked against that hub. The 4B file was downloaded and measured
(and answered one question as `main`). Speeds are in the user guide. That run found eleven defects, all fixed and covered
by tests (listed in the changelog).

A second client, NLP-CLI (2026-11-21, same machine, scratch hub on its own port): its command line, shell, service
protocol and conversation fallback called `misty.status` and `misty.ask` through `model-mistress call`, with
`approvals: deny`. A write attempted through her was refused and created no file. That run found a twelfth defect, in
the router: sums were not offered the calculator (see the changelog). It is fixed and tested; the whole library suite
passes (502 tests, run with `--test-threads=6` and no hub running; under a running hub the timing-sensitive factory
tests fail from CPU contention, as before).

Not done or not verified:
- Bonsai `Q2_0` and `PTQ1_0` packings and other platforms' Cactus libraries. The 27B file (`deep`) was only partly
  downloaded (19%, resumable) and never run, so the `deep` role, its thinking mode and the escalation up to it have been
  tested with stand-ins only. No other CPU has been tried; `misty bench` measures yours.
- Needle's accuracy is moderate: with fifteen tools it picked the right one for 15 of 21 test requests, and never called a
  tool for the six that needed none. It is a shortcut, not the decider, and only tools that read run on its word.
- Mirage returned a degenerate description of a plain grey square (repeated words); on a picture of shapes and a word it
  was right. Whistle misspelled one name ("Valdes" for "Valdez") in synthesized speech. Real recordings were not tried.
- The Vue pages (`frontend/src/views/Misty*.vue`) pass `npm run check` (vue-tsc, 26 Vitest tests, build; 2026-11-22), the
  hub serves them at `/ui`, and the desktop app starts and attaches to a running hub. Nobody has opened the Misty page
  itself in either (the dashboard asks for the owner token). The Qt window's tests run under `py -3.11` (PySide6 6.11.1,
  `QT_QPA_PLATFORM=offscreen`; the default Python here has no PySide6): 45 pass, and two that were already failing still
  fail (`test_look.py::test_the_window_remembers_its_size_and_opens_its_first_page` and
  `test_pages.py::test_misty_stop_cancels_the_run`, both "timed out waiting for the window").
  The ABP dashboard and CLI were not
  run (the CLI needs the dashboard); only the client module under them was. The full `cargo test` suite passes with
  `--test-threads=4`; the factory tests are timing-sensitive under full parallelism on this machine (unchanged by this work).
- The bundled llama.cpp is still b11272. Updating it to b11556 or later would let stock llama.cpp run `Q2_0` Bonsai files
  without Prism; that changes the engine for every model and needs `mm-routing.patch` re-checked, so it is separate.

### Environments and working folders (2026-11-23)

Misty chats can have a working folder and an environment (folder, other folders, variables, a policy per class of
tool): `src/misty/env.rs`, the `misty.env.*`, `misty.session.set` and `misty.browse` operations, the environment bar
and the Environments page of the dashboard, and Up and Down through earlier messages in both message boxes. Described in
the [user guide](misty/USER_GUIDE.md) and the [API](misty/API.md); the changelog lists what was checked.

- Run: 125 Misty tests and the whole library suite (559 passed, 6 ignored, `--test-threads=3`), clippy with warnings
  denied, `cargo fmt --check`, `npm run check` (127 tests); in a browser against a scratch debug hub: the environment bar,
  the folder picker end to end (it found a real defect, fixed), the Environments page, and Up and Down on both pages.
- Not run: the installed desktop app; a real model working in an environment (scripted models only); the CLI against a
  live hub; the picker on a hub on another machine. The plain Chat page has no tools, so the folder and environment apply
  to Misty chats only.

### The model router (`auto`) and the Models page (2026-11-22)

Tested: the hub side (`misty::auto::tests`, 8 tests, and `tests_hosting::auto_sends_each_chat_to_the_model_that_suits_it_and_says_which_answered`
with three stand-in models and a peer key: fast and deep routing, the headers, `@deep` forcing, a peer without `auto` in
its allow-list refused, a peer limited to one model sent only to it, `router.pick` and `router.status`); `pull::is_downloaded`
and `model.list`'s `deletable`; the frontend (`npm run check`: vue-tsc, 75 Vitest tests, build), where the Chat and Models
pages are mounted against a stand-in hub (Auto first, and the default only when a role is ready; the pick before the send;
tags removed; the reply's route and reasons; a hub with no router; filter and sort; role choosers; the settings editor and
the hub's refusal shown; the fit check); the Qt window (14 new tests).

Not done or not verified: nobody has opened the new Chat or Models page against a real hub (the dashboard asks for the
owner token); `auto` was not run with a real picture model; the `ServeProfile` fields the editor does not show
(`cache_reuse`, `kv_unified`, `chat_template_file`, the Rigel options) are kept when a profile is saved but can be
edited only in `config.toml`.

## Not done yet

- A single model file is loaded for one use at a time (llama.cpp serves chat, embeddings or reranking per server); a chat model and a separate embedding model do load together (`max_loaded` / `max_loaded_embedding`).
- Other engines (vLLM, ExLlama); image edits and a resident image model (sd.cpp's server); GPU execution providers, float16 and
  multi-input models and segmentation or pose outputs for vision.
- Text to speech output other than WAV and raw PCM; converting Hugging Face weights (a Python tool).
- Training: LoRA and QLoRA fine-tuning, merge and GGUF export are in the Forge (QLoRA needs bitsandbytes, Linux/CUDA only in practice; untested on a real GPU run). Not yet: expert training, distillation, DPO.
- Hosting: TLS (use a reverse proxy), per-key rate limits by tokens per minute.
- The open items with the code that shows each one (queue depth, approval queue, named key scopes,
  a default embedding model, aliases, batch jobs, the generic `500 io` sites, a dashboard page for keys and usage, a
  smaller MCP tool profile) are in [gap-analysis.md](gap-analysis.md).

## Legacy code

The first design's modules (`backends`, `config`, `mcp`, `models`, `observability`, `plugins`, `protocol`,
`router`, `runtime`, `server`) and the `cli` crate are still in the tree and still compile, but nothing in the
working binary uses them. The old CPU engine (`backends::cpu`) returns placeholder text instead of running a model,
the old server hard-codes port 8000, and the CLI's `tui` and `chat` commands print "not yet implemented". They will
either be rebuilt on the new core or removed.
