# Observability: the model's thoughts

Implements `architecture.md` §2 and the first two pages of §6 (Thoughts, Traces). Code: `src/trace/` (Rust),
`frontend/` (Vue). Everything here is owner-only: the routes are not in the peer allowlist, and a peer's chat
requests are never traced.

## What is recorded

For a traced chat request (`/v1/chat/completions`, a model of ours; an Ollama model cannot give log probabilities):

- the request, the prompt, and the prompt after the model's chat template (llama-server's `/apply-template`);
- the **reasoning kept apart from the answer**: from `reasoning_content` when the server separates it, else from
  `<think>...</think>` in the text; each token is marked `reasoning` or `answer`;
- **every token**: text, probability, the top-k alternatives with probabilities, and entropy (nats, over the returned
  top-k; the tail beyond k is not counted). The token id is stored when the server gives one (the OpenAI-style
  answer does not, so it is empty today);
- timings from llama-server (prompt tokens and ms, generated tokens and ms, tokens per second) and the wall clock;
- **stages**: `prepare` (model load, template), `prompt_eval`, `generation` (for a non-streamed answer the two
  halves are derived from the server's timings);
- the **adapters** (LoRA) on the model's server, with the scale each had for this request;
- **resources** at the start and the end: RAM of the hub and of each model server, free RAM of the machine. GPU
  memory is left out: llama-server does not report it, and a guess would mislead.

Tracing does not change the answer. The hub asks llama-server for `logprobs`/`top_logprobs` and removes from the
answer (streamed or not) whatever the client did not ask for; a client that asked for fewer alternatives than the
trace's `top_k` gets only its number. With tracing off nothing of this runs: the only cost per request is looking
for the `mm_trace` field, the header and one atomic flag.

## Turning it on

- Setting `enabled` (`trace.settings`, or the Traces page): every owner chat request is traced.
- Per request: `"mm_trace": true` in the body, or the header `x-mm-trace: 1`. `"mm_trace": false` opts one out.
- A request that names a job or task (`"mm_job"`, `"mm_task"`, or headers `x-mm-job`, `x-mm-task`) is traced
  unless `mm_trace` is false: this is how the KM factory's model calls (`km-factory.md` §2) are recorded.
  These fields are removed before the request reaches the model.

Settings (`<home>/trace/settings.json`): `enabled`, `top_k` (alternatives per token, 1 to 20, default 8), `routing` (record MoE expert routing, default off),
`max_age_days` (default 30, 0 = keep), `max_total_mb` (default 2048, 0 = no limit). Retention deletes traces older
than the age, then the oldest until the stored size fits; a running trace is never deleted.

## Storage

`<home>/trace/traces.db` (SQLite, WAL): tables `traces`, `stages`, `events`, `specialist_calls`. Per-token data is in
`<home>/trace/segments/<trace id>.tok`, a compact binary file (format in `src/trace/store.rs`): per token the id,
logprob, entropy, phase, the alternatives, and a **routing blob** for per-layer expert routing (MoE models, see below). Records are
appended while the request runs, so a crash loses at most the last record. Export is JSON lines: one trace per line,
with its stages, events and tokens (`trace.export`, `GET /v1/native/export`).

## The Tracer API (for other modules)

`crate::trace::tracer(&home)` gives the `Tracer` of a hub home folder:

| call | meaning |
|---|---|
| `start(kind, model, request, parent, meta)` | begin a trace; `kind` is `chat` here, later `km`, `train`, ... |
| `start_in(..., &Scope{job, task})` | the same, attached to a job id and/or task id (see below) |
| `stage_start(trace, name)` / `stage_end(stage, meta)` / `stage_span(trace, name, start_ms, end_ms, meta)` | stages |
| `event(trace, kind, json)` | anything: `selection`, `residency`, `routing`, `km.load`, ... |
| `specialist_call(trace, specialist, input, output, ms, ok)` | a call to a specialist model |
| `tokens(trace, &[TokenRec])` | a batch of tokens |
| `update(trace, {answer, reasoning, timings, resources, adapters, meta, ...})` | set fields |
| `finish(trace, status, error)` | `ok`, `error`, `aborted`, ... ; applies retention |
| `subscribe()` | the live event channel |

The KM runtime and the Forge only need to call these: a new kind of trace needs no schema change, because events
are rows of (kind, JSON). Events the dashboard already understands:

- `selection`: `{selected: [{id, score, why}], candidates: [...]}` fills "Selected knowledge modules";
- `routing`: `{token, layer, experts: [ids], weights: [gate weights], n_experts}` fills the expert heat map (the hub's own events have `token: null` and summed weights, see "Expert routing capture").

### Jobs and tasks (the KM factory)

Every trace can carry `job_id` and `task_id`. A factory task's pipeline step passes its ids when it starts a trace
(`start_in`) or sends `mm_task`/`mm_job` with its model calls, and everything the task's models did is then
browsable from the task: `trace.list {task: "..."}` / `GET /v1/native/traces?task=...&job=...`, and the Traces page
has task and job filters (the dashboard links a task to `/ui/#/traces?task=<id>`). A rerun stays in its original's
task.

## API

All need the owner's token. Operations are also available as `POST /v1/call/<op>` and as MCP tools.

| route | operation |
|---|---|
| `GET /v1/native/traces` (filters `model`, `status`, `kind`, `q`, `parent`, `job`, `task`, `since_ms`, `until_ms`, `order`; `limit`, `offset`) | `trace.list` |
| `GET /v1/native/traces/{id}` | `trace.get` |
| `GET /v1/native/traces/{id}/tokens?offset=&limit=` | `trace.tokens` |
| `POST /v1/native/traces/{id}/rerun` | `trace.rerun` |
| `GET /v1/native/diff?a=&b=` | `trace.diff` |
| `GET`/`POST /v1/native/settings` | `trace.settings` |
| `GET /v1/native/export` | `trace.export` (writes `<home>/trace/exports/*.jsonl`) |
| `DELETE /v1/native/traces/{id}` | `trace.delete` |
| `GET /v1/native/stream[?trace=]` | live server-sent events (no operation) |

**Rerun** sends the stored request again as a new, non-streamed, traced call linked to the original. Body:
`overrides` (request fields to replace; `null` removes one, e.g. `{"temperature": 0, "seed": 1}`),
`remove_adapters` (`true`, or a list of adapter indexes or file-name parts: their scale becomes 0 for this request),
`adapter_scales` (`{"0": 0.5}`). The answer includes the diff with the original. This is the ablation hook; removing
a KM or masking an expert uses the same call once the runtime exposes them as request fields.

**Diff** compares answers and reasoning word by word, the request parameters and adapters that differ, and the first
token where the two generations part (with the alternatives each had there), plus mean log probability and entropy.

**Live stream** events (`event:` name = `type`): `hello` (running traces), `trace.start`, `stage`
(`state` start/end/span), `event`, `specialist`, `tokens` (`offset`, `tokens`), `update`, `trace.finish`, `lagged`.
Read it with a header, not a URL parameter: `curl -N -H "Authorization: Bearer $TOKEN" .../v1/native/stream`.

## Dashboard

The hub serves it at `/ui/` (static files, no secrets). The built `frontend/dist` is embedded in the binary when it is
built (build the frontend first: `cd frontend && npm ci && npm run build`; without it `/ui/` explains what is
missing). A folder named by `MM_UI_DIR`, or `ui` beside the executable or in the home folder, overrides the embedded
files. The page asks for the owner's token once and keeps it in `sessionStorage` (this tab, never in a URL). In the
desktop app (Tauri) the same pages use the app's `hub_call` command; the live view polls there instead of streaming.

- **Thoughts**: follows requests live. Tokens are tinted by how unsure the model was; hover (or tap) a token for its
  probability, entropy and alternatives. Reasoning is a collapsible panel; stage timeline; resources; panels for the
  selected KMs and the expert-routing heat map (filled by the `routing` events of a traced MoE request). A box sends a traced
  request (browser only).
- **Traces**: list with search, model, status, task and job filters; tracing settings; export. A trace opens with the
  same renderer; rerun with overrides or adapters removed; tick two traces and compare.

Light and dark themes (system default, switch in the header); the layout works at phone width.

## Expert routing capture (mixture-of-experts models)

Which experts each MoE layer picked for each generated token is recorded when the request asks for it: tracing
setting `routing` (`trace.settings`), or `"mm_routing": true` in a traced request. It costs a copy off the device per
layer and token, so it is off by default; a request without it is served exactly as before.

Measured on an RX 7900 XTX (Vulkan) with Qwen3-Coder-30B-A3B (48 layers, 8 of 128 experts), 256 tokens at
temperature 0: the released hub 152.4 tokens/s, the patched hub without the flag 152.4 tokens/s (no cost), with the
flag 33.5 tokens/s (about 4.5x slower: each layer's router output is read back from the GPU as the token is made).
The text was identical in all three.

- **Producer**: the bundled llama.cpp carries a small patch, `engines/patches/mm-routing.patch` (the submodule stays
  pinned; `src/build.rs` applies the patch before building, skips it if it is already applied, and stops the build if
  it does not apply to a newer llama.cpp). It installs the graph evaluation callback (`cb_eval`) in llama-server. The
  callback is idle (one atomic load per graph node) unless a decode contains a token of a request with
  `"mm_routing": true`; then it asks for only the router tensors `ffn_moe_topk-<layer>` (expert ids) and
  `ffn_moe_weights-<layer>` (their gate probabilities) and reads them with `ggml_backend_tensor_get`. The answer
  carries `mm_routing`: non-streamed, `choices[0].mm_routing` is one entry per generated token (the same tokens as
  `logprobs.content`); streamed, each chunk's `choices[0].mm_routing` is that token's entry. An entry is
  `[{layer, experts: [ids], weights: [gate weights]}]`, the routing of the step that produced the token (its input
  token). Not reported: speculative decoding, and tokens the server holds back (an incomplete UTF-8 character).
- **Hub** (`src/trace/capture.rs`): sets `mm_routing` on the request sent to llama-server, parses the answer, writes
  each token's layers into the token's routing blob (`store::encode_routing`: layer count u16, then per layer
  `layer u16 | k u8 | k x expert u16 | k x weight f32`; the token API shows it decoded), and at the end of the request
  emits one `routing` event per layer: `{token: null, tokens: N, layer, experts, weights}` with the gate weights summed
  over the N tokens, which is what the heat map draws. If only the settings asked, the client does not get
  `mm_routing` back; if the client asked, it does.
- A model that is not a mixture of experts simply reports nothing.
