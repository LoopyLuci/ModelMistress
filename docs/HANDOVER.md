# Handover: hardware-aware model loading (work in progress)

For the next agent. Written while the work was under way, in case the session ended before it was done. Read this, then
`docs/serving.md` ("Where a model runs") and `docs/STATUS.md`. Nothing here is committed: the working tree holds it.
Machine-specific facts (paths, process ids, the owner's drives) are kept out of this public file; they are in the
agent memory of the project (`project_hardware_loading.md`).

## What the owner asked for (2026-11-23, in their words, shortened)

1. "Model loading feels broken": fix it.
2. Proper CPU and GPU labelling on models.
3. Proper, optimised CPU/GPU model loading; optimal use of the hardware and software.
4. Misty routes and loads models properly and optimally too.
5. Use a CPU model and a GPU model at the same time, together, for speed, while keeping accuracy and adding robustness.
6. **A hard rule (mid-session): the GPU keeps 2 GB unused for the system; the most models may use of the 24 GB card is
   22 GB.** Implemented as `vram_reserve_mib` (default 2048).
7. **"Use the new AirLLM integration to reduce the model sizes loaded, for any and all models in ModelMistress and
   Misty."** There is no AirLLM integration: only the plan, `docs/airllm-integration-plan.md`, which recommends against
   an AirLLM engine (it reads the whole model from disk for every token). Nothing was installed or built. See item 7 of
   the plan below for what can be done, and the question the owner has to answer.
8. Keep this handover, a TODO and a plan up to date for the next agent (this file).

Standing rules from the owner: do not commit, push, build the installer or release unless asked; temp files only in the
session scratchpad or the `<drive>:\ClaudeCodeTemp\<project>` folder; no new folders elsewhere without asking; clean up
what you start (processes, folders); never change global git config (use `git -c safe.directory=...` per command); do
not touch the owner's running hub (use a scratch hub with its own `--home` and port); no downloads or installs without
the owner saying yes to the exact file.

## What was found (measured, 2026-11-23, Windows 10, RX 7900 XTX 24 GB, Vulkan, 64 GB RAM)

- **A GPU load is charged against the Windows commit limit.** Loading an 11.8 GB model on the GPU took 13.9 GB off the
  commit headroom (1.2 times its size). With 81 of 106 GB committed by the owner's programs, a 20 GB model needs more
  headroom than is left at some moments, and the driver then says `ErrorOutOfDeviceMemory` with 23 GB of VRAM free.
  That is why the same 20 GB load failed at 09:50 and 10:01 and worked at 10:24. The old retry ("keep 2 GB more
  free") changed nothing and the CPU fallback was skipped because only 12 GB of RAM was free.
- `llama-server --list-devices` free memory is optimistic; `--fit-target N` really is the lever: 6000 held a 27B load to
  17.4 GB, 3000 to 19.0 GB (free minus target, to within 20 MiB).
- The models live on a 7200 rpm hard drive: the 20 GB file takes 1 to 2.5 minutes to load, cold. That is most of the
  "loading is slow" feeling. Nothing in the hub tells the user it is reading a disk.
- Misty's brains are ternary Bonsai files served by the **CPU-only** PrismML build (`win-cpu-x64.zip`), so the GPU is
  idle during Misty chats. PrismML also publishes `llama-prism-b12128-d7b2896-bin-win-vulkan-x64.zip` (27 MB) and a
  HIP build (344 MB) in the same release. Not downloaded: needs the owner's yes. Whether the ternary kernels exist on
  Vulkan is unknown.
- `model.list` has no hardware information at all; four different Bonsai quantisations share one display name.
- Misty's roster (`src/misty/roster.rs`) decides by free RAM only; it knows nothing about the GPU or the commit.
- Two hubs run on the owner's machine (the desktop app's own and the Scheduled Task's), the first an older build.

## What is done (uncommitted: modified files and `src/machine.rs`, `docs/HANDOVER.md` are new)

- `src/serving.rs`: `GpuLimit` (largest load that worked, smallest that failed, failure believed for 24 h),
  `fit_target_for`, `asked_of_card`, `plannable_free_mib` (free memory held to total minus the reserve minus what the
  other models hold; the margin is added back, it is estimate slack). Tests in the same file.
- `src/machine.rs` (new): `commit_headroom_mib()` (Windows `GlobalMemoryStatusEx`), `physical_cores()`,
  `commit_for_gpu_load_mib` (x1.2), `gpu_budget_from_commit_mib`, `COMMIT_MARGIN_MIB` (3072). Needs the windows-sys
  feature `Win32_System_SystemInformation`, added to `src/Cargo.toml`.
- `src/engine.rs`:
  - setting `vram_reserve_mib` (default 2048); `limits` (persisted in `<home>/gpu_limits.json`, keyed by device and
    server program); `Try::Held {target, budget}`; `Ground`.
  - `held_first_try`: the first start is already held to the smallest of the reserve cap, the commit budget and the
    learned limit, with the rest of the model in RAM (`retry_gpu` ignores the learned one).
  - `split_after_oom`: after an out-of-memory failure, up to 3 splits, each held an eighth lower, before the CPU; the
    split must fit the free RAM. Without estimate or device list, the old fixed "+2 GiB" step is kept.
  - `plannable_free` is used by `plan_for`, `placement_for` and the refusal messages; `model.plan` therefore honours the
    reserve and the commit.
  - the final out-of-memory error names the commit when it explains it (`commit_hint`).
  - test hooks: `fake_devices`, `fake_commit` (cfg(test)).
- Tests added: `serving::tests::*` (limit, fit target, reserve), `machine::tests::*`, `engine::tests::` the reserve,
  the split ladder and the kept limit, `a_load_that_worked_is_remembered...`, the commit test. The old ladder tests were
  updated for the new `after_failure` signature and one device's `total_mib` (48,000) in
  `a_load_that_does_not_fit_is_planned_not_spilled`.
- **Labels (plan step 2, done in code):** `ModelEntry::quant()`/`label()` (`src/catalog.rs`), `Engine::hardware_json`
  (`src/engine.rs`), a 20 s device cache (`gpu_devices_recent`), the `hardware` block in `model.list` and `/v1/models`,
  chips in `frontend/src/views/ModelsView.vue` (type in `frontend/src/chat/models.ts`) and in `model-mistress models`.
  The frontend was edited but `npm run check` has not been run.
- **`model.plan` (step 1, done in code):** a `memory` block (`Engine::memory_json`).
- **Misty (step 5, first half):** `roster::Engines.gpu_room_bytes` (from `Engine::gpu_room_mib`, which does not shrink
  when a model is loaded), `roster::device_for`, `Resolution.device`, `fits` counts the card, `misty.status` roster
  entries carry `hardware`, and the embedder and reranker are kept on the CPU through `Engine::set_cpu_hints` (applied in
  `resolve_serving` only when neither the request nor the model's profile sets `gpu_layers`). Not done: the router
  preferring loaded models, device-aware fallbacks, and the fast role on the CPU while a big model holds the card.
- Docs: `docs/serving.md` (reserve, commit, split ladder, `gpu_limits.json`, hardware labels) and `CHANGELOG.md`
  (Unreleased). Not done: `docs/STATUS.md`, `docs/misty/` pages, the dashboard has no field for `vram_reserve_mib`
  (neither has it one for `vram_margin_mib`: both are `config.toml` only).
- Real hardware: the new build loaded the 27B Q5_K_XL (20.2 GB) twice on the GPU (8k context 2 slots: 19.1 GB held;
  32k context, 1 slot: 20.5 GB held) and `--fit-target` was checked as a lever. The retry ladder itself has **not**
  been seen to run on the real card, because the failures did not recur (the commit headroom was 25 GB at the time).

## State of the checks (update this when you run them)

- Full library suite (`cargo test -p model-mistress --lib -- --test-threads=3`) on the finished tree: **574 passed,
  0 failed, 6 ignored** (about 3 minutes). Send its output to a file: through a `grep` pipe it shows nothing until the end.
- `cargo clippy -p model-mistress --all-targets -- -D warnings` and `cargo fmt -p model-mistress`: clean.
- Frontend: `npm run typecheck` and `npm run test` pass (127 tests). `npm run check` also runs `vite build`, which was
  not run.
- A build can fail with `rustc-LLVM ERROR: out of memory` when the machine's Windows commit headroom is low (the owner's
  27B on the GPU holds about 23 GB of it). Wait for the headroom, or use `CARGO_BUILD_JOBS=2`.
- Build and test recipe: `CARGO_TARGET_DIR=<a folder the owner allowed> CARGO_NET_OFFLINE=true cargo test -p
  model-mistress --lib -- --test-threads=3` (the default thread count makes the timing-sensitive factory tests fail on
  this machine).

## The plan (in order; each step ends with tests, a real-hardware check where it says, and docs)

1. **Finish the memory fix (mostly done).** Left: show the commit in `/v1/health` or `service.status`; `docs/STATUS.md`;
   a real run of the retry ladder on the card (it has not failed since the fix, so force it with a very large
   `ctx_size` or a second big model loaded).
2. **Labels (done in code, see above; left: check the Models page in a browser against a scratch hub, and `npm run check`).** The original spec: add a `hardware` object to every entry of `model.list` and `/v1/models`: `engine` (llama.cpp, prism,
   rigel, cactus), `can_run_on` (`["gpu","cpu"]`, or `["cpu"]` for ternary files on the CPU-only Prism build), `runs_on`
   now (`gpu`, `split` with `gpu_share`, `cpu`, from `placement_for` with the devices listed once per call), the
   estimated VRAM and RAM, a `quant` read from the file name when `general.file_type` is missing, and a unique display
   `label` (name, quant, device). Show it as chips on the Models page (`frontend/src/views/ModelsView.vue`), in the
   Misty roster and in `model-mistress models`. Loaded models already carry `serving.placement`.
3. **Load feedback.** A load from a slow disk shows "reading from disk" with the file size and the observed rate
   instead of silence; `model.load` returns the stages (waited, read, ready). Consider a longer idle time for models
   that take over a minute to load.
4. **Threads for CPU and GPU at once.** Each llama-server defaults to all physical cores, so a CPU model and a GPU model
   (whose host thread spins) fight for them. Give a fully offloaded GPU server few threads (about 4), and split the
   rest between CPU servers (`-t`, optionally `--cpu-range`), computed from `machine::physical_cores()` and what is
   loaded. Measure a CPU model's tokens per second while a GPU model generates, before and after; keep it only if it
   wins.
5. **Misty on both processors (first half done).** The original spec: `roster::resolve` gets the machine (free VRAM, commit, RAM, GPU present) and each
   `Resolution` a `device`. Fast and the embedder on the CPU, main and deep on the GPU when they fit and the GPU is
   free; the router (`misty/auto.rs`) prefers a model that is already loaded; when the GPU load fails the same role
   falls back to its CPU model and says so. Keep the accuracy rule: a fallback never goes to a smaller class silently.
6. **A CPU draft for a GPU target (speculative decoding), only if it measures faster.** The draft must share the
   vocabulary; `Qwen3.5-0.8B` is the likely draft for the Qwen3.5-family 27B. Earlier tests (`docs/STATUS.md`) made a
   mixture-of-experts target slower; a dense target was never tried. The draft runs on the CPU with `-ngld 0`/`-devd
   none` and `-td`. Do not ship it on by default unless it wins by a clear margin on this machine.
7. **AirLLM.** Needs the owner's answer first (see below). What can reduce the size of what is loaded without AirLLM:
   keep CPU-side layers memory-mapped (already so), the split loads above, KV cache at `q8_0` when a load does not fit
   (near lossless; `q4_0` only on request), a context that matches the use, and a smaller quantisation on offer. The
   AirLLM spike of the plan (Phase 0) means installing AirLLM and PyTorch into the Forge environment: a large download
   that needs the owner's approval.
8. **PrismML GPU build for Misty (done 2026-11-23; left: Linux, a 9B-on-both measurement, the owner's answer below).**
   New setup component `prism-gpu` (`src/misty/setup.rs`, `model-mistress misty setup --install prism-gpu`, not in the
   starter set): `llama-prism-b12128-d7b2896-bin-win-vulkan-x64.zip` from
   `https://github.com/PrismML-Eng/llama.cpp/releases/download/prism-b12128-d7b2896/`, 28,692,091 bytes, SHA-256
   `ec1479514a34272c815b3d194046d27003867aa9c95069d9cd3031b5c31e01ff`, installed into `<home>/engines/prism-gpu/`
   (`MM_PRISM_GPU_SERVER` overrides). `Engine::server_for` runs it for a ternary load that puts layers on the card and the
   CPU build for `gpu_layers = 0` and for the retry on the CPU; `can_use_gpu` and `hardware.can_run_on` say `["gpu","cpu"]`
   for a ternary file only when it is installed. `roster::device_for` sends a ternary file to the card only when the GPU
   build is there and the file fits what is left of the card (that server has no `--fit`, so no RAM split); `fast`, the
   embedder and the reranker stay on the CPU, so a CPU and a GPU model work at once. **Checked on the real card with the
   real installer (scratch hub on port 7890, its own home, the zip reused from the owner-approved download):** the install
   passed its size and hash checks; `models` labelled the 2B `CPU` and the 4B and 9B `GPU`; the 9B loaded with `-ngl 999`
   from `engines/prism-gpu` (2,346 MiB of VRAM) while the 2B ran from `engines/prism` with `-ngl 0`, both at once; both
   answered 17 times 23 correctly (391); the 9B generated 136 tokens/s on the card and the 2B 48 tokens/s on the CPU;
   `misty ask --role main` ran on the GPU build and `--role fast` on the CPU build. Earlier `llama-bench` on the 2B:
   prompt 3,205 against 211 t/s (15 times), generation 100.7 against 46.8 t/s (2.2 times), the same answer. The scratch hub
   and its home were removed. The verified build was also copied into the owner's hub home (`home/engines/prism-gpu/`,
   ignored by git); the owner's running hub is an older binary and does not use it until it restarts on this build. Left:
   Linux (`ubuntu-vulkan-x64.tar.gz`, SHA-256 `a73a76f2...cca4`, 33,757,914 bytes, no asset coded and untested), and a
   9B measurement against the CPU build with nothing else on the card.
9. **Finish.** Whole library suite, clippy, fmt, the frontend (`npm run check` in `frontend/`), a real run against a
   scratch hub, docs, CHANGELOG. Do not commit unless the owner asks.

## Open questions for the owner

- May Misty install the PrismML Vulkan build by default (item 8: wired and checked on the card; today it is opt-in with
  `--install prism-gpu` and the starter set is unchanged)?
- AirLLM: the plan says no engine. Does the owner still want it after reading the plan's numbers, and may AirLLM and
  PyTorch be installed into the Forge environment for the Phase 0 benchmark? (item 7)
- The commit headroom is the limit on this machine: should the page file be larger (a Windows setting, the owner's
  call), and should the models move off the hard drive?
- Two hubs run at once (the desktop app's own and the Scheduled Task's). Which should stay?

## Gotchas

- Docs under `docs/misty/` and `CHANGELOG.md`, `docs/STATUS.md` are CRLF; `src/*.rs` edited here are LF in the working
  copy. Write script files with the file tool: a shell heredoc with quotes fails on this machine.
- The built-in llama.cpp is pinned at b11272; `engines/llama.cpp` is a submodule that `build.rs` patches in place (the
  clone has a local `submodule.engines/llama.cpp.ignore = dirty`).
- Do not print the owner's token from `control.json`.
