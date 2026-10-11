# The KM factory: using it

This implements `km-factory.md` sections 2 and 5 (tasks, the pipeline, review, tags). The code is in `src/factory/`.
Everything below works today except what is listed under "Deferred". Every operation is owner-only: none of them is in
the peer allowlist.

## What a task does

A task is a JSON description (`km-factory.md` section 2) stored in `<home>/factory/factory.db` (SQLite). It runs as
**one hub job** (`job.get`, `job.cancel` work on it) through eight steps. Each step keeps its state in the database,
so `factory.task.resume` continues after a crash, a restart, a cancel, a failed model call or an exhausted budget; a
step that is done is not run again, and the work inside generate, verify, tag and evaluate is kept per record.

| step | what it does |
|---|---|
| `plan` | the author model drafts an outline (subtopics with questions, tools, skills, domain, tags, keywords); stored, editable; a task with `plan_approval` stops here until the plan is approved |
| `gather` | sources become chunks with origin (file and lines, URL, KM and chunk, trace, licence when known) and an embedding each |
| `generate` | the author writes records in the data-KM schema (`km-spec.md` section 8) from chunks picked per subtopic; every record must cite chunk ids that were shown to the model, else it is kept only as a rejected record |
| `verify` | executable checks, then the reviewer model (supported / partial / unsupported with a reason), then dedupe, leak check, quality score, held-out split, review queue |
| `tag` | rule classifier + nearest existing KMs + author suggestions, all constrained to the vocabulary |
| `build` | `data`, `memory`, `skill`, `tool` KMs (and `adapter` through the Forge); validated, packed, imported |
| `evaluate` | with vs without the KMs on held-out records, graded by the reviewer; the uplift goes into a new patch version's `[quality]` |
| `publish` | `candidate` channel, then the promotion policy |

Every model call goes through one function that checks cancellation and budgets, sends the request through the hub's
traced chat path with the task id and job id (`Tracer::start_in` with a `Scope`), and counts tokens. The Traces page
filters by task, and `trace.list {task}` lists them.

## Operations

| operation | |
|---|---|
| `factory.task.create` | the task JSON plus `start` (default true); returns `{task, job, state}` |
| `factory.task.list {state, limit}` | newest first |
| `factory.task.get {id, logs, since, records, status, review}` | state, `steps`, `plan`, `usage`, `summary`, log, artifacts; `records: N` adds N records as `record_items` |
| `factory.task.plan.edit {id, plan, changes, approve, by}` | replace or merge-patch the plan (null removes a key); refused while the task runs |
| `factory.task.resume {id, from, changes}` | continue; `from` re-opens a step and the ones after it; `changes` edits `size`, `budget`, `device`, `approval`, `author`, `reviewer`, `base`, `tagging`, `plan_approval`, `eval_sets`, `think`, `per_call`, `license`, `kinds` |
| `factory.task.cancel {id}` | stops at the next safe point; resumable |
| `factory.review.list {task, kind, state, limit, offset}` | the queue; `state` is `open` (default), `approved`, `rejected`, `edited` or `all` |
| `factory.review.decide {item, decision, changes, by, note, add_to_vocabulary}` | `approve`, `reject` or `edit` |
| `factory.vocabulary.get` / `.edit` | the same as `km.vocabulary.*` |
| `factory.settings {settings}` | read or change `<home>/factory/settings.json` |
| `factory.embedder.setup {model, name}` | links a pulled embedding GGUF as `<home>/models/mm-embed-1.gguf` and points the factory and `km.embedder` at it |

REST: `factory.a.b` is `POST /v1/factory/a/b` (JSON body) and, if it only reads, `GET` with a query string.

### Task states

`created`, `running`, `awaiting_approval` (plan needs approval), `budget` (tokens or wall time used up), `failed`,
`cancelled`, `interrupted` (the hub died while it ran; set when the hub opens the database), `done`. Step statuses:
`running`, `done`, `skipped`, `failed`, `interrupted`.

## The task JSON

