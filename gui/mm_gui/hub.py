"""The hub's HTTP API as the desktop window uses it. Standard library only: the window adds no package to the hub,
and the hub keeps no Python dependency at all."""

from __future__ import annotations

import json
import os
import sys
import threading
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from typing import Any, Callable, Iterable, Iterator
from urllib.parse import quote

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


class _Counted:
    """A file as it is read to be sent, reporting how many bytes have been read so far."""

    def __init__(self, f, on_bytes: Callable[[int], None]):
        self._f = f
        self._on_bytes = on_bytes
        self.sent = 0

    def read(self, n: int = -1) -> bytes:
        chunk = self._f.read(n)
        self.sent += len(chunk)
        self._on_bytes(self.sent)
        return chunk


def _multipart(field: str, filename: str, content_type: str, data: bytes) -> tuple[bytes, str]:
    """One file as a multipart/form-data body, with the Content-Type header that goes with it."""
    boundary = "----mm" + uuid.uuid4().hex
    head = (
        f'--{boundary}\r\nContent-Disposition: form-data; name="{field}"; filename="{filename}"\r\n'
        f"Content-Type: {content_type}\r\n\r\n"
    ).encode("utf-8")
    return head + data + f"\r\n--{boundary}--\r\n".encode("ascii"), f"multipart/form-data; boundary={boundary}"


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

    # ---- the model router: the model `auto` and the router.* operations (src/misty/auto.rs) -----------------------

    def router_status(self) -> dict:
        """The roles (fast, main, deep, and the retrieval models), the model each has and whether it can answer."""
        return self.call("router.status")

    def router_status_or_none(self) -> dict | None:
        """router.status, or None on a hub that has no router (an older one, or one without Misty)."""
        try:
            status = self.router_status()
        except HubError:
            return None
        return status if isinstance(status, dict) and status.get("enabled") is not False else None

    def router_pick(self, messages: list[dict]) -> dict:
        """Which model the router would send this conversation to. Nothing is sent to it."""
        return self.call("router.pick", {"messages": messages})

    def set_role(self, role: str, model: str | None) -> Any:
        """Choose the model for a role, or None to go back to the router's own choice (a JSON merge patch)."""
        return self.call("misty.config.set", {"patch": {"roles": {role: model}}})

    def chat_models(self) -> list[dict]:
        """The models a chat can choose from: the router first when this hub has one, then the models."""
        models = self.models()
        status = self.router_status_or_none()
        if status is None:
            return models
        ready = any(r.get("status") == "ready" for r in status.get("roles") or [])
        return [{"id": "auto", "source": "router", "router": True, "ready": ready}, *models]

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
        options: dict | None = None,
    ) -> str:
        """A streamed answer (POST /v1/chat/completions, stream on). `on_text` gets each piece as it arrives and the
        whole text comes back. Setting `cancel` stops reading before the next piece. `options` are sent as they are
        (top_k, seed, and the rest of the sampling settings)."""
        body = {"model": model, "messages": messages, "stream": True, **(options or {})}
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

    # ---- files, speech and the benchmark (the hub's own routes; see src/hub.rs) ------------------------------------

    def document_text(self, pdf: bytes) -> tuple[str, bool]:
        """The text of a PDF, and whether it was cut short (POST /v1/documents/text). No OCR: a scan has no text."""
        doc = self._json(self._open("POST", "/v1/documents/text", raw=pdf, headers={"Content-Type": "application/pdf"}, timeout=300))
        return str(doc.get("text") or ""), bool(doc.get("truncated"))

    def import_model(self, path: Path, on_bytes: Callable[[int], None] | None = None) -> dict:
        """Copy a .gguf file into the hub's models folder (POST /v1/models/import). `on_bytes` hears the bytes sent so far.
        Returns {"name", "bytes", "path"}. The file is streamed, so a multi-gigabyte model is not read into memory."""
        size = path.stat().st_size
        headers = {
            "Content-Type": "application/octet-stream",
            "Content-Length": str(size),
            "x-file-name": quote(path.name, safe=""),
        }
        with open(path, "rb") as f:
            body = _Counted(f, on_bytes or (lambda _n: None))
            return self._json(self._open("POST", "/v1/models/import", raw=body, headers=headers, timeout=7200))

    def transcribe(self, wav: bytes) -> str:
        """The words in a WAV clip, by the hub's speech to text (POST /v1/audio/transcriptions)."""
        data, content_type = _multipart("file", "speech.wav", "audio/wav", wav)
        doc = self._json(self._open("POST", "/v1/audio/transcriptions", raw=data, headers={"Content-Type": content_type}, timeout=600))
        return str(doc.get("text") or "").strip()

    def speak(self, text: str) -> bytes:
        """`text` spoken by the hub's text to speech, as WAV bytes (POST /v1/audio/speech)."""
        body = {"model": "tts-1", "input": text, "response_format": "wav"}
        with self._open("POST", "/v1/audio/speech", body, timeout=600) as resp:
            return resp.read()

    def bench(self, seconds: float = 10, tokens: int = 96) -> dict:
        """Benchmark this machine and each installed chat model (system.bench). Takes minutes on a CPU."""
        return self.call("system.bench", {"seconds": seconds, "tokens": tokens}, timeout=1800)

    # ---- Misty, the agent inside the hub (misty.* operations; docs/misty/DESIGN.md) ----------------------------------

    def misty_status(self) -> dict:
        return self.call("misty.status")

    def misty_start(self, args: dict) -> dict:
        """Start a run and return at once: {"run": id, "session": id}."""
        return self.call("misty.run.start", args)

    def misty_page(self, run: str, start: int = 0, wait_ms: int = 0) -> dict:
        """The events of a run after the first `start`, waiting up to `wait_ms` for new ones."""
        return self.call("misty.run.get", {"run": run, "from": start, "wait_ms": wait_ms}, timeout=wait_ms / 1000 + 15)

    def misty_cancel(self, run: str) -> Any:
        return self.call("misty.run.cancel", {"run": run})

    def misty_approve(self, approval: str, allow: bool) -> Any:
        return self.call("misty.approve", {"approval": approval, "allow": allow})

    def misty_rate(self, run: str, label: str, correction: str | None = None) -> dict:
        """The owner's verdict on a reply: good, bad, no_tools, needs_tools, or corrected with the right answer."""
        args: dict = {"run": run, "label": label}
        if correction:
            args["correction"] = correction
        return self.call("misty.rate", args)

    def misty_teach(self, question: str, answer: str, correction: str) -> dict:
        """A correction given by voice on a reply that was not a Misty run (the Live conversation)."""
        return self.call("misty.teach", {"question": question, "answer": answer, "correction": correction})

    def misty_feedback(self, limit: int = 100) -> dict:
        """The owner's verdicts, newest first, with the counts: {"verdicts": [...], "counts": {...}}."""
        return self.call("misty.feedback.list", {"limit": limit})

    def misty_forget(self, verdict: int | None = None, everything: bool = False) -> dict:
        """Remove one verdict by its id, or all of them."""
        return self.call("misty.feedback.forget", {"all": True} if everything else {"id": verdict})

    def misty_export(self) -> dict:
        """The good replies and the corrections as chat-format training data: {"count", "jsonl"}."""
        return self.call("misty.training.export")

    def misty_run(self, args: dict, on_page: Callable[[dict], None], cancel: threading.Event | None = None) -> dict:
        """Start a run and follow it to its end. `on_page` gets {"start": the start reply} first, then each page of
        events ({"events", "pending", "done", ...}). Setting `cancel` asks the hub to stop the run; the last events
        (the stop itself) still arrive. Returns the last page."""
        started = self.misty_start(args)
        on_page({"start": started})
        run, start, stopped, page = started["run"], 0, False, {}
        while True:
            if cancel is not None and cancel.is_set() and not stopped:
                stopped = True
                self.misty_cancel(run)
            page = self.misty_page(run, start, 500 if stopped else 2000)  # short waits keep Stop quick to act on
            start = page.get("next", start)
            on_page(page)
            if page.get("done"):
                return page

    def _open(self, method: str, path: str, body: Any = None, timeout: float | None = None, *,
              raw: Any = None, headers: dict | None = None):
        """One request. `body` goes as JSON; `raw` goes as it is: bytes, or a file that is read as it is sent."""
        head = {"Accept": "application/json", **(headers or {})}
        data = raw
        if raw is None and body is not None:
            data = json.dumps(body).encode("utf-8")
            head["Content-Type"] = "application/json"
        if path != "/v1/health":
            head["Authorization"] = f"Bearer {self.token}"
        request = urllib.request.Request(self.url + path, data=data, method=method, headers=head)
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
