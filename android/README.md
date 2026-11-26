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
| `engine` | Android library. llama.cpp through JNI (arm64-v8a only). Compiles the llama.cpp tree at `../engines/llama.cpp`. | Native build passes in the gate and runs on device (Release, dotprod). No unit tests of its own. |
| `litert` | Android library. LiteRT-LM (`.litertlm` chat models) through Google's `litertlm-android` library. It has no native build of its own. | Built in the gate. Not run on device yet. |
| `sd` | Android library. stable-diffusion.cpp through JNI (arm64-v8a only). The source is `engines/stable-diffusion.cpp`, built with hidden symbols. | Native build passes in the gate. Not run on device yet. |
| `runtime` | Android library. Model library, engine hub, image hub, server lifecycle, foreground service, Compose panels for models, desktops and server. | Tested on device |
| `app` | The chat app: Chat, Models and Server tabs. | Tested on device |
| `router` | The headless router app: server and models only. | Tested on device |

Dependency direction: `app` and `router` depend on `runtime`. `runtime` depends on `core`, `server`, `engine`, `litert`, `sd` and `cactus`. `server` depends on `core`.

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

## Desktops (models on another machine)

Both apps can use the models of a ModelMistress desktop hub that you own, from home or away, as well as the models on the phone. The hub's models appear in the chat and in the router's `/v1/models` list, named `<desktop>@<model>`.

There are two ways to pair a desktop. The first needs no address and no firewall rule, and it is built to work from any network. The phone check ran it over the wireless adb link only, so any-network reachability (relays) is untested (see Known limits). The hub stays on the desktop's loopback; the Transferd overlay carries it to paired devices only.

Through the Transferd overlay, on the desktop:

```bash
model-mistress keys create phone
mm-overlay serve --home <folder> --hub 127.0.0.1:<hub port> --name home --invite-ttl 900
```

Copy the key the first command prints; it is shown once. The second command prints an invite. `<hub port>` is the port in the hub's `control.json`. `mm-overlay` is in `overlay/` (see `overlay/README.md`). Each invite pairs one phone; type `invite` in the running window for another.

On the phone, open Models, then Desktops. Under "Add through the Transferd overlay", enter a name, the invite and the key, then press Pair. Press Check. Restart the app if it asks you to, to register on the relay the desktop names.

By address (LAN or Tailscale), the older way: the hub must listen on the network with `allow_network = true` in its `config.toml`:

```bash
model-mistress serve --host 0.0.0.0 --port 8765
```

Allow that port for the Private network in Windows Firewall before the phone can connect. The firewall stays closed until you do. On the phone, open Models, then Desktops, and enter a name, the address (for example `http://100.101.98.77:8765` over Tailscale, or `http://192.168.1.20:8765` on the same Wi-Fi), and the key. Press Check. Over Tailscale, keep Tailscale connected on the phone.

## Models on the test phone

Measured on a MediaTek MT6877 phone (2 big Cortex-A78 cores, 6 Cortex-A55, dotprod, no i8mm, 7.7 GB RAM). Same prompt, 128 new tokens, 4 threads, Release build with dotprod:

| Model (file) | Size | First token | Speed | Notes |
|---|---|---|---|---|
| LFM2.5-1.2B-Instruct QAD Q4_0 | 0.66 GB | 1.1 s | ~16 tok/s | Fast, short answers. |
| LFM2-2.6B Q4_0 | 1.4 GB | 3.4 s | ~8 tok/s | Best answer quality of the three. Recommended default. |
| Bonsai-8B Q1_0 | 1.1 GB | 9.7 s | ~3.6 tok/s | Thinks out loud (`<think>`) by default, so it uses up its token budget. |

The first build took more than eight minutes without a first token. Two changes fixed it: the native engine is built optimised, and it uses dotprod. The two were not measured separately.

## Security defaults

- The server binds to `127.0.0.1` unless you turn on network access.
- Every request except `GET /v1/health` needs `Authorization: Bearer <key>`. The key is generated on first run and can be rotated in the Server tab.
- No CORS headers. Browser pages on other sites cannot call the server.
- Request bodies are limited to 4 MB. Headers to 16 KB.

## Status

