"""The hub client's file, speech, benchmark and Misty calls, against a stand-in that records each request. No Qt needed."""

from __future__ import annotations

import io
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote

import pytest

from mm_gui.audio import wav_bytes
from mm_gui.hub import Hub, HubError, _Counted

TOKEN = "routes-token"


class _Recorder(BaseHTTPRequestHandler):
    def log_message(self, *_args) -> None:
        pass

    def _reply(self, status: int, body: bytes, content_type: str = "application/json") -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _json(self, status: int, doc: object) -> None:
        self._reply(status, json.dumps(doc).encode("utf-8"))

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else b""
        headers = {k.lower(): v for k, v in self.headers.items()}
        self.server.seen.append({"path": self.path, "headers": headers, "body": body})
        if self.headers.get("Authorization") != f"Bearer {TOKEN}":
            return self._json(401, {"error": {"message": "token refused"}})
        refused = self.server.refuse.get(self.path)
        if refused:
            return self._json(refused, {"error": {"message": f"refused with {refused}"}})
        path = self.path
        if path == "/v1/documents/text":
            if not body.startswith(b"%PDF"):
                return self._json(400, {"error": {"message": "not a PDF"}})
            return self._json(200, {"text": "Hello from the PDF", "truncated": False})
        if path == "/v1/models/import":
            name = unquote(headers.get("x-file-name", ""))
            if name == "dup.gguf":
                return self._json(409, {"error": {"message": "dup.gguf is already in the models folder"}})
            return self._json(200, {"name": name, "bytes": len(body), "path": f"/models/{name}"})
        if path == "/v1/audio/transcriptions":
            if b'name="file"' in body and b"RIFF" in body and headers.get("content-type", "").startswith("multipart/form-data; boundary="):
                return self._json(200, {"text": "  hello there  "})
            return self._json(400, {"error": {"message": "no audio file"}})
        if path == "/v1/audio/speech":
            request = json.loads(body)
            return self._reply(200, b"RIFF" + request["input"].encode("utf-8"), "audio/wav")
        if path == "/v1/chat/completions":
            events = [{"choices": [{"delta": {"content": piece}}]} for piece in ("Hel", "lo")]
            text = "".join(f"data: {json.dumps(e)}\n\n" for e in events) + "data: [DONE]\n\n"
            return self._reply(200, text.encode("utf-8"), "text/event-stream")
        if path.startswith("/v1/call/"):
            op = path.rsplit("/", 1)[-1]
            args = json.loads(body or b"{}")
            return self._json(200, {"result": {"op": op, "args": args}})
        self._json(404, {"error": {"message": "no route"}})


@pytest.fixture
def server():
    srv = ThreadingHTTPServer(("127.0.0.1", 0), _Recorder)
    srv.seen = []
    srv.refuse = {}
    thread = threading.Thread(target=srv.serve_forever, daemon=True)
    thread.start()
    yield srv
    srv.shutdown()
    srv.server_close()


@pytest.fixture
def hub(server):
    return Hub(f"http://127.0.0.1:{server.server_address[1]}", TOKEN, timeout=10)


def _last(server, path: str) -> dict:
    return [r for r in server.seen if r["path"] == path][-1]


def _args(server, op: str) -> dict:
    return json.loads(_last(server, f"/v1/call/{op}")["body"])


def test_a_pdf_goes_as_its_body_and_its_text_comes_back(hub, server):
    text, truncated = hub.document_text(b"%PDF-1.4 hello")
    assert (text, truncated) == ("Hello from the PDF", False)
    sent = _last(server, "/v1/documents/text")
    assert sent["headers"]["content-type"] == "application/pdf"
    assert sent["headers"]["authorization"] == f"Bearer {TOKEN}"


