# Serving profiles and the VRAM planner

How ModelMistress starts a model's `llama-server`, how a client reads what it got, and how the hub decides whether a
load fits the GPU. Code: `src/serving.rs` (options, estimate, planner) and `src/engine.rs` (loading).

## Profiles

Every option is optional. An option nobody sets adds no flag, so llama-server's own default applies and a config
written before profiles existed starts models with exactly the arguments it always did (`-c`, `-ngl`, `-np`).

The strongest layer that sets an option wins:

1. the load request (`model.load` arguments; autoload uses none),
2. `[profiles."<model id>"]` in `<home>/config.toml` (the id is what `model.list` shows; case does not matter),
3. `[serving]` in `config.toml` (every model),
4. the top-level `ctx_size`, `gpu_layers`, `parallel` settings.

```toml
[serving]
flash_attn = "on"

[profiles."unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF:Qwen3-Coder-30B-A3B-Instruct-UD-Q4_K_XL"]
parallel     = 4
ctx_size     = 65536        # tokens in all, shared by the 4 slots
kv_unified   = true         # one pool: a slot may use what the others leave free
cache_type_k = "q8_0"
cache_type_v = "q8_0"
cache_prompt = true
```

| Option | llama-server flag | Meaning |
|---|---|---|
| `ctx_size` | `-c` | Context tokens **in all**. llama-server divides them between the slots unless `kv_unified` is on. 0 = the model's own (can be 256k and will not fit). |
| `ctx_per_slot` | `-c` = this x `parallel` | Context per slot. Wins over `ctx_size` in the same layer. |
| `parallel` | `-np` | Slots (requests decoded at once). |
| `gpu_layers` | `-ngl`, or `--fit` | Layers on the GPU. 999 (default) = as many as fit, chosen by llama-server; 0 = CPU only. See "Where a model runs". |
| `cache_type_k`, `cache_type_v` | `-ctk`, `-ctv` | KV-cache type: `f32 f16 bf16 q8_0 q4_0 q4_1 iq4_nl q5_0 q5_1`. A quantised V cache needs flash attention. |
| `flash_attn` | `-fa` | `on`, `off` or `auto`. |
| `cache_prompt` | `--cache-prompt` / `--no-cache-prompt` | Keep a slot's prompt between requests, so the next turn of an agent only processes what is new. llama-server's default is on. |
| `cache_reuse` | `--cache-reuse N` | Smallest chunk (tokens) reused by shifting the KV when the prompt differs in the middle. |
| `cache_ram_mib` | `--cache-ram` | RAM for prompt caches kept outside the slots (0 = off, -1 = no limit; llama-server's default is 8192). |
| `kv_unified` | `--kv-unified` / `--no-kv-unified` | One KV buffer for all slots. |
| `batch_size`, `ubatch_size` | `-b`, `-ub` | Logical and physical batch (`ubatch_size` <= `batch_size`). |
| `jinja` | `--jinja` / `--no-jinja` | Jinja chat templates, which tool calls need. The pinned llama.cpp has it on by default. |
| `chat_template_file` | `--chat-template-file` | A template file instead of the one in the GGUF. |

Values are checked before a process starts (a bad KV type, `ubatch_size` > `batch_size`, a quantised V cache with
flash attention off, a misspelt key in a profile, a missing template file). `config.toml` with such a mistake is
refused when the hub reads it, `config.set` refuses it with 400, and so does `model.load`. The same options are plain
fields of `model.load` and `model.plan`: `{"model": "...", "cache_type_k": "q8_0", "ctx_per_slot": 32768}`.

`extra_args` (for every model, or per load) still comes last and can override anything.

## What a client can read

`GET /v1/models`: every entry has `serving`. For a loaded model it is what the server reported (`n_ctx`,
`n_ctx_per_slot`, `slots`, `kv_unified`, `cache_type_k/v`, `flash_attn`, `cache_prompt`, `vram_mib`,
`estimated_vram_mib`, `source: "server"`); for one that is not loaded it is what a load would use
(`source: "profile"`). **`n_ctx_per_slot` is the window one request can fill**: `n_ctx / slots`, or all of `n_ctx` with a
unified KV. `model.loaded` carries the same block as `serving` on each loaded model.

`vram_mib` is the dedicated GPU memory of the model's server process (Windows performance counter
`GPU Process Memory`, read with `typeperf`), else the sum of the device buffers in its log (a llama.cpp run with
`-lv 4`), else null.

## The VRAM planner

