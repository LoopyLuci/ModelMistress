# The desktop app

`desktop-app/` is a Tauri 2 shell (Vue frontend in `frontend/`) over the hub's API. It has no model server of its own and
does not talk to Ollama: models, loading, downloads, chat (streamed), speech, peer keys and usage all go through the hub
with its token.

## Install (Windows)

Run `ModelMistress_<version>_x64-setup.exe` (per-user, no administrator rights; silent: `/S`, folder: `/D=C:\path`).
An `.msi` is built next to it. The installer puts two programs in the install folder: the app and `model-mistress.exe`,
the hub. Uninstall from Windows "Apps" (or the `uninstall.exe` in the install folder); it leaves your models and the hub
home alone.

## What it starts

1. It looks for a hub that already runs: `<home>/control.json` (`MM_HOME`, else `%LOCALAPPDATA%\ModelMistress`) and
   `GET /v1/health`. A hub found is used and **left running** when the app quits.
2. Otherwise it starts the `model-mistress.exe` next to the app (`serve --port 0`, loopback only, no console window),
   waits for `/v1/health` (up to 90 s), and **stops that hub when the app quits** (gracefully, so models unload).
   Set `{"stop_hub_on_quit": false}` in `<home>/desktop.json`, or `MM_STOP_HUB_ON_QUIT=0`, to keep it running.
3. A development build without the hub next to it runs the hub inside the app.

While it starts the window shows "Starting the ModelMistress hub..."; if it cannot start, the window shows the reason
(with the tail of `<home>/desktop-hub.log`) and a "Try again" button. The tray icon has Open, Stop hub (only a hub the
app started) and Quit.

## Files

`<home>` holds `control.json`, settings, `desktop-hub.log` (the hub's output when the app started it), `desktop-app.log` and (unless configured otherwise) downloads. The app itself
writes nothing else; the webview's own cache lives under `%LOCALAPPDATA%\dev.modelmistress.app`.

## For the frontend

`frontend/src/transport.ts` is the one place that talks to the hub. In the app, `hub_info` returns `{url, token, hub_url}`
and the page calls the hub with `fetch`: `hubFetch(path, init)` and `sse(path, body, onEvent, signal)` (chat
`stream:true`, `/v1/native/stream`, job progress) work the same in the app and in a browser. `call(op, args)` goes
through the `hub_call` command. `url` is a loopback relay inside the app (`proxy.rs`): it forwards to the hub, streams included, and adds CORS headers
for the app's own origin only, because the webview's origin (`http://tauri.localhost`) is not the hub's. The content
security policy lets the webview connect to `http://127.0.0.1:*` only.

Other commands: `execute_command` (slash commands), `hub_models`, `hub_chat_stream` (`chat-delta` events),
`hub_status`, `retry_hub`.

## Build

```
cd frontend && npm ci && npm run build
cargo build --release -p model-mistress --features bundled-engine
copy target\release\model-mistress.exe desktop-app\binaries\model-mistress-x86_64-pc-windows-msvc.exe
cd desktop-app && cargo tauri build --bundles nsis,msi
```
The installers appear under `target/release/bundle/`. NSIS and WiX are fetched by `cargo tauri` on first use.

## Tests

`cargo test -p model-mistress-desktop`: the lifecycle decisions (`hub_proc`) and the app's `api`, `commands` and
`config` modules, which start a hub in the test process on a free port with a stand-in for `llama-server`.
