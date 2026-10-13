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
| `gpu_layers` | `-ngl` | Layers on the GPU (999 = all). |
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
