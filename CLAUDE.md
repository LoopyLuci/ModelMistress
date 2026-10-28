# ModelMistress: notes for agents

Read this first, then the documents it names. The repository is public: keep secrets, machine paths and user names out
of tracked files.

## What this is

A standalone local model hub written in Rust (`src/`). It serves an OpenAI-compatible API under `/v1` and an
Ollama-compatible API under `/api`, and it has a control API (every operation is also an MCP tool). The binary is
`model-mistress`; llama.cpp is built in with `--features bundled-engine`. Other parts: a Tauri desktop app
(`desktop-app/`), a dashboard (`frontend/`), a Python desktop window (`gui/`, PySide6) and a Knowledge Module factory.

## Where things are

| Need | Read |
|---|---|
| What works, and when it was last measured | `docs/STATUS.md` |
| What is still open, with the code that shows it | `docs/gap-analysis.md` |
| Each suggestion from the agent that uses the hub, and its status | `docs/agents.md` |
| Peer keys, quotas, the key log, revocation | `docs/hosting.md` |
| The Ollama-compatible API | `docs/STATUS.md` and `docs/agents.md` (row 7: error codes) |
| What changed and is not released yet | `CHANGELOG.md`, the Unreleased section |
| The original design (not fully re-audited) | `DESIGN.md` |

## Build and test

```bash
cargo test --lib                          # the library suite (about 400 tests)
cargo fmt --check
cargo clippy --all-targets -- -D warnings
python ci/pipeline.py                     # the gates the pre-push hook runs, plus the dashboard, GUI and a smoke test
python ci/pipeline.py --only gui          # the GUI tests; MM_GUI_PYTHON names a Python that has the pinned PySide6 and pytest
cargo build --release -p model-mistress --features bundled-engine   # builds llama.cpp in (minutes the first time)
```

Set `CARGO_TARGET_DIR` to build somewhere else. Every change to `src/` ships with a test, and docs and `CHANGELOG.md`
change in the same commit.

## Pushing

- `python ci/pipeline.py --install-hook` installs a pre-push hook that runs the pipeline. A push takes several minutes;
  a failed gate stops it. Push only when the owner asks, and never with `--no-verify`.
- A hub that is running holds its release binary open. When the build folder is the one that hub runs from, the
  pipeline's build stage fails with "Access is denied". Stop the hub, or set `CARGO_TARGET_DIR` to a different folder
  for the push.
- The hook reads the environment: `CARGO_TARGET_DIR` and `MM_GUI_PYTHON`. `MM_SKIP_PIPELINE=1` skips the hook once; do not
  use it to get a push through.

## The hub at runtime

- Home folder: `--home`, else `MM_HOME`, else the platform default. Inside it: `config.toml`, `keys.json`, `usage.json`,
  `presets.json`, `models/`, and `control.json` while the hub runs.
- `control.json` holds the hub's URL, its process id and the owner's token. It is a secret. Read the URL and the process
  id from it when you need them; never print the token or put it in a file that is committed.
- `keys.json` holds the peer keys as SHA-256 hashes, and a `events` list of every key change with its time and `via`
  (`hub` or `offline`). `model-mistress keys log` prints the list. Do not edit `keys.json` by hand while a hub runs.
- Errors: on `/v1` the body is `{"error": {"code", "message", "type"}}`; on `/api` it is `{"error": message, "code"}`.
  The stable codes, and the ones still generic, are listed in `docs/agents.md`.
- A peer key reaches the model routes only. The owner's token is accepted from this machine only.

## Conventions

- Commit messages: `area: summary`, a lowercase area (`keys`, `errors`, `gui`, `docs`), then a body that says why.
- Do not remove a screen, option or command without the owner's decision.
- `ci/reports/` is written by the pipeline; leave it out of commits unless the change is about it.
- `engines/llama.cpp` is a submodule. Do not commit changes inside it from here.

## Open decisions (owner)

- Thread cap and hardware-error gate: the owner must say what counts as a hardware error.
- Approval queue for loads that a peer asks for, named key scopes, and a default embedding model.

`docs/gap-analysis.md` has the full list, with the evidence for each item.
