"""The pipeline steps the hub can start: name -> module with `run(args, emitter) -> result dict`."""

from __future__ import annotations

import importlib

# name -> (module, one-line description). Modules are imported on use so that a step's dependencies (torch...) cost
# nothing for the others.
STEPS = {
    "env_info": ("mm_forge.envinfo", "versions of python, torch and the GPU runtime (no GPU work)"),
    "preflight": ("mm_forge.preflight", "prove that an optimizer step really runs on the GPU"),
    "dusty_convert": ("mm_forge.dusty", "Qwen3.5-4B -> Dusty (qwen35moe): HF checkpoint, GGUF, quantized GGUF"),
    "generate": ("mm_forge.generate", "generate text (tokens with logprobs) from a Hugging Face model, GPU or CPU"),
    "generate_serve": ("mm_forge.genserve", "the same as a server: one JSON request per stdin line, one result per request"),
    "dusty_equiv": ("mm_forge.equiv", "greedy-token equivalence of two GGUF models on a set of prompts"),
}


def load(name: str):
    if name not in STEPS:
        raise KeyError(f"no step {name!r}; known: {', '.join(sorted(STEPS))}")
    return importlib.import_module(STEPS[name][0])
