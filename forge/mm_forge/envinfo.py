"""env_info: what is installed, without touching the GPU."""

from __future__ import annotations

import importlib.metadata as md
import platform
import sys

from .protocol import Emitter


def run(args: dict, em: Emitter) -> dict:
    out = {"python": sys.version.split()[0], "executable": sys.executable, "platform": platform.platform(), "packages": {}}
    for pkg in ("torch", "numpy", "safetensors", "transformers", "huggingface_hub", "sentencepiece", "rocm-sdk-core"):
        try:
            out["packages"][pkg] = md.version(pkg)
        except md.PackageNotFoundError:
            out["packages"][pkg] = None
    try:
        import torch

        out["torch"] = {"version": torch.__version__, "hip": getattr(torch.version, "hip", None),
                        "gpu_available": bool(torch.cuda.is_available())}
    except Exception as e:  # noqa: BLE001
        out["torch"] = {"error": str(e)}
    return out
