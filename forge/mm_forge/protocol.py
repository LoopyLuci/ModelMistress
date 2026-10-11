"""The worker protocol: one JSON object per line on stdout.

    {"type": "progress", "done": 3, "total": 10, "note": "layer 3 of 10"}
    {"type": "metric",   "name": "loss", "value": 1.25, "step": 40}
    {"type": "log",      "level": "info", "message": "..."}
    {"type": "artifact", "name": "dusty-bf16", "path": "...", "bytes": 123, "kind": "gguf"}
    {"type": "result",   "data": {...}}                   (at most one, the last thing a step says)
    {"type": "error",    "message": "...", "code": "..."} (the step failed)

stdout carries nothing else: while a step runs, `sys.stdout` is pointed at stderr so a stray `print` (or a library's)
cannot corrupt the stream; the hub keeps stderr as the run's log. Arguments arrive as one JSON object on stdin.
"""

from __future__ import annotations

import contextlib
import json
import math
import sys
import threading
from typing import Any, Iterable, Iterator, TextIO

TYPES = ("progress", "metric", "log", "artifact", "result", "error")


class Emitter:
    """Writes protocol lines to one stream, thread-safely, flushing after each."""

    def __init__(self, stream: TextIO):
        self._stream = stream
        self._lock = threading.Lock()

    def emit(self, type_: str, **fields: Any) -> dict:
        if type_ not in TYPES:
            raise ValueError(f"unknown event type {type_!r}")
        event = {"type": type_, **{k: _clean(v) for k, v in fields.items()}}
        line = json.dumps(event, ensure_ascii=True, separators=(",", ":"), allow_nan=False)
        with self._lock:
            self._stream.write(line + "\n")
            self._stream.flush()
        return event

    def progress(self, done: float, total: float, note: str = "") -> dict:
        return self.emit("progress", done=done, total=total, note=note)

    def metric(self, name: str, value: float, step: int | None = None) -> dict:
        fields: dict[str, Any] = {"name": name, "value": value}
        if step is not None:
            fields["step"] = int(step)
        return self.emit("metric", **fields)

    def log(self, message: str, level: str = "info") -> dict:
        return self.emit("log", level=level, message=str(message))

    def artifact(self, name: str, path: str, bytes: int | None = None, kind: str = "file") -> dict:
        fields: dict[str, Any] = {"name": name, "path": str(path), "kind": kind}
        if bytes is not None:
            fields["bytes"] = int(bytes)
        return self.emit("artifact", **fields)

    def result(self, data: Any) -> dict:
        return self.emit("result", data=data)

    def error(self, message: str, code: str = "failed") -> dict:
        return self.emit("error", message=str(message), code=code)


def _clean(v: Any) -> Any:
    """JSON-safe: NaN and infinities become strings (JSON has none), paths and numpy/torch scalars plain values."""
    if isinstance(v, float):
        if math.isnan(v):
            return "nan"
        if math.isinf(v):
            return "inf" if v > 0 else "-inf"
        return v
    if isinstance(v, dict):
        return {str(k): _clean(x) for k, x in v.items()}
    if isinstance(v, (list, tuple, set)):
        return [_clean(x) for x in v]
    if v is None or isinstance(v, (bool, int, str)):
        return v
    item = getattr(v, "item", None)
    if callable(item):  # numpy / torch scalars
        try:
            return _clean(item())
        except (ValueError, RuntimeError):
            pass
    return str(v)


_emitter: Emitter | None = None


def emitter() -> Emitter:
    """The process-wide emitter (stdout as it was before `capture_stdout`)."""
    global _emitter
    if _emitter is None:
        _emitter = Emitter(sys.stdout)
    return _emitter


@contextlib.contextmanager
def capture_stdout() -> Iterator[Emitter]:
    """Keeps the real stdout for protocol lines and sends everything else printed to stderr."""
    global _emitter
    real = sys.stdout
    _emitter = Emitter(real)
    sys.stdout = sys.stderr
    try:
        yield _emitter
    finally:
        sys.stdout = real


def parse_line(line: str) -> dict | None:
    """One protocol line as a dict, or None if it is not one (blank, not JSON, no known type)."""
    line = line.strip()
    if not line.startswith("{"):
        return None
    try:
        v = json.loads(line)
    except ValueError:
        return None
    if isinstance(v, dict) and v.get("type") in TYPES:
        return v
    return None


def read_events(lines: Iterable[str]) -> list[dict]:
    return [e for e in (parse_line(l) for l in lines) if e is not None]


def read_args(stream: TextIO | None = None, first_line: bool = False) -> dict:
    """The step's arguments: the JSON object on stdin (empty stdin: {}). A step that keeps reading requests from
    stdin takes its arguments from the first line only."""
    s = stream or sys.stdin
    text = (s.readline() if first_line else s.read()).strip()
    if not text:
        return {}
    v = json.loads(text)
    if not isinstance(v, dict):
        raise ValueError("a step's arguments must be a JSON object")
    return v
