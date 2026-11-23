# Misty: API reference

Misty is reached three ways: hub **operations** (`misty.*`, also the MCP tools and the REST routes), the **OpenAI
routes** (as the models `misty`, `misty-fast`, `misty-main`, `misty-deep`), and the **run stream** (server-sent events).
All of them need the owner's token, except the OpenAI routes, which also accept a peer key whose allow-list names her.

How to use her is in [USER_GUIDE.md](USER_GUIDE.md); why she is built this way is in [DESIGN.md](DESIGN.md).

## Operations

Each operation is `POST /v1/call/<id>` with a JSON body, and also a REST route: the id with dots as slashes
(`misty.run.get` is `POST /v1/misty/run/get`). An operation that does not change anything also answers `GET` with query
parameters (numbers and `true`/`false` are converted). `GET /v1/operations` (and MCP `tools/list`) lists them with their input
schemas, so a client such as ABP's operations panel renders forms for them without knowing Misty.

| Operation | Changes things | What it does |
| --- | --- | --- |
| `misty.status` | no | the roster, Cactus runtime, engines, free memory, index, policy, roots, active runs, what to do next |
| `misty.doctor` | yes | checks everything and makes one real call of each kind: `{ok, failed, checks: [{check, ok, detail, fix}]}` |
| `misty.setup.plan` | no | `{components: [{id, what, kind, size_bytes, installed, default, from, note}], default, default_download_bytes}` |
| `misty.setup.run` | yes | `{install: ["default" \| ids...], pack?}` starts jobs (`job.get` follows each): `{jobs, skipped}` |
| `misty.ask` | yes | one answer, waits for it; tools that need approval are **denied**; returns an outcome (below) |
| `misty.run.start` | yes | the same arguments; returns at once `{run, session}`; tools that need approval **wait** for `misty.approve` |
| `misty.run.get` | no | `{run, from, wait_ms}` events after `from` (waits up to `wait_ms`, max 25 000): `{events, next, done, running, pending}` |
| `misty.run.cancel` | yes | stops a run; a tool waiting for approval is refused |
| `misty.run.list` | no | `{active, recent}` |
| `misty.approvals` | no | `{pending: [{approval, run, tool, class, args}]}` |
| `misty.approve` | yes | `{approval, allow}`; not found when it has already been answered or timed out |
| `misty.session.list` / `.new` / `.get` / `.rename` / `.delete` | mixed | sessions and their messages; a listed session has `env`, `env_name` and `cwd`, and `.new` takes `{title, env, cwd}` |
| `misty.session.set` | yes | `{session, env, cwd}`: the session's environment (an id or a name) and working folder (a full path that exists); `null` clears one, a key left out stays. Returns `{session, env, env_name, cwd, scope}`; an unknown session or environment is `not_found`, a folder that is not there or not a full path is `invalid` |
| `misty.env.list` | no | `{environments: [{id, name, description, cwd, roots, vars, policy, created, updated}]}` |
| `misty.env.save` | yes | creates an environment (no `id`) or changes one (`id`): `{name, description, cwd, roots, vars, policy}`. A field left out stays; `roots` and `vars` replace the whole list. Returns `{environment}`. A name already in use, a folder that is not there, a bad variable name and a policy that is not `auto`, `ask` or `deny` are `invalid` |
| `misty.env.delete` | yes | `{id}` (an id or a name): `{deleted, id, name}`; sessions that used it keep their own folder and go back to her policy |
| `misty.browse` | no | `{path, hidden}`: the folders inside `path`, or with no path the usual starting places: `{path, parent, dirs: [{name, path}], places: [{label, path}], truncated}`. This is what the dashboard's folder picker calls |
| `misty.route` | no | `{input}`: features, a score per role, the winner and the reasons; nothing runs |
| `misty.rag.ingest` | yes | `{path}` or `{uri, text}`, plus `collection`: `{collection, files_seen, new, updated, unchanged, skipped, chunks, embedded}`; a `path` that is not there is `not_found` |
| `misty.rag.search` | no | `{query, k, collection}`: `{collection, hits: [{chunk, collection, source, start_line, end_line, text, score, coverage, via}]}` |
| `misty.rag.sources` | no | `{sources: [{collection, source, bytes, chunks, vectors}], collections: [...]}` |
| `misty.rag.remove` | yes | `{source}` or `{all: true}`, plus `collection` |
| `misty.tools.list` | no | `{tools: [{name, class, policy, description, parameters}], policy}` |
| `misty.config.get` / `.set` | no / yes | `set` takes `{patch: {...}}`, merged into the current settings (`null` removes a key and restores its default) and checked before saving |
| `misty.pipeline.list` / `.show` / `.save` / `.run` | mixed | `run`: `{name, inputs, wait}`; a job unless `wait: true` |
| `misty.bench` | yes | `{tokens}`: times one short generation on every ready role |

