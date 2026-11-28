# Misty: design

Misty is ModelMistress's own agent. She runs on the CPU first: a small roster of local models, a router that picks
the cheapest model that can do each step, local retrieval, a set of system tools behind an approval policy, and
pipelines that chain them. She lives in the ModelMistress hub, so every surface that reaches the hub reaches Misty:
the ModelMistress dashboard and desktop app, its CLI, its MCP tools, the OpenAI API (`model: "misty"`), and
AgenticBotPlatform (ABP), which uses her through that API and owns none of her code.

Written 2026-11-21 against ModelMistress `main` (0b39405). Facts about other projects were read from their
Hugging Face and GitHub pages on that day; each says where it came from. Anything not measured on this machine is
marked **unmeasured**.

## 1. Naming: Misty the agent, Misty the model

`docs/native/architecture.md` already uses "Misty" for a small model trained from scratch (the Forge's second core, beside
Dusty). The agent and that model are different things and keep both names:

| | What it is | Where it shows |
|---|---|---|
| **Misty** (this document) | the agent: router, roster, RAG, tools, pipelines, sessions | `misty.*` operations, `/v1/misty/*`, model id `misty` |
| **Misty core** (native model) | weights ModelMistress trains; `family: misty` in `model.list` | unchanged |

When a Misty core exists in the catalog, the roster can name it as a brain like any other model. Nothing about the
native model is removed or renamed.

## 2. What was researched

### 2.1 Prism ML: Bonsai (Hugging Face `prism-ml`, GitHub `PrismML-Eng`)

- Bonsai 2 is Qwen3.5 (hybrid linear/full attention, 262k context ceiling) with ternary weights, in sizes 2B, 4B, 9B
  and 27B, plus a vision tower (`mmproj`). It is a dense model, not a sparse mixture of experts: Misty's "mixture" is of
  *models*, not of expert layers inside one model.
- GGUF sizes, read from the repositories' file lists (language model only; the `mmproj` is 0.36 to 0.63 GB extra):

  | | PTQ1_0 | PQ2_0 | Q2_0 |
  |---|---|---|---|
  | 2B | 0.43 GB | 0.52 GB | 0.55 GB |
  | 4B | 0.95 GB | 1.15 GB | 1.21 GB |
  | 9B | 2.00 GB | 2.42 GB | 2.56 GB |
  | 27B | 5.95 GB | 7.21 GB | 7.63 GB |

  The 27B is therefore 6 to 8 GB, not 4 to 5 GB. The model cards also list MTP (multi-token prediction) and DFlash2
  drafter files for speculative decoding.
- **The engine matters, and ModelMistress's is too old.** The model card says `Q2_0` runs on upstream llama.cpp
  b11556 or newer (older builds "give wrong output without any warning"), and `PTQ1_0` and `PQ2_0` run only on the
  PrismML fork (`PrismML-Eng/llama.cpp`, branch `prism`, MIT). ModelMistress pins llama.cpp b11272 (`7fee178`). So
  today a Bonsai 2 file would load and answer wrongly. Misty therefore (a) refuses ternary files on an engine that
  cannot run them, and (b) adds the Prism build as a second engine, like Rigel (section 5.1).
- The fork publishes a prebuilt `win-cpu-x64` zip (21 MB, tag `prism-b12128-d7b2896`, 2026-10-31).
- Sampling advice from the cards: thinking on `temp 1.0, top_p 0.95, top_k 20, presence_penalty 1.0`; thinking off
  `temp 0.7, top_p 0.8, top_k 20, presence_penalty 1.5`; give thinking at least 8192 output tokens, or turn it off
  with `chat_template_kwargs: {"enable_thinking": false}`. Misty sets these per tier.
- Speed and accuracy numbers in the request that started this work (MMLU-Redux 86.95, 11 to 12 tokens/s on a CPU, 132
  tokens/s on an M4) came from third-party pages and were not checked. **Unmeasured** here; `misty bench` measures them.

### 2.2 Cactus Compute: Needle 3, Whistle, Mirage (Hugging Face `Cactus-Compute`, GitHub `cactus-compute`)

