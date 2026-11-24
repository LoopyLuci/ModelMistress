# Rigel as ModelMistress's own engine, with AirLLM's method built in: plan

Status: a plan plus the first facts, written 2026-11-24. Owner's instruction (2026-11-24): make Rigel a custom replacement of
Betelgeuse, not something that pulls Betelgeuse's source every time, and weave AirLLM's idea into it as one engine. Read this
with [rigel.md](rigel.md) (what Rigel is today), [airllm-integration-plan.md](airllm-integration-plan.md) and
[airllm-spike-results.md](airllm-spike-results.md) (why AirLLM itself is not used).

## 1. Facts this plan stands on

- **AirLLM is not code that can be linked into Rigel.** It is Python and PyTorch over Hugging Face safetensors; Rigel is C++ over GGUF
  [airllm-integration-plan.md section 2]. What can be taken is the *method*: keep one layer's weights resident, read the next
  from disk on a worker thread while the current one computes.
- **The method has two halves, and they behave differently** (measured in the spike, and by the plan's arithmetic):
  - *Decode* (one token per pass) re-reads every layer for every token. AirLLM was 38 times slower than the whole model even
    with the files in the OS cache. Nothing in Rigel should copy that for chat.
  - *Prefill and batches* are compute-bound, so streaming weights once per pass is nearly free.
- **llama.cpp, and so Rigel, already has the batch half.** When a layer's weights sit in host memory and the batch is large,
  the scheduler copies them to the GPU for that op (`op_offload`, on by default; `-nopo` turns it off in `llama-bench`;
  `ggml/src/ggml-backend.cpp:970-972`). A split load therefore already prefills on the GPU through streamed weights.
- **Rigel already pages the MoE half.** Routed experts stream from disk or an expert pack into VRAM slots, with a host-RAM
  cache, a GPU doorbell and predictor kernels (`src/moe_pager.cpp`, `moe_doorbell.cu`, `moe_predictor.cu`, `llama-moe-cache.cpp` in the
  Rigel tree). Decode of a 17.7 GB MoE model measured 3.5 tokens/s on a cold start, 7 to 22 once the slots warm ([rigel.md]).
- **What neither does** is a *dense* model bigger than RAM plus VRAM: there the weights are memory-mapped and the operating
  system faults pages in at random as the layers run. That is the one place AirLLM's sequential prefetch could beat what exists
  (plan test T2). It is not measured: the machine has no such model. Phase 1 (`paged` loads) is the way to measure it.
- **Rigel's source is MIT-licensed** (the ggml authors' licence file at the root of `engines/rigel`), so owning a modified copy is allowed.

## 2. Where Rigel stands in ModelMistress today (checked 2026-11-24)

| Item | State |
|---|---|
| Source | `engines/rigel` is a git submodule of `https://github.com/Betelgeuse-AI/Betelgeuse.git` at `a6b46ade3` (the pinned commit; 174 MB); it was **not checked out** on this machine until 2026-11-24. |
| Our changes | Were `engines/patches/rigel.patch` (137 KB). **Since 2026-11-24 baked into `LoopyLuci/Rigel`** (step R1 done): the patch file and the build step that applied it are gone. |
| Build | `cargo build --features rigel-engine` needs the HIP SDK (`HIP_PATH`), CMake, Ninja and Visual Studio's libraries. **It failed on this machine with the newest MSVC toolset (14.51)**: HIP's clang 21 cannot compile against its headers (`__device__ function 'isless' cannot overload __host__ __device__ function`). Fixed in `src/build.rs` by `MM_RIGEL_MSVC_TOOLSET=14.44.35207`, which pins an older toolset installed beside it. See section 5 for whether it then built. |
| In releases | **No.** Releases 0.4.1 and 0.4.2 were built without `rigel-engine` (the HIP SDK is needed to build it), so the installers do not contain Rigel. |
| In the hub | Engine choice, one-slot rule, `moe_*` flags, `backend.list`, `model.plan` are implemented and unit-tested; the hub found a built-in Rigel on 2026-11-15. |
| Not integrated | Misty and `auto` never choose Rigel (`engine = "rigel"` is a profile choice only); the Models page has no Rigel chip or MoE-paging advice; `moe_slots` is not sized from free VRAM; no install path for users without the HIP SDK. |

## 3. The design

Name: keep **Rigel**, the engine ModelMistress owns. Betelgeuse stays an upstream to merge from, not a dependency to pull.

