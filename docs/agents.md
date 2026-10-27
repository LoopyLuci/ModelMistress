# ModelMistress for agents: what exists, what this changed, what is still open

This answers a list of suggestions from an agent that used the hub for a long summary pass. Each item says where it
stands now. "Done" means it works and was checked on this machine; "partly" names what is missing.

| # | Suggestion | Status |
|---|---|---|
| 1 | Find the hub somewhere Windows does not redirect; say which URL and file were checked | **Done.** `MODELMISTRESS_URL` with `MODELMISTRESS_TOKEN` names the hub directly, with no file needed. `MM_HOME` (or `--home`) moves the control file to any folder, for example one on another drive. Every error names the URL, the file and the variable that would fix it. A fixed port is `serve --port N`, used with `MODELMISTRESS_URL`. |
| 2 | Scoped agent tokens: chat and models, no load, unload, pull or settings; revocable; usage counted | **Partly.** Peer keys already exist (`keys create`): they reach the model routes only (`/v1/models`, chat, completions, embeddings, rerank, audio) and every other route answers `403 forbidden`. Revocation (`keys revoke`) and usage (`usage`) exist. **Done now:** an MCP server that uses a peer key, `mcp --peer` with the key in `MODELMISTRESS_KEY`, which exposes `models_list` and `chat_complete` only. **Missing:** named scopes (`--scope chat,models`) and a `token` alias; today a key is all model routes. |
| 3 | Work without ABP; ABP's relay as an optional extra | **Done, checked.** The hub ran with a minimal environment (no PATH, no Python, no ABP or MM variables). ABP's own client found the hub through `MM_HOME` and authenticated. ABP's relay lives in ABP's repository, not here. |
| 4 | Batch jobs: submit prompts, get a job ID, run in order at a set concurrency, survive disconnects and crashes, check progress and fetch results | **Not done.** The hub has jobs for downloads and quantization (`job.get`, `job.cancel`), but no batch of prompts. Next step: a `batch.submit` operation whose per-item results are written to disk as they finish, so a crash loses only the item in progress. |
| 5 | A status call that shows loaded models, context size, GPU memory, queue depth and uptime | **Partly.** `service.status` (MCP `service_status`, CLI `status`) reports uptime, memory and loaded models; `model.loaded` reports context, slots, KV cache, VRAM and uptime per model. **Missing:** queue depth and aliases in that report. |
| 6 | Short, stable model aliases such as `coder` | **Partly.** Presets (`create` from a Modelfile, `cp`) already give a stable name for a GGUF file. **Missing:** a documented alias list and a `model_list` column that shows it. |
| 7 | Clear error codes: `model_not_loaded`, `busy` with a retry time, `unauthorized`, `context_exceeded` with the limit | **Mostly done.** Every MCP and CLI error reads `code: message`. Chat, completions, embeddings and rerank map llama-server's own errors to stable codes: a prompt over the context window is `context_exceeded` (400, with `n_prompt_tokens` and `n_ctx`), a server still loading is `loading`, and one that reports itself unavailable is `busy` (both 503, with `retry_after_s` and a `Retry-After` header). **Still open:** a full set of slots queues a request rather than answering `busy`, so a fail-fast option is the next step. `not_loaded` stays the name for a model that is not loaded, because renaming it would break clients. Most other failures are still a generic `500 io`. |
| 8 | Structured output (JSON schema or grammar) on chat | **Not verified.** The hub passes chat bodies through to llama-server, which supports JSON schema, but no chat model is installed on the machine used here, so it was not run. |
| 9 | Embeddings with a named default model, for search | **Partly.** `/v1/embeddings` and `embeddings.create` exist and load the model on demand. **Missing:** a default embedding model setting and an MCP tool for it. |
| 10 | Loads and unloads that the owner approves in the dashboard | **Not done.** Peer keys cannot load or unload today, so an agent cannot load alone. An approval queue (request, then approve in `/ui`) is the next step. |
| 11 | Limit CPU threads, and refuse batch work while the machine shows hardware errors | **Not done.** Threads can be set per load (`threads`), but there is no global cap, and no hardware-error check exists. |

## Other gaps found while checking

- **MCP stdio hang (fixed).** A hub started by `mcp --start` inherited the MCP process's stdout pipe, so an MCP client
  would have waited for the hub to exit. The standard handles are now made non-inheritable before the hub starts.
- **Tool count.** The MCP server exposes every hub operation (94 tools). That costs an agent context on every turn. A
  smaller tool profile is the next step.
- **Stale skill.** `skills/mcp-server-development/SKILL.md` describes a `McpServer` struct that the working server does
  not use; it should be rewritten from `src/client.rs`.
- **Smoke test model.** The pipeline's live-chat check picks the smallest chat model of 2 GB or less (never an embedding
  or reranking model), loads it on the CPU with a 2,048-token context, and sends a real chat and a streamed one. It runs
  on the CPU so that another hub holding the GPU's memory does not fail it. The owner's own config lists no model folder,
  so the default run reports the check as skipped; `MM_SMOKE_CONFIG` names a config copy that adds one.

## Using a scoped key

```bash
model-mistress keys create claude-code --requests-per-min 60
```

The command prints the key once (it starts with `mmk_`). Keep it in the environment of the agent, not in a file that
is committed:

```bash
export MODELMISTRESS_KEY=mmk_...
model-mistress mcp --peer
```

To reach a hub that is not in this machine's home folder, also set `MODELMISTRESS_URL` to its address.
