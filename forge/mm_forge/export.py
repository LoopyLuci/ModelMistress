"""Getting a trained model out of the Forge so the hub can serve it: steps `merge_lora`, `lora_to_gguf`,
`merged_to_gguf`.

* `merge_lora`: base model + LoRA adapter -> one Hugging Face folder of safetensors (`base_model`, `adapter`,
  `out_dir`, `dtype` auto|bfloat16|float16|float32, `device` cpu|gpu, `max_shard_size` "4GB").
* `merged_to_gguf`: a Hugging Face folder -> GGUF with llama.cpp's own `convert_hf_to_gguf.py` (the Dusty path),
  then optional `quantize` types through `llama-quantize` (`model_dir`, `out_dir`, `name`, `outtypes` ["bf16"],
  `quantize` ["Q4_K_M"], `quantize_exe`, `convert_script`). The 16-bit GGUF is kept.
* `lora_to_gguf`: the two in a row (`base_model`, `adapter`, `out_dir`, plus the GGUF arguments above); the merged
  folder stays in `<out_dir>/merged` unless `keep_merged` is false.

The resulting `.gguf` files are what `model-mistress` serves (import them with the hub's model operations).
"""

from __future__ import annotations

import os
import shutil
import time
from pathlib import Path

from .checkpoint import file_sha256
from .protocol import Emitter

INSTALL_HINT = "run `forge install` (it installs torch, transformers and peft into the Forge environment)"


def _need(args: dict, *names: str) -> list:
    missing = [n for n in names if not args.get(n)]
    if missing:
        raise ValueError(f"give {', '.join(missing)}")
    return [args[n] for n in names]


def merge_adapter(base_model: str, adapter: str, out_dir: str, em: Emitter, dtype: str = "auto",
                  device: str = "cpu", max_shard_size: str = "4GB") -> dict:
    try:
        import torch
        from peft import PeftModel
        from transformers import AutoModelForCausalLM, AutoTokenizer
    except ImportError as e:
        raise RuntimeError(f"{e.name or 'a training package'} is not installed: {INSTALL_HINT}") from e
    if not Path(adapter).is_dir():
        raise FileNotFoundError(f"adapter folder not found: {adapter}")
    if dtype == "auto":
        dtype = "bfloat16"
    dev = "cuda" if device in ("gpu", "cuda") and torch.cuda.is_available() else "cpu"
    em.log(f"merging {adapter} into {base_model} ({dtype}, {dev})")
    model = AutoModelForCausalLM.from_pretrained(base_model, torch_dtype=getattr(torch, dtype)).to(dev)
    model = PeftModel.from_pretrained(model, adapter)
    merged = model.merge_and_unload()
    merged.config.use_cache = True
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    merged.save_pretrained(out, safe_serialization=True, max_shard_size=max_shard_size)
    tok_src = adapter if (Path(adapter) / "tokenizer_config.json").is_file() else base_model
    AutoTokenizer.from_pretrained(tok_src).save_pretrained(out)
    files = sorted(out.glob("*.safetensors"))
    em.artifact("merged", str(out), bytes=sum(f.stat().st_size for f in files), kind="hf-checkpoint")
    return {"dir": str(out), "files": [f.name for f in files], "dtype": dtype,
            "bytes": sum(f.stat().st_size for f in files)}


def run_merge(args: dict, em: Emitter) -> dict:
    base, adapter, out = _need(args, "base_model", "adapter", "out_dir")
    return merge_adapter(base, adapter, out, em, args.get("dtype", "auto"), args.get("device", "cpu"),
                         args.get("max_shard_size", "4GB"))


def gguf_plan(args: dict, name: str, out_dir: Path) -> list[tuple[str, Path]]:
    """(kind, path) of every GGUF file the arguments ask for, in order: the 16-bit ones, then the quantized."""
    plan = [(ot, out_dir / f"{name}-{ot.upper()}.gguf") for ot in args.get("outtypes", ["bf16"])]
    plan += [(q.lower(), out_dir / f"{name}-{q.upper()}.gguf") for q in args.get("quantize", [])]
    return plan


def to_gguf(model_dir: Path, out_dir: Path, name: str, args: dict, em: Emitter) -> dict:
    from . import dusty

    outtypes = list(args.get("outtypes", ["bf16"]))
    quants = list(args.get("quantize", []))
    exe = args.get("quantize_exe") or os.environ.get("MM_FORGE_LLAMA_QUANTIZE")
    if quants and not exe:
        raise ValueError("quantize needs quantize_exe (llama-quantize) or MM_FORGE_LLAMA_QUANTIZE")
    if quants and not any(o in ("f16", "bf16") for o in outtypes):
        raise ValueError("quantize needs a 16-bit GGUF: put f16 or bf16 in outtypes")
    if not (model_dir / "config.json").is_file():
        raise FileNotFoundError(f"{model_dir}: not a Hugging Face model folder (no config.json)")
    outputs: dict = {}
    for ot in outtypes:
        path = out_dir / f"{name}-{ot.upper()}.gguf"
        dusty.run_gguf(model_dir, path, ot, args, em, virtual=False, model_name=name)
        outputs[ot] = {"path": str(path), "bytes": path.stat().st_size}
        em.artifact(f"{name}-{ot}", str(path), bytes=path.stat().st_size, kind="gguf")
    for q in quants:
        src = next(outputs[k]["path"] for k in ("f16", "bf16") if k in outputs)
        path = out_dir / f"{name}-{q.upper()}.gguf"
        dusty.quantize(exe, Path(src), path, q, em)
        outputs[q.lower()] = {"path": str(path), "bytes": path.stat().st_size}
        em.artifact(f"{name}-{q.lower()}", str(path), bytes=path.stat().st_size, kind="gguf")
    if args.get("sha256", True):
        for o in outputs.values():
            o["sha256"] = file_sha256(o["path"])
    return outputs


def run_merged_to_gguf(args: dict, em: Emitter) -> dict:
    (model_dir,) = _need(args, "model_dir")
    model_dir = Path(model_dir)
    out_dir = Path(args.get("out_dir") or model_dir.parent / "gguf")
    name = args.get("name") or model_dir.parent.name or model_dir.name
    t0 = time.time()
    return {"name": name, "outputs": to_gguf(model_dir, out_dir, name, args, em),
            "elapsed_s": round(time.time() - t0, 1)}


def run_lora_to_gguf(args: dict, em: Emitter) -> dict:
    base, adapter, out = _need(args, "base_model", "adapter", "out_dir")
    out = Path(out)
    t0 = time.time()
    merged = out / "merged"
    result = {"merged": merge_adapter(base, adapter, str(merged), em, args.get("dtype", "auto"),
                                      args.get("device", "cpu"), args.get("max_shard_size", "4GB"))}
    name = args.get("name") or out.name
    result["name"] = name
    result["outputs"] = to_gguf(merged, out / "gguf", name, args, em)
    if not args.get("keep_merged", True):
        shutil.rmtree(merged, ignore_errors=True)
    result["elapsed_s"] = round(time.time() - t0, 1)
    return result
