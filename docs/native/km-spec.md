# Knowledge Module specification (`mmkm/1`)

A **Knowledge Module (KM)** is the unit of knowledge, skill and capability in ModelMistress's native models. KMs work
**in parallel with a model's weights**, like skills, never instead of them: a model is its own (small, general) weights
plus whichever KMs a task needs, and everything else stays on disk. How KMs are created, and how a model learns a
KM's behaviour into its weights, is in `km-factory.md`. The total knowledge available to
a model is bounded only by storage; the memory it uses is bounded by what the task loads.

This document is normative. "Must" means a KM, or the runtime, that does otherwise is rejected.

## 1. Goals

- **Load only what is needed.** Selecting, loading and unloading a KM is cheap, explainable and safe at any moment.
- **Findable.** Every KM carries enough structure (domain path, tags, descriptor, embedding centroid) to be found by a
  router, a person or an agent.
- **Data, never code.** A KM holds tensors, text and metadata. It never contains anything that executes.
- **Verifiable.** Every payload file is content-addressed; a KM can be signed; the runtime checks both before use.
- **Immutable versions.** A published KM version never changes. Edits make a new version with a parent link.
- **Explainable.** Why a KM was chosen, and what it changed, is recorded for every request (see `observability.md`).

## 2. Layout

A KM is a directory. For transport it may be packed into one `.mmkm` file: an uncompressed POSIX tar of that directory
(no links, no absolute paths, no `..`, regular files only), so tensors stay memory-mappable after unpacking.

```
<id>@<version>/
  km.toml            the manifest (required)
  payload/           tensors, indexes, records (named in the manifest)
  evals/             evaluation sets and the last results (optional)
  README.md          human notes (optional)
  SIGNATURE          detached ed25519 signature over km.toml (optional)
```

`km.toml` lists every payload file with its SHA-256 and size. The manifest's own hash (SHA-256 of `km.toml`) is the
KM version's **content id**. Because the manifest pins every file's hash, the content id identifies the whole KM.

## 3. Kinds

| kind | payload | what loading it does |
|---|---|---|
| `adapter` | LoRA tensors for one base (GGUF LoRA) | attached to the base at a scale, per request; megabytes |
| `experts` | MoE expert tensors for named layers and expert slots (GGUF fragment) | assembled into the base's expert slots |
| `memory` | text chunks + embeddings (+ optional ANN index) | retrieval: matching chunks are placed in the context |
| `data` | training records (JSONL, schema in §8) | nothing at inference; the source other KMs are trained from |
| `specialist` | a binding to a model the hub serves (speech, image, vision...) + its I/O schema | the core may call it as a tool/expert |
| `projector` | a small network mapping a specialist's features into the core's embedding space | lets the core read images/audio |
| `router` | router weights for one base (classifier over KM descriptors) | replaces the base's KM selector |
| `skill` | instructions, examples and tool references (procedural knowledge) | its instructions join the context; its examples are a data KM |
| `tool` | a binding to a tool the host already provides (hub operation or MCP tool): schema, model-facing description, examples, failure cases | the core may call that tool; never contains the tool's code |
| `bundle` | a list of other KMs | loads/unloads them together |

## 4. Manifest (`km.toml`)

```toml
spec = "mmkm/1"
id = "code.rust.async"            # lowercase dotted slug: [a-z0-9]+(\.[a-z0-9-]+)*, <= 96 chars
version = "1.2.0"                 # semver; immutable once published
uuid = "0b6c1d2e-..."             # stable across versions of the same KM
kind = "adapter"
name = "Rust async"
description = "Writing and debugging async Rust with tokio: tasks, cancellation, Send bounds, select!."
domain = "code/rust/async"        # taxonomy path, '/'-separated, most general first
tags = ["task:codegen", "task:debug", "lang:rust", "lib:tokio", "modality:text"]
license = "Apache-2.0"
created = 2026-10-10T12:00:00Z
parent = "code.rust.async@1.1.0"  # the version this one was derived from (optional)

[compat]                          # which cores it works with; checked before load
base = "dusty"                    # dusty | misty | any (memory/data/specialist KMs are usually "any")
base_version = ">=1.0, <2.0"
arch = "qwen35moe"
base_hash = "sha256:..."          # adapters/experts/projectors: the exact base weights they were trained on

[modalities]
input = ["text"]
output = ["text"]

[routing]
descriptor = "async rust tokio futures select cancellation Send Sync pin"   # what the selector embeds
embed_model = "mm-embed-1"        # the embedder the centroid was made with; a mismatch disables vector scoring
centroid = "payload/centroid.f16" # mean embedding of the KM's training inputs (f16, little endian)
keywords = ["tokio", "async fn", "JoinHandle"]
intents = ["write_code", "fix_bug"]
prior = 0.0                       # log-odds added to the selection score (owner or router tuned)

[resources]                       # declared by the builder, verified by the runtime on first load
ram_bytes = 18874368
vram_bytes = 18874368
load_ms = 40

[quality]
eval_set = "evals/heldout.jsonl"
eval_hash = "sha256:..."
scores = { pass_rate = 0.81, delta_vs_base = 0.23 }

[provenance]
created_by = "auto"               # manual | auto | distill | import
training_run = "run-2026-10-10-0042"
teachers = ["dusty@1.0.0"]
data = ["data.code.rust@3.0.0"]   # data KMs it was trained on

[[payload]]
path = "payload/adapter.gguf"
role = "lora"
format = "gguf"
bytes = 18874368
sha256 = "..."
```

