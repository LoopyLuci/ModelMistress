"""The pipeline steps the hub can start: name -> module with `run(args, emitter) -> result dict`."""

from __future__ import annotations

import importlib
import types

# name -> (module, one-line description). Modules are imported on use so that a step's dependencies (torch...) cost
# nothing for the others.
STEPS = {
    "env_info": ("mm_forge.envinfo", "versions of python, torch and the GPU runtime (no GPU work)"),
    "preflight": ("mm_forge.preflight", "prove that an optimizer step really runs on the GPU"),
    "dusty_convert": ("mm_forge.dusty", "Qwen3.5-4B -> Dusty (qwen35moe): HF checkpoint, GGUF, quantized GGUF"),
    "generate": ("mm_forge.generate", "generate text (tokens with logprobs) from a Hugging Face model, GPU or CPU"),
    "generate_serve": ("mm_forge.genserve", "the same as a server: one JSON request per stdin line, one result per request"),
    "dusty_equiv": ("mm_forge.equiv", "greedy-token equivalence of two GGUF models on a set of prompts"),
    "lora_train": ("mm_forge.train", "LoRA fine-tune of a Hugging Face model (resumable, VRAM-checked)"),
    "qlora_train": ("mm_forge.train:run_qlora", "4-bit QLoRA fine-tune (needs bitsandbytes; else 16-bit LoRA)"),
    "merge_lora": ("mm_forge.export:run_merge", "merge a LoRA adapter into its base model (safetensors)"),
    "merged_to_gguf": ("mm_forge.export:run_merged_to_gguf", "Hugging Face folder -> GGUF (+ quantized types)"),
    "lora_to_gguf": ("mm_forge.export:run_lora_to_gguf", "merge a LoRA adapter, then convert to GGUF"),
}


def load(name: str):
    if name not in STEPS:
        raise KeyError(f"no step {name!r}; known: {', '.join(sorted(STEPS))}")
    module, _, func = STEPS[name][0].partition(":")
    mod = importlib.import_module(module)
    if not func:
        return mod
    # "module:function": several steps may share a module; the function is the step's `run`
    return types.SimpleNamespace(run=getattr(mod, func), INTERACTIVE=False)