### 3.1 Own the source (step R1) - done 2026-11-24, see the log at the end
Replace "submodule plus 137 KB patch applied at build time" with a repository the owner controls that holds the patched tree
(`rigel` branch: upstream at the pinned commit, the patch committed, further work on top), and point `.gitmodules` at it. The
upstream stays a second remote for cherry-picks. Then `src/build.rs` no longer applies a patch. **This needs the owner to create
the repository (an outward-facing action); nothing here pushes anywhere.** Alternative if the owner prefers: keep the patch
and add new work as further patches in `engines/patches/` (smaller, but the patch stack grows).

### 3.2 Make the existing integration whole (step R2)
- A reproducible Windows build: the toolset pin above, written into `rigel.md`, with the build log of a passing build.
- Ship `rigel-server` as a setup component, like `prism-gpu` (`misty setup --install rigel`): a pinned asset checked by size
  and SHA-256. Needs the owner to host the binary (a GitHub release asset) and to accept that the AMD HIP runtime must be on the
  machine (`HIP_PATH`/`PATH`).
- Models page: engine chip and "paged experts" advice; `model.plan` already reports `serving.engine`.
- Misty: a profile helper that sets `engine = "rigel"`, `moe_direct_io` and `moe_slots` for a MoE GGUF that does not fit the card,
  sized from free VRAM and the model's expert size, never chosen for a model that fits (llama.cpp is faster there).
- Tests on real hardware: a MoE model bigger than VRAM (the 2026-11-15 run used Qwen3-Coder-30B-A3B UD-Q4_K_XL).

### 3.3 The hybrid (step R3, only after measuring)
Goal: one engine that serves *any* GGUF bigger than the card, at the best speed the machine allows, by choosing per model:

| Model | Today | Hybrid |
|---|---|---|
| Fits the card | llama.cpp | unchanged |
| MoE, bigger than the card | Rigel paged experts | unchanged (this is the part that works) |
| Dense, fits card + RAM | split (`--fit`), op-offload for batches | unchanged |
| Dense, bigger than card + RAM | refused (`insufficient_memory`), or Phase 1 paged load through the OS | **sequential layer prefetch**: a reader thread reads layer N+1 (and N+2) from the file with large aligned reads into a ring of host buffers while layer N computes, replacing random page faults; with op-offload the ring feeds the GPU for batches. Reuses Rigel's host cache, io_uring shim and doorbell. |