Before a load starts a server, the hub estimates its VRAM from the GGUF header (layers, KV heads, head size,
file size) and the profile:

    weights (file size; minus the expert tensors with --cpu-moe / --n-cpu-moe; x GPU layers / layers)
  + KV cache  (for each attention layer: KV heads x (key size x K type + value size x V type) x cells)
  + recurrent state (hybrid models such as Qwen3.5: per slot)
  + compute buffers (about 300 MiB per 512 of `ubatch_size`) + 200 MiB per context

and compares it, plus `vram_margin_mib` (default 768), with the GPU's free memory. Free memory is the card's total
(from `llama-server --list-devices`) minus the dedicated memory in use on the adapter (Windows counter `GPU Adapter
Memory`): llama-server's own "free" figure does not see what other processes hold (measured: it reported 18.7 GB free
with 5.8 GB taken by other programs, and 5 GB fewer after a 18.5 GB model was loaded). Where the counter is not
available the planner uses llama-server's figure.

The cells of a layer are the context tokens, except in a **sliding-window** layer (most of a Gemma's): it keeps the
window and a batch, rounded up to 256, for each slot (`-c 4096` on gemma-4-31B: 4,096 cells in its 10 full layers,
1,536 in its 50 window layers; with `--kv-unified` one window serves all slots, with `--swa-full` the window layers
keep the whole context). The heads, head sizes and which layers have a window are read per layer from the header
(`head_count_kv` as a list, `key_length_swa`, `sliding_window_pattern`, `shared_kv_layers`). For gemma 2, gemma 3,
gemma 3n, cohere2, gpt-oss, olmo2, afmoe and plamo3 the pattern llama.cpp builds in is used when the file has none;
for any other architecture a file without a pattern counts as full attention, which can overstate but not understate.
The reader keeps lists of at most 1,024 numbers or flags; `/v1/models` shows them in `metadata`.

**What models may use.** The card keeps `vram_reserve_mib` (default 2048) free for the system: models may use at most
the card's total minus that (22 GB of a 24 GB card), and what other loaded models already hold counts against it. The
planner, the first start of a load and the refusal messages all use this figure, not the card's raw free memory.

**Threads for a CPU model and a GPU model together.** llama-server starts one thread per physical core. A model wholly on
the card needs only a few threads to feed it, and a CPU model next to it fights those threads for the cores. With
`thread_budget` on (the default) the hub gives a server wholly on the card a third of the physical cores (2 to 4), and a
CPU-only server the cores the GPU servers leave, shared with the CPU servers already running (`-t`, see
`serving::thread_budget`). A split load, a CPU model alone, a machine with fewer than 4 cores, and any `-t` or `--threads`
in a profile, a load or `extra_args` are left as they are. A server that is already running keeps its count when another
loads. `thread_budget = false` turns it off.

**Load progress.** A load from a hard drive takes minutes. `service.status` lists `loading`: for each server that has
started and is not ready, its `stage` (`starting`, `reading from disk`, `preparing`), `percent`, `read_mib` of `size_mib`,
`rate_mib_s` and `eta_s`, taken from how much of the file the server holds in memory.

**Windows commit.** On Windows the device memory of a GPU load is also charged against the system's *commit* (RAM plus
page files, less what open programs have committed), about 1.2 times its size. When the commit has less headroom than
that, the driver says `ErrorOutOfDeviceMemory` with gigabytes of VRAM still free. Measured on an RX 7900 XTX: 25 GB of
headroom fell to 11 GB when an 11.8 GB model loaded on the GPU. The hub reads the headroom before each load and holds
the load to what it allows ((headroom - 3 GiB) / 1.2); the part that does not fit stays in RAM. When a load still runs
out of memory the error names the commit and what to do (close programs, a larger page file).

- It fits: load.
- It does not, but would after unloading idle models: they are unloaded, least recently used first (a model with a
  request running is never chosen), and the load waits for their memory to come back.
- It does not fit at all: `503 insufficient_vram`, with the estimate by part, the free memory, and what to change
  (lower `ctx_size` or `parallel`, a smaller KV type). Nothing is started, so nothing spills into shared memory.

`vram_guard = false` in `config.toml`, or `skip_vram_check: true` in one load, turns it off. With no estimate (the GGUF
lacks the layer count) or no device listing (a CPU build), the guard does nothing.

