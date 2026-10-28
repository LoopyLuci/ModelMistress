# ModelMistress gap analysis

What is missing or rough in the hub, the dashboard (`frontend/`) and the Python window (`gui/`), with the code
that shows each one. "Open" means nothing has been built yet; "in progress" means this change addresses it. The
agent-facing list is in [agents.md](agents.md); this page covers the whole application.

How it was checked: the operations the hub serves, the routes and views of the dashboard, the pages and calls of
the Python window, and the code paths named below. The dashboard and the Python window were read, not run against
a live hub, except where the window was launched on the owner's hub.

## Hub (backend)

| Gap | Evidence | Status |
|---|---|---|
| **Key changes leave no audit trail.** Creating and revoking peer keys writes `keys.json` and nothing else. Keys changed during a restart left no log line; only the timestamps in the file show it. | `src/hosting.rs` (`save_keys`), hub log has no key events | **Done.** `keys.json` keeps every change with its time, key, peer and who made it (`hub` or `offline`); `model-mistress keys log` shows them. |
| **Offline key writes are allowed while no hub is running.** The write guard only refuses when a live hub that does not answer owns the home. Between a stop and a start there is no control file, so the write goes through. | `refuse_key_write_under_a_live_hub` in `src/bin/model-mistress/main.rs` | **Done (guarded).** Offline writes with no hub running still go through, as the docs say, and are logged as `offline`. The loss was a write being overwritten: a hub reads `keys.json` before each key change and as it starts serving, so no change is undone, and an unreadable file refuses changes. Residual: a running hub does not re-read `keys.json` per request, so an offline revoke reaches it at its next key change or restart. The offline commands refuse to write while a hub that does not answer runs. |
| **A full set of slots queues requests instead of answering `busy`.** `busy` only appears when llama-server refuses. | `passthrough` in `src/hub.rs`; llama-server answers `busy` only from `/slots?fail_on_no_slot=1` | Open (fail-fast opt-in) |
| **Queue depth is not reported.** | `service.status` and `model.loaded` have no queue field | Open |
| **No default embedding model setting.** Callers must name the model on each call. | `/v1/embeddings` in `src/hub.rs` | Open |
| **No model aliases.** Presets give stable names, but there is no alias list or column in `model_list`. | `model_list` output | Open |
| **Batch jobs.** A prompt batch with a job id that survives a crash. Jobs exist only for downloads and quantization. | `job.*` operations | Open |
| **No thread cap and no hardware-error gate.** Threads are set per load only. | `threads` in the load options; no global check | Open |
| **No approval queue for loads.** Peer keys cannot load, but nothing lets the owner approve a request. | `peer_route` in `src/hub.rs` | Open |
| **Named key scopes.** A key reaches every model route or none. | `peer_route` | Open |
| **Error codes do not cover every path.** Chat, completions, embeddings and rerank map llama-server's errors, and so do the Ollama shim and the `embeddings.create` and `rerank.run` operations now. About 57 sites still return a generic `500 io` (storage, model files and the like). | `src/ollama_api.rs`, `forward_json` in `src/hub.rs` | **Done for llama-server's errors.** The `500 io` sites are open. |
| **MCP exposes every operation as a tool** (about 94), which costs context on every turn. | `hub.rs` operations; `docs/claude.md` | Open (tool profile) |

## Dashboard (`frontend/`)

| Gap | Evidence | Status |
|---|---|---|
| **No page for peer keys or usage.** Both are only in the CLI and the API, although the Python window's README says the dashboard has them. | No keys or usage view in `frontend/src/views` or the router | Open |
| **The theme state is duplicated and partly dead.** `App.vue` keeps the theme in `localStorage`; the store has its own `theme` and `toggleTheme`, which nothing calls. | `frontend/src/store/index.ts`, `frontend/src/App.vue` | Open |
| **No style or text-size setting.** Only light and dark exist. | `frontend/src/styles/global.css` | Open |

## Python window (`gui/`)

| Gap | Evidence | Status |
|---|---|---|
| **No theme or style setting.** The window always uses the platform's default look. | `gui/mm_gui/app.py` | **Done.** Settings, Appearance: theme (follow the system, light, dark), style (Fusion by default, or another Qt style) and text size. Kept per user, never sent to the hub. |
| **The Settings page is a flat table of raw keys.** No groups, no filter, no help text. | `SettingsPage` in `gui/mm_gui/pages.py` | **Partly.** The Appearance group and a filter are in. The hub settings are still one flat table with no help text. |
| **Pulling models is missing.** | `model.pull` exists in the hub; the window has no call for it | Open |
| **Loading takes no options.** No context size, GPU layers or threads. | `ModelsPage._load` | Open |
| **No model details and no delete.** `model.show` and `model.info` are not called. | `gui/mm_gui/hub.py` | Open |
| **Chat: Enter inserts a newline and only the Send button sends.** | `ChatPage` | **Done.** Ctrl+Enter sends; Enter still inserts a newline. |
| **Chat re-renders the whole transcript on every piece of a reply.** This slows long replies. | `ChatPage._render`, called from `_render_timer` | Open |
| **Chat has no sampling controls, no copy and no saved conversations.** | `ChatPage` | Open |
| **Status is refreshed only by hand.** Opening the page does not refresh it. | `StatusPage` | **Done.** It refreshes when opened. |
| **No page shortcuts, and the window size is not remembered.** | `gui/mm_gui/app.py` | **Done.** Ctrl+1 to Ctrl+4 switch pages; the size and place are remembered. |
| **The README says the dashboard has keys and usage.** It does not. | `gui/README.md` | **Done.** The README is corrected. |

## What this does not cover

- The desktop app's shell (`desktop-app/`) was checked only for how it loads the dashboard (`frontendDist`).
- Forge, speech, images, vision, knowledge modules, factory and residency were not reviewed beyond their routes.
