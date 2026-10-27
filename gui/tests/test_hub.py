"""The hub client, against a small local stand-in for the hub's HTTP API (no Qt needed)."""

from __future__ import annotations

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

from mm_gui.hub import Hub, HubError, default_home, sse_events

TOKEN = "secret"


class _StandIn(BaseHTTPRequestHandler):
    def log_message(self, *_args) -> None:  # keep test output quiet
        pass

    def _send(self, status: int, body: bytes, content_type: str = "application/json") -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _json(self, status: int, doc: object) -> None:
        self._send(status, json.dumps(doc).encode("utf-8"))

    def do_GET(self) -> None:
        if self.path == "/v1/health":
            self._json(200, {"ok": True, "version": "test"})
        else:
            self._json(404, {"error": {"message": "no route"}})

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        if self.headers.get("Authorization") != f"Bearer {TOKEN}":
            self._json(401, {"error": {"message": "token refused"}})
        elif self.path == "/v1/chat/completions":
            events = [{"choices": [{"delta": {"content": piece}}]} for piece in ("Hel", "lo")]
            text = "".join(f"data: {json.dumps(e)}\n\n" for e in events) + "data: [DONE]\n\n"
            self._send(200, text.encode("utf-8"), "text/event-stream")
        elif self.path == "/v1/call/service.status":
            self._json(200, {"result": {"version": "test", "echo": body}})
        elif self.path.startswith("/v1/call/"):
            self._json(404, {"error": {"message": f"no operation {self.path.rsplit('/', 1)[-1]}"}})
        else:
            self._json(404, {"error": {"message": "no route"}})


@pytest.fixture
def server():
    srv = ThreadingHTTPServer(("127.0.0.1", 0), _StandIn)
    thread = threading.Thread(target=srv.serve_forever, daemon=True)
    thread.start()
    yield srv
    srv.shutdown()
    srv.server_close()


def _url(srv: ThreadingHTTPServer) -> str:
    return f"http://127.0.0.1:{srv.server_address[1]}"


def test_sse_events_yield_each_data_block_in_order():
    lines = [b'data: {"a": 1}\n', b"\n", b"event: x\n", b"data: [DONE]\n", b"\n"]
    assert list(sse_events(lines)) == ['{"a": 1}', "[DONE]"]


def test_sse_events_join_multi_line_data_and_keep_a_last_unterminated_event():
    assert list(sse_events([b"data: one\n", b"data: two\n", b"\n", b"data: three\n"])) == ["one\ntwo", "three"]


def test_find_reads_the_control_file_and_checks_the_hub(tmp_path, server):
    (tmp_path / "control.json").write_text(json.dumps({"url": _url(server), "token": TOKEN}), encoding="utf-8")

    hub = Hub.find(tmp_path)

    assert hub.status()["version"] == "test"


def test_find_says_when_no_hub_is_running(tmp_path):
    with pytest.raises(HubError, match="not running"):
        Hub.find(tmp_path)


def test_a_refused_token_is_reported_with_its_status(server):
    with pytest.raises(HubError, match="token refused") as info:
        Hub(_url(server), "wrong").call("service.status")

    assert info.value.status == 401


def test_an_unknown_operation_is_reported_by_the_hub(server):
    with pytest.raises(HubError, match="no operation nope") as info:
        Hub(_url(server), TOKEN).call("nope")

    assert info.value.status == 404


def test_an_unreachable_hub_says_it_is_not_answering():
    with pytest.raises(HubError, match="not answering"):
        Hub("http://127.0.0.1:1", TOKEN, timeout=2).health()


def test_chat_stream_returns_the_whole_answer_and_each_piece(server):
    pieces: list[str] = []

    text = Hub(_url(server), TOKEN).chat_stream("m", [{"role": "user", "content": "hi"}], pieces.append)

    assert text == "Hello"
    assert pieces == ["Hel", "lo"]


def test_chat_stream_stops_before_the_next_piece_when_cancelled(server):
    cancel = threading.Event()

    text = Hub(_url(server), TOKEN).chat_stream("m", [], lambda _piece: cancel.set(), cancel=cancel)

    assert text == "Hel"


def test_default_home_follows_mm_home(monkeypatch, tmp_path):
    monkeypatch.setenv("MM_HOME", str(tmp_path))
    assert default_home() == tmp_path

    monkeypatch.setenv("MM_HOME", "   ")
    assert default_home().name == "ModelMistress"