`model.plan` (same arguments as `model.load`) answers without loading: the arguments the server would get, the
estimate, the devices and their free memory, and the verdict `fits`, `fits_after_unloading` (with `unload`),
`does_not_fit` or `unknown`. `memory` adds what holds the load: `reserve_mib`, `usable_mib` (what models may use now),
`commit_headroom_mib` and `commit_budget_mib` (Windows), and `learned_budget_mib` with the card's `gpu_limit`.

The estimate is checked against measurements in [STATUS.md](STATUS.md).

## Where a model runs: the GPU, both, or the CPU

`gpu_layers` says how much of a model goes on the GPU. Three cases:

| `gpu_layers` | What the hub does |
|---|---|
| 999 (the default) or a negative number | **Automatic.** The hub and llama-server place the model: every layer on the GPU if they fit, else as many as fit with the rest running from RAM, else all on the CPU. |
| 1 to 998 | That many layers (`-ngl N`). It must fit the GPU's free memory (planner above), or the load is refused with `503 insufficient_vram`. |
| 0 | CPU only: `-ngl 0 -dev none` (the GPU backend is left out, so a driver that crashes the server is not touched). |

**Automatic placement.** llama.cpp's own `--fit` measures the free memory of each device when the server starts, which is
more exact than the estimate from the GGUF header, and it keeps experts of mixture-of-experts models in RAM when that
saves the GPU. The hub passes `--fit on --fit-target <margin>` (the margin is `vram_margin_mib`, at least 512 MiB)
instead of `-ngl` when the stock llama-server has the flag (`--help` lists `--fit`; the answer is remembered). A PrismML
or Rigel server has no `--fit`, so it still gets `-ngl`. Before the server starts the hub uses its header estimate only
for two things: unloading idle models to make room (a model that holds no VRAM is never unloaded for VRAM), and refusing
a load that cannot run at all, `503 insufficient_memory`: the part that does not fit the GPU would have to fit in free
RAM, and it does not. Where `model.plan` can estimate, it adds `placement`: `{"where": "gpu"}`,
`{"where": "gpu_after_unloading", "unload": [...]}`, `{"where": "split", "gpu_share": 0.57}`, `{"where": "cpu"}` or
`{"where": "none", "need_ram_mib": ..., "free_ram_mib": ...}` or, only when paging is allowed (below), `{"where": "paged",
"paged_mib": ..., "need_ram_mib": ...}`.

**Loading from disk (`paged`, off by default).** A model bigger than the free RAM plus the VRAM models may use is refused
(`insufficient_memory`). To try it anyway, turn on the setting `allow_paged_load` **and** set `paged = true` in the model's
profile (or pass `paged` to `model.load`), then load it with `model.load`. The weights that do not fit are read from disk
by the operating system as the model runs (llama.cpp memory-maps the file): expect seconds per token, a slow start (the
load deadline is at least 30 minutes) and a machine that may stall while it pages. It still needs the KV cache, the state
and the server's buffers to fit in RAM, else it is refused. A request never loads such a model: it gets 409
`needs_explicit_load`, so peers, Misty and `auto` cannot start one (the roster also keeps it `unfit`). The Models page shows
"from disk (slow)". Only for the stock llama.cpp with `--fit` and automatic GPU layers; other engines ignore `paged`. Not
yet measured on a real over-memory model: see the HANDOVER.