def test_a_model_file_is_streamed_with_its_name_and_reports_the_bytes_sent(hub, server, tmp_path):
    path = tmp_path / "my model.gguf"
    path.write_bytes(b"GGUF" * 50_000)
    progress: list[int] = []

    done = hub.import_model(path, on_bytes=progress.append)

    assert done == {"name": "my model.gguf", "bytes": 200_000, "path": "/models/my model.gguf"}
    sent = _last(server, "/v1/models/import")
    assert sent["headers"]["x-file-name"] == "my%20model.gguf"
    assert sent["headers"]["content-length"] == "200000"
    assert sent["headers"]["content-type"] == "application/octet-stream"
    assert progress and progress[-1] == 200_000 and progress == sorted(progress)


def test_a_model_already_in_the_folder_is_refused_with_its_status(hub, tmp_path):
    path = tmp_path / "dup.gguf"
    path.write_bytes(b"GGUF")
    with pytest.raises(HubError) as refused:
        hub.import_model(path)
    assert refused.value.status == 409


def test_counted_reads_report_each_chunk_in_order():
    seen: list[int] = []
    f = _Counted(io.BytesIO(b"abcdefghij"), seen.append)
    assert f.read(4) == b"abcd"
    assert f.read(4) == b"efgh"
    assert f.read(100) == b"ij"
    assert seen == [4, 8, 10]


def test_speech_to_text_sends_the_clip_as_multipart_and_returns_the_words(hub, server):
    assert hub.transcribe(wav_bytes(b"\x01\x00" * 100)) == "hello there"
    sent = _last(server, "/v1/audio/transcriptions")
    assert sent["headers"]["content-type"].startswith("multipart/form-data; boundary=")
    assert b'filename="speech.wav"' in sent["body"] and b"Content-Type: audio/wav" in sent["body"]


def test_speech_to_text_refused_by_a_build_without_it_says_so_with_the_status(hub, server):
    server.refuse["/v1/audio/transcriptions"] = 501
    with pytest.raises(HubError) as refused:
        hub.transcribe(wav_bytes(b"\x00\x00"))
    assert refused.value.status == 501


def test_text_to_speech_sends_the_json_and_returns_the_wav_bytes(hub, server):
    assert hub.speak("Good morning") == b"RIFFGood morning"
    request = json.loads(_last(server, "/v1/audio/speech")["body"])
    assert request == {"model": "tts-1", "input": "Good morning", "response_format": "wav"}


def test_the_benchmark_call_takes_its_settings_and_a_long_wait(hub, server):
    result = hub.bench(seconds=3, tokens=16)
    assert result == {"op": "system.bench", "args": {"seconds": 3, "tokens": 16}}


def test_chat_options_go_into_the_request_as_they_are(hub, server):
    pieces: list[str] = []
    text = hub.chat_stream("m", [{"role": "user", "content": "hi"}], pieces.append, options={"top_k": 40, "seed": 7})
    assert text == "Hello" and pieces == ["Hel", "lo"]
    body = json.loads(_last(server, "/v1/chat/completions")["body"])
    assert body["top_k"] == 40 and body["seed"] == 7 and body["stream"] is True


def test_a_rating_sends_the_run_label_and_the_correction_only_when_given(hub, server):
    hub.misty_rate("r1", "corrected", "Paris")
    assert _args(server, "misty.rate") == {"run": "r1", "label": "corrected", "correction": "Paris"}
    hub.misty_rate("r1", "good")
    assert _args(server, "misty.rate") == {"run": "r1", "label": "good"}


def test_a_correction_by_voice_sends_the_question_answer_and_correction(hub, server):
    hub.misty_teach("capital of spain", "Lyon.", "Madrid")
    assert _args(server, "misty.teach") == {"question": "capital of spain", "answer": "Lyon.", "correction": "Madrid"}


def test_the_learning_calls_send_their_arguments(hub, server):
    hub.misty_feedback(limit=5)
    assert _args(server, "misty.feedback.list") == {"limit": 5}
    hub.misty_forget(verdict=3)
    assert _args(server, "misty.feedback.forget") == {"id": 3}
    hub.misty_forget(everything=True)
    assert _args(server, "misty.feedback.forget") == {"all": True}
    hub.misty_export()
    assert _args(server, "misty.training.export") == {}