### Asking: arguments of `misty.ask` and `misty.run.start`

| Argument | Meaning |
| --- | --- |
| `input` (required) | the request |
| `session` / `new_session` | continue a session / start one (the id comes back) |
| `env`, `cwd` | work in an environment (id or name) and a folder (a full path that exists). With `session` or `new_session` the session keeps them (a key left out keeps what the session has); with neither they apply to this request only. See below |
| `role` | `fast`, `main` or `deep`: skip the router (`@fast`, `@main`, `@deep` in the text do the same) |
| `rag` | `true` always search the index, `false` never, unset: the router decides; `collection` picks the index |
| `tools` / `no_tools` | offer only these tools / none |
| `panel` | answer through two proposer roles and an aggregator (needs `panel` in the settings) |
| `approvals` | `ask` or `deny` for a tool that needs approval (default: `ask` for `run.start`, `deny` for `ask`) |
| `system` | replace her persona for this request |
| `audio_path`, `audio_base64` | a WAV, transcribed by Whistle first |
| `image_paths`, `images_base64` | images, described by Mirage first |
| `temperature`, `max_tokens` | sampling |

### Environments and working folders

A session works in the folder it was given (`cwd`), else the one in its environment, else her own workspace, and under
its environment's policy, else hers. The scope is read again at every tool call, so a change made while a run is going
applies from the run's next tool call.

- **Folders.** With a folder from either place, `fs_*` tools reach that folder and the environment's `roots` and no
  others (the `roots` in her settings do not apply there), a path without a folder means the working folder, and
  `shell_run` starts in it. An environment with no folder at all changes only the policy and the variables. The secret
  and escape guards hold inside every folder.
- **Policy.** A class named in the environment's `policy` (`read`, `write`, `exec`, `net`, `hub`) takes `auto`, `ask` or
  `deny` there instead of the value in her settings; a class left out keeps hers. A refusal names the environment.
- **Variables.** `vars` are added to the few variables `shell_run` passes on (names `[A-Za-z_][A-Za-z0-9_]*`). They are
  kept as plain text in her state database under the hub's home, `misty.env.list` returns them to the owner's token, and
  a command can print them, so a model can read them.
- **Who can change them.** Only the owner's token reaches `misty.env.*`, `misty.session.set` and `misty.browse`; none of
  her tools does, so she cannot widen her own folders or loosen her own policy. Peers reach none of them.

### The outcome (`misty.ask`)

```json
{"run": "5f2c...", "status": "done", "text": "...", "role": "fast",
 "citations": [{"n": 1, "source": "notes/release.md", "start_line": 3, "end_line": 9}],
 "usage": {"prompt_tokens": 412, "completion_tokens": 38}, "session": "ab12...", "ms": 5120, "error": null}
```

`status` is `done`, `cancelled` or `error` (then `error` says why).

### Errors

Standard hub errors: `invalid` (400), `unauthorized` (401), `forbidden` (403), `not_found` (404), `pipeline_failed` (502),
and `engine_cannot_run` (409) when a model file needs an engine that is not installed (ternary Bonsai without Prism).

## The run stream

`GET /v1/misty/run/stream?run=<id>&from=<n>` sends a live run's events as server-sent events, one `event:` per event
`type`, then `event: end`. `misty.run.get` gives the same events by polling, and also answers for runs from before a
restart (without the token events).

| Event `type` | Fields |
| --- | --- |
| `route` | `role`, `scores`, `reasons`, `rag`, `tools`, `reflex`, `escalate_to`, `features` |
| `transcript`, `vision` | what Whistle heard / Mirage saw |
| `retrieve` | `collection`, `hits`, `used`, `citations` |
| `reflex` | Needle's `calls`, `confidence`, `accepted`, `reasoning` (or `error`); `note` when a call was left to the model (only tools that read run without it); `wrap_up_role` when the `fast` role will put the result into words |
| `model` | `role`, `model`, `step`, `thinking` |
| `token`, `reasoning` | `text` |
| `tool` | `name`, `class`, `args`, `status` (`ok`, `error`, `denied`), `note` |
| `approval`, `approval_result` | a tool waits for you (`approval` id, `tool`, `class`, `args`) / the answer |
| `escalate` | `from`, `to`, `why`: the answer looked weak and a role up was tried; clients discard the text so far |
| `panel` | proposer progress |
| `error` | `message` |
| `final` | the outcome: `status`, `text`, `role`, `citations`, `usage`, `ms`, `session`, `error` |