**What was measured.** Each loaded model reports `serving.placement`: `gpu` (the weights are on the GPU), `partial`
(some are), `cpu`, or `unknown` (no GPU counters on this machine, or not measured yet). It comes from the dedicated
GPU memory of the model's server process compared with the weights and KV cache of every layer (compute buffers are on
the GPU either way, so they are left out). Where the counters work, a server
that holds none is `cpu`. When the server's log says how many layers it put on the GPU (it does at verbosity 4, which
the hub asks for: [the server log](#the-server-log-and-model-telemetry)), that count decides instead of the memory: all
layers and the output layer on the card is `gpu`, some is `partial`, none is `cpu`, and a load that moves experts to
the CPU (`--cpu-moe`, `--n-cpu-moe`, `-ot`, or a fit that did) is `partial`. `serving.layers` carries the counts.
`serving.placement_note` says why a model is not where it was asked to be (below). The Models page shows both.

**When the GPU fails a load.** With `gpu_fallback = true` (the default), a load that fails because of the GPU is tried
again instead of being reported at once:

1. *Out of memory* (the log says an allocation on a device failed): the load is held lower, to an eighth less than
   what failed, with the rest of the model in RAM (llama-server's `--fit-target` is set so that it holds). Up to three
   such splits, as long as the part in RAM fits the free RAM. The card's answer is kept (below).
2. *Out of memory again, or a crash* (the server died the way a driver crash kills a process: exit code `0xC0000409` or
   another `0xC...` code on Windows, a signal elsewhere): the model is started on the CPU, if free RAM holds the file and
   a GiB more. If it does not, the GPU's own error is the answer.

Even the first start is held: to the smallest of what the reserve allows, what the system's commit allows, and what
this card has lately shown it can hold. The load is then a split from the start instead of a failure and a retry.

**What the card has shown.** The largest load that worked and the smallest that ran out of memory are kept per card and
server program in `<home>/gpu_limits.json`. A failure is believed for 24 hours (what else holds the memory changes), a
success for good. `model.load` with `"retry_gpu": true` ignores a remembered failure.

A failure that is not the GPU's (an unknown architecture, a file that cannot be read, a load that times out) is never
retried. When every start fails, the error is the last one, with what was tried before it: `(before this: ...)`.

A **crash** is remembered. When the CPU start works after one, `<home>/placements.json` records the model and the
llama-server program (path, size, date) it crashed with; the next load of that model starts on the CPU at once and says
so in `placement_note`. A different or updated llama-server gets a new try on the GPU. `model.load` with
`"retry_gpu": true` tries the GPU again (and forgets the record when it works); the load form of the Models page has a
box for it. An out-of-memory failure is not remembered: it depends on what else is using the GPU at the time.

`gpu_fallback = false` turns all of this off: one start, one error.

## Hardware labels

`model.list` and `/v1/models` carry a `hardware` block on every model, and `quant` and `label` beside it:

| Field | Meaning |
|---|---|
| `quant` | The quantisation as the file names it (`Q5_K_XL`, `PQ2_0`), else the GGUF header's. Files of one name differ in it. |
| `label` | A name that tells models apart: the name in the file and the quantisation. |
| `hardware.engine` | `llama.cpp`, `prism` or `rigel`. |
| `hardware.can_run_on` | `["gpu","cpu"]`, or `["cpu"]` when no card is seen or the engine's build is CPU-only (the PrismML build `misty setup` installs). |
| `hardware.runs_on`, `now` | `gpu`, `split`, `cpu`, `none` (does not fit) or `unknown`. Measured when loaded (`now` true), else where a load would go. |
| `hardware.gpu_share`, `vram_mib`, `ram_mib` | The share on the card and the memory it takes there and in RAM: measured or estimated. |
| `hardware.chip`, `label` | The words the Models page and `model-mistress models` show: `GPU`, `CPU`, `GPU 80% + CPU`, `does not fit`. |

The cards are probed at most every 20 seconds for listings, so a listing does not start a process each time.
Misty's status (`misty.status`) gives each roster entry the same block, and a `device`: where Misty means the model to
run. The embedder and the reranker are kept on the CPU (a model with GPU layers set in its profile is not moved), so the
card is left to the language models; a language model goes to the card when it fits there, and the roster counts the
card's room when it asks whether a file fits. The `fast` role stays on the CPU too, so that a CPU model and a GPU model
work at once.

**The Prism GPU build.** Ternary Bonsai files are served by the PrismML build, which `misty setup --install prism` puts in
`<home>/engines/prism/` and which runs on the CPU. `misty setup --install prism-gpu` adds the build with the Vulkan
backend in `<home>/engines/prism-gpu/` (`MM_PRISM_GPU_SERVER` overrides). A load that puts layers on the card runs the
GPU build; one with `gpu_layers = 0`, and the retry on the CPU after the card ran out of memory, runs the CPU build. That
server has no `--fit`, so a ternary file is not split with RAM: it fits the card (counting `vram_reserve_mib`) or runs on
the CPU. `hardware.can_run_on` says `["gpu","cpu"]` for a ternary file only when the GPU build is installed.

## The server log and model telemetry

Each model's llama-server writes `<home>/logs/<model id>.log`. llama.cpp (b12726) prints how it placed a model only at
verbosity 4, so the hub starts a stock llama-server with `-lv 4` when `--help` lists `--log-verbosity`, unless the
model's `extra_args` (profile or load) set a verbosity of their own. PrismML and Rigel servers are started as before.
The cost is about 19 KB of log per load and about 2.8 KB per request.

**What the log holds.**

- The top, written by the hub before the server starts: `# mm <version> <time>: <model> <how>`, `# mm server: <program>`,
  `# mm args: ...` and one `# mm env: NAME=value` per variable it set (the Vulkan cooperative-matrix switches of a retry
  among them). A server that dies before it prints a line still leaves the story of how it was started.
- The server's own account: the devices and their free memory, the memory fit (what it projected, whether it moved
  tensors off the card, how long it took), the model's metadata, how many layers went to the GPU, every buffer on every
  device (weights, KV cache, recurrent state, compute), the KV cache of each kind (a sliding-window model has two), the
  compute graph with its splits (a layer split shows in the prompt graph), flash attention, and how long each stage of
  the load took.
- One timing block per request: prompt and generation tokens and speeds, total time, graphs reused.

The logs of the three loads before the current one stay as `<model id>.1.log`, `.2.log` and `.3.log`. A load that
crashed and was tried again leaves its log there for the owner to read, with its own header (the GPU start, the GPU
start with the cooperative-matrix kernels off, the CPU start).

**Reading it.** Two operations:

- `logs.tail` (`model`, `lines`, `previous` 0 to 3): the last lines of the current log, or of an earlier load's.
  A `previous` with no such log is `404 no_such_log`.
- `model.telemetry` (`model`): the log read into numbers. The model must be loaded (`409 not_loaded` otherwise).

`model.telemetry` answers with these parts; a value that was not found is left out, never printed as null or zero:

| Part | Holds |
|---|---|
| `model`, `pid`, `port`, `uptime_s`, `args` | which server and how it was started |
| `placement` | `where` (`gpu`, `partial`, `cpu`), `note`, `layers` (total, offloaded, output on the GPU, the memory of the weights on each side, KV and state, graph splits), a one-line `line`, `vram_mib` and `estimated_vram_mib`, `gpu_dedicated_now_mib` (what the process holds on the card now, on Windows) |
| `load` | the report of the log: `devices`, `cpu`, `fit`, `model` (architecture, file type and size, bits per weight, layers, experts, trained context, sliding window, recurrent), `layers`, `buffers`, `caches`, `context`, `graphs`, `flash_attention`, `fused_ops`, `server` (slots, prompt cache, checkpoints), `stages` (fit, weights read and MiB/s, context, warm-up, time to ready), `warnings` and `notes` |
| `runtime` | `recent_requests` (the last 10 timings), `server_counters` (the server's `/metrics`), `slots` (`/slots`) and the hub's own `hub` counters |
| `logs` | the log files kept, with size and time |

What it cannot say: a server started at the default verbosity (an owner's `-lv 3`, an engine without the flag) has no
layers or buffers in its log; `load.notes` says so, `placement.layers` is absent, and the placement falls back to the
memory guess above. The load report is read from the first 1 MiB of the log and the requests from the last 256 KiB, so
a server that has run for a long time still answers quickly and shows only its latest requests. The log restarts only
when the model is loaded again.

## Hub watchdog

`model-mistress serve` runs a watchdog thread. Every 10 seconds it asks the hub's own `/v1/health` over the loopback; if
that does not answer for `watchdog_s` seconds (default 90; 0 turns it off; 3 minutes of grace after the start) it writes
`<home>/logs/hang-<time>.txt` (the time, when the runtime's timer task last ran, memory and, on x86-64 Windows, the
stack of every thread; the newest five are kept) and ends the process with exit code 70, so that the Scheduled Task starts a fresh hub. It is for a hub that is
alive but deaf: the operating system still accepts connections for a process that has stopped answering, so nothing else
notices. The desktop app's own hub does not run it.

## Recommended profile: Qwen3-Coder-30B-A3B (Q4_K_XL, 17.7 GB) on a 24 GB card for four agents

```toml
[profiles."<the coder's model id>"]
parallel     = 4
ctx_size     = 32768        # one shared pool: any agent may use what the others leave free
kv_unified   = true
cache_type_k = "q8_0"
cache_type_v = "q8_0"
flash_attn   = "on"
cache_prompt = true
```

Measured: loads in 42 s, 18.5 GB of VRAM, 149 tokens/s for one stream and 171 tokens/s across four at once. That
leaves about 5 GB on a 24 GB card, which a desktop with a browser and a launcher already takes part of: there is no
room for a second large model, nor for a larger pool. KV cost in q8_0 is 52 KB a token (1.6 GB for 32k tokens); more
context needs a smaller weight share on the GPU (`n_cpu_moe`, which moves the experts of the first N layers to RAM:
unmeasured here) or `q4_0` KV. A small helper model (Qwen3-1.7B, 1.4 GB) and an embedder (0.6B, 0.6 GB) do fit
next to it by estimate; `model.plan` says so before the load. Without `kv_unified`, 32k in all means 8k per slot.