All three are Apache-2.0, ship as `.cact` archives (Cactus's own 2-bit "cactus-quants" format, **not** GGUF) and run on
one small C++ runtime that is also Apache-2.0 "including the prebuilt runtime binaries".

| Model | Does | Size | Notes |
|---|---|---|---|
| Needle 3 | tool calling, structured extraction, text embeddings | `needle3.cact` 35.3 MB (121M parameters) | picks tools and fills arguments under a grammar built from the tool schemas, so the JSON is always well formed; returns a calibrated `confidence`; never writes free text; no tool-retrieval head, so keep a toolset small |
| Whistle | speech to text, word timestamps, speech embeddings | `whistle.cact` 16.9 MB | 7 languages (en de fr es it nl pl), 16 kHz mono, 30 s per pass |
| Mirage | image description, detection, text reading, forms | `mirage.cact` 21.6 MB | structured JSON; the text reader covers Latin script only |

- Runtime: `Cactus-Compute/cactus` on Hugging Face has `releases/<version>/<platform>/` (latest 4.1.4): for
  `windows-x86_64` a `cactus.exe` runner, `libcactus.a` and a 1.7 KB `cactus.h`; Python wheels carry the shared library
  (`win_amd64` 0.89 MB). Every file has a SHA-256 in `manifest.json`.
- The C interface is eleven functions: `cactus_load` (bytes of a `.cact`), `cactus_models` (a bitmask of what is loaded:
  text 1, speech 2, vision 4), `cactus_init` (system facts, tools JSON), `cactus_complete`, `cactus_transcribe`,
  `cactus_describe`, `cactus_embed`, `cactus_set_threads`, `cactus_reset`, `cactus_last_error` and streaming
  transcription. There is no model handle: the runtime is a process-wide singleton, and the Python package serializes
  calls. Misty does the same behind one lock.
- `cactus-hybrid` (confidence-gated hand-off from a small model to a bigger one) is the idea behind Misty's escalation
  rule; its probes ship only inside Cactus's own Gemma checkpoints, so Misty uses Needle's confidence for tools and
  signals she can compute herself for text (section 4.2).
- The Python package sends anonymous usage counts. Misty does not use it: she calls the runtime library directly.

### 2.3 What ModelMistress and ABP already have

ModelMistress (Rust hub): catalog and loading of GGUF models through llama-server, a VRAM planner, the OpenAI and Ollama
APIs, jobs, traces, Knowledge Modules (versioned knowledge with search and residency), the KM factory, the Forge,
speech, vision, images, hosting keys, an MCP server that turns every operation into a tool, a Vue dashboard, a Tauri
desktop app and a PySide6 window. Its `router/` module is legacy scaffolding (placeholder load balancer, not wired in).

ABP (Python): a module contract (`abp-module.toml`), a client for the hub (`bot/modelmistress/`), agent tools, its own
model router and "router brain", memory fabric, context vault, swarm, and CLI, TUI and desktop surfaces. ABP already
reaches the hub's operations and `/v1` routes, so a new `misty.*` family is visible to ABP's generic operations panel
without any ABP change; ABP adds only thin conveniences (section 7).

## 3. Architecture

```
 surfaces      dashboard /misty   desktop (same page)   CLI `model-mistress misty`   MCP tools   OpenAI `model: misty`   ABP
                      \                |                      |                         |              |                 /
                       +---------------+----------- hub: misty.* operations, /v1/misty/* (REST + SSE) -------------------+
                                                              |
                                              +---------------+----------------+
                                              |           Misty (src/misty)    |
                                              |  session  ->  router  ->  agent loop                                  |
                                              |     |            |         |  \                                       |
                                              |   state.db   roster      tools  pipelines                             |
                                              |              |           |                                            |
                                              |            RAG index   approvals                                      |
                                              +---------------+----------------+
                                                              |
                         +------------------------------------+---------------------------------+
                         | llama-server (stock)   | llama-server (Prism fork)   | Cactus runtime (in process, one lock)
                         | any GGUF               | Bonsai 2 PTQ1_0 / PQ2_0     | Needle 3, Whistle, Mirage
```

### 3.1 The roster: experts by role

A role is a job; a member is a model that can do it. Misty never names a model in code, only a role; the roster file
(`<home>/misty/roster.toml`, written by `misty setup`, editable) maps roles to catalog ids or Cactus archives.

| Role | Default member | Used for |
|---|---|---|
| `reflex` | Needle 3 | choose a tool and fill its arguments; extract fields; confidence-gated shortcut |
| `fast` | Ternary-Bonsai-2-2B | greetings, rewrites, classification, short answers, query rewriting |
| `main` | Ternary-Bonsai-2-9B | the default brain: chat, tool use, RAG answers, summaries |
| `deep` | Ternary-Bonsai-2-27B | hard reasoning, long synthesis, escalation target; loaded only when needed and only if RAM allows |
| `ear` | Whistle | audio to text |
| `eye` | Mirage | image description, text in images, form extraction |
| `embed` | a GGUF embedding model if installed, else none | dense retrieval; without it RAG is lexical |
| `rerank` | a reranker model if installed, else none | optional second-stage ranking |

Hardware fit is checked, not assumed: a member whose file plus a context estimate does not fit free RAM is reported as
`unfit` and the router skips it. The default set for a 16 GB machine is `fast` + `main`; the 27B is offered only when
`free RAM - size - KV estimate` stays above a margin.

The card counts too: a file that goes to the GPU (not a ternary file, which the CPU-only Prism build serves, and not
the embedder or reranker) fits when it fits the free RAM plus what models may use of the card (its free memory and what
loaded models hold, less the `vram_reserve_mib` kept for the system). Each roster entry says where it is meant to run
(`device`: `gpu`, `split`, `cpu`), and `misty.status` adds the model's `hardware` block (where it runs now or would go).
The embedder and reranker are kept on the CPU, so that the card is left to the language models.

### 3.2 "MoE / MoA" as Misty means it

- **Mixture of experts (gate)**: the router is the gate. For every step it scores the roles from cheap features and
  picks one (or, for audio and images, the modality expert first).
- **Mixture of agents (panel)**: for requests marked hard, or after a failed escalation, `mode = panel` runs two
  proposers from different roles and an aggregator that sees both drafts. On a CPU the proposers share cores, so the
  panel is off by default and always visible in the run log (it costs time, not hidden).
- The router is data-driven where it can be: every decision and its outcome are stored (section 3.5) so a learned
  gate can replace the heuristic later without changing the interface.

### 3.3 The agent loop

1. **Intake.** Audio goes to `ear`, images to `eye` (a vision-capable brain gets the image itself when one is loaded).
2. **Route.** Features in, a `Route {role, reason, budget}` out (section 4.1).
3. **Retrieve** when the router asks for it: hybrid search over the RAG index, a token-budgeted context with numbered
   sources.
4. **Reflex shortcut.** If a tool is clearly implied and Needle's confidence clears the threshold, run it directly, but
   only a tool that reads (a call that changes something goes to the model and its approval). The request is offered
   only the tools it is plainly about, because Needle scores a right call higher among fewer tools and every definition
   is prompt text a CPU has to read. After an accepted call the `fast` role puts the result into words unless the owner
   named a role. Measured, not assumed: on the real Needle3 a right call scores 0.1 to 0.75 and the engine gives no call
   under 0.1, so the threshold is 0.10; Needle also reads its tools as text, and the key order of that JSON changes what
   it picks (alphabetical, which `serde_json` writes, picked the right tool for 11 of 21 test requests; `name`,
   `description`, `parameters` picked 15), so `cactus::needle_json` writes it by hand.
5. **Generate** with the chosen brain through the hub's own chat path (so loading, VRAM planning, quotas, metrics all
   apply), with the toolset as OpenAI `tools`.
