"""Checkpoint helpers: safetensors with a manifest of hashes, and a streaming sharded writer.

`save_checkpoint` / `load_checkpoint` / `verify_checkpoint` are for training state (small tensors held in memory).
`write_sharded` writes a model whose tensors are produced one at a time (the Dusty conversion makes tensors far
bigger than RAM would like to hold at once); tensors given as `None` are all zeros and are never materialised: on
a file system with sparse files (NTFS, ext4...) they take no disk space either.
"""

from __future__ import annotations

import hashlib
import json
import os
import struct
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable

MANIFEST = "manifest.json"
WEIGHTS = "weights.safetensors"

_DTYPE_BYTES = {"F64": 8, "F32": 4, "F16": 2, "BF16": 2, "I64": 8, "I32": 4, "I16": 2, "I8": 1, "U8": 1, "BOOL": 1}


def file_sha256(path: str | os.PathLike, chunk: int = 1 << 22) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while block := f.read(chunk):
            h.update(block)
    return h.hexdigest()


def _atomic_write(path: Path, data: bytes) -> None:
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_bytes(data)
    os.replace(tmp, path)


def save_checkpoint(directory: str | os.PathLike, tensors: dict, meta: dict | None = None) -> dict:
    """Writes `tensors` (name -> torch tensor, on any device) and a manifest {meta, sha256, tensors}. Atomic per
    file; returns the manifest."""
    from safetensors.torch import save_file

    d = Path(directory)
    d.mkdir(parents=True, exist_ok=True)
    weights = d / WEIGHTS
    tmp = d / (WEIGHTS + ".tmp")
    save_file({k: v.detach().cpu().contiguous() for k, v in tensors.items()}, str(tmp))
    os.replace(tmp, weights)
    manifest = {
        "format": "mm-forge-checkpoint/1",
        "meta": meta or {},
        "sha256": file_sha256(weights),
        "bytes": weights.stat().st_size,
        "tensors": {k: {"dtype": str(v.dtype).replace("torch.", ""), "shape": list(v.shape)} for k, v in tensors.items()},
    }
    _atomic_write(d / MANIFEST, json.dumps(manifest, indent=2, sort_keys=True).encode())
    return manifest


def verify_checkpoint(directory: str | os.PathLike) -> bool:
    d = Path(directory)
    try:
        manifest = json.loads((d / MANIFEST).read_text())
        return file_sha256(d / WEIGHTS) == manifest["sha256"]
    except (OSError, ValueError, KeyError):
        return False


def load_checkpoint(directory: str | os.PathLike, device: str = "cpu") -> tuple[dict, dict]:
    """(tensors, meta). Refuses a checkpoint whose hash does not match its manifest."""
    from safetensors.torch import load_file

    d = Path(directory)
    if not verify_checkpoint(d):
        raise ValueError(f"{d}: missing, or its hash does not match the manifest")
    manifest = json.loads((d / MANIFEST).read_text())
    return load_file(str(d / WEIGHTS), device=device), manifest.get("meta", {})


# ---- the streaming sharded writer -------------------------------------------------------------------------------


@dataclass
class Spec:
    """One tensor to write: its name, safetensors dtype ("BF16"), shape, and a function making it (None: zeros)."""

    name: str
    dtype: str
    shape: tuple[int, ...]
    make: Callable[[], "object"] | None = None

    @property
    def nbytes(self) -> int:
        n = _DTYPE_BYTES[self.dtype]
        for s in self.shape:
            n *= int(s)
        return n


def plan_shards(specs: Iterable[Spec], max_bytes: int) -> list[list[Spec]]:
    """Greedy, in order: a shard is closed when the next tensor would take it past `max_bytes` (a tensor bigger than
    the limit gets a shard of its own)."""
    shards: list[list[Spec]] = []
    cur: list[Spec] = []
    size = 0
    for s in specs:
        if cur and size + s.nbytes > max_bytes:
            shards.append(cur)
            cur, size = [], 0
        cur.append(s)
        size += s.nbytes
    if cur:
        shards.append(cur)
    return shards


