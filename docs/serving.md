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
  + KV cache  (attention layers x KV heads x (key size x K type + value size x V type) x context tokens)
  + recurrent state (hybrid models such as Qwen3.5: per slot)
  + compute buffers (about 300 MiB per 512 of `ubatch_size`) + 200 MiB per context

and compares it, plus `vram_margin_mib` (default 768), with the GPU's free memory. Free memory is the card's total
(from `llama-server --list-devices`) minus the dedicated memory in use on the adapter (Windows counter `GPU Adapter
Memory`): llama-server's own "free" figure does not see what other processes hold (measured: it reported 18.7 GB free
with 5.8 GB taken by other programs, and 5 GB fewer after a 18.5 GB model was loaded). Where the counter is not
available the planner uses llama-server's figure.

- It fits: load.
- It does not, but would after unloading idle models: they are unloaded, least recently used first (a model with a
  request running is never chosen), and the load waits for their memory to come back.
- It does not fit at all: `503 insufficient_vram`, with the estimate by part, the free memory, and what to change
  (lower `ctx_size` or `parallel`, a smaller KV type). Nothing is started, so nothing spills into shared memory.

`vram_guard = false` in `config.toml`, or `skip_vram_check: true` in one load, turns it off. With no estimate (the GGUF
lacks the layer count) or no device listing (a CPU build), the guard does nothing.

`model.plan` (same arguments as `model.load`) answers without loading: the arguments the server would get, the
estimate, the devices and their free memory, and the verdict `fits`, `fits_after_unloading` (with `unload`),
`does_not_fit` or `unknown`.

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
`{"where": "none", "need_ram_mib": ..., "free_ram_mib": ...}`.

**What was measured.** Each loaded model reports `serving.placement`: `gpu` (the weights are on the GPU), `partial`
(some are), `cpu`, or `unknown` (no GPU counters on this machine, or not measured yet). It comes from the dedicated
GPU memory of the model's server process compared with the weights and KV cache of every layer (compute buffers are on
the GPU either way, so they are left out). Where the counters work, a server
that holds none is `cpu`. `serving.placement_note` says why a model is not where it was asked to be (below). The Models page
shows both.

**When the GPU fails a load.** With `gpu_fallback = true` (the default), a load that fails because of the GPU is tried
again instead of being reported at once:

1. *Out of memory* (the log says an allocation on a device failed): llama-server is asked to keep 2 GiB more free
   (`--fit-target` margin + 2048), where it chooses the layers itself.
2. *Out of memory again, or a crash* (the server died the way a driver crash kills a process: exit code `0xC0000409` or
   another `0xC...` code on Windows, a signal elsewhere): the model is started on the CPU, if free RAM holds the file and
   a GiB more. If it does not, the GPU's own error is the answer.

A failure that is not the GPU's (an unknown architecture, a file that cannot be read, a load that times out) is never
retried. When every start fails, the error is the last one, with what was tried before it: `(before this: ...)`.

A **crash** is remembered. When the CPU start works after one, `<home>/placements.json` records the model and the
llama-server program (path, size, date) it crashed with; the next load of that model starts on the CPU at once and says
so in `placement_note`. A different or updated llama-server gets a new try on the GPU. `model.load` with
`"retry_gpu": true` tries the GPU again (and forgets the record when it works); the load form of the Models page has a
box for it. An out-of-memory failure is not remembered: it depends on what else is using the GPU at the time.

`gpu_fallback = false` turns all of this off: one start, one error.

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
