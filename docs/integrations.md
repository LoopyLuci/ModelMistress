# Running ModelMistress on its own, and with ABP

## On its own

ModelMistress is one program. `model-mistress serve` starts the hub, and everything else (the CLI, the dashboard at
`/ui`, the desktop app, the MCP server, the OpenAI- and Ollama-compatible APIs) talks to that hub. It needs:

- nothing else installed to serve, chat, download and quantize models, when built with `--features bundled-engine`
  (llama.cpp is built into the binary; at run time only the GPU driver is needed);
- no Python and no Node, except for the optional Forge (training) and for converting Hugging Face weights to GGUF, which
  answer 501 with the steps when Python is not there;
- no ABP. Nothing in the program reads an ABP setting or path.

Checked on this machine: the hub started with a minimal environment (no PATH, no Python, no ABP or MM variables), reported
its built-in engine, and answered its API.

## With ABP

ABP runs ModelMistress through the module contract in [`abp-module.toml`](../abp-module.toml). ABP builds the binary,
starts the hub with `--home` set to the shared ModelMistress home, and reaches it through the control file the hub
writes there (`url`, `token`). The contract is the only coupling: ModelMistress does not depend on ABP, and ABP works
with a hub it did not start. Models, downloads and the control file live in the same home, so a standalone
`model-mistress` and ABP share one hub when they use the same home.

ABP's client uses these endpoints, all of which the hub serves:

| What ABP does | Endpoint |
|---|---|
| finds the hub, checks it is up | `GET /v1/health`, the control file |
| runs operations | `POST /v1/call/{op}` |
| lists models | `GET /v1/models` |
| chats | `POST /v1/chat/completions` |
| MCP tools for ABP's own agents | `model-mistress --home <home> mcp` (stdio) |

### Misty from ABP

ABP is a thin client of Misty: everything she needs lives in this hub. ABP's `bot/modelmistress/misty.py` calls the
`misty.*` operations; ABP's agents get the tools `misty_status`, `misty_ask`, `misty_search` and `misty_ingest`; the
dashboard API has `/api/modelmistress/misty/*`; and `abp misty status|ask|search|ingest|setup|doctor` is the CLI. ABP's
`ask` is read-only on purpose (a tool of hers that needs approval is refused), since ABP has its own approvals. By ABP's module contract
(`GET /v1/operations`, with `panel = "operations"` in the manifest) its generic module panel and its `module_call` tool should
also list the `misty.*` operations; that was not opened in a running ABP.
Through the OpenAI routes she is the model `modelmistress/misty`, but ABP's model picker builds its list from
`model.list`, which does not include her, so she is not in that picker. The ABP side was tested against a stand-in hub;
the pieces that need her models running have not been run end to end. See [misty/USER_GUIDE.md](misty/USER_GUIDE.md).

Checked against a live hub: ABP's client found the hub through `MM_HOME`, authenticated, and read the hub's status.
ABP's own provider registry (which marks ModelMistress as the provider to prefer) exists only while ABP runs, so that
check needs ABP running.

For a Claude session outside ABP, see [claude.md](claude.md). The plugin and ABP can use the same hub: the MCP server
with `--start` joins a hub that ABP started, and does not start a second one.
