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
- **Brings its own llama.cpp.** Built with `--features bundled-engine`, the binary carries `llama-server` (Vulkan by
  default) and needs nothing else installed but the GPU driver.
- **Passes `ollama/<name>` through** to a running Ollama, if you still have one, so both show up in one model list.
- **One token.** The hub's token is also the OpenAI API key.

## Quick start

```bash
git submodule update --init engines/llama.cpp
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

Operations: `service.status`, `backend.list`, `model.list`, `model.info`, `model.loaded`, `model.load`,
`model.unload`, `model.pull`, `model.search`, `model.files`, `model.delete`, `job.list`, `job.get`, `job.cancel`,
`chat.complete`, `bench.run` (llama-bench), `metrics.get`, `logs.tail`, `config.get`, `config.set`.

## Development

`python ci/pipeline.py` runs everything a change must pass (rustfmt, clippy `-D warnings`, tests, a release build,
and a smoke test against a real hub; with llama-server and a small model on the machine, a real chat too).
`python ci/pipeline.py --install-hook` runs it before every push. What works and what does not is in
[docs/STATUS.md](docs/STATUS.md); the original design is in [DESIGN.md](DESIGN.md).

## License

Apache-2.0