- Host tests pass for `core` and `server`: 213 tests in 32 suites (`core` 194 in 29, `server` 19 in 3). The last `core` run had 0 failures. The new ones cover chat history, Hugging Face listing and GGUF choice, and downloads against a local server. They include the desktop client, run against a real server on loopback, and the overlay routes (through a stand-in for the overlay endpoint). Results are in `build/test-results/` in each module.
- The gate `./gradlew --offline :core:test :server:test :app:assembleDebug :router:assembleDebug` passes. That includes the native llama.cpp build, the Transferd overlay library, and both debug APKs.
- Run on one phone: install, model import, chat, the server, the router, and a paired desktop's models (through a USB port tunnel). The Transferd overlay path also ran on this phone: pairing by invite, Check, and a desktop model's chat reply, over the wireless adb link (see Known limits). The address route over Tailscale and the LAN is not yet tested.
- Built but not yet run on the phone: the chat history's delete. The Hugging Face repo listing, the already-installed refusal, and a full download of LFM2.5-1.2B-Instruct-Q4_K_M.gguf (697 MB) did run. Hugging Face downloads cover single-file GGUF models only.
- Tools (the clock, the calculator, workspace files to read, create and edit, the device status and the models list) are built and host-tested. A question is offered only the tools its wording points at, so a greeting gets none. On the phone, a greeting and the clock question were checked. A file write and a file edit each ran after an Allow dialog that matched the request.
- LiteRT-LM chat models and image generation (`POST /v1/images/generations`, Stable Diffusion checkpoints) are built and host-tested. Neither has run on the phone yet: the LiteRT check needs a `.litertlm` model file, and image generation needs a Stable Diffusion checkpoint.
- Chat attachments (a text file, a PDF, a picture, or a camera photo, read on the phone with ML Kit), the mic and read-aloud, the chat's auto-scroll, and the device benchmark with model suggestions are built. On the phone: a text file and a camera photo were attached and the model answered from them; the mic opened the speech recognizer; scrolling up during a streaming reply paused it, and Jump to latest resumed it; the benchmark ran for four models and produced suggestions. Not yet checked on the phone: a PDF attachment, dictation of spoken words, and read aloud. The feature map has the details.
- Misty, the chat agent, is built and host-tested, with Needle 3 as its reflex. On the phone, a greeting got no tool, the clock tool answered, and a write and an edit each showed an Allow dialog and ran when allowed. A refused write is reported as not done, and a reply that claims a file change with no tool behind it is replaced. Rating a reply and clearing the ratings were checked on the phone. Open issues are listed in the feature map. It is off until the owner turns it on in Models, and file writes ask each time. Not on Android yet: skills (Knowledge Modules), the Whistle and Mirage roles (audio and images), embeddings, the Ollama API, speech models (Whisper, Whistle), vision, traces, and MCP. Dictation uses the phone's own speech recognizer. The feature map lists each one.

## Known limits

- CPU only, arm64-v8a only. The native build needs a CPU with dotprod (ARMv8.2 or later).
- The Transferd overlay path ran on the test phone (Redmi Note 12 Pro 5G, Android 14): pairing by invite, Check reached the desktop's 3 models, and a desktop model answered in chat. The phone reached the overlay through `adb reverse` over the wireless adb link, not a separate network. Any-network reachability (relays, a separate network) is untested. After pairing, the chat's model list showed the desktop's models only after the app was restarted. On the PC: a two-node loopback test (`overlay/tests/loopback.rs`) and the Android JNI bridge called from Java.
- Relays are not verified. The overlay's relays use UDP, which `adb reverse` does not carry, so the path from any network through a relay is untested. Without a relay, the phone reaches the desktop only through a direct address the invite carries (in the tests, a loopback address behind a tunnel).
- Desktop access by address is plain HTTP. Use it only on a network you trust (Tailscale or a LAN).
- The UTF-8 handling in the reply stream is not yet verified. A reply can contain a replacement character (�) where a multi-byte character splits across tokens.
- One chat model loaded at a time. Switching models reloads it, which takes time and memory. One image model is loaded at a time, alongside it.
- Image generation takes minutes per image on a phone's CPU. The image route makes up to four images per request.
- LiteRT-LM replies report zero token counts, because the runtime does not report them through this path.
- Misty uses one brain, the selected chat model. Needle loads once per process and cannot be unloaded, so it stays in memory after its first use.
- Chat history is kept on the device, one file per chat, and is not synced anywhere. Deleting the app's data deletes it.
- Low memory: with a chat model loaded, the app uses about 3 GB. If another app takes the screen (the speech recognizer or the camera), Android can end the app, and its draft, attachments and any reply in progress are lost. Saved chats are kept. Seen once on the test phone.
- The benchmark's speed estimate for models that were not run comes from file size, and it under-predicts the 1-bit Bonsai model. "Best balance" can name a model that is not installed.
- The chat template fallback (ChatML) is used when a GGUF file has no template. Check replies for models that need another format.
- `llama_batch_get_one` is deprecated in the llama.cpp version used here. It still compiles, and the warning is expected.