6. **Act.** Tool calls pass the policy (section 3.4). A call that needs approval pauses the run and emits an
   `approval` event; the run resumes when the owner answers. A reply may carry at most `max_calls_per_reply` calls
   (default 4; the rest are not run and the model is told), and a run may make `max_tool_calls` (default 16, refused
   calls included). Both are counted by the gate, which the model cannot reach.
7. **Verify and escalate.** If the answer looks weak (empty, refusal pattern, malformed call, runaway thinking), retry
   one role up, once per run, and say so in the event stream. The bigger model is told why the first try was rejected.
8. **Finish.** Final text with citations; the run, its route, tool calls and token counts are stored.

A run is bounded by steps (`max_steps`), tool calls, wall time (`run_timeout_s`) and tokens, and can be cancelled. When
a run runs out of steps or tool calls it makes one last call with no tools, which asks the model to say what it found,
what it did not finish and what the owner could do; when it runs out of time, which is the owner's hard limit, no
further model call is made and the note is written from what the run did. Either way the run ends `done` with
`partial` set to `steps`, `tools` or `time`, and the text starts with a line saying it stopped early. (A pipeline step
that ends this way fails instead, so a later step does not build on it.)

Everything a model reads from a tool or a document is passed as data, wrapped and labelled, never as instructions, and
the host adds two defences that do not depend on the model behaving (`inject.rs`): a tool result cannot contain a
`<tool_result` or `</tool_result` tag, so it cannot close its label early or open a fake one; and wording that reads
as orders ("ignore your instructions", a fake `system:` line, "do not tell the user", "run the following command") is
named in the tool event's `flags`, in a warning after the result the model reads, and beside every approval the run
asks for afterwards, so the owner sees why before saying yes. The list is a heuristic: a flag never blocks anything, and
the approvals are what stop a hostile text from writing a file or changing the hub.

