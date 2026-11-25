# ModelMistress for Android

Two apps built from one codebase. Both run GGUF models on the device, offline, and serve them over an OpenAI-compatible API.

- **ModelMistress** (`com.modelmistress.app`): chat with a model on the phone, manage models, and optionally run the server.
- **ModelMistress Router** (`com.modelmistress.router`): headless. Only the server and the model library. Other apps on the device, or on the network if you allow it, send requests to it.

Status and gaps against the desktop app are in [docs/FEATURE_MAP.md](docs/FEATURE_MAP.md).

## Modules

| Module | What it holds | Tests |
|---|---|---|
| `core` | Pure JVM. JSON, GGUF header reader, OpenAI request and response shapes, model router, UTF-8 chunker. No Android. | `:core:test` |
| `server` | Pure JVM HTTP/1.1 server. Routes, bearer auth, SSE streaming. Depends on `core`. | `:server:test` |
| `engine` | Android library. llama.cpp through JNI (arm64-v8a only). Compiles the llama.cpp tree at `../engines/llama.cpp`. | Native build passes in the gate. No unit tests; needs a device run. |
| `runtime` | Android library. Model library, engine hub, server lifecycle, foreground service, Compose panels for models and server. | Not yet |
| `app` | The chat app: Chat, Models and Server tabs. | Not yet |
| `router` | The headless router app: server and models only. | Not yet |

Dependency direction: `app` and `router` depend on `runtime`. `runtime` depends on `core`, `server` and `engine`. `server` depends on `core`.

## Build

Run from `android/`. The first build needs the Android SDK with NDK 27.2.12479018 and CMake 3.22.1, and a JDK 17.

Host tests (no Android SDK needed beyond Gradle):

```bash
./gradlew :core:test :server:test
```

Debug APKs:

```bash
./gradlew :app:assembleDebug
./gradlew :router:assembleDebug
```

The APKs are written to `app/build/outputs/apk/debug/` and `router/build/outputs/apk/debug/`.

The native engine builds through CMake from `engine/src/main/cpp`. It needs the llama.cpp source at `../engines/llama.cpp` (the desktop app's copy). The build stops with an error if that folder is missing.

## Use

1. Install the app and open the Models tab. Import a `.gguf` file with the file picker. The file is copied into app storage and checked before it is listed.
2. Choose a default model with "Use by default".
3. In the Chat tab, pick a model and send a message.
4. In the Server tab, set the port. Leave "Allow other devices on this network" off unless another device needs it. Press Start. The base URL and API key are shown on the screen.

Client example, with the server running on the same phone:

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H "Authorization: Bearer <api key>" \
  -H "Content-Type: application/json" \
  -d '{"model":"auto","messages":[{"role":"user","content":"Hello"}]}'
```

With "Allow other devices" on, the server listens on all interfaces and still requires the key. The key is not sent anywhere else.

## Security defaults

- The server binds to `127.0.0.1` unless you turn on network access.
- Every request except `GET /v1/health` needs `Authorization: Bearer <key>`. The key is generated on first run and can be rotated in the Server tab.
- No CORS headers. Browser pages on other sites cannot call the server.
- Request bodies are limited to 4 MB. Headers to 16 KB.

## Status

- Host tests pass for `core` and `server`: 25 tests, 0 failures. Results are in `build/test-results/` in each module.
- The gate `./gradlew --offline :core:test :server:test :app:assembleDebug :router:assembleDebug` passes. That includes the native llama.cpp build and both debug APKs.
- Nothing has been run on a phone yet. Treat the Android apps as built but not verified until a device run is done.
- Not on Android yet: model downloads, embeddings, the Ollama API, speech, images, vision, Knowledge Modules, traces, the Misty agent, and MCP. The feature map lists each one.

## Known limits

- CPU only, arm64-v8a only.
- One model loaded at a time. Switching models reloads it, which takes time and memory.
- No chat history saved. It is cleared when the app process ends.
- The chat template fallback (ChatML) is used when a GGUF file has no template. Check replies for models that need another format.
- `llama_batch_get_one` is deprecated in the llama.cpp version used here. It still compiles, and the warning is expected.