Only the last row is new, and it is only worth building if a measurement says so. **Gate (T2 of the AirLLM plan):** run a dense GGUF bigger
than RAM plus VRAM through the Phase 1 paged load and record decode and prefill; the prefetch ring is built only if the OS
paging is under 0.3 tokens/s *and* a read-ahead test shows sequential reads of the same file at least 3 times faster than the page-fault rate. If
paging is fine, stop and say so. Expected result, from the arithmetic: decode stays disk-bound (about the file size divided by
the disk's read rate per token), so the ring mostly helps prefill and a patient batch use, not chat.

## 4. What I will not claim
- That the hybrid is faster for chat: the spike and the arithmetic say a dense model bigger than memory is disk-bound per token.
- That anything runs on a model bigger than memory here: the machine has none, and downloading one needs the owner's yes
  (name, size, drive).

## 5. Build and test log
See the section "Rigel build, 2026-11-24" appended below by the agent that ran it.

## Rigel build, 2026-11-24
- Fetched `engines/rigel` at the pinned `a6b46ade3` (`git submodule update --init --depth 1`, 6 s, 174 MB), on the owner's yes.
- First build (`cargo build -p model-mistress --features rigel-engine`, HIP SDK 7.2, `gfx1100`, jobs 2): **failed**, 761 compile errors, all
  HIP's clang 21 against MSVC 14.51 headers. Second build with `MM_RIGEL_MSVC_TOOLSET=14.44.35207` (new in `src/build.rs`): **built**, 353
  steps in 5 min 17 s; `rigel-server.exe` is 100 MB and reports `0.8.0-dev`, Clang 21, device `ROCm0: AMD Radeon RX 7900 XTX`.
- Run through a scratch hub (own home, port 7891, removed afterwards; the owner's hubs untouched) with the same model as 2026-11-15,
  `Qwen3-Coder-30B-A3B-Instruct-UD-Q4_K_XL` (17.7 GB) and `engine = "rigel"`, `moe_direct_io`, `moe_slots 1536`: `backend.list` found the
  built-in Rigel; `model.plan` showed the engine and arguments; the load took 10 s and held 5.75 GB of VRAM; "17 times 23" answered
  391; a 200-token code answer ran at 12.0 tokens/s (the first short answer: 2.2 tokens/s, cold). `service.status` said `placement: gpu`.
- The built-in version string read `unknown` (git refused the submodule checkout as another user's); `build.rs` now passes
  `safe.directory=*`. Not rebuilt to confirm.
- Not done: the hybrid (section 3.3), the owner's repository for the source (3.1), the setup component and Models page work (3.2).

### R1 done, 2026-11-24
On the owner's instruction (the patched tree "must stay separate", not a branch on top of Betelgeuse): `git archive` of the pinned
Betelgeuse tree, the patch applied, `NOTICE.md` added, one fresh commit (`0ae9e6f5c`, no upstream history), pushed to the new public
repository `LoopyLuci/Rigel` (`main`). `.gitmodules` now points there; `engines/patches/rigel.patch` and its `apply_patch` call in
`src/build.rs` were removed. To take later Betelgeuse changes, merge them by hand into `LoopyLuci/Rigel` (no remote for it is set).

## Layer read-ahead built and measured, 2026-11-24
**Built (in `LoopyLuci/Rigel`, `src/llama-readahead.h`, hooked in `src/llama-context.cpp`; ModelMistress `layer_readahead` Rigel option):**
a worker thread that prefetches the next N layers' memory-mapped weights while the current layer computes; the layer end is seen through
the scheduler's eval callback (`l_out-<il>`), chained to any callback the caller set. Off by default, off with paged experts.

**Verified:** builds with the HIP toolchain; `--layer-readahead` shows in the server's help; with it on, a 9B dense model
(`Qwen3.5-9B-Q4_K_M`, `-ngl 0`) logs "layer read-ahead: 4 layers ahead, 4085.2 MiB of weights in 32 layers in host memory" and
generates the same text as without it. ModelMistress: 582 library tests pass, clippy clean.

**Measured:** (a) model fully cached: 6.45 tokens/s without, 6.27 with (about 3% cost). (b) cold-file read rates, 3 GB regions of a 22 GB GGUF,
12 threads: faults 1.70 and 1.81 GB/s, whole-range prefetch 1.85 GB/s, 4-chunk pipeline 2.00 GB/s.

**What that says about the hybrid.** The gate in section 3.3 asked for sequential reads at least 3 times faster than page faults. They are
1.0 to 1.2 times as fast here, so **the prefetch ring is not worth more than this small option**: the operating system already reads
a multi-threaded matrix multiply's faults nearly at disk speed. Decode of a dense model bigger than memory stays disk-bound
(model size divided by about 1.8 GB/s per token), as the AirLLM spike also showed. What helps, and is **built** (see "Pinned resident set" below):
1. *A resident set that is not evicted.* A walk over all layers for every token, with a cache smaller than the model, makes least-recently-used
   eviction read the *whole* model every token (every page is evicted just before it is needed again). Pinning as many layers as fit
   (`VirtualLock`/`mlock` on the first K layers) and streaming only the rest cuts the bytes read per token from S to S minus the pinned part:
   a model 1.2 times the free memory would read about a sixth as much. This is the one place the fusion can beat both llama.cpp's paging and
   AirLLM (which re-reads everything).
2. *Reading fewer bytes*: a smaller quantisation, or paged experts (Rigel's MoE paging) for a MoE model.

**Memory-pressure test, 2026-11-24 (owner stopped their hubs and said to run it): inconclusive.** A script
(a short Python program that allocates and touches memory until a set amount stays free; it frees it when killed or after 25 minutes) held
15.2 GB, then 19.8 GB, of the 63.9 GB of RAM, leaving 3.3 GB and then 1.6 GB available. `Qwen3.5-9B-Q4_K_M` (5.5 GB), `-ngl 0`, 16 tokens
per run, readahead 0 / 4 / 8:

| available | readahead | generate tokens/s |
|---|---|---|
| 3.3 GB | 0 (first run, cold file) | 2.09 |
| 3.3 GB | 4 / 8 / 0 | 6.26 / 6.24 / 6.23 |
| 1.6 GB | 0 / 4 / 8 / 0 / 4 | 6.28 / 1.06 / 6.02 / 6.30 / 5.84 |

The model stayed in memory in every run except two cold ones (the first read of the file, and one read-ahead 4 run, 1.06 tokens/s,
that did not repeat), because Windows took the room from other cached pages rather than from the model's mapped pages. So **the machine did
not put the model under real pressure, and read-ahead shows no effect either way**: 6.0 to 6.3 tokens/s with it on or off. The only
slow runs are the cold ones. To test the idea properly one needs a dense GGUF clearly bigger than free RAM (here about 40 GB or more), or a
memory cap on the server process (a job object with a hard limit, or a VM with 4 GB), so that its pages really are evicted. The hog
and its logs are in the session scratchpad only; nothing was left running and the RAM was returned (24.6 GB free afterwards).
Also found: this Rigel build cannot load
`qwen3.5-9b.gguf` from the owner's models folder (`qwen35.rope.dimension_sections has wrong array length; expected 4, got 3`); the unsloth
`Qwen3.5-9B-Q4_K_M.gguf` loads.

## Pinned resident set built and measured under a hard memory cap, 2026-11-24
**Built (Rigel `--layer-resident-mib N`, ModelMistress `layer_resident_mib`):** the first layers whose weights sit in host memory are locked
(`VirtualLock` on Windows, with the working set grown to fit; `mlock` elsewhere) up to N MiB (-1: what is free, less 2 GB or a tenth of the RAM,
whichever is more; never more than 85% of the RAM; 0 off). The rest streams as before, so the bytes read per token fall from S to S minus the
locked part. Dense models only; off with paged experts. Combines with `--layer-readahead` (which then skips the locked layers). Smoke test, `Qwen3.5-9B-Q4_K_M`
on the CPU, 2042.6 MiB locked (16 of 32 layers): identical text, 6.41 tokens/s uncapped.

**Test:** a hard working-set cap on the server process (`SetProcessWorkingSetSizeEx`, 4096 MiB maximum; the model is 5.5 GB) while a holder kept
about 1 GB of the machine's RAM available, 12 tokens a run, **single runs, noisy**. Generate tokens/s:

| resident set | runs |
|---|---|
| none | 0.238, 0.373 (strong pressure), 0.535 (weaker) |
| 2048 MiB | 0.495, 0.519 |
| 3000 MiB | 0.533 |
| 3500 MiB | 1.088, 0.879 |
| read-ahead 4 | 0.350 (none), 0.444 (with 2048), 0.623 (with 3500) |

So a 3500 MiB resident set read about 2 to 3 times faster than none (the formula S / (S - R) with S about 5.5 GB gives 2.75), more resident
helped more, and read-ahead added nothing on top. **Not statistically solid:** the baseline moved by a factor of two between batches with how much the
system cache could take, and the interleaved three-round batch was stopped after one round, because the holder had driven the machine's free commit
down to 0.1 GB (one VirtualLock failed on 126 MiB with "paging file too small", and the first capped run timed out at 500 s). Everything was cleaned
up. To repeat it: cap the server, keep the holder from taking more than the commit headroom allows (it must stop at 8 GB or more free commit).

## GPU and CPU together, 2026-11-24
`--fit` works in Rigel (HIP): `Qwen3.5-9B-Q4_K_M`, with `--fit on --fit-target 20500` (3.9 GB of the 24 GB card usable) it ran at 19.3 tokens/s, with
`--fit-target 23500` (almost nothing on the card) 6.3 tokens/s. ModelMistress used `-ngl 999` for Rigel; it now lets Rigel `--fit` unless paged
experts are on. New setting `vram_extends_ram` (default on): a load that was to run on the CPU and does not fit the free RAM puts the fewest layers
that make it fit on the card, within the free VRAM (`serving::spill_layers`); a model that fits in RAM stays on the CPU.

**VRAM only, no RAM, for a CPU-class (Bonsai ternary) model, measured 2026-11-24:** `Ternary-Bonsai-2-9B` on the Prism Vulkan build with every layer on the card:
default memory map: 2.2 GB working set; `-lm none` (the hub now adds it for a Prism load wholly on the card): 0.45 GB, 131 tokens/s; the CPU build: 2.3 GB, 12.6 tokens/s.
Through a scratch hub: 612 MB of RAM, 2.6 GB of VRAM, about 120 tokens/s. A CPU profile load (`gpu_layers = 0`) of the 9B on Rigel with about 1 to 4 GB of RAM available
became `-ngl 14` with the placement note "the weights do not fit the free RAM" (3.9 GB of VRAM, about 10 tokens/s, the first answer in 34 s).