Every event also has `t_ms`, the milliseconds since the run started.

## OpenAI routes

`GET /v1/models` lists `misty`, `misty-fast`, `misty-main` and `misty-deep` (unless the catalog already has a model of that
name, which wins). `POST /v1/chat/completions` with one of them runs the agent and answers in the OpenAI format, streaming
or not:

- the answer and `usage` in the usual places, and an `x_misty` object with the `role`, the `run` id and the `citations`
  (a failed run is a 502 `misty_failed`);
- a request carrying its own `tools` is refused with a message, because her tools are hers (call the operations for tool use);
- on this route tools that need approval are denied; a client that disconnects cancels the run;
- `misty-fast`, `-main` and `-deep` pin the role, `misty` lets the router choose;
- an optional `misty` object in the request body sets `session`, `collection`, `rag`, `no_tools` and `panel`; the
  `temperature` and `max_tokens` (or `max_completion_tokens`) fields work as usual; earlier messages in `messages` are the
  conversation, and `system` (or `developer`) messages replace her persona for that request.

### Peers

A peer key reaches these models only if its allow-list names them; a key with no list does not get her along with every
model. A peer's request has no tools, no files and no index, and the owner-only operations are closed to it. Without that
allow-list entry the route answers 403.

## The model router (`auto`)

The router that chooses her role is also a model of its own, for chats that do not need her: `auto`. These are hub
operations (`POST /v1/call/<id>`, also MCP tools; they change nothing) and one OpenAI model.

| Operation | What it does |
| --- | --- |
| `router.status` | `{model: "auto", enabled, roles, support, overrides, free_ram_mb}`. `roles` are `fast`, `main` and `deep`: `{role, model, status, source, loaded, vision, usable, note, score}` where `status` is `ready`, `missing`, `unfit`, `unavailable` or `not_allowed`, and `source` is `config` (your `roles` choice), `preferred` or `fallback`. `support` is the same for the other roles (`embed`, `rerank`, `reflex`, `ear`, `eye`). `enabled` is false when the catalog has a model named `auto` |
| `router.pick` | `{input}` (and `has_image`) or `{messages}` (a conversation in chat format, pictures as `image_url` parts): `{role, model, forced, reasons, candidates}`; `model` is null when nothing can answer. Nothing is sent to a model |

To choose the model of a role, `misty.config.set` with `{patch: {roles: {main: "model-id"}}}`; `null` instead of an id
goes back to the router's own choice.

`POST /v1/chat/completions` with `model: "auto"` (listed by `GET /v1/models`, unless the catalog has a model of that
name, which wins):

- the router scores the last user message and the conversation, picks the role (`@fast`, `@main` or `@deep` in the
  message force it, and are removed before the model sees it), and forwards the chat to that role's model; streaming and
  the usual fields work as for any model;
- the response carries `x-mm-routed-model`, `x-mm-routed-role` and `x-mm-routed-why`; the body is the model's;
- a conversation with a picture goes only to a role whose model has an `mmproj`;
- nothing ready: 409 `no_model_ready`, with the reasons in the message;
- there are no tools, files or index here; her tools are on the `misty` models.

A peer key may use `auto` only when its allow-list names it (otherwise 403 `model_not_allowed`), and the router then
considers only the models the key allows. `model.list` also says `deletable` for each model: true when ModelMistress
downloaded the file, so `model.delete` accepts it.

## Settings file

`<home>/misty/config.toml` (see the table in the user guide). Other files under `<home>/misty/`: `state.db` (sessions and
runs), `rag.db` (the index), `pipelines/` (your pipelines) and `workspace/` (the default file root). Installed pieces
live beside the hub's own: `<home>/engines/prism/` (the Prism llama-server), `<home>/engines/cactus/` (the Cactus
runtime) and `<home>/models/cactus/` (the `.cact` archives of Needle3, Whistle and Mirage).

## Extra engine setting

`engine = "prism"` in a model's serving profile (or the automatic choice for a ternary file) runs it with the Prism
llama-server: `<home>/engines/prism/llama-server[.exe]`, the `prism_server` setting, or `MM_PRISM_SERVER`.
`service.status` shows `prism_server`, and `backend.list` has a `prism` entry.
