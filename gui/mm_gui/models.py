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


ROUTER = "auto"  # the model name that asks the hub's router (a chat sent to it goes to the model that suits each message)

BRAIN_ROLES = ("fast", "main", "deep")
ROLES = (*BRAIN_ROLES, "embed", "rerank")
ROLE_HELP = {
    "fast": "short, simple turns",
    "main": "most requests: writing, code, documents",
    "deep": "hard reasoning and long inputs",
    "embed": "turns text into vectors for searching documents",
    "rerank": "orders search results (optional)",
}
STATUS_TEXT = {
    "ready": "ready",
    "missing": "no model",
    "unfit": "does not fit the free memory",
    "unavailable": "needs an engine that is not installed",
    "not_allowed": "not allowed",
}
_ROLE_TAG = re.compile(r"@(fast|main|deep)", re.IGNORECASE)
_EMBED_LIKE = re.compile(r"embed|bge|e5-|gte-|nomic", re.IGNORECASE)


def without_role_tags(text: str) -> str:
    """The message without `@fast`, `@main` and `@deep` (the words that ask the router for a role). Every other character
    stays as it was; a message with no tag comes back unchanged."""
    out: list[str] = []
    skip = removed = False
    for part in re.split(r"(\s)", text):
        if skip and part.isspace():
            skip = False  # the space that followed the tag goes with it
            continue
        skip = False
        if _ROLE_TAG.fullmatch(part):
            removed = skip = True
            continue
        out.append(part)
    return "".join(out).strip() if removed else text


def without_tags_in_last_user_message(messages: list[dict]) -> list[dict]:
    """The conversation as it goes to the model the router picked: the role tags of the last user message are not sent.
    A message with pictures has a list of parts; only its text parts are changed, the pictures go as they are."""
    last = max((i for i, m in enumerate(messages) if m.get("role") == "user"), default=None)
    if last is None:
        return messages
    out = list(messages)
    content = out[last].get("content", "")
    if isinstance(content, list):
        parts = []
        for part in content:
            if isinstance(part, dict) and part.get("type") == "text":
                part = {**part, "text": without_role_tags(str(part.get("text", "")))}
            parts.append(part)
        new_content: str | list = parts
    else:
        new_content = without_role_tags(str(content))
    out[last] = {**out[last], "content": new_content}
    return out


def suits_role(role: str, model_id: str, chat: bool) -> bool:
    """May this model fill the role? Language models the three brains; embedders and rerankers by name."""
    embed_like = bool(_EMBED_LIKE.search(model_id))
    if role == "embed":
        return embed_like and "rerank" not in model_id.lower()
    if role == "rerank":
        return "rerank" in model_id.lower()
    return chat and not embed_like


def roles_of(status: dict | None, model_id: str) -> list[str]:
    """The router's roles that name this model."""
    if not status:
        return []
    rows = [*(status.get("roles") or []), *(status.get("support") or [])]
    return [str(r.get("role")) for r in rows if r.get("model") == model_id]


def model_label(model: dict) -> str:
    if model.get("router"):
        return "Auto: the router picks the model for each message"
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
