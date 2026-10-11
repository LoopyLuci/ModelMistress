import io
import json
import os
import subprocess
import sys
from pathlib import Path

from mm_forge import protocol
from mm_forge.protocol import Emitter

ROOT = Path(__file__).resolve().parents[1]


def lines(buf: io.StringIO):
    return [json.loads(l) for l in buf.getvalue().splitlines()]


def test_every_event_type_is_one_json_line():
    buf = io.StringIO()
    em = Emitter(buf)
    em.progress(1, 4, "one")
    em.metric("loss", 2.5, step=7)
    em.log("hello", level="warn")
    em.artifact("w", "some/path", bytes=12, kind="gguf")
    em.result({"ok": True})
    em.error("boom", code="x")
    ev = lines(buf)
    assert [e["type"] for e in ev] == ["progress", "metric", "log", "artifact", "result", "error"]
    assert ev[1] == {"type": "metric", "name": "loss", "value": 2.5, "step": 7}
    assert ev[3]["bytes"] == 12 and ev[4]["data"] == {"ok": True}
    assert buf.getvalue().count("\n") == 6


def test_nan_and_inf_become_strings_and_paths_become_text():
    buf = io.StringIO()
    em = Emitter(buf)
    em.metric("loss", float("nan"))
    em.metric("loss", float("inf"))
    em.log(Path("a") / "b")
    ev = lines(buf)
    assert ev[0]["value"] == "nan" and ev[1]["value"] == "inf"
    assert ev[2]["message"].replace("\\", "/") == "a/b"


def test_unknown_type_is_refused_and_non_ascii_is_escaped():
    em = Emitter(io.StringIO())
    try:
        em.emit("nope")
        raise AssertionError("accepted")
    except ValueError:
        pass
    buf = io.StringIO()
    Emitter(buf).log("café 中")
    assert buf.getvalue().isascii() and json.loads(buf.getvalue())["message"] == "café 中"


def test_parse_line_ignores_what_is_not_an_event():
    assert protocol.parse_line('{"type":"log","message":"x"}')["message"] == "x"
    for bad in ("", "plain text", "{not json", '{"type":"nope"}', '["log"]', '{"x":1}'):
        assert protocol.parse_line(bad) is None
    got = protocol.read_events(['{"type":"progress","done":1,"total":2}', "noise", '{"type":"result","data":1}'])
    assert [e["type"] for e in got] == ["progress", "result"]


def test_read_args():
    assert protocol.read_args(io.StringIO("")) == {}
    assert protocol.read_args(io.StringIO('{"a": 1}')) == {"a": 1}
    try:
        protocol.read_args(io.StringIO("[1]"))
        raise AssertionError("accepted")
    except ValueError:
        pass


def test_stray_prints_do_not_reach_the_protocol_stream(capsys):
    with protocol.capture_stdout() as em:
        print("a stray print")
        em.log("real")
    out = capsys.readouterr()
    assert "a stray print" in out.err
    assert "a stray print" not in out.out
    assert json.loads(out.out.strip())["message"] == "real"


def run_worker(*argv, stdin=""):
    env = dict(os.environ, PYTHONPATH=str(ROOT), PYTHONIOENCODING="utf-8")
    return subprocess.run([sys.executable, "-m", "mm_forge", *argv], input=stdin, capture_output=True, text=True,
                          env=env, timeout=120)


def test_worker_process_speaks_the_protocol():
    p = run_worker("env_info", stdin="{}")
    assert p.returncode == 0, p.stderr
    ev = protocol.read_events(p.stdout.splitlines())
    assert ev[-1]["type"] == "result" and "python" in ev[-1]["data"]
    assert all(protocol.parse_line(l) for l in p.stdout.splitlines()), "stdout holds only protocol lines"


def test_worker_reports_an_unknown_step_and_bad_args_as_errors():
    p = run_worker("no_such_step", stdin="{}")
    assert p.returncode == 2
    ev = protocol.read_events(p.stdout.splitlines())
    assert ev[-1]["type"] == "error" and ev[-1]["code"] == "bad_request"
    p = run_worker("env_info", stdin="[1,2]")
    assert p.returncode == 2 and protocol.read_events(p.stdout.splitlines())[-1]["type"] == "error"
