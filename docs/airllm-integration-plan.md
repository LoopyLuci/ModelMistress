# AirLLM integration plan

Status: a plan. **Phase 0 was partly run on 2026-11-24: see [airllm-spike-results.md](airllm-spike-results.md)** (T3 and T4
pass on a 1.7B model; AirLLM was 38 times slower; T2 and T5 still open). Nothing is built and no source file was changed. Written 2026-11-23 for ModelMistress 0.4.0 on branch
`feat/misty`. The owner reviews it and decides; section 11 lists the decisions.

AirLLM (https://github.com/lyogavin/airllm) runs a model that is bigger than GPU memory by keeping only one transformer
layer in memory at a time and reading the next one from disk.

How to read the evidence. Every claim carries a tag.

| Tag | Meaning |
|---|---|
| [R] | ModelMistress code or docs in this checkout, cited as `path:line`. |
| [S] | AirLLM source, `main` branch, read as text only. It was never installed, imported or run. Cited as `airllm/<file>:line`. |
| [I] | A report by a third party (a GitHub issue). Not reproduced here. |
| [D] | AMD's documentation, "Use ROCm on Radeon and Ryzen" (the Windows support matrix and the PyTorch pip pages). |
| [A] | Arithmetic or inference from the above. Its assumptions are stated beside it. |

## 1. Verdict

**Do not integrate AirLLM as a serving engine.** Do three smaller things instead, each with a stop point:

1. Measure first (Phase 0, no hub changes), on this machine, with thresholds fixed in advance (section 4.4).
2. Fix the gap that is real today: a model larger than free RAM plus VRAM is refused with `503 insufficient_memory`
   [R `src/engine.rs:1545-1560`], although llama.cpp can run such a model by paging its weights from disk. Make that path
   explicit, opt-in and honest about its speed (Phase 1). This serves GGUF files, which the hub already has.
3. Only if the owner names a batch workload that needs a model bigger than memory (scoring, distillation, evaluation),
   add a layer-streaming step to the Forge, written in the Forge's own pinned Python stack and reading the model's
   original safetensors files (Phase 2). It is not a chat engine.

Why, in three sentences. AirLLM reads the whole model from disk for every generated token [S], so a 70B model in bf16
(about 140 GB) takes one to several minutes per token on any disk this machine can have [A, section 3]. What
ModelMistress already does for models bigger than the GPU (CPU/GPU split, experts in RAM, Rigel's paged experts) is
faster and uses the GGUF files the hub already manages [R]. AirLLM would add a second model format, a Python runtime
that runs model-supplied code paths, and a server that does not exist yet, for a speed that no interactive client
tolerates.

| Option | Verdict | Reason | It flips if |
|---|---|---|---|
| (a) Do not integrate; use what exists | **Recommended**, together with (d) | The machine already has four ways to run a model bigger than VRAM (section 3). | Phase 0 shows none of them can run the model the owner wants. |
| (b) `engine = "airllm"`: a managed Python sidecar behind `/v1` | **Rejected for now.** Design kept in appendix A. | Seconds to minutes per token; a second format; a server to write; remote-code exposure. | The owner names a model bigger than RAM plus VRAM that must be served over `/v1`, and Phase 0 shows AirLLM's read path is at least 3x more efficient than llama.cpp's paged path (T2). |
| (c1) Layer streaming as a Forge batch step | **Conditional** (Phase 2) | Prefill is compute-bound, so reading the weights once per pass costs little when a pass carries a whole batch. | Needs a named batch workload and T3, T4 and T5 passing. |
| (c2) Layer streaming inside llama.cpp or Rigel | **No** | `engines/llama.cpp` is a submodule that is not changed from here (`CLAUDE.md`, Conventions); patches live in `engines/patches/`. The need is unproven. | Phase 0 shows llama.cpp's paged reads are the bottleneck and AirLLM's sequential prefetch is not. Then port the method as a patch, not the package. |
| (d) Make bigger-than-memory loads honest (paged placement) | **Recommended** (Phase 1) | Closes a real refusal with an existing engine and format. | Phase 0 shows paged decoding below 0.1 tokens/s on every model the owner cares about. Then leave the refusal and document why. |

## 2. What AirLLM is (verified from source)

All lines below are from the `main` branch. Its last release is v4.0.0 (2026-09-05) and `main` is 96 commits ahead of
it; `setup.py` still says 4.0.0 [S `air_llm/setup.py:18`]. The license is Apache-2.0 [S `setup.py:45`]. The repository has
about 43,000 stars, 114 open issues and 53 open pull requests, and one maintainer, with releases in bursts (2026-08-18,
08-28, 09-05) [GitHub API, read 2026-11-23].

- **Mechanism.** The model is built on the `meta` device. A forward pre-hook loads one module's weights from disk onto
  the device, and a post-hook moves them back to `meta` [S `airllm_base.py:889-919`]. The next module is read on a worker
  thread meanwhile [S `airllm_base.py:187-191, 889-905`]. `generate` just calls transformers' `generate` [S
  `airllm_base.py:923-924`]. Therefore **each generated token is one forward pass, and each forward pass reads every
  layer from disk**.
- **Device.** `default_device()` returns `cuda:0` when `torch.cuda.is_available()`, else `mps`, else `cpu` [S
  `utils.py:88-101`, used at `airllm_base.py:150`]. A ROCm build of PyTorch answers `is_available()` with True, so on
  this machine it would use the GPU, through the Forge's ROCm PyTorch (section 4.1).
- **Input format.** Hugging Face checkpoints (safetensors, or `pytorch_model.bin`) only. A search of the whole tree for
  "gguf" finds nothing [S]. The checkpoint is split into one safetensors file per module [S `utils.py:544-807`]. A shard
  that already holds exactly one module is hard-linked, not copied [S `utils.py:514-532, 656-676`]. Every other shard is
  rewritten, so the split copy is about as large as the model, and `check_space` only warns when the disk is too small
  [S `utils.py:463-484`]. `delete_original` exists and is off by default [S `airllm_base.py:86`, `utils.py:737-741, 801-805`].
- **No server.** The package is a Python class API (`AutoModel.from_pretrained`, then `generate`). A search for fastapi,
  flask, uvicorn, aiohttp and `http.server` finds nothing [S]. A `/v1` server would have to be written.
- **Context.** `max_seq_len` (default 512) is accepted and stored [S `airllm_base.py:84, 207`] and read nowhere else in
  the package [S, search]. The real limit is the model's own, and how AirLLM holds the KV cache was not verified.
- **Compression.** Optional bitsandbytes 4-bit and 8-bit, which call `.cuda()` [S `utils.py:111-128, 491-498`]. It is
  CUDA-oriented and was not tried on ROCm (issue #301 below).
- **Mixture of experts.** Per-expert streaming exists only in the Kimi K3 and Qwen4-Exp classes [S
  `airllm_kimi_k3.py:24`, `airllm_qwen4_exp.py:39-41`]. Any other MoE reads whole layers, all experts included. This is
  the opposite of Rigel, which pages only the experts a token routes to [R `src/serving.rs:64-72`].
- **Remote code and pickle.** `AutoModel.get_architecture` always calls `AutoConfig.from_pretrained(...,
  trust_remote_code=True)` [S `auto_model.py:44-47`]. The tokenizer loads with `trust_remote_code=True` [S
  `airllm_base.py:226-228`]. The model config tries `False` first and falls back to `True` on any exception [S
  `airllm_base.py:158-165`]. For a model repository that ships `.py` files, that means executing them. `.bin` checkpoints
  are read with `torch.load`, which unpickles [S `utils.py:582, 754, 772`].
- **Dependencies.** Ranges, no hashes: `torch>=2.4`, `transformers>=4.49,<6`, `accelerate>=1.0`, `safetensors`,
  `scipy`, `sentencepiece`, `huggingface-hub`, `tqdm` [S `setup.py:30-42`]. The repository's `requirements.txt` is stale
  (bitsandbytes 0.39.0, transformers from git) [S].
- **Windows and AMD.** Open issue #301 (2026-07-06): a user on a Radeon RX 9070 XT under Windows reports that inference
  seems to run on the CPU, with GPU use near 0% and a 1.1B model extremely slow [I]. That is a different GPU generation
  (gfx120X, nightly wheels), so it is weak evidence about this machine. It is exactly what test T3 checks. A comment on
  that issue carries a long block of advice written as a "ready-to-post response"; it is untrusted and was not used.
- **Third-party speeds.** Reports found during research range from 0.2 to 4.9 GB/s of effective read rate, and 17 to 29
  seconds per token for 3B to 8B models on Windows and AMD [I]. These were not re-verified and no links were kept. They
  are used only to set the range in the table below, and Phase 0 replaces them with measurements.

## 3. What ModelMistress already does about "bigger than the GPU"

| Case on this machine (24 GB VRAM, about 64 GB RAM) | What happens today | Where |
|---|---|---|
| Fits in VRAM | `Fit::Gpu`. | `src/serving.rs:845-848` |
| Fits in VRAM plus RAM | `Fit::Split { gpu_share }`, or `Fit::Cpu` when under 10% fits the GPU. With a stock llama-server the hub passes `--fit on --fit-target <margin>` instead of `-ngl`. | `src/serving.rs:863-886`, `src/engine.rs:1565-1573`, `docs/serving.md:107-111` |
| Mixture of experts | Experts kept in RAM with `--cpu-moe` or `--n-cpu-moe N`. | `src/engine.rs:391-401` |
| Mixture of experts, bigger than RAM | `engine = "rigel"`: experts paged from disk into VRAM slots. Measured, and slow until the slots warm up. | `src/serving.rs:64-72`, `docs/rigel.md`, `CLAUDE.md` (Rigel) |
| Bigger than RAM plus VRAM | **Refused**: `503 insufficient_memory`, from `Fit::No`. | `src/serving.rs:854-855, 871-876`, `src/engine.rs:1545-1560`, `docs/serving.md:113` |

Two facts matter for the plan.

- `STATUS.md` lists "a model larger than VRAM" among the things not yet run on the real hub [R `docs/STATUS.md:12-14`].
  So the paged path has never been measured here, in either direction.
- The hub passes no `--no-mmap` and no `--mlock` [R, no match in `src/engine.rs`], so llama-server's own default applies.
  That default is to memory-map the file, which lets the operating system page weights in from disk [A: llama.cpp
  behaviour from memory; `engines/llama.cpp` is not checked out here, so it could not be read]. If so, the refusal at
  `Fit::No` is stricter than the engine needs to be.

So AirLLM's one claim that no existing path covers is "run a model bigger than RAM plus VRAM". The cheaper first
question is whether llama.cpp's own paging already does that well enough. The speed model explains why it might.

**Speed model [A].** Tokens per second is about the effective read rate divided by the bytes read per token. AirLLM reads
all the weights in the checkpoint's own dtype for every token. llama.cpp reads the (smaller) GGUF; for a paged dense
model that is also every weight per token, and for a mixture of experts only the experts a token uses.

| Model, bf16 size (bytes read per AirLLM token) | at 0.5 GB/s | at 2 GB/s | at 5 GB/s |
|---|---|---|---|
| 8B, 16 GB | 32 s/token | 8 s/token | 3.2 s/token |
| 32B, 64 GB | 128 s/token | 32 s/token | 12.8 s/token |
| 70B, 140 GB | 280 s/token | 70 s/token | 28 s/token |

A 300-token answer from the 70B model at 2 GB/s takes about 70 x 300 s, close to six hours. The rates come from section 2
[I]; this machine's disk is not known (section 10). The same models as Q4_K_M GGUF are about 0.3 times the size [A: 4.85
bits per weight against 16], that is 4.9, 19.8 and 42 GB. The first two fit in VRAM. The 70B model splits roughly 24 GB on
the GPU and 18 GB in RAM, and at an assumed 40 GB/s of RAM bandwidth that is about 0.45 s per token (2 tokens/s) [A]. Both
the bandwidth and the result must be measured (Phase 0). Past RAM plus VRAM everything is disk-bound, and there the byte
count and the access pattern decide: a Q4 GGUF reads 3.3 times fewer bytes than bf16, but memory-mapped page faults can
read slower than AirLLM's sequential prefetched reads. Which one wins is the central question of Phase 0 (T2).

**Where layer streaming does pay [A].** Reading the weights once per pass costs the same whatever the batch. When a pass
carries 64 sequences of 2,048 tokens (131,000 tokens), a 70B model needs about 18 PFLOP of compute, which is about 400 s
at 45 TFLOPS (half of the 91 TFLOPS bf16 the Forge preflight measured [R `docs/native/forge.md:36-38`]). Streaming the
140 GB takes about 70 s at 2 GB/s, and it overlaps with compute. So **prefill, batch scoring and distillation are
compute-bound, and layer streaming runs them at about 300 tokens/s on a model that nothing else here can load**. That is
the only workload where AirLLM's idea fits, and it matches an open item: "Batch jobs" [R `docs/gap-analysis.md:22`] and the
Forge's planned distillation.

## 4. Fit with this machine

### 4.1 What AirLLM would run on, and at what speed

- **On the GPU**, not the CPU, through PyTorch ROCm. AMD ships ROCm PyTorch for Windows, and the RX 7900 XTX (gfx1100) is
  in its support matrix [D]. The Forge already installs it: a managed Python 3.12 environment with torch 2.9.1+rocm7.2.1
  wheels from `repo.radeon.com`, each pinned with its SHA-256 [R `forge/requirements-rocm-windows.txt:5-8`], and its
  preflight is recorded as passing on this exact GPU under Windows 10 (91 TFLOPS bf16 matmul) [R
  `docs/native/forge.md:36-38`].
- **Caveats.** AMD's matrix names Windows 11 as the supported OS and says the full ROCm stack is not supported on Windows
  [D]; this machine runs Windows 10 (build 19045), and the Forge preflight passed anyway [R]. The only Windows plus ROCm
  plus AirLLM report is issue #301 [I]. Neither is a reason to stop; both are reasons to run T3 before anything else.
- **Expected speed** (section 3 table): decode is bounded by disk, not by the GPU. For an 8B model, 3 to 30 seconds per
  token. For 70B, 30 seconds to 5 minutes. Batch prefill is compute-bound: about 300 tokens/s at 70B, about 700 at 32B [A,
  assumes 45 TFLOPS sustained; the preflight's 91 TFLOPS is a peak].
- **Not interactive.** `Engine::new` builds one shared HTTP client with only a 5 s connect timeout and no overall timeout [R
  `src/engine.rs:586-614`], so the hub itself would wait. But OpenAI clients, the dashboard chat and every agent have
  their own timeouts, which a minutes-long first token would break.

### 4.2 The format mismatch: Hugging Face safetensors against GGUF

The hub's catalog, its scan and its pull are GGUF-only: `scan` lists `.gguf` files (`src/catalog.rs:6-11, 134`), a
`ModelEntry` has one `path` to a `.gguf` (`src/catalog.rs:23-26`), and `pull` picks a GGUF file by quant label (`src/pull.rs:5, 120-122, 142`).
The Forge already takes a Hugging Face folder as input: `generate_serve` loads `model_path` with
`AutoModelForCausalLM.from_pretrained` (`forge/mm_forge/generate.py:60-72`). The answer therefore differs by phase.

| Question | Phase 1 (paged GGUF) | Phase 2 (Forge streaming step) | Appendix A (sidecar, if ever) |
|---|---|---|---|
| Files | The same GGUF files as today. | The original Hugging Face folder, read-only: `*.safetensors`, `model.safetensors.index.json`, `config.json`, tokenizer files. | A Hugging Face folder plus AirLLM's split copy. |
| Where they live | Existing model roots. | A folder the owner names, under the Forge data dir or another drive (decision 4). The Forge already keeps its data under `forge_data_dir` (`src/engine.rs:180, 655`). | `<forge_data_dir>/hf/<org>--<name>/` and `<forge_data_dir>/stream/<name>/` (the split copy), both owner-approved. |
| Who converts | Nobody. | **Nobody.** The step opens the original shards with `safetensors.safe_open` and reads tensors by name through the index file. That avoids AirLLM's split copy and its hours of conversion time. | AirLLM's own splitter, run once as a job, after a disk check the hub does first. |
| Disk cost | The GGUF only (about 0.3x of bf16). | The original only: 2 bytes per parameter (16 / 64 / 140 GB at 8B / 32B / 70B). | The original plus a split copy of about the same size, minus any shard that is already one module [S `utils.py:656-676`]. With `delete_original=True` the copy replaces the original and is not resumable [S]. |
| In `model.list` and `/v1/models` | Yes, with `serving.placement: "paged"`. | **No.** It is a Forge input, not a served model. The step appears in `forge.status` and the Forge pages. | A catalog entry with `format: "hf"`, `engine: "airllm"` and a `stream` placement. |
| In `model.plan` | New `placement: {"where": "paged", "paged_mib": ..., "note": "disk-bound"}` (`src/serving.rs:889`). | Not applicable. The step reports its own plan (read time per pass, VRAM per layer, batch size) before it starts. | `{"where": "stream", "disk_gb_per_token": ...}`. |
| In `pull` | Unchanged. | Not a catalog pull. A one-off download of a named repository, only on the owner's say-so, with size, source and SHA-256 shown first. | `hf.co/<org>/<repo>:safetensors`, new code in `src/pull.rs`. |

Disk space seen on this machine during this session [observed once, not guaranteed]: `X:` 78 GB free, `D:` 354 GB free
and 96% full. A 70B bf16 model (140 GB) does not fit on `X:`. On `D:` it would take 140 GB, and 280 GB with a split copy.
Nothing in this plan downloads a model without the owner choosing the drive (decision 4).

### 4.3 What Phase 0 must not do

No hub changes, no use of `home\`, no use of the Forge's own environment (a spike package that fails must not break
Forge training), and no model download that the owner has not approved.

### 4.4 Phase 0: the benchmark spike

Run in a disposable folder, for example `D:\ClaudeCodeTemp\ModelMistress\airllm-spike\`, that holds a throwaway virtual
environment, the downloaded models and the results. AirLLM is installed only there, only by the owner's command, as an
exact version with `pip --require-hashes`, after recording the wheel's SHA-256. The hub that is running is never used;
llama.cpp is run directly with the same flags the hub passes (`src/engine.rs:1846-1902`), or by a second hub with its own
`--home` on another port.

Model names are examples from memory and must be checked to still exist before download: `Qwen/Qwen3-8B` (bf16 about 16
GB), `Qwen/Qwen3-32B` (about 65 GB) and, only with the owner's drive and approval, a 70B (about 140 GB). AirLLM's generic
path takes "any standard `*ForCausalLM`" [S `auto_model.py:13-16`]. For the bigger-than-memory test, use any GGUF larger than free
RAM plus free VRAM, as `model.plan` measures it (`Fit::No`, with `need_ram_mib` and `free_ram_mib`): on a machine with
about 50 GB of free RAM that means a model of 75 GB or more, such as a large MoE GGUF at Q4.

Measure, per model, with the same prompt and the same greedy sampling:

| # | Measurement | How |
|---|---|---|
| M1 | Is the GPU used? | GPU utilisation (Task Manager or `rocm-smi` equivalent) and `torch.cuda.memory_allocated()` during a forward pass. |
| M2 | Raw disk speed | A plain sequential read of a large file on the same drive, with the OS cache dropped. This is the ceiling for everything below. |
| M3 | Conversion | Time and extra bytes written by AirLLM's split. |
| M4 | Decode | Seconds per token over 32 tokens after one warm-up token. |
| M5 | Prefill | Tokens per second for a 1k and a 4k prompt, and for a batch of 32 sequences of 2k tokens (streaming only). |
| M6 | Effective read rate | Bytes per pass divided by pass time, against M2. |
| M7 | Peak RAM and peak VRAM | Task Manager and `torch.cuda.max_memory_allocated()`. |
| M8 | Correctness | The first 32 greedy tokens of the 8B model, against the Forge's whole-model `generate` in bf16. |
| M9 | Side effects | Files written outside the spike folder and network connections opened during a run, with `HF_HUB_OFFLINE=1` set. |
| M10 | llama.cpp baselines | Same models as Q4_K_M and Q8_0 GGUF: prompt processing and generation tokens/s, and for the over-memory GGUF the paged rate, time to first token, and whether the machine stays responsive (page file use, other programs). |

Pass and fail. These thresholds are proposals; the owner can change them before the spike starts.

| Test | Pass | Fail means |
|---|---|---|
| **T1** speed class of any path | Decode of 2 tokens/s or more with a first token inside 30 s is *interactive*. 0.3 to 2 tokens/s is *patient* (explicit opt-in). Under 0.3 is *batch only*. | The path is never offered to a chat client. |
| **T2** is AirLLM's read path better? | On a model bigger than RAM plus VRAM, llama.cpp paged decode is 0.3 tokens/s or more: **AirLLM is closed; Phase 1 proceeds.** If llama.cpp is under 0.1 tokens/s and AirLLM's effective read rate (M6) is at least 3x llama.cpp's, port the *method* (option c2), not the package. | |
| **T3** the GPU is really used | M1 shows the GPU busy and VRAM allocated in the 8B run. | Stop: AirLLM on this stack runs on the CPU (the #301 failure). Option (b) and Phase 2 are dead for AirLLM itself. |
| **T4** correctness | The first 32 greedy tokens equal the Forge's whole-model run, or the divergence is explained by bf16 reduction order. | Stop: do not trust its output. |
| **T5** batch prefill | A sustained 150 tokens/s or more in batch prefill on a model bigger than RAM plus VRAM, with compute at least half the time (GPU busy). | Phase 2 is not worth building. |

Duration: two to three days elapsed, mostly downloads and runs, about one day of attention. Output: a results table
appended to this file (or `docs/airllm-spike-results.md`), a recommendation per threshold, and nothing in `src/`. Rollback:
delete the spike folder. No hub, Forge or settings state is touched.

## 5. Architecture of the recommendation

### 5.1 Phase 1: explicit paged placement (GGUF, llama.cpp)

Goal: a model bigger than RAM plus VRAM can be loaded on purpose, runs through llama.cpp's own paging, is labelled
"paged", and can never be chosen by anything automatic. Default behaviour does not change.

| Integration point | Change |
|---|---|
| `src/serving.rs:845-856` `enum Fit` | Add `Paged { paged_mib: u64, need_ram_mib: u64 }`. |
| `src/serving.rs:863-886` `place()` | When the result would be `Fit::No` and the load allows paging, return `Paged` if the parts that cannot be paged (KV cache, state, compute buffers, `RAM_OVERHEAD_MIB`) fit in free RAM. The weights beyond RAM plus VRAM are `paged_mib`. `Fit::No` stays the answer when paging is not allowed or the non-pageable parts do not fit. |
| `src/serving.rs:889` `fit_json` | `{"where": "paged", "paged_mib": ..., "note": "disk-bound; expect seconds per token"}`. Appears in `model.plan` (`src/hub.rs:879-912`). |
| `src/serving.rs:26` `ServeFlags` and `src/engine.rs:80` `Settings` | `ServeFlags.paged: Option<bool>` (per model, in profiles) and `Settings.allow_paged_load: bool`, default **false**. A paged load needs both. |
| `src/engine.rs:1538-1562` `placement_decision` | Let `Paged` through as `Ok(None)` when both are on. The `insufficient_memory` message gains the words "set `allow_paged_load` and the model's `paged` to try it from disk, which is slow". |
| `src/engine.rs:1783-1960` `spawn` | No new flag if the spike confirms llama.cpp's default (mmap) is what is wanted. Never pass `--mlock`. Check what `--fit on` (`src/engine.rs:1565-1573`) does for a model larger than memory; if it refuses, pass `-ngl` explicitly for paged loads. |
| `src/engine.rs:1955-1977` health loop, `Settings.load_timeout_s` (`src/engine.rs:128`) | The first pass touches every page. A paged load gets a longer deadline (a per-load override, not a changed global). |
| `src/engine.rs:2112-2130` `forward` | A request alone must not load a paged model: if the model is paged and not loaded, return `409 needs_explicit_load` with the message. The owner loads it with `model.load`. This also keeps peer keys from thrashing the machine (peer keys reach the model routes only, `CLAUDE.md`). |
| `src/serving.rs:540, 976` `Serving.placement`, `placement_of` | A new value `"paged"` when the process's memory is below the weights. |
| `src/misty/roster.rs:178-181` `fits`, `:233, 267` | No change: `fits` compares the file size against free RAM, so a paged model stays `unfit` and the roster and `auto` cannot pick it [R]. Add a test that locks this in. |
| `src/misty/auto.rs:114-182` | No change. The router resolves roles from the roster, so it never reaches a paged model. A paged model fills a role only if the owner writes it into `[roles]` themselves, and then the `unfit` status shows. |
| CLI and MCP | No new operation. `model.load` and `model.plan` gain the optional argument `paged`; operations are declared once in `src/hub.rs:234` (`backend.list` at `:283`, `model.plan` at `:354`) and every one is an MCP tool (`CLAUDE.md`), so both follow. |
| Dashboard | `frontend/src/chat/profiles.ts:20-27` (profile field list) gets `paged`, with the warning as its help text. `frontend/src/views/ModelsView.vue:433-435` (where the weights went) shows a "paged, disk-bound" chip, and `:543-552` (the advice built from `placement.where`) gets a `'paged'` case next to `'split'`, `'cpu'` and `'none'`. The Settings page gets the `allow_paged_load` switch with the same words. |

What this must not do. It must not make `auto` or Misty's quick path use a paged model. It must not change the default
refusal. It must not load a paged model from a request alone. It must not claim a speed: it shows the measured
tokens/s once the model has run (`/props` and the log, as `learn_serving` already does, `src/engine.rs:1598-1604`).

Duration: three to four days, one pull request: code, tests, `docs/serving.md`, `docs/STATUS.md`, `CHANGELOG.md` in the
same commit (`CLAUDE.md`). Done means: the stand-in tests and Vitest tests pass; with the switch off, `model.plan` for an
oversized model returns exactly what it returns now; with it on, a real over-memory GGUF loads on the owner's machine and
the measured rate is written into `docs/STATUS.md`. Rollback: revert the commit, or leave `allow_paged_load` off (the
default restores today's behaviour exactly).

### 5.2 Phase 2: layer streaming as a Forge step (gated)

Build only if the owner names a workload (decision 1) and T3, T4 and T5 passed.

- **What it is.** A new Forge step (`forge/mm_forge/stream_score.py`, registered beside the others in
  `forge/mm_forge/steps.py`) that takes a Hugging Face folder and a batch of sequences, and returns per-sequence
  log-probabilities (scoring), or hidden states at chosen positions (distillation targets). There is no decode loop,
  which is the part that is slow.
- **How.** Build the model on `meta` with `accelerate.init_empty_weights`. For each layer: read its tensors from the
  original shards with `safetensors.safe_open`, move them to the GPU, run the *whole batch's* hidden states through the
  layer, free the weights, and prefetch the next layer on a thread. Layers are the same decoder blocks that transformers
  defines, so rotary embedding and attention masks come from transformers rather than being rewritten. The first release
  supports only dense Llama, Qwen2 and Qwen3 style models and refuses anything else with a message.
- **Dependencies.** None added. `torch`, `transformers`, `safetensors` and `accelerate` are already pinned in the Forge
  environment (`forge/requirements-rocm-windows.txt:8-21`). AirLLM is not installed.
- **No remote code and no pickle by construction.** The step refuses a folder that contains `.py` files, an `auto_map` in
  `config.json`, or `.bin` files, and passes `trust_remote_code=False`. This removes AirLLM's two main hazards (section 2).
- **Hub integration.** The same way Forge steps are run now (`src/forge/runner.rs`, the `forge.*` operations in
  `docs/native/forge.md:62-64`): a job with progress, a result file. It does not touch `Engine`, the catalog, `Fit`,
  the roster or `auto`.
- **Environment hygiene.** The Forge's worker spawn passes the hub's whole environment on (`.envs(cmd.env...)` with no
  `env_clear` [R `src/forge/runner.rs:195-212`]). If the hub was started with `MM_TOKEN`, a Python worker inherits it [R
  `src/hub.rs:5`]. For this step (and as a good change in its own right), the worker is started with an allowlist
  environment. That is a small change to `spawn_with` that existing steps must opt out of explicitly; it is part of this
  phase, with a test.
- **Duration** one to two weeks. **Done** means: the step on a tiny random-weights model matches the whole-model forward
  pass in CI (CPU); on the owner's machine it scores a batch on a model bigger than RAM plus VRAM at the rate T5 required.
  **Rollback:** remove the registration; no catalog, serving or settings state exists to undo.

### 5.3 Phase 3: not planned

The sidecar (appendix A). It starts only if the flip condition in section 1 for option (b) is met and the owner
asks for it.

## 6. Safety and supply chain

The risk is specific. The hub holds the owner's token and starts processes; Python packages and model repositories can run
code at install time and at load time. All of the following apply to anything of AirLLM's that ever runs here.

- **Refused by default.** No setting turns it on without the owner naming it. Any install or download step this plan
  adds must print the source, size and SHA-256 and wait for the owner's yes, as `misty setup` does today (components are
  installed as jobs, "checked by size and SHA-256", only on the owner's request: `src/misty/ops.rs:68`).
- **No AirLLM in the Forge environment** unless Phase 0 and a later decision say so. Phase 0 uses a throwaway environment.
  If a sidecar is ever built, it gets its own environment and its own requirements file, with `--require-hashes`.
  AirLLM's declared dependencies are ranges without hashes [S `setup.py:30-42`], so every transitive package would need
  a recorded hash. The Forge's AMD wheels already have theirs; its PyPI lines (`numpy==2.2.6`, `safetensors==0.8.0`,
  `transformers==4.57.6`, `accelerate==1.13.0`) are pinned by version only [R `forge/requirements-rocm-windows.txt:11-21`],
  which is a gap worth closing separately.
- **No remote code.** Reject model folders with `.py` files or `auto_map`; reject `.bin` and any pickle. AirLLM itself
  does neither: it tries `trust_remote_code=True` for the architecture lookup, the tokenizer and as a fallback (section
  2). The Forge already has one pickle read, of its own optimiser state, `torch.load(..., weights_only=False)` [R
  `forge/mm_forge/train.py:286`]; nothing here widens it to files from outside.
- **Token hygiene.** Python workers get an allowlist environment, never the hub's (section 5.2). Engine children
  already get a random key per process and listen on 127.0.0.1 only [R `src/engine.rs:4, 1846, 1852, 1897`]; any Python
  child follows the same rule and receives its key through the environment, never on a command line.
- **Files.** Model folders are read-only to the worker. It writes only to its job folder. Run with `HF_HUB_OFFLINE=1` and
  `TRANSFORMERS_OFFLINE=1`; downloads are separate, owner-approved steps.
- **Network.** The worker needs none at run time. Windows cannot restrict one process's network without a firewall rule,
  and changing firewall settings is the owner's call, so the plan only offers the rule and does not apply it.
- **Peers.** Peer keys reach the model routes only [R `CLAUDE.md`]. Nothing in this plan gives them a way to start a paged
  or streaming job (section 5.1, `needs_explicit_load`).
- **Maintainer risk.** One maintainer, bursty releases, `main` ahead of the release [S]. Pin an exact version and its hash;
  never track `main`.

## 7. Test plan

The repository's style: unit tests in the same file as the code, a stand-in server for engine behaviour, Vitest for the
dashboard, and the pipeline (`python ci/pipeline.py`) as the gate. Every change to `src/` ships with a test (`CLAUDE.md`).

| Layer | Tests | Phase |
|---|---|---|
| `place()` and `fit_json` (`src/serving.rs` tests, next to `placement_of` at `:1343-1359`) | Oversized model: `No` with the switch off, `Paged` with it on. Non-pageable parts too large: still `No`. `fit_json` shape. | 1 |
| Engine with a stand-in (`adopt`, `StandIn`, `src/engine.rs:1158`, and the fake `llama-server` scripts near `:3444-3457`) | `placement_decision` lets `Paged` through only when both flags are set. `forward` returns `409 needs_explicit_load` for an unloaded paged model. A paged load gets the longer deadline. `spawn` passes no `--mlock` and no `--no-mmap`. | 1 |
| Hub calls (`json_post` against `/v1/call/model.plan`, `src/hub.rs:2608, 2692`) | `model.plan` for an oversized model, both settings. `model.load` with `paged`. | 1 |
| Misty (`src/misty/roster.rs:406-418` style) | A paged-only model is `unfit` and is never returned by `router.pick` or `auto`. | 1 |
| Vitest (`frontend/src/views/ModelsView.test.ts`, `frontend/src/chat/profiles.test.ts`) | The "paged" chip and warning; the `paged` profile field; the Settings switch. | 1 |
| Forge Python tests (`forge/tests/`, runnable on the CPU with a tiny random-weights Llama built in the test, no download) | Layer-streamed forward equals whole-model forward. The step refuses `.py`, `auto_map` and `.bin`. Allowlist environment does not contain `MM_TOKEN`. | 2 |
| Forge Rust side (`src/forge/tests.rs`) | The worker spawn with the allowlist; opt-out for existing steps. | 2 |
| Real hardware only, not in CI | Whether the GPU is used; true speeds; llama.cpp's paging on an over-memory GGUF; whether Windows stays responsive while paging (page file, other programs); disk throughput; time to first token; `--fit` behaviour for a model larger than memory; streaming-step throughput on the 7900 XTX. Each result is written to `docs/STATUS.md` with its date, as the other measurements are. | 0, 1, 2 |

No test downloads a model, and none starts the live hub.

## 8. Phases and stop points

| Phase | Scope | Files touched | Duration | Done means | Rollback | Stop point |
|---|---|---|---|---|---|---|
| **0** Benchmark spike | Section 4.4. | None in the repo, except appended results. | 2-3 days elapsed | All of M1-M10 recorded; T1-T5 answered. | Delete the spike folder. | Read the results. If T3 or T4 failed, AirLLM is closed. If T2 shows llama.cpp paging is fine, go to Phase 1. If every paged path is under 0.1 tokens/s, stop and document. |
| **1** Paged placement | Section 5.1. | `src/serving.rs`, `src/engine.rs`, `src/hub.rs` (arguments), `src/misty/roster.rs` (test), `frontend/src/chat/profiles.ts`, `frontend/src/views/ModelsView.vue`, the Settings view, tests, `docs/serving.md`, `docs/STATUS.md`, `CHANGELOG.md`. | 3-4 days | Tests pass; real over-memory GGUF loaded and measured on the machine. | Revert the commit; or leave the setting off. | Is the measured rate useful? If not, ship the honest refusal text and stop. |
| **2** Forge streaming step (gated) | Section 5.2. | `forge/mm_forge/` (new step, `steps.py`), `forge/tests/`, `src/forge/runner.rs` (environment allowlist), `src/forge/tests.rs`, `docs/native/forge.md`, `CHANGELOG.md`. | 1-2 weeks | CPU equivalence test passes in CI; the owner's batch runs at the T5 rate. | Remove the step's registration. | Does the owner's real workload use it? If not, stop. |
| **3** Sidecar | Appendix A. | Many. | 3-5 weeks | Not planned. | Not planned. | Not started without the flip conditions and a new decision. |

## 9. Risks

| Risk | Effect | Mitigation |
|---|---|---|
| llama.cpp paging thrashes Windows (page file, unresponsive machine) | The owner's machine stalls during a paged load. | Phase 1 is opt-in on two switches; the spike measures it first; never `--mlock`. |
| `--fit on` mishandles a model bigger than memory | The load fails or fits wrongly. | Spike M10; pass `-ngl` explicitly if needed. |
| AirLLM is slower than the third-party range on this machine, or runs on the CPU (#301) | The idea is dead for AirLLM. | T3 and T1 before anything is built. |
| A paged model is picked by `auto`, Misty or a peer | A quick chat waits minutes. | `fits` keeps it `unfit`; `needs_explicit_load`; tests that lock it in. |
| The Forge environment is polluted by an AirLLM install | Training and `generate_serve` break. | AirLLM only in a throwaway environment. |
| The worker inherits the owner's token | A bug or malicious package in Python can call the hub as the owner. | Allowlist environment (Phase 2), found in `src/forge/runner.rs:195-212`. |
| A single maintainer stops, or `main` changes the API | The sidecar breaks. | Not building the sidecar; pin if ever built. |
| Disk fills (a 140 GB checkpoint plus a split copy) | Other programs fail ("Disk full" has happened on this machine, `CLAUDE.md`). | The owner picks the drive; a free-space check before any download; no download without the owner's yes. |
| The thresholds in section 4.4 are wrong for the owner's use | A good path is closed or a bad one kept. | The owner edits them before the spike starts. |

## 10. What could not be verified

- llama.cpp's behaviour when the file is larger than RAM plus VRAM, and its default for memory-mapping. `engines/llama.cpp`
  is not checked out here (only a pointer file), so the claim comes from memory and is marked [A]. Phase 0 settles it.
- This machine's disk type and sequential read rate, and how much RAM is free in normal use. The speed table uses a range,
  not a measurement.
- AirLLM's speed and GPU use on this machine. The third-party numbers were not re-verified and no links were kept.
- How AirLLM holds the KV cache (on the GPU for all layers, or per layer), and so how its memory grows with context.
- Whether the model names in section 4.4 still exist as written, and the exact sizes of an over-memory GGUF.
- AMD's matrix and pip pages were read this session; the support statement for Windows 10 and the effect of the
  "full ROCm stack is not supported on Windows" footnote on PyTorch were not tested beyond the Forge preflight.
- The `main` branch was read as text; behaviour of v4.0.0 as released (the version pip installs) can differ from it by 96
  commits.

## 11. Decisions for the owner

Each is a question, with a recommended answer.

1. **What do you want to run, and how?** A model bigger than RAM plus VRAM for chat, or a batch job (scoring,
   distillation, evaluation) on one? *Recommended:* name the model and the workload. With none named, stop after Phase 1.
2. **May Phase 0 download models, and which?** *Recommended:* yes, an 8B in bf16 (about 16 GB, plus about 16 GB for AirLLM's
   split copy); a 32B only if the 8B passes T3 and T4; no 70B. For the paging test, the smallest GGUF that exceeds free RAM
   plus VRAM, as `model.plan` reports it.
3. **Which drive holds the spike's files?** *Recommended:* `D:\ClaudeCodeTemp\ModelMistress\airllm-spike\` (it has about
   354 GB free; `X:` has 78 GB). The folder is deleted afterwards.
4. **Where do Hugging Face weights live if Phase 2 happens?** `X:` is too small for 70B; `D:` is 96% full. *Recommended:*
   decide when a model is named; do not choose a drive now.
5. **Are the speed classes right?** Interactive at 2 tokens/s or more; patient from 0.3 to 2; batch only below 0.3.
   *Recommended:* yes.
6. **May paged loads exist at all?** Default off, two switches, owner-only load. *Recommended:* yes.
7. **May AirLLM be installed even in a throwaway environment?** *Recommended:* yes, for Phase 0 only, as an exact version
   with a recorded hash, never in the Forge environment.
8. **Should the Forge worker get an allowlist environment now, as its own change?** It is a real exposure whatever happens
   with AirLLM (section 5.2). *Recommended:* yes, as a separate small change.

---

## Appendix A: the sidecar design (option b), kept in case the flip conditions are met

Not recommended now. It is written down so that, if the owner decides otherwise, the plan is concrete. It adds one engine
name, `airllm`, whose "server" is a Python worker that the hub starts the way it starts `llama-server`.

**Install.** `model-mistress airllm setup`, in the style of `misty setup` (`src/misty/ops.rs:68`): a plan first (sizes,
URLs, hashes), then a job that runs only when the owner says so. It creates a separate virtual environment from the
Forge's Python 3.12, installs the Forge's pinned torch and the packages above with `--require-hashes`, then
`airllm==<exact>` with `--no-deps` and its few extra dependencies (`scipy`, `sentencepiece`, `huggingface-hub`, `tqdm`)
each pinned with a hash. It sets no network at run time, and installs nothing on its own.

**Start.** A branch in `Engine::spawn` (`src/engine.rs:1783-1960`) picks the command: the sidecar's Python with
`-m mm_forge airllm_serve`, a free port from `free_port()` (`:1845`), 127.0.0.1 only, and a fresh key from
`hub::new_token()` (`:1846`) passed in the environment where `LLAMA_API_KEY` is today (`:1897`), plus an allowlist
environment. **Health:** the existing loop (`:1955-1977`) polls `GET /health` (200 ready, 503 loading); the worker must
implement it. **Stop:** `Instance::stop`, as for llama-server.

**The server (to be written).** AirLLM has none (section 2). The worker would implement `/health`,
`/v1/chat/completions` and `/v1/completions`, one request at a time (a second one gets the hub's existing `503 busy` path,
`src/engine.rs:2151-2157`), streaming through transformers' `TextIteratorStreamer` as server-sent events, with a comment
event as keep-alive between tokens that take minutes. It would call `AirLLMBaseModel` directly, never `AutoModel` (section
2), after the folder check of section 5.2. Cancellation: a flag checked in the post-hook, so a cancel takes effect after
the current layer (seconds), not the current token (minutes). Context is the model's own limit; `max_seq_len` does nothing
[S].

**Placement.** `src/serving.rs:88` gets `"airllm"` in `ENGINES` (checked at `:243-245, 316-318`). `Fit` gets a `Stream`
variant: the VRAM need is the largest layer plus KV cache plus a margin, the RAM need is the prefetch buffer, and the
estimate is a disk-bound time per token: bytes divided by a measured read rate stored with what the hub already learns
(`load_learned`, `src/engine.rs`). `placement_decision` returns no eviction (it needs little VRAM), and `insufficient_memory`
is returned when one layer plus KV does not fit. `placement_of` reports `"stream"`.

**Fallback ladder.** `Try` (`src/engine.rs:2282`) and `after_failure` (`:1747`) get one rule: no CPU fallback for this engine
(it would be 5 to 10 times slower and look like a hang), and a crash is reported, not retried. `load_with_fallback`
(`:1690`) is unchanged.

**Watchdog.** The hub's watchdog watches only the hub's own `/v1/health` (`src/watchdog.rs:101-125, 136`). A sidecar's death
is seen by the load loop (`child.try_wait`, `src/engine.rs:1963`) and its hang by a per-token deadline inside the
worker. The hub watchdog is not extended.

**Catalog and profiles.** A new entry type `format: "hf"` in `src/catalog.rs`, a scan of folders that hold
`config.json` and `*.safetensors`, `source: "hf"` in `src/pull.rs`, a profile engine value in
`frontend/src/chat/profiles.ts:20-27`. `Serving.engine` (`src/serving.rs:521`) already carries the engine name.

**Misty, `auto` and the dashboard.** A streaming model is never `ready` in the roster, so `auto` cannot pick it;
the Models page shows it with a "seconds per token" badge from its estimate; the chat window warns before sending.

**Tests.** The same stand-in approach as Phase 1, plus a stand-in worker that speaks the same protocol; the install job with
a fake index; the folder check with `.py`, `auto_map` and `.bin` fixtures. Real hardware: everything about speed.

**Duration:** three to five weeks. **Rollback:** remove `"airllm"` from `ENGINES`; delete the sidecar environment.
