# Knowledge Module runtime: using it

This is the implementation of `km-spec.md` (`mmkm/1`) and the tag/consolidation parts of `km-factory.md`. The code is in
`src/km/`. Everything below works today except what is listed under "Deferred".

## What it does

| piece | file | notes |
|---|---|---|
| manifest | `manifest.rs` | all ten kinds, every table in the spec, unknown keys kept (`unknown_keys`), content id = SHA-256 of `km.toml` |
| archives | `pack.rs` | `.mmkm` = uncompressed tar of `<id>@<version>/`; two-pass unpack, only manifest-named files are ever written |
| store | `store.rs` | `<home>/km/store/<id>@<version>/`, SQLite index `<home>/km/index.db` (FTS5, tags, channels, pins/bans, history) |
| selection | `select.rs` | the spec's score, every term recorded, filters with reasons |
| residency | `residency.rs` | RAM/VRAM budgets, heat-based eviction, pins, in-flight guards, events |
| requires/conflicts | `resolve.rs` | cycles, ranges, conflicts, bundles |
| memory / data | `memory.rs`, `data.rs` | retrieval with citations; record CRUD with history and commit to a new version |
| vocabulary | `vocab.rs` | `<home>/km/vocabulary.toml` |
| operations | `ops.rs` | `km.*` hub operations (so MCP tools) and `/v1/km/...` routes |
| CLI | `cli.rs` | `model-mistress km ...` |
| samples | `sample.rs` | builders for tiny KMs of every kind (tests) |

## CLI

`model-mistress km help` prints the list. The hub must be running, except for `keygen`, `sign`, `pack` and `verify`,
which work on files.

```
model-mistress km import ./code.rust.async@1.2.0 --channel candidate
model-mistress km import friend.mmkm --source peer              # refused unless signed by a trusted key
model-mistress km import friend.mmkm --source peer --allow-unsigned   # the owner's override, recorded on the KM
model-mistress km list --kind adapter
model-mistress km search "cancel a tokio task" tag:lang:rust domain:code/* --top 5
model-mistress km show code.rust.async
model-mistress km channel set code.rust.async stable 1.2.0 --expect 1.1.0   # compare-and-swap; rollback = move back
model-mistress km load code.rust.async --core qwen3-4b                      # an adapter attaches to that hub model
model-mistress km pin code.rust.async ; model-mistress km ban bad.kit
model-mistress km residency ; model-mistress km events --limit 20
model-mistress km data list data.code.rust --status pending
model-mistress km data edit data.code.rust r-00042 '{"output":"..."}' ; model-mistress km data approve data.code.rust r-00042
model-mistress km data commit data.code.rust                                # a new version, parent = the old one
model-mistress km memory query docs.rust "how do I cancel a task" --top 3
model-mistress km keygen owner.key ; model-mistress km sign ./mykm owner.key ; model-mistress km pack ./mykm my.mmkm
```

## API

Each operation `km.a.b` is `POST /v1/km/a/b` (JSON body) and, if it only reads, `GET /v1/km/a/b?x=1` as well. They are
also `POST /v1/call/km.a.b` and MCP tools. The owner's token only: none of them is in the peer allowlist.

| operation | mutating | |
|---|---|---|
| `km.list`, `km.show`, `km.search` | no | `km.search` returns `results` (each with `terms`: vec, lex, route, prior, pin, tags) and `excluded` (with reasons) |
| `km.import`, `km.export`, `km.remove` | yes (`remove` destructive) | import takes a folder or `.mmkm` on the hub's machine |
| `km.load`, `km.unload`, `km.prefetch` | yes | load verifies, resolves `requires`/`conflicts`, then admits |
| `km.pin`, `km.ban` | yes | by KM id |
| `km.channel.set` / `km.channel.get` | set: yes | atomic move; `expect` makes it a compare-and-swap |
| `km.residency`, `km.events` | no | tiers, budgets, resident set with heat; the recent event ring |
| `km.vocabulary.get` / `.edit` | edit: yes | |
| `km.data.list/get`, `.edit/approve/reject/commit` | edit, approve, reject, commit: yes | |
| `km.memory.query` | no (it may load the KM into RAM) | hits carry chunk id, score and a `km@version#chunk` citation |

In Rust: `km::runtime(&engine)` gives the `Km`; `Km::open_request(&[LoadReq])` loads a request's whole KM set and returns
a `Guard`: nothing in it is evicted until the guard is dropped. `Km::manager.events().subscribe()` is the broadcast
stream (the observability branch can persist it); `events().recent(..)` is the in-memory ring (1000 events).
`Km::set_embedder` and `Km::set_attacher` replace the embedder and the adapter attacher (tests do).

## Settings (`[km]` in `<home>/config.toml`, also `config.set`)