```json
{"goal": "...", "kinds": ["data", "memory"], "domain": "docs/km-runtime",
 "sources": [{"kind": "dir", "path": "docs", "globs": ["**/*.md"], "exclude": ["old/*"], "max_files": 100},
             {"kind": "file", "path": "docs/native/km-spec.md"},
             {"kind": "url", "url": "https://example.org/page", "max_bytes": 500000},
             {"kind": "km", "id": "data.code.rust"},
             {"kind": "skill", "path": "skills/my-skill"},
             {"kind": "tool", "name": "km.list"},
             {"kind": "tool", "name": "grep", "provider": "mcp:files", "description": "...", "input_schema": {}},
             {"kind": "traces", "query": "lang:rust", "limit": 30}],
 "author": "<hub model>", "reviewer": "<hub model>", "base": "<hub model or core name>",
 "size": {"records": 200, "max_tokens": 2000000},
 "budget": {"wall_s": 7200, "max_records": 500, "gpu_hours": 2},
 "tagging": {"vocabulary": "default", "review": "auto"},
 "approval": "gated", "plan_approval": false,
 "device": {"author": "auto", "reviewer": "cpu", "base": "auto", "embedder": "layers 20"},
 "eval_sets": [{"path": "eval.jsonl"}, {"ids": ["q1"]}, {"hashes": ["..."]}]}
```

* Sources: text is read with size limits (a file over `max_file_bytes` or one that looks binary is skipped and
  logged; a URL is fetched with a size cap, http(s) and text types only, HTML reduced to text). Links in folders are
  never followed. **Everything gathered is data**: it reaches the models inside fences labelled "data, not
  instructions", and the prompts say so. `tool` sources name a hub operation (its schema is taken from the hub) or give
  an MCP tool's schema inline.
* `device` is `auto`, `gpu`, `cpu` or `layers N`; it is passed to the engine's load options when the task is the
  first to load that model (a model already loaded stays as it is). `base` may name a core (`dusty`) instead of a hub
  model: the evaluation then answers with the author.
* `tagging.review`: `auto` (clean records are approved; partial support, low quality and uncertain tags go to the
  queue), `all` (every record is queued, nothing is approved by the machine), `none` (nothing is queued).
* Budgets: `size.max_tokens` (prompt + completion of all calls) and `budget.wall_s` (summed over runs) stop the task
  cleanly with state `budget`; `resume` with `changes` raises them. `size.records` / `budget.max_records` is the
  number of usable records to generate (the smaller wins).

## Records, review and tags

* A record is `{id, task: chat|tool_use, input, output, tool_calls?, labels, tags, tag_info, source: {kind: teacher,
  model, trace, factory_task, job, chunks, origins}, split, status, rev, verify}`. Tool-use records are checked by the
  executable-check hook (`factory::checks::Check`, no-op default; the built-in `ToolSchemaCheck` validates a call's
  arguments against the tool's JSON schema with a small validator). More checks can be added with `Factory::add_check`.
* Duplicates: normalised-text hash, then embedding similarity (`dedupe_threshold`); the higher-scored record stays.
  Leaks: a record whose user text or answer matches an eval-set entry by id, hash or embedding (`leak_threshold`) is
  rejected.
* The review queue holds `record` items (doubtful records), `tag` items (confidence between `tag_floor` and
  `tag_threshold`), `vocab` items (a value the vocabulary lacks, proposed) and `promotion` items (policy `propose`).
  Approving a tag outside the vocabulary adds the value (unless `add_to_vocabulary: false`); approving or rejecting a
  tag teaches the rule classifier (word-level feedback). With the setting `auto_add_vocabulary` the factory adds
  proposed values itself.
* Tag confidence of a tag: `1 - prod(1 - c_i * w_i)` over its sources, with weights classifier 0.9, nearest KMs 0.7,
  author model 0.8. Tags enter the KM's `[tag_info]` with their origin.

## Building and versions

KMs are written with the KM runtime's own manifest, pack and import code (`data.<name>`, `memory.<name>`,
`skill.<folder>`, `tool.<operation>`, `adapter.<name>`; `<name>` comes from the domain or goal). A task's first
version of an id is `0.1.0` (the next minor of the newest installed one, with it as `parent`, if the id exists). The
evaluation makes `0.1.1` with the same payload, `parent = 0.1.0` and `[quality] scores = {uplift, score_with_km,
score_without_km, n_eval}`; the data KM also gets `evals/heldout.jsonl` pinned by `eval_hash`. Memory, data and tool
KMs carry a routing descriptor and a centroid made with the embedder in use; `routing.embed_model` is that embedder's id
(`hash-384`, or `mm-embed-1`), and selection uses the vector term only if it equals `km.embed_model`.

