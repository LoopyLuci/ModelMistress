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

If the build stops with errors such as `__device__ function 'isless' cannot overload __host__ __device__ function`, HIP's clang 21
does not like the newest MSVC headers (toolset 14.51). Install an older toolset beside it and pin it:
`set MM_RIGEL_MSVC_TOOLSET=14.44.35207` (the folder name under `VC\Tools\MSVC`). That built on 2026-11-24.
The submodule must be checked out first: `git submodule update --init engines/rigel`.

`engines/rigel` is ModelMistress's own repository (https://github.com/LoopyLuci/Rigel, a standalone snapshot of Betelgeuse with
this port applied, with its own history and a `NOTICE.md` that credits its origin). `build.rs` configures it with the HIP SDK's clang, Ninja and Visual Studio's
libraries, and builds `rigel-server`. The build needs the HIP SDK, CMake, Ninja and Visual Studio's C++ tools. It can
be combined with `bundled-engine`.

What the patch does:
- It runs the paged MoE runtime (the pager, the GPU doorbell and the predictor kernels) on HIP. Upstream runs it on
  CUDA under Linux.
- `src/rigel-port.cpp` and `rigel-gpu.h` supply io_uring and the other POSIX pieces on Windows.
- It draws a plain-text logo, so the build needs neither rsvg-convert nor ImageMagick.
- It names the programs `rigel-server` and `rigel-cli`.

## Layer read-ahead (`layer_readahead`, `--layer-readahead N`)

For a **dense** model whose weights the operating system pages in from the memory-mapped file because they do not all fit in
memory. While layer *i* computes, a worker thread asks the system to read layers *i+1* to *i+N* (`PrefetchVirtualMemory` on
Windows, `madvise(WILLNEED)` elsewhere), and the next token's first layers wrap round at the end. This is AirLLM's "read the next
layer while this one computes", built on the existing memory map, so the weights keep their place and the GGUF is unchanged.

```toml
[profiles."<model id>"]
engine = "rigel"
gpu_layers = 0          # or the number of layers that fit the card
layer_readahead = 4     # 0 or unset: off
```

- Off by default. On a model that fits in memory it only costs: 6.45 against 6.27 tokens/s (3%) with a 9B Q4_K_M model on the CPU, with
  identical output.
- It watches the end of each layer (`l_out-<il>`), which splits the graph there; it is off while paged experts (`--moe-direct-io`) are on.
- **It does not make a model that is bigger than memory fast.** Measured on 2026-11-24 on a cold 3 GB region of a GGUF, 12 threads
  (disk: the model drive): plain page faults read 1.7 to 1.8 GB/s, a prefetch of the whole range 1.85 GB/s, and a pipeline that prefetches
  four 128 MB chunks ahead 2.0 GB/s. So the faults of a multi-threaded matrix multiply already read sequentially and nearly saturate the
  disk; read-ahead gains at most about 10%. A dense model of *S* GB bigger than memory still costs about *S* divided by the disk rate
  per token. A run with 15 to 20 GB of the machine's RAM held back (1.6 GB left available) showed no effect either way (6.0 to 6.3
  tokens/s, read-ahead 0, 4 or 8) because the 5.5 GB test model stayed cached; a model clearly bigger than RAM is still untested.
  Results and what is left: [rigel-hybrid-plan.md](rigel-hybrid-plan.md).

## Pinned resident set (`layer_resident_mib`, `--layer-resident-mib N`)

For a **dense** model bigger than the memory it may use. Instead of letting the system evict and re-read every layer on every token, the first
layers whose weights sit in host memory are **locked in memory** up to N MiB (`VirtualLock` on Windows, which grows the process's working set
to fit; `mlock` elsewhere), and only the rest streams from the file. Bytes read per token fall from S to S minus the locked part, which neither
llama.cpp's paging nor AirLLM does.

```toml
[profiles."<model id>"]
engine = "rigel"
layer_resident_mib = -1   # MiB to lock; -1: what is free (less 2 GB or a tenth of the RAM); 0 or unset: off
```

- Never more than 85% of the RAM; layers are locked whole, in order. Layers on the GPU are not host memory and are not counted.
- Off with paged experts; it combines with `layer_readahead` (locked layers are not prefetched). If the system refuses a lock (commit limit),
  the server logs a warning and runs with what it got.
- Measured with a hard working-set cap (4096 MiB for a 5.5 GB model): a 3500 MiB set read about 2 to 3 times faster than none; single runs,
  noisy. Details: [rigel-hybrid-plan.md](rigel-hybrid-plan.md).

## GPU and CPU together

A mixture-of-experts model that does not fit the card whole goes to Rigel on its own (`auto_rigel`, default on) with paged experts and as many VRAM slots as the card has room for; a Rigel load that fits the card whole is started with `-lm none`. Measured numbers: [rigel-hybrid-plan.md](rigel-hybrid-plan.md).

`gpu_layers` left at its default lets Rigel's own `--fit` choose how many layers go to the card (ModelMistress passes `--fit on --fit-target`
as for llama.cpp; with paged experts it keeps `-ngl`). The rest runs on the CPU from RAM, and with `layer_resident_mib` the part of that
which does not fit RAM is streamed. The setting `vram_extends_ram` (default on) also moves layers to a card that has room for a load that was to
run on the CPU but does not fit the free RAM. Measured: a 9B model with 3.9 GB of the card usable ran at 19.3 tokens/s, on the CPU alone 6.3.

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
