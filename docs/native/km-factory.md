# Knowledge Modules beside weights: the factory, consolidation and extraction

Knowledge Modules (KMs) work **in parallel with a model's weights**, like skills: the weights stay small and general,
KMs add knowledge, skills and tools on demand. They never replace the weights. Three systems keep the two in balance:

- the **factory** creates KMs, with good training data and correct tags, using any model ModelMistress can run;
- **consolidation** moves what a KM does better than the weights *into* the weights, slowly and safely;
- **extraction** turns skills, tools and model weights into KMs.

```
            sources (files, code, docs, skills, tools, traces, other models)
                                   │
                          ┌────────▼────────┐   author / reviewer models: any format, GPU or CPU
                          │     factory      │◄── tasks from the owner or agents (API / MCP)
                          └────────┬────────┘
                                   ▼
        weights ◄── consolidation ── KMs ──► selected per request, beside the weights
           │                          ▲
           └──────── extraction ──────┘
```

## 1. Any model, any format, GPU and/or CPU

The factory and the cores use one model runtime that loads:

| format | runs on | how |
|---|---|---|
| GGUF | GPU (Vulkan/ROCm), CPU, or split by layers | the bundled llama.cpp |
| Hugging Face safetensors / PyTorch | GPU (ROCm) or CPU | converted to GGUF when llama.cpp knows the architecture (cached); otherwise served by the Forge worker (PyTorch) |
| ONNX | CPU (GPU when an execution provider is available) | ONNX Runtime |
| Ollama store, Hugging Face cache | as GGUF | found in place, never copied |
| LoRA (GGUF / safetensors) | with its base | attached per request |

`device = auto` puts the whole model on the GPU when it fits the VRAM budget, splits layers when it fits partly, and
falls back to the CPU; `gpu`, `cpu` and `split:<n layers>` force a placement. The runtime reports where every model
actually runs. A model loaded for factory work is an ordinary hub model: the same leases, eviction and hot swap apply.

## 2. The factory

A **factory task** describes a KM (or a family of KMs) to create:

```json
{"goal": "Async Rust with tokio: tasks, cancellation, select!, Send bounds",
 "kinds": ["data", "memory", "adapter"],
 "domain": "code/rust/async",
 "sources": [{"kind": "dir", "path": "..."}, {"kind": "url", "url": "..."}, {"kind": "km", "id": "data.code.rust"},
             {"kind": "skill", "path": "..."}, {"kind": "tool", "name": "cargo_check"}, {"kind": "traces", "query": "lang:rust"}],
 "author": "dusty", "reviewer": "qwen3.8-4b", "base": "dusty",
 "size": {"records": 2000, "max_tokens": 4000000},
 "tagging": {"vocabulary": "default", "review": "auto"},
 "approval": "gated", "budget": {"gpu_hours": 2}}
```

Each task runs as a pipeline of jobs, every step resumable, observable (progress, logs, metrics, traces of every model
call) and cancellable:

1. **Plan.** The author model proposes an outline: subtopics, the skills involved, the questions a user would ask, the
   tools that apply, and where the KM sits in the taxonomy. The plan is stored and editable before anything is generated.
2. **Gather.** Sources are ingested and chunked with their origin kept (file, line, URL, licence).
3. **Generate.** The author model writes records grounded in the gathered material: questions and answers, instructions,
   step-by-step solutions, tool-use trajectories, counter-examples and refusals where they belong. Every record cites
   the chunks it came from.