A conversation that outgrows the context (`context_tokens`, default 8192, the context the roster runs at) is trimmed
before each call, never the system prompt or the last question: first the results of earlier tool rounds become a note
the model can act on by calling the tool again, then the oldest turns go, then the latest results are cut. The log has a
`context` event when that happened.

If a request is unclear and a wrong guess would change, delete or run something, the model asks one short question and
does nothing else (the answer is the run's final text, and the session carries on from it); otherwise it works and says
what it assumed. This is a rule in the system prompt, so it is only as reliable as the model that reads it.

### 3.4 Tools and approvals

Every tool declares a class: `read` (files inside allowed roots, the RAG index, hub status), `write` (files inside
roots, RAG ingest of new sources), `exec` (shell commands), `net` (fetch a URL), `hub` (load or unload a model, pull a
model). Policy per class is `auto`, `ask` or `deny`; defaults: read `auto`, write and hub `ask`, net and exec `deny`
until the owner turns them on. A single tool can have a policy of its own (`policy.tools`, a tool's name to `auto`,
`ask` or `deny`), which wins over its class: `fs_read` can ask while the rest of `read` runs, or `rag_ingest` can run
while the rest of `write` asks. An environment sets the same, and an environment that sets a class replaces Misty's own
per-tool entries for that class, so a per-tool entry in Misty's settings never undoes an environment that turns a
whole class off. A name that is not a tool is refused when it is saved. Rules enforced in code, not in the prompt:

- when a caller names the tools it allows (`tools` on a request, as an OpenAI client or a pipeline step does), a call to
  any other is refused even when the model names it and the policy would allow it; every call, refused ones too, counts
  against the run's budget (section 3.3). When no list was named the request is only narrowed to the tools it is
  about, for the model's sake, and the policy alone decides;
- `remember` (class `write`, so the owner approves each note) keeps one short fact in the document index so it comes
  back by retrieval later. A note that gives orders is refused, so text Misty read today cannot steer her tomorrow;
- file tools resolve the real path and refuse anything outside the configured roots, and always refuse the hub's own
  secrets (`control.json`, `keys.json`, `.env`, `*.pem`, `*.key`) wherever they are;
- `exec` runs with a timeout, no inherited secrets, in a root, and only when enabled;
- `net` fetches go through one function with a size cap, a timeout, and no credentials;
- every call, its arguments, its result size and the approval decision are written to the run log.

The hub already turns its own operations into MCP tools, so external agents can use Misty's tools too; Misty consuming
*external* MCP servers is planned, not built (section 8).

### 3.5 State

`<home>/misty/state.db` (SQLite): sessions, messages, runs, steps (route, retrieve, model call, tool call, approval),
and RAG sources and chunks (in `<home>/misty/rag.db`, a separate file so it can be wiped or moved alone). Nothing is
sent anywhere; all of it is local.

### 3.6 Pipelines

A pipeline is a short file (`<home>/misty/pipelines/<name>.toml`) of steps run in order, each reading earlier outputs
with `{{steps.<id>.text}}`. Step kinds: `ask`, `route`, `rag.search`, `rag.ingest`, `transcribe`, `describe`, `extract`,
`tool`, `map` (over lines or JSON items), `moa`. A run is a hub job with progress and cancel. Built in: `voice-note`
(transcribe, summarize, file into RAG), `describe-image`, `repo-qa` (ingest a folder, answer a question with citations),
`research-brief` (when `net` is allowed). It is a sequence, not a general graph: no branching in v1.

## 4. Decisions

### 4.1 Routing

Features (all computed locally, no model call): modality present, token estimate, tool-trigger matches, code / math /
reasoning / lookup keyword scores, retrieval hit strength, turns so far, explicit override (`@fast`, `@main`, `@deep`,
or model ids `misty-fast`, `misty-deep`), free RAM and which roles are already loaded (a loaded member gets a bonus: a
cold 27B load costs more than a slightly weaker answer from a warm 9B), and the latency budget. The rule is "the
cheapest role whose score clears its bar", with a warm-model bias and a hard rule that audio and images go to their
experts. `misty route "<text>"` prints the features, each role's score and the winner, so a bad routing choice is
debuggable.

### 4.2 Escalation signals for text

Cactus's confidence probes do not exist for Bonsai. Misty uses what she can see: empty or very short output for a
long prompt, a refusal template, a tool call that fails schema validation twice, thinking that hits its budget with
no answer, and (when the engine returns them) low mean token log-probability. One escalation per run.

### 4.3 CPU first

Default serving profiles for roster members: `gpu_layers = 0`, flash attention on, context 8192 (not the model's 262k:
the KV cache is the cost), one slot, threads = physical cores minus two. A GPU is used only when the owner sets
`gpu_layers` for a member; the VRAM planner already guards that.

### 4.4 Embeddings and retrieval

BM25 (SQLite FTS5) always; dense vectors when the `embed` role resolves, stored as f32 with the model id, searched
by exact cosine (fine to about 100k chunks on this CPU), merged with reciprocal rank fusion, then optionally
reranked. Chunks follow headings and blank lines, about 900 characters with overlap, with the source path, line range
and a content hash so re-ingesting an unchanged file does nothing.

### 4.5 One serialized Cactus runtime

Because the runtime has no handle, `src/misty/cactus.rs` owns a process-wide lock and a `CactusRuntime` trait. The real
implementation loads the library with `libloading` (already a dependency) from `CACTUS_LIB_PATH` or
`<home>/engines/cactus/`; tests use a scripted fake. A missing library is a normal state: the roles that need it report
`unavailable` with the fix, and Misty still works with the GGUF roster.

## 5. Changes outside `src/misty`

### 5.1 The Prism engine

`engine = "prism"` in a serving profile starts `<home>/engines/prism/llama-server` (setting `prism_server`, env
`MM_PRISM_SERVER`), the same way Rigel is started. The build is a download, not a compile: `misty setup` fetches the
pinned release asset, checks its size and SHA-256, and unpacks it. A guard in the load path refuses Q2_0 / PQ2_0 /
PTQ1_0 files on an engine that cannot run them, with the setup command in the message.

### 5.2 The virtual model

`GET /v1/models` lists `misty`, `misty-fast`, `misty-main`, `misty-deep`. `POST /v1/chat/completions` with one of them
runs the agent loop and answers in OpenAI format (streamed or not); extra fields (`misty: {rag: true, tools: [...]}`)
are optional. Tool events appear as `x_misty` fields in the stream, which standard clients ignore. A peer key may use
`misty` only if its allow-list names it, and peers get no tools, no filesystem and no `exec` (they get routing, RAG over
collections the owner shares, and chat).

## 6. Surfaces

| Surface | What the owner gets |
|---|---|
| Hub | `misty.status`, `misty.setup.*`, `misty.ask`, `misty.run.*`, `misty.session.*`, `misty.route`, `misty.rag.*`, `misty.tools.*`, `misty.approve`, `misty.pipeline.*`, `misty.bench`; REST under `/v1/misty/`; SSE run stream |
| CLI | `model-mistress misty setup / status / chat / ask / route / rag ... / tools / approve / pipeline ... / sessions / bench` |
| Dashboard and desktop | a Misty page: chat with route chips, tool cards, approval prompts and citations; sources; roster and setup; pipelines |
| PySide6 window | a Misty page: chat, status, approvals |
| MCP | every `misty.*` operation, automatically |
| OpenAI clients | `model: "misty"` |
| ABP | provider models `modelmistress/misty*`, agent tools, `abp misty`, a `/misty` slash command, its operations panel |

## 7. ABP's part (thin by rule)

ABP adds `bot/modelmistress/misty.py` (a client for `/v1/misty`), agent tools that call it (`misty_ask`,
`misty_rag_search`, `misty_rag_ingest`, `misty_status`), `abp misty ...` in `abp_cli`, and docs. It holds no roster,
router, index or tool implementation. If the hub has no Misty, every one of them says so and ABP is otherwise
unchanged.

## 8. Not in v1 (and why)

- PDF and Office parsing for RAG (no parser in the dependency set; Mirage OCR is the planned route).
- Consuming external MCP servers as Misty tools.
- A learned router (the decision log is there so it can be trained; the Forge already trains the KM selector).
- Branching pipelines.
- Prompt-cache and context-checkpoint tuning for the Prism server (see the Bonsai demo's PROMPT-CACHE notes).
- Speculative decoding with the MTP / DFlash2 drafters: wired as profile options, not benchmarked.
- A Misty core (native weights) as the default brain.

## 9. Open decisions for the owner

1. Where `exec` stands by default: `deny` (proposed). Turning it on is one setting, and each call still asks.
2. Whether web fetch (`net`) is allowed at all on this machine (proposed: off until enabled).
3. Whether `deep` (27B, 6 to 8 GB) is part of the default setup or opt-in (proposed: opt-in).
4. Whether to bump the bundled llama.cpp pin to b11556 or later so stock `Q2_0` files run without the Prism build
   (proposed: yes, as a separate change: it moves a submodule and the `mm-routing.patch` must be re-checked).
