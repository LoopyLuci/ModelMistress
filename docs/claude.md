# ModelMistress with Claude

Claude can use the models ModelMistress serves on this machine through its MCP server. The server exposes every hub
operation as a tool (`model_list`, `model_load`, `model_unload`, `chat_complete`, `service_status`, ...), and the Claude
Code plugin adds slash commands on top.

Both need the `model-mistress` program on the PATH. Build it with `cargo build --release -p model-mistress --features
bundled-engine` and put `target/release` on the PATH, or use the full path to the program in the commands below.

## In any project: the plugin

In Claude Code, add this repository as a marketplace and install the plugin once. It then works in every project:

```text
/plugin marketplace add LoopyLuci/ModelMistress
/plugin install model-mistress@model-mistress
```

The plugin gives Claude:

- the MCP server, which starts a hub in the background when none is running (`model-mistress mcp --start`);
- the slash commands `/model-mistress:status`, `/model-mistress:models`, `/model-mistress:load <model>`,
  `/model-mistress:unload <model or *>`, `/model-mistress:chat <model> <question>` and `/model-mistress:help`;
- a skill that tells Claude when a local model is the right choice and how to use the tools.

## One project, without the plugin

Run this in the project folder. It writes the server into the project's `.mcp.json`, so every session there has it:

```bash
claude mcp add --scope project model-mistress -- model-mistress mcp --start
```

To keep it to your own machine instead, leave out `--scope project` (the default is local to the folder).

## Scoped access: list and chat only

An agent that should not load, unload or change anything can use a peer key instead of the owner's token. Create the key
once, then start the server with `--peer`; it exposes two tools, `models_list` and `chat_complete`:

```bash
model-mistress keys create claude-code --requests-per-min 60
claude mcp add --scope project model-mistress-chat -- model-mistress mcp --peer
```

The key is read from the `MODELMISTRESS_KEY` environment variable, not from the command line, so set it in the
environment Claude starts in. Revoke it with `model-mistress keys revoke <name>`, and see what it used with `model-mistress usage`. See [agents.md](agents.md) for what else is and is not covered yet.

## Other MCP clients

Any client that reads the `mcpServers` format can start the same server. For Claude Desktop, add this to its own
configuration file (it is your file, so edit it yourself):

```json
{
  "mcpServers": {
    "model-mistress": {
      "command": "model-mistress",
      "args": ["mcp", "--start"]
    }
  }
}
```

## How the hub is found

The server finds the hub through its control file in the ModelMistress home folder: `MM_HOME` when it is set, else the
platform folder. Pass `--home <folder>` before `mcp` to choose another home. Without `--start`, the server needs a hub
that is already running and exits with a message when none answers.

A hub that `--start` launches keeps running after the session ends, so the models stay loaded for the next one. Stop it
with `model-mistress stop`, which also unloads every model.
