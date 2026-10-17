# The Forge: ModelMistress's training system (part 1)

The Forge is a job runner in the hub plus a managed Python worker (PyTorch on ROCm) in its own environment (see
`architecture.md` section 4). The hub owns runs, metrics and cancelling; the worker only computes. This first part
contains the worker protocol, the hardware preflight, the Dusty conversion and its equivalence test, and a way to
serve any Hugging Face model for generation (`km-factory.md` section 1).

## Install

```
model-mistress call config.set '{"settings": {"forge_dir": "<where the env goes>", "forge_data_dir": "<weights and outputs>"}}'
model-mistress forge install --python <python 3.12>
```

| setting (environment variable) | meaning | default |
|---|---|---|
| `forge_dir` (`MM_FORGE_DIR`) | holds the Python environment, `<forge_dir>/env` | `<home>/forge` |
| `forge_data_dir` (`MM_FORGE_DATA_DIR`) | weights, checkpoints, outputs | `<forge_dir>/data` |
| `forge_src` (`MM_FORGE_SRC`) | the repository's `forge/` folder (`mm_forge/` and the requirements) | looked for next to the program and the working folder |
| `forge_python` (`MM_FORGE_PYTHON`) | the Python 3.12 that creates the environment | `python` on PATH |

`forge.install` is a job (and a run): it creates the venv, runs `pip install -r forge/requirements-rocm-windows.txt`
(temporary files go under `<forge_data_dir>/tmp`), then the preflight. The requirements pin AMD's PyTorch ROCm
wheels for Windows (ROCm 7.2.1, torch 2.9.1, from `repo.radeon.com`, each with its SHA-256) and PyPI packages.
They need Python 3.12 and a graphics driver with ROCm for Windows support. There is no requirements file for other
platforms yet; `--requirements` takes one.

## Preflight

`forge.preflight` (or `model-mistress forge preflight`) proves that an optimizer step really runs on the GPU. It
builds a small seeded transformer, runs 5 AdamW steps on the CPU and on the GPU from the same weights and data, and
requires the losses, the gradients and the updates to agree; it also checks bf16 (autocast training against the fp32
reference, and a matrix product), fills 4 GiB of VRAM, and times a bf16 matrix product. With no usable GPU it runs
on the CPU and says so (`device_kind: "cpu"`, `ok: false`); GPU jobs should refuse to start then.

Result on the development machine (Windows 10, Radeon RX 7900 XTX, driver 32.0.11029): `ok: true`,
`torch 2.9.1+rocm7.2.1`, HIP `7.2.53211`, device `gfx1100` with 48 compute units, 25.75 GB VRAM, bf16 supported,
loss difference GPU vs CPU 7e-8, gradient relative error 8e-7, 91 TFLOPS bf16 matrix product, about 8 seconds. Windows
ROCm works directly; WSL was not needed.

## Runs and the worker protocol

The hub starts `python -m mm_forge <step>`, writes the step's JSON arguments on stdin and reads JSON lines on stdout:

```
{"type":"progress","done":3,"total":10,"note":"layer 3 of 10"}
{"type":"metric","name":"loss","value":1.25,"step":40}
{"type":"log","level":"info","message":"..."}
{"type":"artifact","name":"dusty-q8","path":"...","bytes":123,"kind":"gguf"}
{"type":"result","data":{...}}            at most once, last
{"type":"error","message":"...","code":"..."}
```

Anything else on stdout is kept as log text; stderr is the log too. NaN and infinities arrive as strings.
Every run is stored in SQLite at `<home>/forge/runs.db`: arguments, status, progress, result, every metric,
artifacts and the log (20,000 lines per run; later lines are counted). A run left `running` by a hub that died
becomes `interrupted`. Cancelling (`forge.run.cancel`, or `job.cancel`) kills the worker's whole process tree (a Job
Object on Windows, a process group elsewhere); the tree also dies when the hub exits.

| operation | CLI | |
|---|---|---|
| `forge.status` | `forge status` | where it lives, installed or not, last preflight, running runs, served models |
| `forge.install` | `forge install` | job |
| `forge.preflight` | `forge preflight` | waits |
| `forge.run` | `forge run <step> '<json>'` | job (`--detach` to return at once) |
| `forge.runs`, `forge.run.get`, `forge.run.cancel` | `forge runs`, `forge show <run> [--metric m]`, `forge cancel <run>` | |

