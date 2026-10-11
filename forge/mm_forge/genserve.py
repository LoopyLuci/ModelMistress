"""generate_serve: the long-running generation server (protocol in `generate`)."""

from __future__ import annotations

from .generate import serve as run  # noqa: F401

INTERACTIVE = True  # the first stdin line holds the arguments; the rest are requests
