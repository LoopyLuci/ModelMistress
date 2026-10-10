# ModelMistress native models: Misty and Dusty

ModelMistress has two models of its own. Both are cores that think, choose Knowledge Modules (KMs, see
`km-spec.md`) and call specialists; both are trained, observed and improved inside ModelMistress.

| | **Dusty** | **Misty** |
|---|---|---|
| Starts from | Qwen3.5-4B (Apache-2.0), converted into a mixture of experts | nothing: a new tokenizer and new weights |
| Architecture | `qwen35moe` (served by the bundled llama.cpp) | `qwen35moe`-compatible, small (configs in §3) |
| Strength | useful from day one; strong reasoning and tool use | entirely MM's own; tiny and fast; grows with training |
| Teachers | the specialists, the owner's data | Dusty, the specialists, open corpora, the owner's data |

Both use the same KM system, runtime, observability, training pipelines and autonomy settings. Everything below applies
to both unless it says otherwise.

## 1. What runs

```
request ─► selector ─► KM set ─► residency manager ─► core (llama.cpp, qwen35moe)
              │                        │                   │   ├─ adapters (LoRA, per request)
              │                        │                   │   ├─ experts (assembled expert slots)
              │                        │                   │   └─ memory chunks (context)
              │                        │                   └─► tool calls ─► specialist KMs
              │                        │                                     (Qwen3-TTS, SDXL, Whisper,
              ▼                        ▼                                      vision models, ...)
           trace ◄──────────── events ◄──────────── per-token telemetry
```

- **Core.** A `qwen35moe` model served by the bundled llama.cpp, so MM needs no second inference engine. Expert tensors
  stay memory-mapped and, when VRAM is short, on the CPU (`--override-tensor`), so the operating system pages in only
  the experts the router actually uses.
- **Experts as KMs.** An `experts` KM provides tensors for named layers and expert slots. Loading or unloading one
  re-assembles the core's GGUF (a tensor copy, no conversion) and hot-swaps the running model through the engine's
  existing swap, with the old model serving until the new one is ready.