def _mark_sparse(f) -> bool:
    """Asks the file system to leave unwritten ranges of this file unallocated. Linux and macOS do that anyway."""
    if sys.platform != "win32":
        return True
    try:
        import ctypes
        import msvcrt
        from ctypes import wintypes

        FSCTL_SET_SPARSE = 0x000900C4
        handle = msvcrt.get_osfhandle(f.fileno())
        returned = wintypes.DWORD()
        ok = ctypes.windll.kernel32.DeviceIoControl(
            wintypes.HANDLE(handle), FSCTL_SET_SPARSE, None, 0, None, 0, ctypes.byref(returned), None
        )
        return bool(ok)
    except Exception:
        return False


def _header(specs: list[Spec], metadata: dict | None) -> tuple[bytes, int]:
    header: dict = {}
    if metadata:
        header["__metadata__"] = {str(k): str(v) for k, v in metadata.items()}
    off = 0
    for s in specs:
        header[s.name] = {"dtype": s.dtype, "shape": [int(x) for x in s.shape], "data_offsets": [off, off + s.nbytes]}
        off += s.nbytes
    raw = json.dumps(header, separators=(",", ":")).encode()
    raw += b" " * (-len(raw) % 8)
    return struct.pack("<Q", len(raw)) + raw, off


def write_safetensors(path: str | os.PathLike, specs: list[Spec], metadata: dict | None = None,
                      on_tensor: Callable[[Spec], None] | None = None) -> int:
    """Writes one .safetensors file, one tensor at a time. Returns its size in bytes."""
    import torch

    head, data_len = _header(specs, metadata)
    path = Path(path)
    tmp = path.with_name(path.name + ".part")
    with open(tmp, "wb") as f:
        _mark_sparse(f)
        f.write(head)
        base = len(head)
        off = 0
        for s in specs:
            if s.make is not None:
                t = s.make()
                if tuple(t.shape) != tuple(s.shape):
                    raise ValueError(f"{s.name}: made shape {tuple(t.shape)}, declared {tuple(s.shape)}")
                t = t.detach().to("cpu").contiguous().reshape(-1)
                if t.numel() * t.element_size() != s.nbytes:
                    raise ValueError(f"{s.name}: made {t.numel() * t.element_size()} bytes, declared {s.nbytes}")
                f.seek(base + off)
                f.write(t.view(torch.uint8).numpy())
            off += s.nbytes
            if on_tensor:
                on_tensor(s)
        f.truncate(base + data_len)  # a trailing run of zero tensors is a hole until here
    os.replace(tmp, path)
    return path.stat().st_size


def write_sharded(directory: str | os.PathLike, specs: list[Spec], max_shard_bytes: int = 4 << 30,
                  metadata: dict | None = None, on_tensor: Callable[[Spec], None] | None = None) -> dict:
    """Shards `model-0000i-of-0000n.safetensors` plus `model.safetensors.index.json` (the Hugging Face layout).
    Returns {"files": [...], "total_size": n, "tensors": n}."""
    d = Path(directory)
    d.mkdir(parents=True, exist_ok=True)
    shards = plan_shards(specs, max_shard_bytes)
    weight_map: dict[str, str] = {}
    files = []
    total = 0
    for i, shard in enumerate(shards, 1):
        name = f"model-{i:05d}-of-{len(shards):05d}.safetensors"
        write_safetensors(d / name, shard, metadata={"format": "pt", **(metadata or {})}, on_tensor=on_tensor)
        files.append(name)
        for s in shard:
            weight_map[s.name] = name
            total += s.nbytes
    index = {"metadata": {"total_size": total}, "weight_map": weight_map}
    (d / "model.safetensors.index.json").write_text(json.dumps(index, indent=2))
    return {"files": files, "total_size": total, "tensors": len(weight_map)}
