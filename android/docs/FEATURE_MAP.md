# ModelMistress: desktop to Android feature map

Status values:

- **Built, tested** – code is in place and covered by host JVM tests (`:core:test`, `:server:test`). Not yet run on a device.
- **Built, unverified** – the code compiles in the gate (`:app:assembleDebug`, `:router:assembleDebug`, native `:engine` build), but it has not run on a device.
- **Next** – planned for Android, not started.
- **Desktop only** – stays on the desktop hub. No Android plan yet.

| Desktop capability | Android status | Where it lives | Notes |
|---|---|---|---|
| OpenAI `/v1` API: `health`, `models` | Built, tested | `core` (`OpenAiApi.kt`), `server` (`ModelMistressServer.kt`) | Same paths also answer without `/v1`. |
| OpenAI `/v1/chat/completions` (non-streaming) | Built, tested | `core`, `server` | Refuses tools, `n != 1`, and non-text content with stable error codes. |
| OpenAI chat completions streaming (SSE, `data: [DONE]`) | Built, tested | `server` (`Http.kt`, `ModelMistressServer.kt`) | Stops generating when the client disconnects. |
| Bearer API key auth | Built, tested | `server` | Constant-time comparison. Key is generated on first run and can be rotated. |
| CORS | Desktop only (deliberately absent) | – | Browser pages cannot call the Android server. Other apps can. |
| `auto` model routing | Built, tested | `core` (`ModelRouter.kt`) | `auto` or blank picks the default model. |
| Multiple models served at once | Next | – | Android holds one loaded model at a time to limit memory. |
| llama-server per model (llama.cpp) | Built, unverified | `engine` (`llama_jni.cpp`, `LlamaEngine.kt`) | Runs in process through JNI, not as a child server. Compiles against `engines/llama.cpp`. |
| Sampling: temperature, top-p, top-k, seed, max tokens | Built, tested | `core` (`Chat.kt`), `engine` | Temperature ≤ 0 uses greedy decoding. |
| Chat template from the GGUF file | Built, unverified | `engine` | Falls back to ChatML when the file has no template. The fallback is a guess and needs checking. |
| GGUF header and metadata reading | Built, tested | `core` (`GgufReader.kt`) | Reads architecture, name, file type, context length, block and embedding counts. |
| Importing a GGUF file | Built, unverified | `runtime` (`ModelLibrary.kt`, `ModelsPanel.kt`) | File picker copies the file and validates it before it is listed. |
| Model downloads with resume and SHA-256 | Next | – | Import from the file picker works today. Downloading does not. |
| Model list, default model, delete | Built, unverified | `runtime` | Default is stored in preferences. |
| Chat screen | Built, unverified | `app` (`ChatScreen.kt`, `ChatViewModel.kt`) | Streams tokens. History lives in memory only. |
| Chat history persistence | Next | – | Cleared when the app process ends. |
| Foreground service for the server | Built, unverified | `runtime` (`ServerService.kt`) | Notification has a Stop action. Service type `specialUse`. |
| Headless router build | Built, unverified | `router` (`RouterActivity.kt`) | Same server and model code, with only the server and model screens. |
| Desktop GUIs (PySide, Tauri) | Built, unverified | `app` | Android uses Jetpack Compose. Not a port of either GUI. |
| Ollama `/api` endpoints | Desktop only | – | Not on Android yet. |
| Embeddings | Next | – | Needs an embedding path in the engine. |
| Speech (whisper, llama-tts) | Desktop only | – | |
| Images (stable-diffusion.cpp) | Desktop only | – | |
| Vision (ONNX) | Desktop only | – | |
| Knowledge Modules (`.mmkm`, ed25519, SQLite FTS) | Desktop only | – | |
| Knowledge Module factory | Desktop only | – | |
| Traces | Desktop only | – | |
| Misty agent | Desktop only | – | Reuse or copy from MobileAgents is still open (ADR-011). |
| MCP over stdio | Desktop only | – | |
| Dashboard at `/ui` | Desktop only | – | |
| Forge (Python/PyTorch), Rigel (HIP), Dusty experts | Desktop only | – | |

## Known limits

- No device has run this yet. The gate covers host tests and the APK build, not runtime behaviour on a phone.
- CPU-only inference on arm64-v8a. No GPU or NPU path.
- One model is loaded at a time.
- `llama_batch_get_one` is deprecated in the llama.cpp tree used here. It still compiles.