Steps: `env_info`, `preflight`, `dusty_convert`, `dusty_equiv`, `generate`, `generate_serve`, `lora_train`,
`qlora_train`, `merge_lora`, `merged_to_gguf`, `lora_to_gguf`.
Checkpoint helpers (`mm_forge.checkpoint`: safetensors with a hash manifest, a streaming sharded writer) and
deterministic seeding (`mm_forge.seeding`) are the building blocks for the training steps that follow.

## Dusty-1.0: `dusty_convert`

Converts Qwen3.5-4B (`Qwen/Qwen3.5-4B`, Apache-2.0, revision 851bf6e) into a `qwen35moe` model:

* each dense FFN becomes the always-on **shared expert**: `gate_proj` and `up_proj` unchanged, `down_proj` doubled;
* `mlp.shared_expert_gate` (one row, hidden wide) is **all zero**, so its sigmoid is exactly 0.5 for every token;
* 64 routed experts per layer (top-4), each 1/8 of the FFN wide (1152 of 9216): `down_proj` all zero, `gate_up_proj`
  taken from slices of the dense FFN (slot s gets the (s mod 8)-th eighth), and a router of small random weights
  (standard deviation 0.02, seeded per layer);
* the HF-format config is `Qwen3_5MoeForCausalLM` / `qwen3_5_moe_text` with `num_experts`, `num_experts_per_tok`,
  `moe_intermediate_size`, `shared_expert_intermediate_size`, `norm_topk_prob`.

**Why the result is exact** (read from `engines/llama.cpp/src/models/qwen35moe.cpp`, `build_layer_ffn`): the layer
computes `moe_out + sigmoid(ffn_gate_inp_shexp . x) * shexp(x)`. The gate is a sigmoid (confirmed), so with zero gate
weights it is 0.5; 0.5 * (2 * W_down) h equals W_down h because scaling by two only changes the exponent. The routed
part is a sum of expert outputs times normalised top-k weights, and every routed down projection is zero, so it adds
exactly 0 whatever the router picks. Python tests check this on a synthetic checkpoint with a reference
implementation of the Hugging Face MoE block (`torch.equal`).

**What is dropped**: the vision tower (297 tensors, `model.visual.*`) and the multi-token-prediction head
(`mtp.*`, 15 tensors; converted with `--no-mtp`, since llama.cpp's MTP head for `qwen35moe` would need its own MoE
tensors). The text model, tokenizer and chat template are kept. `dusty.json` next to the output lists what was
dropped, the settings, the sizes and the hashes.