Kind-specific tables:

- `experts`: `[experts] layers = [4, 5, ...]`, `slots_per_layer`, `hidden`, `ffn`, `gate = "swiglu"`; each payload file
  names its layer and slot range.
- `memory`: `[memory] chunks = "payload/chunks.jsonl"`, `embeddings = "payload/emb.f16"`, `dim`, `index = "hnsw" | "flat"`.
- `specialist`: `[specialist] engine = "tts" | "image" | "stt" | "vision"`, `model = "<hub model id>"`,
  `input_schema`, `output_schema` (JSON Schema), `tool_description`. A specialist KM can only name engines the hub
  already has; it cannot point at a program.
- `projector`: `[projector] from = "<specialist KM id>"`, `feature_dim`, `tokens` (how many embedding slots it emits).
- `bundle`: `members = ["id@range", ...]`.
- `skill`: `[skill] instructions = "payload/SKILL.md"`, `examples = "<data KM id@range>"`, `tools = ["<tool KM id>"]`.
- `tool`: `[tool] provider = "hub" | "mcp:<server>"`, `name`, `input_schema`, `output_schema`, `description`,
  `examples = "payload/examples.jsonl"`, `side_effects = "none" | "read" | "write"`. Loading a tool KM never grants a
  permission the host would not grant the caller anyway.
- `[consolidation]` (any kind, per core): `state = "none" | "learning" | "absorbed"`, `uplift`, `last_step`, `steps`.

Unknown keys are kept and ignored (forward compatibility). Missing optional keys take documented defaults.

## 5. Identity, versions, channels

- A KM version is `id@version`; its content id is the manifest hash. Two different contents may never share an
  `id@version`; the registry refuses the second.
- **Channels** point at versions: `stable`, `candidate`, and any named channel. Cores load channels, not raw versions, by
  default, so promotion and rollback are a channel move (one row, atomic).
- `requires = ["id@range"]` and `conflicts = ["id@range"]` (manifest top level) are resolved before load; a cycle or an
  unsatisfiable range refuses the load and says why.

## 6. Finding a KM (selection)

The selector scores every compatible KM for a request and returns a ranked, explained list.

`score = w_vec·cos(query_emb, centroid) + w_lex·bm25(query, descriptor+name+description+keywords)
       + w_route·router_logit(KM | query) + prior + pin_bonus`

- Filters first: `compat`, modalities, owner bans, and the residency budget (a KM that can never fit is skipped and
  reported).
- `embed_model` must match the selector's embedder, or the vector term is dropped for that KM and the trace says so.
- `domain` and `tags` support exact queries (`tag:lang:rust domain:code/*`).
- Every term of every score is recorded in the request trace, so "why this KM" is always answerable.
- The learned router (a `router` KM) is optional; without it the first two terms decide.

## 7. Residency (loading and unloading)

Tiers: **disk** (everything) → **RAM** (memory-mapped, page cache) → **VRAM** (attached to a running core).

- Budgets are bytes per tier, set by the owner (and per core). The manager keeps `sum(resident) <= budget`.
- Eviction: least-recently-used weighted by use frequency with decay; **pinned** KMs and KMs used by an in-flight
  request are never evicted. A request's whole KM set is protected until it finishes.
- Loading is verify-then-swap: hashes (and the signature, if trust requires one) are checked, the KM is attached, and
  only then does it become visible. A failed load leaves the previous state untouched.
- Prefetch: the selector's runner-up KMs may be warmed into RAM when the budget has room.
- Every load, unload, eviction, refusal and its reason is an event (see `observability.md`).

## 8. Training records (`data` KMs)

One JSON object per line:

```json
{"id": "r-00042", "task": "chat", "input": [{"role": "user", "content": "..."}], "output": "...",
 "media": [{"kind": "image", "sha256": "...", "path": "payload/media/ab12.png"}],
 "labels": {"quality": 0.9, "lang": "rust"}, "tags": ["task:codegen"],
 "source": {"kind": "teacher", "model": "dusty@1.0.0", "trace": "tr-..."},
 "split": "train", "status": "approved", "rev": 3, "edited_by": "owner"}
```

- `status`: `pending` (generated, not yet used), `approved`, `rejected`. Autonomous training uses only `approved`
  records, or `pending` ones that passed the automatic filters when the policy allows it.
- Editing a record bumps `rev`; the previous text is kept in the KM's history so every training run can be reproduced.

## 9. Trust and safety

- Every payload hash is verified on every load (memory-mapped files are verified once per process per content id).
- Signatures: the owner's trusted keys live in the hub settings. Policy per source: own KMs may be unsigned; KMs from
  peers or downloads must be signed by a trusted key, or are refused (the owner can override per KM).
- Unpacking refuses links, absolute paths, `..`, device files, and any file not named in the manifest.
- Size limits per kind; a declared `ram_bytes`/`vram_bytes` that the measured load exceeds by more than 10% marks the KM
  `misdeclared` and refuses further automatic loads.
- A KM can never make the hub run a program or open a network address.
