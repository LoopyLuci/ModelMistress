# The desktop app

`desktop-app/` is a Tauri shell (Vue frontend in `frontend/`) over the hub's API. It has no model server of its own and
does not talk to Ollama: models, loading, downloads, chat (streamed), speech, peer keys and usage all go through
`model_mistress::client::Client` with the hub's token.

- **Finding the hub.** `HubLink` (`desktop-app/src/api.rs`) reads `<home>/control.json` (`MM_HOME`, else the standard
  folder) and checks `/v1/health`. If no hub runs, the app starts one inside itself, the same hub `model-mistress serve`
  runs, on a free loopback port; it stops with the app.
- **Window commands** (`invoke` from the frontend): `execute_command` (slash commands: `/models`, `/scan`, `/load`,
  `/unload`, `/pull`, `/job`, `/rm`, `/search`, `/infer`, `/transcribe`, `/speak`, `/keys`, `/usage`, `/status`,
  `/health`), `hub_call` (any operation by id, for example `model.pull`, `speech.transcribe`, `key.create`),
  `hub_models` and `hub_chat_stream` (pieces arrive as `chat-delta` events).
- **Tests.** The typed client is tested in the `model-mistress` crate; the app's `api`, `commands` and `config`
  modules have tests that start a hub in the test process on a free port, with a stand-in for `llama-server`
  (`model_mistress::testing`): `cargo test -p model-mistress-desktop` (it needs Tauri's build dependencies).