| key | default | meaning |
|---|---|---|
| `trusted_keys` | `[]` | ed25519 public keys (hex) whose signatures are trusted |
| `embedder` | `"hash"` | `hash` = built in, offline (`hash-384`); `hub` = the model in `embed_model`, through the hub's embeddings |
| `embed_model` | none | the hub model that embeds; it is also the id a KM's `routing.embed_model` must equal |
| `budget_disk_bytes` / `budget_ram_bytes` / `budget_vram_bytes` | 0 / 4 GiB / 2 GiB | 0 = no limit |
| `core_budgets` | `{}` | `[km.core_budgets.dusty] ram_bytes = .. vram_bytes = ..`, inside the global ones |
| `w_vec`, `w_lex`, `w_route`, `w_tag`, `pin_bonus` | 1, 0.25, 1, 0.5, 2 | selection weights |
| `heat_half_life_s` | 600 | half-life of the use count eviction ranks by |
| `default_core` | none | the model adapters attach to when a load names none |
| `max_km_bytes` | 0 | refuse bigger KMs (0 = the per-kind limits) |

A peer or download that is signed by an untrusted key, or not signed, is refused unless `--allow-unsigned`; a signature
that does not verify is refused whatever the source. The `SIGNATURE` file is one line, `ed25519 <pubkey hex> <sig hex>`,
over the bytes of `km.toml`. Trust is checked again on every load against today's `trusted_keys`.

## How the rules were implemented

- **Selection** `score = w_vec·cos + w_lex·bm25 + w_route·0 + prior + pin_bonus + w_tag·Σ confidence(query tags)`. The
  vector term uses the f16 centroid and drops, with a note, when the KM's `embed_model` is not the selector's. BM25 is
  FTS5's `bm25()` (name 3, description 1, descriptor 2, keywords 2), negated so larger is better. The router term is 0
  until a router KM drives it. Versions come from the `stable` channel, else the newest (`channel` and `all_versions`
  change that). Filters: owner bans, `misdeclared`, kind, `tag:` (with `min_tag_confidence`), `domain:` (`code/*` =
  under `code/`), base, base version, architecture, modalities, "can never fit" the budget, and `absorbed`
  (see below), each excluded KM listed with every reason.
- **Residency**: a KM occupies one tier. RAM-tier KMs count their measured RAM bytes, VRAM-tier ones their VRAM bytes.
  Eviction takes the lowest decayed use count (ties: least recently used). Loads are verify-then-swap: files hashed
  (once per process per content id), signature policy, `requires`/`conflicts`, then the adapter is attached to the core
  with the engine's LoRA mechanism, and only then do the books change; a failed attach leaves everything as it was. If
  a later member of a set fails, the members this call loaded are unloaded again (evictions already made are not undone).
- **misdeclared**: measured bytes are the payload files' size. Over the declared `ram_bytes`/`vram_bytes` by more than
  10% marks the KM, refuses the load, and refuses later loads (`force` overrides) and selection.
- **Adapter KMs** attach to a hub model named by `core` (or `default_core`) in the VRAM tier; without a core they are
  staged in RAM. Attachments are recorded in `<home>/km/attached.json` so an adapter left attached by a crashed hub is
  detached on the next load or residency call.
- **Experts, projector, router, specialist and tool KMs** are verified, accounted and held; their output says
  `attach: "pending"`. Memory KMs report `on-demand`.
- **Data KMs**: the working copy of each KM id lives in `<home>/km/data.db` with an append-only history of every
  revision of every record; `km.data.commit` writes a new version whose `parent` is the version it started from.
  Editing a record does not change its status; approving or rejecting is itself a new `rev`.
- **Tool KMs** may only reference a tool (`provider = "hub"` with an existing hub operation, or `mcp:<server>`). The
  `[tool]` table accepts nothing else (no command, address, credential or permission key), the payload is text and JSON
  only, and `km.show` says `grants_permission: false`. Loading one adds no permission; the caller's own still decide.
- **Consolidation**: `[consolidation]` (for the KM's own base) or `[consolidation.dusty]` / `.misty` / `.any` with
  `state`, `uplift`, `last_step`, `steps`. Stored in the index and shown by `km.show` and `km.list`. A KM whose state is
  `absorbed` for the core asked about is excluded from `km.search` with the reason
  `absorbed into <core>'s weights (uplift .., .. steps)`, unless `include_absorbed`.
- **Tags**: `[tag_info]` gives `"lang:rust" = { origin = "classifier", confidence = 0.9 }`; other tags are `manual`, 1.0.
  Both are in the `tags` table. The vocabulary (`km.vocabulary.get/edit`) has namespaces with approved values or `open`;
  a KM with unapproved tags is still installed, and the import answer and `km.show` list them.

## Deviations from the spec

- Directory entries in an archive are tolerated (and ignored); any other non-regular entry is refused.
- An archive may hold only `km.toml`, `README.md`, `SIGNATURE`, the payload and the eval set the manifest names.
- A memory KM with `index = "hnsw"` is served by an exact flat scan (no ANN crate is built in); the answer says so.
- `README.md` and the eval set are not hash-pinned unless `quality.eval_hash` is given.

## Deferred

- Assembling experts, projectors and routers into a running core, and the learned router term (`attach: pending`).
- Wiring selection and `open_request` into `/v1/chat/completions`, and persisting the event ring (observability branch).
- Restoring the resident set after a restart; the consolidation state is read from the manifest only (no operation
  changes it yet); automatic re-loading of an absorbed KM when its uplift returns.
- Using a `skill` KM's instructions in the context, and calling a `tool` KM's tool; the factory itself.
- A fixed-ANN index (HNSW), and signature creation from the API (the CLI has `keygen` and `sign`).
