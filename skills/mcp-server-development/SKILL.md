# MCP Server Development

**trigger**: Use when changing the Model Context Protocol server of ModelMistress, or when exposing a new hub operation to agents
**description**: How the `mcp` command serves the hub's operations as MCP tools over stdio, and how a scoped peer key gets only two of them

## Overview

ModelMistress has no MCP server of its own process. The `model-mistress mcp` command runs a JSON-RPC 2.0 loop on stdio
(one message per line) that calls the running hub over HTTP. The code is in `src/client.rs`:

- `serve_mcp(hub)` serves the owner's session: every operation the hub lists, with the owner's token.
- `serve_mcp_io(access, input, output)` is the loop itself, over any reader and writer. The CLI uses stdio; the tests use
  buffers.
- `McpAccess::Owner(&Client)` gives the owner's tools. `McpAccess::Peer(&Client)` gives a scoped peer key two tools only.
- `model-mistress mcp --peer` starts the peer session, with the key in `MODELMISTRESS_KEY`.

## Tools come from the hub

The owner's tool list is not written in the MCP code. `Client::operations()` reads `GET /v1/operations`, and each entry
becomes a tool:

- name: the operation id with `.` replaced by `_` (`model.load` is served as `model_load`), because MCP names allow only
  letters, digits, `_` and `-` (`tool_name` in `src/client.rs`).
- description: the operation's summary.
- input schema: the operation's `input_schema`.
- annotations: `readOnlyHint` is the inverse of the operation's `mutating` flag, and `destructiveHint` is its `destructive`
  flag.

`tools/call` looks the name up in that list and calls `Client::call(op, args)` (`POST /v1/call/<op>`). A name that is not
in the list answers `no tool <name>`. So adding an operation to the hub makes it an MCP tool with no change to the MCP code,
and the operation's own flags set how the client sees it.

Which operations are offered is a `ToolSet`: `Core` (the default, `mcp`) offers the nine operations in `CORE_OPS`, and `All`
(`mcp --all-tools`) offers every operation the hub lists. `tools/list` and `tools/call` both use the same set, so a tool
outside it answers `-32602` as if it did not exist. Add an operation to `CORE_OPS` only on purpose: each tool costs an
agent context on every turn.

## Peer keys get two tools

A scoped peer key sees only `models_list` (the models the key may use) and `chat_complete` (one answer, not streamed).
Their definitions are `peer_tools()` in `src/client.rs`, and the instructions the client sees say the owner does loading,
unloading, downloads and settings. The hub enforces the same limit on the server side: a peer key's requests to any other
route answer `403 forbidden` (see `peer_route` in `src/hub.rs`).

## Protocol

`initialize` returns the protocol version the client asked for (2024-11-05 by default), the `tools` capability, the server
name and version, and the instructions. `ping` answers `{}`, and `tools/list` returns the list above. A message without
an `id` is a notification and gets no reply. An unknown method answers `-32601`, an unknown tool `-32602`, and a line that
is not JSON `-32700`. A tool that fails returns a result with `isError: true` and the hub's error text, not a JSON-RPC error.

## Tests

`src/client.rs` tests drive `serve_mcp_io` with a buffer of requests, against a stand-in hub. Test the protocol through that
path, not by starting a process.

## Rules

1. An operation is a tool when the session's `ToolSet` includes it. Mark it `mutating` and `destructive` honestly: the client
   reads those flags for its own hints.
2. A peer key must not gain a route by accident. Add a route to `peer_route` only on purpose, and then also to `peer_tools()`.
3. Error text comes back as the hub's message. Keep it actionable; it is what the agent reads.

## Related

- `docs/agents.md`: what agents can and cannot do, and the open items.
- `src/hub.rs`: the operation list (`operations()`) and `peer_route`.
