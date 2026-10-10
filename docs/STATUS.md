# Status

Last checked 2026-10-07 on Windows 10 with an RX 7900 XTX, with the built-in llama.cpp (commit 7fee178, Vulkan).

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

## Not done yet

- A model is loaded for one use at a time: chat, embeddings or reranking (llama.cpp serves one of them per server).
- Hosting for other machines (per-peer keys, quotas): `serve --host` exposes the token-protected API, and that is all.
- Other engines (vLLM, ExLlama); image generation (stable-diffusion.cpp).
- Text to speech output other than WAV and raw PCM; converting Hugging Face weights (a Python tool).
- Training (what Unsloth does). It needs Python and PyTorch, so it can only be an optional add-on.
- The desktop app (`desktop-app/`, Tauri) still talks to Ollama directly, not to the hub.

## Legacy code

The first design's modules (`backends`, `config`, `mcp`, `models`, `observability`, `plugins`, `protocol`,
`router`, `runtime`, `server`) and the `cli` crate are still in the tree and still compile, but nothing in the
working binary uses them. The old CPU engine (`backends::cpu`) returns placeholder text instead of running a model,
the old server hard-codes port 8000, and the CLI's `tui` and `chat` commands print "not yet implemented". They will
either be rebuilt on the new core or removed.