Arguments: `source` (folder or repo id), `name` (default `Dusty-1.0`), `slots`, `top_k`, `seed`, `router_std`,
`expert_init` (`slice`|`random`), `experts` (`auto`|`write`|`virtual`), `outtypes` (`bf16`, `f16`, `q8_0`),
`quantize` (e.g. `["Q8_0"]`, runs the hub's `llama-quantize` on the 16-bit file), `hf_dir`, `out_dir`.

The output goes to `<forge_data_dir>/dusty/<name>/`: `hf/` (the checkpoint), `gguf/<name>-<TYPE>.gguf` (converted by
llama.cpp's own `convert_hf_to_gguf.py`), `dusty.json`. With 64 slots the routed experts are about 36 GB in 16 bit and
mostly zeros, so `experts: "write"` writes them all into `hf/` (all-zero tensors are sparse holes), while
`"virtual"` leaves them out of `hf/` and generates them lazily inside the converter (`mm_forge.dusty_gguf` adds them
to the converter's tensor index); both give the same GGUF (tested). `auto` picks `write` when the disk has room.

## Equivalence: `dusty_equiv`

Serves two GGUF models with the bundled engine (a model-mistress executable started with a temporary home, stopped
afterwards, or an already running hub), completes 10 prompts greedily for 64 tokens with both, and compares token
ids, texts and the logprobs of the chosen token and of the top 5. Everything is stored in a run (metrics
`ids_equal`, `max_logprob_diff`, `tokens` per prompt).

Result, Dusty with 8 slots (`--slots 8`, bf16) against Qwen3.5-4B converted directly with the same converter at
bf16, CPU build of llama.cpp 7fee178: **10 of 10 prompts identical, 625 tokens compared, identical texts, maximum
logprob difference 0.0 (top-5 included)**; the run is `run-20261010-194304-905c` in the development hub's store. All
ten prompts reached 64 tokens except one that both models ended after 49. The slot count does not enter the
argument above; it only changes the file size, which is why the 8-slot model could be tested at bf16 on a disk that
cannot hold 64 slots at 16 bit.

**Dusty-1.0 itself** (64 slots, top-4, Q8_0, 23.76 GB; a 16-bit file would be about 45 GB, which the development disk
could not hold, so the shipped file is Q8_0 and the 8-slot bf16 test above stands in for 16 bit) against Qwen3.5-4B
converted directly to Q8_0 by the same converter: **10 of 10 prompts identical, 625 tokens, identical texts, maximum
logprob difference 0.0 (top-5 included)**, run `run-20261011-014024-4bc7`.

Q8_0 needed one repair, found by this test. The first Dusty Q8_0 differed (8 of 10 prompts identical, mean logprob
difference about 0.003, two prompts diverging after 10 and 32 tokens). Cause: Q8_0 stores a float16 scale per block of
32 weights, `f16(amax / 127)`; doubling a weight doubles `amax`, but where that scale is a float16 subnormal the
rounding grid is absolute, so `f16(2d)` is not `2 * f16(d)`: 92,932 blocks (0.4 percent of some layers' down
projections) had a scale one step off. The int8 values were identical. `fix_q8_doubled_scales` (run by
`dusty_convert` after every Q8_0 output) rewrites those scales as exactly twice the original's, which makes the doubled
tensors `2 * Q8_0(W)` bit for bit; a test reproduces the case. bf16 and f16 need no repair (doubling is exact there).
Other quantisations (Q4_K and the like, and any later requantisation) are not exact by construction; the
equivalence test is the way to see how far they are.


## Serving any Hugging Face model: `generate`, `generate_serve`, `forge.load`

When llama.cpp cannot run a model's architecture, the Forge worker runs it with PyTorch and `transformers`, on the
GPU (ROCm) or the CPU. `device` is `auto` (the GPU when the weights plus a fifth fit its free memory, else the CPU),
`gpu` or `cpu`; the placement is reported. The server protocol (`generate_serve`): the first stdin line is the
arguments (`model_path`, `device`, `dtype`), answered by a result with `id: "init"` and the model's info (device,
dtype, parameters, architecture); then one JSON request per line, one result per request with the same id:

```
{"id":"7","prompt":"Hello","max_tokens":32,"temperature":0,"top_logprobs":3}     (or prompt_ids, or messages)
{"type":"result","data":{"id":"7","text":"...","tokens":[{"id":12,"text":" world","logprob":-0.3,"top":[...]}],
                         "finish":"length","prompt_tokens":1,"device":"cuda:0"}}
```

`forge.load` is a stub that picks the engine by format and records where the model runs (`forge.status` lists the
placements): GGUF (a model of this hub) loads on llama.cpp (`device` gpu/cpu sets the GPU layers); a Hugging Face
folder whose architecture llama.cpp's converter knows gets `needs_conversion` (the conversion itself is the
`dusty_convert` machinery, not yet a general step); any other architecture is started on the Forge worker, after which
`forge.generate` returns tokens with logprobs and `forge.unload` stops it; ONNX is planned only (`not_available`).
`--plan-only` only says what would be done. The KM runtime proper is another branch's work.

## Fine-tuning: `lora_train`, `qlora_train`, export

Replaces Unsloth: PEFT LoRA (and 4-bit QLoRA) on PyTorch, on the GPU (ROCm or CUDA) or, with `device: "cpu"`, the CPU.
Start a run with `model-mistress forge run lora_train '<json>'` (or the `forge.run` operation); `forge show <run>
--metric loss` follows it, `forge cancel <run>` stops it, and starting the same arguments again resumes it.

```
model-mistress forge run lora_train '{"base_model": "<HF folder>", "data": "train.jsonl", "out_dir": "<run folder>",
  "rank": 16, "alpha": 16, "learning_rate": 2e-4, "epochs": 2, "batch_size": 2, "grad_accum": 4, "max_seq_length": 1024}'
```

Arguments: `base_model` (folder or repo id), `data` (file or list), `out_dir`; LoRA `rank` 16, `alpha` 16, `dropout` 0,
`target_modules` (default the attention and MLP projections present), `rslora`, `dora`; `learning_rate` 2e-4,
`epochs` 1 or `max_steps`, `batch_size` 2, `grad_accum` 4, `max_seq_length` 1024, `warmup_steps`, `weight_decay`,
`max_grad_norm`; `load_in_4bit` (always on for `qlora_train`), `dtype` (auto = bf16 on the GPU), `device`
(auto|gpu|cpu; `auto` without a GPU fails rather than training on the CPU), `gradient_checkpointing`, `eval_fraction`,
`eval_every`, `save_every` 50, `keep_checkpoints` 2, `resume` true, `seed`, `merge` (also write `<out_dir>/merged`),
`vram_check`, `free_vram_gb`.

**Data** (`mm_forge.traindata`; `.jsonl`, `.json`, `.parquet`; formats are detected per row): chat
`{"messages": [...]}`, ShareGPT `{"conversations": [{"from", "value"}]}`, alpaca `{"instruction", "input", "output"}`
(+ `system`), `prompt`/`completion`, `question`/`answer`, and plain `{"text"}` (trained in full). Rows are rendered with
the model's chat template (ChatML, saved with the result, when the base has none) and the loss is on the assistant's
turns only. `eval_fraction` splits off a seeded eval set (at most 200 rows).

**Checkpoints and resume**: every `save_every` steps and at the end, `<out_dir>/checkpoints/ckpt-<step>` holds the
adapter tensors (`checkpoint.py`: safetensors + hash manifest), the optimizer state (hash in the manifest) and the
position in the data. A run started with the same `out_dir` continues from the newest checkpoint whose hashes verify,
so cancelling (which kills the worker) loses at most `save_every` steps; the batch order is deterministic, and a
resumed run ends with the same adapter as an uninterrupted one (tested). Metrics per step: `loss`, `lr`, `grad_norm`,
`gpu_mem_gb`, and `eval_loss`. The adapter ends in `<out_dir>/adapter`; `train.json` summarises the run.

**VRAM preflight**: before loading, the free VRAM (`torch.cuda.mem_get_info`, or `free_vram_gb`) is compared with a
pessimistic estimate (weights, adapters with Adam state, activations, the logits). If the request does not fit, the
batch is halved (`grad_accum` raised to keep the effective batch), then 4-bit is used if it is available; if nothing
fits the run fails at once with the free and needed GiB. What was changed is logged and returned as `vram_plan`.

**QLoRA caveat**: it needs `bitsandbytes` with a GPU backend, which in practice means Linux with CUDA (or a ROCm build
on Linux). It is not in the Windows/ROCm requirements and was not available on the development machine. Without it
`qlora_train` falls back to 16-bit LoRA with a warning (`qlora_fallback: false` makes it fail instead); LoRA in
bf16/fp16 is the supported path on Windows/ROCm.

**Export**: `merge_lora` (`base_model`, `adapter`, `out_dir`) merges the adapter into the base as safetensors plus
tokenizer; `merged_to_gguf` (`model_dir`, `out_dir`, `name`, `outtypes`, `quantize`, `quantize_exe`) converts a Hugging
Face folder with llama.cpp's `convert_hf_to_gguf.py` and `llama-quantize` (the Dusty path); `lora_to_gguf` does both.
The GGUF files are imported into the hub like any model. A real run needs `forge install` (torch, transformers,
peft), a base model folder and a dataset; ABP calls it as `forge.run` with the step name and these arguments.

## Tests

* Python (`forge/tests`, `python -m pytest tests`, about 10 seconds): protocol, seeding, checkpoints (sparse holes),
  the Dusty tensor mapping and exactness on a tiny synthetic checkpoint, the comparison logic, the generation
  server on a tiny random Llama (CPU, and GPU when there is one), and fine-tuning (datasets, VRAM planning,
  checkpoint and resume, export wiring, a tiny LoRA run on the CPU with a tokenizer built in the test).
* Rust (`cargo test -p model-mistress forge`): protocol parsing, the run store, the process runner (stdin, noise,
  timeout, cancel kills a grandchild), format and engine planning, and the operations against a real venv made by
  `forge.install` with a stand-in `mm_forge` package (install, run, metrics, failure, cancel, load, generate).

## Not yet

Expert training, distillation, DPO, evaluate and promote steps, the dashboard views, budgets
(GPU hours, idle windows, pausing for the owner's work), Linux/WSL requirements, a general safetensors to GGUF
conversion step, ONNX language models.
