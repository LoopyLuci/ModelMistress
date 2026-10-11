"""Deterministic seeding: one base seed, derived sub-seeds, and the framework switches."""

from __future__ import annotations

import hashlib
import os
import random


def derive_seed(base: int, *keys: object) -> int:
    """A stable 63-bit seed from a base seed and names (layer index, purpose...): the same inputs give the same
    seed on every machine, and different keys give unrelated ones."""
    h = hashlib.sha256(repr((int(base),) + tuple(str(k) for k in keys)).encode()).digest()
    return int.from_bytes(h[:8], "little") & 0x7FFF_FFFF_FFFF_FFFF


def seed_everything(seed: int, deterministic: bool = True) -> dict:
    """Seeds python, numpy and torch (CPU and GPU). Returns what was done."""
    seed = int(seed)
    random.seed(seed)
    os.environ.setdefault("PYTHONHASHSEED", str(seed))
    done = {"seed": seed, "python": True, "numpy": False, "torch": False, "cuda": False, "deterministic": False}
    try:
        import numpy as np

        np.random.seed(seed % (2**32))
        done["numpy"] = True
    except ImportError:
        pass
    try:
        import torch
    except ImportError:
        return done
    torch.manual_seed(seed)
    done["torch"] = True
    if torch.cuda.is_available():
        torch.cuda.manual_seed_all(seed)
        done["cuda"] = True
    if deterministic:
        try:
            torch.use_deterministic_algorithms(True, warn_only=True)
            done["deterministic"] = True
        except Exception:  # not every build supports it
            pass
    return done


def generator(seed: int, device: str = "cpu"):
    """A torch.Generator seeded with `seed` (for initialisations that must not depend on global state)."""
    import torch

    g = torch.Generator(device=device)
    g.manual_seed(int(seed) & 0x7FFF_FFFF_FFFF_FFFF)
    return g