- **Adapters as KMs.** LoRA adapters attach per request with a scale (llama-server's per-request LoRA), so switching
  skills costs nothing.
- **Memory KMs** give facts without parameters: retrieved chunks are placed in the context and cited in the trace.
- **Specialists (mixture of agents).** Speech, image generation, transcription and vision models are `specialist` KMs:
  the core calls them as tools with a typed schema. `projector` KMs let the core read their features directly
  (CLIP/vision features → the core's embedding space) once trained.

The honest limits: the knowledge on disk is unbounded, but a single request can only use what fits the budget it
loads, the selector must pick well, and loading a KM from disk costs time (milliseconds for adapters and memory,
longer for expert re-assembly). The design makes all three visible and tunable rather than hiding them.

## 2. Observability ("the model's thoughts")

Every request produces a **trace**; everything a person or an agent needs to understand a behaviour is in it.

- **Selection.** Every candidate KM with each score term (vector, lexical, router, prior, pin), the filters that
  removed others, and the final set.
- **Residency.** Loads, unloads, evictions, refusals, verification time, bytes per tier before and after.
- **Context assembly.** The system prompt, the memory chunks inserted (with KM and chunk ids), the adapters and scales.
- **Generation, per token.** The token, its probability, the top-k alternatives with probabilities, entropy, and for MoE
  cores the experts each layer routed to with their gate weights (from an evaluation callback in the vendored
  llama.cpp, recorded only when tracing is on, because it costs time).
- **Reasoning.** The model's thinking text, kept separate from its answer.
- **Specialist calls.** Inputs, outputs (or their hashes for large media), timings.
- **Resources.** Time per stage, tokens per second, RAM/VRAM per tier, GPU utilisation.

Analysis on demand: **ablation** (rerun with a KM removed or an expert masked and diff the answers), **logit lens** for
small cores (what each layer would predict), **attribution** of answer spans to memory chunks.

Storage: SQLite for traces, events and indexes; per-token arrays in compact segment files beside it; retention by age
and size, exportable to JSONL/Parquet. Access: the hub API (`/v1/native/...`), a live stream (SSE), and MCP tools (every
hub operation is already an MCP tool).

## 3. Model configurations

| | Dusty-1 | Misty-S (first) | Misty-M (later) |
|---|---|---|---|
| layers / hidden | Qwen3.5-4B's | 16 / 768 | 24 / 1536 |
| FFN | the dense FFN becomes the always-on **shared expert**; 64 routed slots per layer (top-4), each 1/8 of the FFN wide | 16 routed (top-2) + a shared expert | 32 routed (top-4) + a shared expert |
| total / active parameters | ~4B + what KMs fill / ~4.4B | ~0.5B / ~0.12B | ~3B / ~0.5B |
| tokenizer | Qwen3.5's | MM-trained BPE, 32k | same |
| context | 32k (training), more at inference | 8k | 32k |

Numbers are starting points; the training pipeline records the exact configuration of every version.

## 4. Training (the Forge)

The Forge is ModelMistress's training system: a job runner in the hub plus a managed Python worker (PyTorch on ROCm),
installed and updated by MM itself into its own environment. The hub owns scheduling, data, versions and telemetry; the
worker only computes. They talk over JSON lines on stdio.

Pipelines (each step is a job with progress, logs, metrics and cancel):

1. **Data.** Ingest (files, chats, traces, teacher output) → clean, deduplicate, score quality, filter secrets/PII →
   split → a new `data` KM version. Records are viewable and editable in the dashboard; every edit is versioned.
2. **Teach.** Specialists generate supervised pairs: Whisper (audio → text), Qwen3-TTS (text → audio for listening
   tests), the vision models (image → labels, boxes, embeddings), SDXL (prompt → image, for image-understanding and
   prompt-writing data); Dusty teaches Misty text and tool use.
3. **Dusty.** Convert Qwen3.5-4B to `qwen35moe`: each dense FFN becomes the shared expert (its sigmoid gate weights set
   to zero and its output weights doubled, so its contribution is exactly the original), and the routed slots start
   with zero output weights. Dusty-1.0 therefore answers exactly like Qwen3.5-4B, which a test checks token for token.
   Then, per domain (data KMs), train only that KM's routed experts and the router rows for its slots, everything else
   frozen (cheap enough for one 24 GB GPU) → LoRA adapters for narrow skills → export to GGUF, quantize → evaluate →
   package experts/adapters as KMs. Slots no KM fills output zero.
4. **Misty.** Train the tokenizer → pretrain on curated open text plus MM's data KMs → distil from Dusty → instruction and
   tool-use tuning → projectors for vision and audio features → export/evaluate/package like Dusty.
5. **Router.** Train the KM selector on traces (which KMs were chosen, and whether the answer was good).
6. **Evaluate.** Per-KM held-out sets, global regression suites, safety checks; results stored with the eval set's hash.
7. **Promote.** Move a channel (see autonomy). Rollback is moving it back.

Hardware guard: a preflight proves an optimizer step really runs on the GPU before a long job starts; jobs run only
within the owner's budget (GPU hours per day, idle windows, disk caps) and pause when the owner's own work needs the GPU.

## 5. Autonomy

The self-training loop: signals (owner ratings, eval failures, low-confidence traces, teacher disagreement) → a
curriculum (which domains to improve) → data generation → training → evaluation → a promotion decision.

Promotion policy, set in the dashboard, the API or MCP, globally and per KM or core:

- **gated** (default): promote only if the candidate beats the current version on held-out evals and passes the safety
  suite, with automatic rollback if live signals degrade;
- **propose**: train and evaluate on its own, wait for the owner's approval to promote;
- **auto**: promote whatever the evaluations favour, with sanity checks only;
- **off**: no autonomous training.

The owner can pin or ban any KM, freeze a core, cap budgets, and see every decision with its evidence.

## 6. Dashboard

Served by the hub to any browser and shown in the desktop app:

- **Thoughts**: a live view of a request: selected KMs and why, tokens coloured by confidence, alternatives on hover,
  the expert-routing heat map per layer, reasoning text, specialist calls.
- **Traces**: search, filter, compare two traces, ablate and rerun.
- **Knowledge**: the KM library (search by text, tag or domain; load, unload, pin, ban; versions and channels;
  residency map of disk/RAM/VRAM).
- **Data studio**: view, filter, edit, approve or reject training records; provenance of every record.
- **Training**: runs, loss and eval curves, GPU telemetry, the promotion queue, autonomy settings and budgets.
- **Models**: Misty and Dusty versions, channels, evaluations, rollback.
