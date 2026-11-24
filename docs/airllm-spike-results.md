# AirLLM Phase 0 spike: results (2026-11-24)

Part of [airllm-integration-plan.md](airllm-integration-plan.md) (section 4.4). Run by an agent on the owner's yes ("download to
the drive with the largest free space"). **Nothing in the hub, the Forge environment or `home/` was changed**, and no source
file in this repository changed. This is a partial spike: it answers T3 and T4 and gives one speed point. It does **not**
answer T2 or T5, which need a model bigger than RAM plus VRAM.

## What was run

| Item | Value |
|---|---|
| Machine | Windows 10, RX 7900 XTX 24 GB (gfx1100), about 64 GB RAM |
| Where | A throwaway folder on the drive with the most free space (325 GB free), `<drive>:\ClaudeCodeTemp\ModelMistress\airllm-spike\` (~11 GB: venv, AirLLM's split copy, pip cache, results) |
| Python | 3.12.6 virtual environment, separate from the Forge's |
| torch | `2.9.1+rocm7.2.1`, the Forge's own pinned AMD wheels (`forge/requirements-rocm-windows.txt`, each checked by SHA-256); `torch.cuda.is_available()` is True and names the RX 7900 XTX |
| Other pins | The Forge's PyPI pins (`transformers==4.57.6`, `accelerate==1.13.0`, `safetensors==0.8.0`, ...), plus `scipy==1.18.1` |
| AirLLM | `airllm==4.0.1` (PyPI), `--require-hashes --no-deps`, wheel SHA-256 `a7027d5e77cf473bbfd825d2489eb8d888bcccf132633cbf2ddcf9362896ab41` |
| Model | `Qwen/Qwen3-1.7B` bf16 (3.4 GB), already in the owner's Hugging Face cache, read-only. No model was downloaded. |
| Run | `HF_HUB_OFFLINE=1`, `TRANSFORMERS_OFFLINE=1`, greedy, 32 new tokens, the same chat prompt ("Count from 1 to 40...") |

## Results

| Measurement | Whole model (transformers, on the GPU) | AirLLM 4.0.1 (layer by layer, on the GPU) |
|---|---|---|
| Load | 17.9 s | 11.5 s, after a one-off split into one file per layer (about 3.3 GB written) |
| 32 tokens | 3.06 s (0.096 s/token) | 116.3 s (**3.64 s/token**) |
| Peak VRAM | 3,322 MiB | **729 MiB** |
| The 32 tokens | `1,2,3,4,...,14` | **identical token ids** |

- **T3 (the GPU is really used): pass.** Peak VRAM was 729 MiB, so the weights went through the card on the ROCm build of
  PyTorch. The failure in the AirLLM issue #301 (an RX 9070 XT, where it ran on the CPU) did not happen on this machine.
  The GPU load percentage was not recorded, only the allocation.
- **T4 (correctness): pass** for this one model and prompt: the 32 greedy tokens are exactly the whole-model run's.
- **Speed: AirLLM was 38 times slower than the whole model** on a model that fits on the card. Every token re-reads all 28
  layers. Here the files were in the operating system's cache (the split copy had just been written), so this is a best
  case for the disk, and it is still 3.6 s per token for a 1.7B model. By the plan's own classes (T1) that is "batch only".
- **Side effects (M9):** the run wrote only inside the spike folder (the split copy, 3.3 GB) and opened no network
  connection that showed in its log (`HF_HUB_OFFLINE=1`). The Qwen3-1.7B folder has no `.py` files, so AirLLM's
  `trust_remote_code=True` calls had nothing to execute. They are still in the code (plan, section 2) and still matter for any
  model folder that ships Python.
- The split copy of this model is as large as the model itself, as the plan said.

## What this does not show (and what to do about it)

1. **T2 is open.** The question that decides everything is whether llama.cpp's own paging beats AirLLM's sequential reads on
   a model bigger than free RAM plus VRAM. Neither was run on one. The plan's Phase 1 stays gated on this.
2. **T5 (batch prefill) is open.** It needs a bigger model than anything on the machine in Hugging Face form.
3. **M10 (llama.cpp baselines) was not run.** `qwen3.5-9b.gguf` (6.3 GB, in the owner's model folder) is
   the only GGUF the owner pointed at; a GGUF cannot be read by AirLLM, so it cannot pair with these numbers.
4. **Which local models can AirLLM read at all?** Only Hugging Face checkpoints of plain causal LMs. On this machine that is
   `Qwen3-0.6B` and `Qwen3-1.7B` (Hugging Face cache). `Dusty-1.0` (`forge/dusty/Dusty-1.0/hf`) is `Qwen3_5MoeForCausalLM`
   with linear-attention layers: not tried, and the pinned `transformers==4.57.6` probably does not know the architecture.
   The Qwen2-VL and Whisper folders are not causal LMs.
5. Per-layer reads from a cold disk (the case that matters) were not measured. To do it, run the spike script on a model
   bigger than RAM, or drop the OS file cache first.

## Conclusion

AirLLM 4.0.1 **runs correctly on this machine's ROCm PyTorch** and does reduce VRAM sharply (729 MiB against 3,322 MiB), at a
cost of two orders of magnitude in speed. That confirms the plan's verdict: **do not build an AirLLM serving engine.** What
remains useful is unchanged: Phase 1 (an honest, opt-in paged load of a GGUF through llama.cpp) and, only if the owner names a
batch workload, Phase 2 (layer streaming as a Forge step, which does not need AirLLM).

## How to repeat or extend it

```
<spike>\venv\Scripts\python.exe <spike>\spike.py whole     # transformers baseline
<spike>\venv\Scripts\python.exe <spike>\spike.py airllm    # AirLLM, layer shards saved under <spike>\split
```

`spike.py` loads the model snapshot, builds the chat prompt, runs `generate(max_new_tokens=32, do_sample=False)` and writes
`results/<mode>.json` (tokens, seconds, peak VRAM). AirLLM is called as
`AutoModel.from_pretrained(<snapshot>, layer_shards_saving_path=<spike>\split)` then `generate(ids, max_new_tokens=32,
do_sample=False, use_cache=True, return_dict_in_generate=True)`. It prints "built empty Qwen3ForCausalLM" when it is ready. To
test a bigger model, point the snapshot at another Hugging Face folder (read-only, no `.py` files, no `.bin`) **and check the
free space first**: the split copy is as big as the model.

The spike folder was left in place for the next agent. Remove it with
`Remove-Item -Recurse -Force <drive>:\ClaudeCodeTemp\ModelMistress\airllm-spike` when it is no longer needed (about 11 GB). Do
not install anything from it into the Forge environment.
