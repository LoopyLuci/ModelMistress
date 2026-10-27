"""What the window shows of the hub's data: which models a chat can use, how a model is named, and sizes and times."""

from __future__ import annotations

import re

_NOT_CHAT_ARCHITECTURES = {"clip", "sd", "sd3", "flux", "whisper", "wavtokenizer-dec"}
_NOT_CHAT_NAME = re.compile(r"embed|rerank", re.IGNORECASE)


def is_chat_model(model: dict) -> bool:
    """A model a chat can talk to: not speech, vision, image or embedding models (the dashboard's rule too)."""
    return (
        model.get("source") not in ("whisper", "vision")
        and not model.get("kind")
        and model.get("architecture") not in _NOT_CHAT_ARCHITECTURES
        and not _NOT_CHAT_NAME.search(str(model.get("id", "")))
    )


def model_label(model: dict) -> str:
    size = f" ({model['size_gb']} GB)" if model.get("size_gb") else ""
    loaded = " - loaded" if model.get("loaded") else ""
    return f"{model.get('id', '?')}{size}{loaded}"


def fmt_mb(mb: float | None) -> str:
    if mb is None:
        return "-"
    return f"{mb / 1024:.1f} GB" if mb >= 1024 else f"{mb:.0f} MB"


def fmt_uptime(seconds: int | None) -> str:
    if seconds is None:
        return "-"
    minutes, secs = divmod(int(seconds), 60)
    hours, minutes = divmod(minutes, 60)
    if hours:
        return f"{hours} h {minutes:02d} min"
    if minutes:
        return f"{minutes} min {secs:02d} s"
    return f"{secs} s"
