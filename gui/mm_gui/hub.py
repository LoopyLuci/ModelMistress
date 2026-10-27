"""The hub's HTTP API as the desktop window uses it. Standard library only: the window adds no package to the hub,
and the hub keeps no Python dependency at all."""

from __future__ import annotations

import json
import os
import sys
import threading
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any, Callable, Iterable, Iterator

# The hub is on this machine: never send its requests through a proxy from the environment.
_OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


class HubError(Exception):
    """The hub refused, or cannot be reached. `status` is the HTTP status, 0 when there was no answer."""

    def __init__(self, message: str, status: int = 0):
        super().__init__(message)
        self.status = status


def default_home() -> Path:
    """The hub's home folder as the hub picks it: MM_HOME when set, else the platform's standard folder."""
    env = os.environ.get("MM_HOME", "").strip()
    if env:
        return Path(env)
    if sys.platform == "win32":
        base = os.environ.get("LOCALAPPDATA")
    elif sys.platform == "darwin":
        base = _under_home("Library/Application Support")
    else:
        base = os.environ.get("XDG_DATA_HOME") or _under_home(".local/share")
    return Path(base or ".") / "ModelMistress"


def _under_home(rel: str) -> str | None:
    home = os.environ.get("HOME")
    return str(Path(home) / rel) if home else None


def sse_events(lines: Iterable[bytes]) -> Iterator[str]:
    """The data of each server-sent event, in order. A blank line ends an event."""
    data: list[str] = []
    for raw in lines:
        line = raw.decode("utf-8", "replace").rstrip("\r\n")
        if not line:
            if data:
                yield "\n".join(data)
                data = []
        elif line.startswith("data:"):
            value = line[5:]
            data.append(value[1:] if value.startswith(" ") else value)
    if data:
        yield "\n".join(data)


def _http_error(e: urllib.error.HTTPError) -> HubError:
    message = f"HTTP {e.code}"
    try:
        message = json.loads(e.read().decode("utf-8", "replace"))["error"]["message"] or message
    except (ValueError, KeyError, TypeError, AttributeError):
        pass
    return HubError(message, e.code)


class Hub:
    """One running hub: its address and its token. Each call is a plain HTTP request made on the caller's thread."""

    def __init__(self, url: str, token: str, timeout: float = 60.0):
        self.url = url.rstrip("/")
        self.token = token
        self.timeout = timeout

    @classmethod
    def find(cls, home: Path | None = None, timeout: float = 5.0) -> Hub:
        """The hub that <home>/control.json describes, if it answers; HubError otherwise."""
        path = (home or default_home()) / "control.json"
        try:
            doc = json.loads(path.read_text(encoding="utf-8"))
        except OSError as e:
            raise HubError(f"ModelMistress is not running ({path} is missing)") from e
        except ValueError as e:
            raise HubError(f"{path} is not a control file") from e
        if not isinstance(doc, dict) or not isinstance(doc.get("url"), str) or not isinstance(doc.get("token"), str):
            raise HubError(f"{path} has no url and token")
        hub = cls(doc["url"], doc["token"], timeout)
        hub.health()
        return hub

    def health(self) -> dict:
        return self._json(self._open("GET", "/v1/health"))

    def call(self, op: str, args: dict | None = None, timeout: float | None = None) -> Any:
        """Run one hub operation (POST /v1/call/{op}) and return its result."""
        doc = self._json(self._open("POST", f"/v1/call/{op}", args or {}, timeout))
        return doc.get("result", doc) if isinstance(doc, dict) else doc

    def status(self) -> dict:
        return self.call("service.status")

    def models(self, refresh: bool = False) -> list[dict]:
        return self.call("model.list", {"include_ollama": True, "refresh": refresh}) or []

    def loaded(self) -> list[dict]:
        return self.call("model.loaded") or []

    def load(self, model: str) -> dict:
        # Loading a large model can take minutes before the hub answers, so the wait is longer than usual.
        return self.call("model.load", {"model": model}, timeout=900)

    def unload(self, model: str) -> dict:
        return self.call("model.unload", {"model": model})

    def config(self) -> dict:
        return self.call("config.get")

    def set_config(self, changes: dict) -> Any:
        return self.call("config.set", {"settings": changes})

    def stop(self) -> None:
        """Ask the hub to unload its models and stop."""
        self._open("POST", "/v1/service/stop", {}).close()

    def chat_stream(
        self,
        model: str,
        messages: list[dict],
        on_text: Callable[[str], None],
        cancel: threading.Event | None = None,
    ) -> str:
        """A streamed answer (POST /v1/chat/completions, stream on). `on_text` gets each piece as it arrives and the
        whole text comes back. Setting `cancel` stops reading before the next piece."""
        body = {"model": model, "messages": messages, "stream": True}
        pieces: list[str] = []
        with self._open("POST", "/v1/chat/completions", body, timeout=600) as resp:
            for payload in sse_events(resp):
                if payload.strip() == "[DONE]" or (cancel is not None and cancel.is_set()):
                    break
                try:
                    text = json.loads(payload)["choices"][0]["delta"].get("content") or ""
                except (ValueError, KeyError, IndexError, TypeError, AttributeError):
                    continue
                if text:
                    pieces.append(text)
                    on_text(text)
        return "".join(pieces)

    def _open(self, method: str, path: str, body: Any = None, timeout: float | None = None):
        headers = {"Accept": "application/json"}
        data = None
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if path != "/v1/health":
            headers["Authorization"] = f"Bearer {self.token}"
        request = urllib.request.Request(self.url + path, data=data, method=method, headers=headers)
        try:
            return _OPENER.open(request, timeout=timeout or self.timeout)
        except urllib.error.HTTPError as e:
            raise _http_error(e) from None
        except (urllib.error.URLError, OSError) as e:
            raise HubError(f"the hub at {self.url} is not answering: {getattr(e, 'reason', e)}") from e

    @staticmethod
    def _json(resp) -> Any:
        with resp:
            return json.loads(resp.read().decode("utf-8"))
