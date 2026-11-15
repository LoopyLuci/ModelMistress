# Rigel: a second engine for AMD GPUs on Windows

Rigel is a llama.cpp fork ported to Windows and AMD HIP. Its main addition is **paged mixture-of-experts**: the routed experts of a MoE model stay off the GPU and
stream from disk (the GGUF or an expert pack) into a pool of VRAM slots as the router asks for them. That lets a MoE
model that does not fit in VRAM run on the GPU.

The hub serves each model with llama.cpp unless that model's serving profile chooses Rigel.

## Choosing Rigel for a model

Put this in `config.toml`, or set it with `config.set` (the profile format is in [serving.md](serving.md)):

```toml
[profiles."unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF:Qwen3-Coder-30B-A3B-Instruct-UD-Q4_K_XL"]
engine = "rigel"
moe_direct_io = true   # page the experts (--moe-direct-io)
moe_slots = 1536       # VRAM expert slots shared by all layers (--moe-slots)
# moe_pack_dir = "…"   # expert pack folder with experts.bin (--moe-pack-dir); needed for stripped bundles
# moe_profile = "…"    # hot-expert profile that seeds the slots (--moe-profile)
# moe_cache_mib = 4096 # pinned host memory for evicted experts (MOE_CACHE_MIB); 0 = off
```

- A Rigel model always gets **one slot**, whatever `parallel` says: Rigel has one paged context per process.
- The `moe_*` options are Rigel-only. A profile that sets them without `engine = "rigel"` is refused, and the error
  names the option.
- With paging on, the VRAM estimate leaves out the routed experts, as it does for `--cpu-moe`.
- `model.plan` shows `serving.engine` and the exact arguments. `backend.list` lists `rigel`, with whether it was found,
  its path, its built-in version and how many loaded models use it.

## Where the hub finds rigel-server

In this order:
1. the `rigel_server` setting;
2. `MM_RIGEL_SERVER`;
3. the copy built into the binary (feature `rigel-engine`, written to `<home>/engines/rigel-bundled/<hash>/`);
4. `<home>/engines/rigel/`;
5. `PATH`.

A HIP build loads the HIP runtime at run time. When `HIP_PATH` is set, the hub puts `%HIP_PATH%\bin` first on the
server's `PATH`. Otherwise the runtime must already be on `PATH`.

## Building it in

```bash
set HIP_PATH=C:\Program Files\AMD\ROCm\7.2\
set MM_RIGEL_GPU_TARGETS=gfx1100          # default; a list such as gfx1100,gfx1201 works too
cargo build --release -p model-mistress --features rigel-engine
```

`build.rs` applies `engines/patches/rigel.patch` to the `engines/rigel` submodule (the submodule stays pinned; the
patch is skipped when it is already applied), configures it with the HIP SDK's clang, Ninja and Visual Studio's
libraries, and builds `rigel-server`. The build needs the HIP SDK, CMake, Ninja and Visual Studio's C++ tools. It can
be combined with `bundled-engine`.

What the patch does:
- It runs the paged MoE runtime (the pager, the GPU doorbell and the predictor kernels) on HIP. Upstream runs it on
  CUDA under Linux.
- `src/rigel-port.cpp` and `rigel-gpu.h` supply io_uring and the other POSIX pieces on Windows.
- It draws a plain-text logo, so the build needs neither rsvg-convert nor ImageMagick.
- It names the programs `rigel-server` and `rigel-cli`.

## Paged decoding speed

With paging on, decode used to slow down over a long answer. The pager and the doorbell handshake were not the cause.
Measured on an RX 7900 XTX:
- the GPU waited about 4 ms per 512 pager services;
- copy time matched the host's busy time.

The extra time was in queueing graphs: up to 30 ms per token instead of 4. `RIGEL_TIME_GRAPH=1` prints launch and
wait times every 32 graphs.

**Still open.** Waiting for every graph before queueing the next was tried on 2026-11-15 and made things worse, so it
was not kept. Five prompts, 256 tokens each, with a fresh server:

| Prompt (in order) | Asynchronous | Wait per graph |
|---|---|---|
| def quicksort(arr): | 10.0 tok/s | 4.1 tok/s |
| Write a haiku about the sea. | 16.6 | 10.6 |
| def is_prime(n): | 6.9 | 7.7 |
| The history of Rome begins | 15.9 | 17.5 |
| Explain how a refrigerator works. | 21.7 | 25.2 |

Speed mostly grows as the VRAM slots fill with hot experts. A `moe_profile` that seeds the slots, or a larger
`moe_slots`, is the first thing to try. The per-token queueing spikes are not explained yet.