4. **Verify.** A reviewer model (preferably a different one) checks each record against its citations; executable checks
   run where possible (code compiles and its tests pass, a tool call validates against the tool's schema); records are
   deduplicated, scored for quality, and checked against evaluation sets so test data never leaks into training data.
5. **Tag.** Tags come from three sources, each with a confidence: a taxonomy classifier, the nearest existing KMs (by
   embedding), and the author model's suggestions, all constrained to the controlled vocabulary (§5). Low-confidence
   tags go to the review queue.
6. **Build.** `data` KM (the records), `memory` KM (chunks + embeddings), `adapter` KM (a LoRA trained by the Forge on the
   chosen base), `skill` and `tool` KMs (§4) as requested; routing descriptor and centroid computed; resources measured.
7. **Evaluate.** The base with the KM vs without it on held-out records and the global regression suite. The uplift is
   stored in the KM's `[quality]` table.
8. **Publish.** To the `candidate` channel; the promotion policy (gated / propose / auto / off) decides the rest.

Review is a queue the owner or an agent works through in the dashboard or over the API/MCP: approve, edit, reject a
record or a tag; every decision is recorded and feeds the tag classifier and the quality scorer.

Operations (all are MCP tools): `factory.task.create`, `.list`, `.get`, `.plan.edit`, `.resume`, `.cancel`,
`factory.review.list`, `.decide`, `factory.vocabulary.get/edit`, plus `runtime.load` / `runtime.unload` /
`runtime.list` for models in any format.

## 3. Consolidation: from KM into weights

For every pair (core, KM) the hub tracks the **uplift**, the evaluation score with the KM minus without it, and how
often the KM is selected. When a KM is used often and its uplift stays clearly positive, the core starts learning it:

- **Teacher and student.** The teacher is the core *with* the KM; the student is the core's own weights. Training data
  is the KM's data plus the teacher's answers to it, mixed with a **replay** sample of general data so nothing else is
  forgotten.
- **Slowly.** Small steps, a low learning rate, a penalty on drift from the current weights, one bounded step per cycle.
  After each step the uplift is measured again.
- **Gated.** A step is kept only if the global regression suite does not drop; otherwise it is discarded (the policy
  in `architecture.md` §5 applies, so "propose" waits for the owner).
- **Absorbed.** When the uplift approaches zero, the weights now do what the KM did: the KM is marked `absorbed` for that
  core and is no longer loaded by default, so the request is faster and lighter. It stays available as the source of
  truth, and is loaded again automatically if its uplift returns (for example after a later change to the weights).

Consolidation never makes the weights bigger: for MoE cores it trains the routed experts and router rows that a KM's
domain maps to; for dense parts it uses bounded low-rank updates that are merged only after the gate passes.

## 4. Extraction: skills, tools and weights into KMs

- **Skills → `skill` KM.** A skill (instructions plus examples, e.g. a SKILL.md with its files) becomes a `skill` KM:
  the instructions as a context block, its examples as a `data` KM, and its tool references as bindings. The factory can
  then train an adapter from it.
- **Tools → `tool` KM.** A tool the hub or an MCP server already exposes becomes a `tool` KM: its schema, a description
  written for the model, usage examples and failure cases (from traces when available). A tool KM only *refers* to a
  tool the host provides; it never contains the tool's code (see `km-spec.md` §9).
- **Weights → `adapter` KM.** The difference between a fine-tuned model and its base, layer by layer, compressed by a
  truncated SVD into LoRA factors, computed one layer at a time so memory stays bounded.
- **Other models → KMs.** Any model the runtime can load can be the author for a factory task, so its knowledge can be
  captured as data and memory KMs and taught to a core through an adapter or consolidation.

## 5. Tags and taxonomy

- **Domain path**: `/`-separated, most general first (`code/rust/async`), from a taxonomy the owner can edit.
- **Tags**: `namespace:value` from a controlled vocabulary: `task:` (codegen, debug, explain, summarise, translate,
  plan, tool-use...), `lang:` (natural or programming language), `lib:`, `modality:`, `skill:`, `tool:`, `source:`,
  `license:`, `quality:` (gold, silver, synthetic), `risk:` (none, pii, security). New values are proposed by the factory
  and enter the vocabulary only when approved (or auto-approved by policy).
- Every tag carries its origin and confidence in the index, so selection can weigh or ignore uncertain tags.