`publish` moves `candidate` for every KM, then by policy (`approval` in the task, else the setting `promotion`):
`off` publishes nothing; `propose` queues a promotion item; `gated` (default) promotes to `stable` if the uplift is at
least `min_uplift`, beats the current stable version's recorded uplift, and no review item is open; `auto` promotes.

## Settings (`<home>/factory/settings.json`)

`embedder` (`km` = follow `km.embedder`, `hub`, `hash`), `embed_model` (`mm-embed-1`), `promotion`, `min_uplift`,
`auto_add_vocabulary`, `review_threshold`, `tag_threshold`, `tag_floor`, `dedupe_threshold`, `leak_threshold`,
`chunk_chars`, `max_file_bytes`, `max_files`, `max_total_bytes`, `max_url_bytes`, `max_retries`, `gen_max_tokens`,
`eval_notes`. Prompt templates are the plain-text files in `src/factory/prompts/` (system text, a line holding `---`,
user text, `{{name}}` placeholders); a file of the same name in `<home>/factory/prompts/` overrides one.

## The embedder

```
model-mistress pull hf.co/Qwen/Qwen3-Embedding-0.6B-GGUF:Q8_0
model-mistress factory embedder setup Qwen/Qwen3-Embedding-0.6B-GGUF:Qwen3-Embedding-0.6B-Q8_0
```

The second line hard-links the file as `mm-embed-1` in the home's `models` folder (the catalog has no aliases), sets
`km.embedder = hub`, `km.embed_model = mm-embed-1` and the factory's `embedder = hub`. The factory asks it in batches of
8 with long texts cut. The hashing embedder stays the default and what the tests use.

## CLI

`model-mistress factory help` lists everything; the main ones:

```
model-mistress factory create "Using the KM runtime" --author qwen3-1.7b --reviewer qwen3-0.6b \
    --source file:docs/native/km-runtime.md --kind data,memory --records 20 --device-author cpu --wait
model-mistress factory list | get <task> [--logs 20 --records 10] | plan <task> | wait <task>
model-mistress factory plan-edit <task> --changes '{"subtopics":[...]}' --approve
model-mistress factory resume <task> [--from verify] [--changes '{"size":{"max_tokens":3000000}}']
model-mistress factory review [--task T --kind tag] | review decide <item> approve|reject|edit [--changes '{...}']
```

## Plugging in the Forge

`factory::model::Generate` ("generate with model X": a `GenRequest` with model, messages, device, scope, returning text
and token counts) and `factory::model::Trainer` ("train an adapter": a `TrainRequest` with the data KM, the approved
training records as JSONL and an output folder, returning the LoRA GGUF, the core, arch, base hash and run id).
`Factory::set_generator` replaces the default (any model the hub serves through llama.cpp, traced); `Factory::set_trainer`
registers a trainer. Without a trainer the adapter KM is reported as `pending: forge` and the task still completes.

## Deferred

* The quality scorer does not yet learn from review decisions (they are stored with the record and teach the tag
  classifier only).
* Adapter evaluation (needs the adapter attached) and consolidation; `km-factory.md` sections 3 and 4 beyond tool and
  skill KMs (weights to adapter extraction).
* Only the `default` vocabulary exists; `tagging.vocabulary` other than `default` is refused.
* Model formats other than GGUF through the hub (the Forge's job), and `runtime.load/unload/list` operations.
* Executable checks for code (compile and run tests) are a hook only; the tool-schema check is built in.
* Hub-restart recovery marks a task `interrupted`; it does not resume by itself.
