"""The Android parity pieces of the native window, offscreen against an in-memory hub: Misty's queue, steering and ratings,
the learn dialog, attachments, the model import and the speed test."""

from __future__ import annotations

import os
import time

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import pytest  # noqa: E402
from PySide6.QtWidgets import QApplication, QFileDialog, QInputDialog, QMessageBox  # noqa: E402

from mm_gui.attachments import AttachmentBox, attach_paths  # noqa: E402
from mm_gui.hub import Hub, HubError  # noqa: E402
from mm_gui.learn import LearnDialog, counts_text  # noqa: E402
from mm_gui.misty_page import MistyPage  # noqa: E402
from mm_gui.pages import ModelsPage  # noqa: E402


class Fake(Hub):
    """A hub that answers from a dict and records each call. An operation it was not given answers 404."""

    def __init__(self, replies: dict | None = None):
        super().__init__("http://127.0.0.1:9", "token")
        self.calls: list[tuple] = []
        self.replies: dict = {"model.list": [], **(replies or {})}

    def call(self, op, args=None, timeout=None):
        self.calls.append((op, dict(args or {})))
        if op not in self.replies:
            raise HubError(f"no operation {op}", 404)
        return self.replies[op]


class Misty(Fake):
    """Misty's hub. Each scripted page is handed out only once the test allows it; after the script the run is quiet until
    it is cancelled, then it ends. Every run is run r1."""

    def __init__(self, pages: list[dict]):
        super().__init__({"misty.rate": {}, "misty.feedback.list": {"counts": {}, "verdicts": []}, "misty.feedback.forget": {}})
        self.pages = list(pages)
        self.allowed = 0
        self.served = 0
        self.misty_calls: list[tuple] = []

    def misty_status(self):
        return {"ready": True, "roster": [{"role": "fast", "model": "bonsai-2b", "status": "ready"}], "next": []}

    def misty_start(self, args):
        self.misty_calls.append(("start", dict(args)))
        return {"run": "r1", "session": "s1"}

    def misty_page(self, run, start=0, wait_ms=0):
        if self.served < self.allowed and self.pages:
            self.served += 1
            return self.pages.pop(0)
        if any(c[0] == "cancel" for c in self.misty_calls):
            return {"events": [], "next": start, "done": True, "pending": []}
        time.sleep(0.005)
        return {"events": [], "next": start, "done": False, "pending": []}

    def misty_cancel(self, run):
        self.misty_calls.append(("cancel", run))
        return {"run": run, "cancelled": True}

    def starts(self) -> list[dict]:
        return [args for op, args in self.misty_calls if op == "start"]


class ImportHub(Fake):
    def import_model(self, path, on_bytes=None):
        size = path.stat().st_size
        if on_bytes is not None:
            on_bytes(size)
        self.calls.append(("import", path.name))
        return {"name": path.name, "bytes": size, "path": str(path)}


BENCH = {
    "cpu": {},
    "memory_gbps": 22.5,
    "ram_gb": 32.0,
    "models": [
        {"model": "qwen3-2b", "size_gb": 1.5, "tokens": 96, "ms": 2000, "tokens_per_s": 48.0, "cold_start": False},
        {"model": "qwen3-9b", "size_gb": 6, "tokens": 96, "ms": 9000, "tokens_per_s": 10.7, "cold_start": False},
        {"model": "broken", "size_gb": 1, "error": "no engine"},
    ],
    "suggestions": {"fastest": "qwen3-2b", "balanced": "qwen3-9b", "best": None, "reading_tokens_per_s": 8},
    "note": "A cold start includes loading the model.",
}


@pytest.fixture(scope="module")
def app():
    return QApplication.instance() or QApplication([])


def wait_for(app: QApplication, ready, timeout: float = 5.0) -> None:
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        app.processEvents()
        if ready():
            return
        time.sleep(0.005)
    raise AssertionError("timed out waiting for the window")


# ---- Misty: queue, steering, ratings ----------------------------------------------------------------------------------


def test_a_message_sent_during_a_reply_waits_in_the_queue_and_goes_next(app):
    hub = Misty([])
    page = MistyPage()
    page.attach(hub)
    page.input.setPlainText("first")
    page.send()
    wait_for(app, lambda: page._cancel is not None)

    page.input.setPlainText("second")
    page.send()
    assert [m["text"] for m in page._queue] == ["second"]
    assert page.queue_list.count() == 1
    assert page.input.toPlainText() == ""

    page.stop()
    wait_for(app, lambda: len(hub.starts()) == 2 and page._cancel is None)
    assert hub.starts()[1]["input"] == "second"
    assert page._queue == []


def test_a_steering_note_cuts_the_reply_after_its_next_sentence_and_goes_next(app):
    hub = Misty([
        {"events": [{"type": "token", "text": "Alpha. "}], "next": 1, "done": False, "pending": []},
        {"events": [{"type": "token", "text": "Beta is here. Gamma"}], "next": 2, "done": False, "pending": []},
    ])
    page = MistyPage()
    page.attach(hub)
    page.input.setPlainText("tell me")
    page.send()
    hub.allowed = 1
    wait_for(app, lambda: bool(page.turns) and page.turns[0]["text"] == "Alpha. ")

    page.input.setPlainText("change course")
    page.steer_now()
    hub.allowed = 2
    wait_for(app, lambda: page.turns[0]["text"] == "Alpha. Beta is here.")
    assert page.turns[0]["run"] == ""  # a reply cut by a note is not rated
    wait_for(app, lambda: ("cancel", "r1") in hub.misty_calls)  # the worker asks the hub to stop the run

    wait_for(app, lambda: len(hub.starts()) == 2 and page._cancel is None)
    assert hub.starts()[1]["input"] == "change course"


def test_a_message_after_the_reply_has_ended_is_sent_not_steered(app):
    hub = Misty([{"events": [{"type": "final", "text": "Done.", "status": "done"}], "next": 1, "done": True, "pending": []}])
    page = MistyPage()
    page.attach(hub)
    hub.allowed = 1
    page.input.setPlainText("hi")
    page.send()
    wait_for(app, lambda: page._cancel is None and bool(page.turns) and page.turns[0]["text"] == "Done.")

    page.input.setPlainText("next question")
    page.send()  # nothing is running now, so this goes out as a new run
    wait_for(app, lambda: len(hub.starts()) == 2)
    assert page._steer is None and page.steer_row.isHidden()
    assert hub.starts()[1]["input"] == "next question"
    page.stop()  # the second run is quiet; stop it so the test ends with nothing running
    wait_for(app, lambda: page._cancel is None)


def test_a_finished_reply_is_rated_and_the_verdict_is_sent_to_the_hub(app):
    hub = Misty([{"events": [{"type": "final", "text": "Hello", "status": "done"}], "next": 1, "done": True, "pending": []}])
    page = MistyPage()
    page.attach(hub)
    hub.allowed = 1
    page.input.setPlainText("hi")
    page.send()
    wait_for(app, lambda: page._cancel is None and page.rate_pick.count() == 1)

    page._rate("good")
    wait_for(app, lambda: page.turns[0]["rated"] == "good")
    assert ("misty.rate", {"run": "r1", "label": "good"}) in hub.calls
    assert page.rate_state.text() == "Rated: good."


def test_a_correction_is_sent_with_the_right_answer(app, monkeypatch):
    hub = Misty([{"events": [{"type": "final", "text": "Lyon", "status": "done"}], "next": 1, "done": True, "pending": []}])
    page = MistyPage()
    page.attach(hub)
    hub.allowed = 1
    page.input.setPlainText("capital?")
    page.send()
    wait_for(app, lambda: page._cancel is None and page.rate_pick.count() == 1)

    monkeypatch.setattr(QInputDialog, "getMultiLineText", lambda *a, **k: ("Paris", True))
    page._correct()
    wait_for(app, lambda: page.turns[0]["rated"] == "corrected")
    assert ("misty.rate", {"run": "r1", "label": "corrected", "correction": "Paris"}) in hub.calls


def test_the_no_tools_box_adds_its_flag_only_when_checked(app):
    hub = Misty([])
    page = MistyPage()
    page.attach(hub)
    page.input.setPlainText("plain")
    page.send()
    wait_for(app, lambda: len(hub.starts()) == 1)
    assert "no_tools" not in hub.starts()[0]
    page.stop()
    wait_for(app, lambda: page._cancel is None)

    page.no_tools.setChecked(True)
    page.input.setPlainText("quiet")
    page.send()
    wait_for(app, lambda: len(hub.starts()) == 2)
    assert hub.starts()[1]["no_tools"] is True
    page.stop()
    wait_for(app, lambda: page._cancel is None)


# ---- the learn dialog --------------------------------------------------------------------------------------------------


def test_counts_say_what_she_has_learned():
    assert counts_text({}).startswith("Misty has no verdicts yet")
    assert counts_text({"total": 0, "good": 0}).startswith("Misty has no verdicts yet")
    assert counts_text({"total": 3, "good": 2, "bad": 1, "needs_tools": 0}) == "3 verdicts: good 2, bad 1."


def test_the_learn_dialog_lists_the_verdicts_and_forgets_one(app, monkeypatch):
    hub = Misty([])
    hub.replies["misty.feedback.list"] = {
        "counts": {"total": 2, "good": 1, "bad": 1},
        "verdicts": [
            {"id": 7, "label": "good", "question": "2+2?", "answer": "4", "correction": ""},
            {"id": 8, "label": "bad", "question": "capital?", "answer": "Lyon", "correction": "Paris"},
        ],
    }
    dialog = LearnDialog(hub)
    wait_for(app, lambda: dialog.table.rowCount() == 2)
    assert dialog.summary.text() == "2 verdicts: good 1, bad 1."
    assert dialog.table.item(1, 3).text() == "Paris"

    dialog.table.selectRow(1)
    monkeypatch.setattr(QMessageBox, "question", lambda *a, **k: QMessageBox.StandardButton.Yes)
    dialog._forget_selected()
    wait_for(app, lambda: ("misty.feedback.forget", {"id": 8}) in hub.calls)


def test_the_training_data_is_saved_as_jsonl(app, tmp_path, monkeypatch):
    hub = Misty([])
    hub.replies["misty.training.export"] = {"count": 1, "jsonl": '{"messages": []}\n'}
    dialog = LearnDialog(hub)
    target = tmp_path / "train.jsonl"
    monkeypatch.setattr(QFileDialog, "getSaveFileName", lambda *a, **k: (str(target), "JSON Lines (*.jsonl)"))
    dialog._export()
    wait_for(app, lambda: target.exists())
    assert target.read_text(encoding="utf-8") == '{"messages": []}\n'


# ---- attachments -------------------------------------------------------------------------------------------------------


def test_a_picture_is_refused_unless_the_model_can_see(tmp_path):
    picture = tmp_path / "shot.png"
    picture.write_bytes(b"\x89PNG\r\n\x1a\n" + b"0" * 16)
    box = AttachmentBox()
    reported: list[str] = []
    attach_paths(box, [str(picture)], hub=None, can_picture=False, report=reported.append)
    assert box.items() == []
    assert "cannot read pictures" in reported[0]


def test_a_long_text_file_goes_in_as_data_and_is_marked_cut(tmp_path):
    note = tmp_path / "notes.md"
    note.write_text("alpha\n" * 1000, encoding="utf-8")
    box = AttachmentBox()
    reported: list[str] = []
    attach_paths(box, [str(note)], hub=None, can_picture=False, report=reported.append)
    [item] = box.items()
    assert item.name == "notes.md" and item.cut is True
    assert "alpha" in item.block and len(item.block) < 6000
    assert reported == []


def test_a_pdf_needs_the_hub_and_an_unknown_file_type_is_reported(tmp_path):
    pdf = tmp_path / "paper.pdf"
    pdf.write_bytes(b"%PDF-1.4")
    exe = tmp_path / "tool.exe"
    exe.write_bytes(b"MZ")
    box = AttachmentBox()
    reported: list[str] = []
    attach_paths(box, [str(pdf), str(exe)], hub=None, can_picture=True, report=reported.append)
    assert box.items() == []
    assert "hub is not connected" in reported[0]
    assert "only text, source, JSON, PDF and picture files" in reported[1]


# ---- models: the import and the speed test ----------------------------------------------------------------------------


def test_importing_a_model_file_copies_it_and_lists_it(app, tmp_path, monkeypatch):
    gguf = tmp_path / "tiny.gguf"
    gguf.write_bytes(b"GGUF" + b"0" * 64)
    hub = ImportHub({"model.list": [{"id": "tiny", "source": "file"}]})
    page = ModelsPage()
    page.attach(hub)
    wait_for(app, lambda: page.table.rowCount() == 1 and page.status.text() == "")

    monkeypatch.setattr(QFileDialog, "getOpenFileName", lambda *a, **k: (str(gguf), "Model files (*.gguf)"))
    page._import()
    wait_for(app, lambda: page.status.text() == "Imported tiny.gguf. It is in the list now.")
    assert ("import", "tiny.gguf") in hub.calls
    assert page.importer.isEnabled()


def test_a_failed_import_says_why_and_the_button_comes_back(app, tmp_path, monkeypatch):
    gguf = tmp_path / "dup.gguf"
    gguf.write_bytes(b"GGUF")

    class Refusing(Fake):
        def import_model(self, path, on_bytes=None):
            raise HubError("a model called dup.gguf is already there", 409)

    hub = Refusing()
    page = ModelsPage()
    page.attach(hub)
    wait_for(app, lambda: page.status.text() == "")
    monkeypatch.setattr(QFileDialog, "getOpenFileName", lambda *a, **k: (str(gguf), "Model files (*.gguf)"))
    page._import()
    wait_for(app, lambda: "already there" in page.status.text())
    assert page.importer.isEnabled()


def test_the_speed_test_says_what_fits_and_lists_each_model(app):
    hub = Fake({"system.bench": BENCH})
    page = ModelsPage()
    page.attach(hub)
    wait_for(app, lambda: any(c[0] == "model.list" for c in hub.calls) and page.status.text() == "")

    page._bench()
    wait_for(app, lambda: page.bench_result.text() != "")
    assert page.bench_table.rowCount() == 3
    assert page.bench_table.item(0, 2).text() == "48.0"
    assert page.bench_table.item(1, 2).text() == "10.7"
    assert page.bench_table.item(2, 3).text().startswith("not timed")
    text = page.bench_result.text()
    assert "Fastest: qwen3-2b." in text
    assert "Keeps up with reading: qwen3-9b." in text
    assert "Best quality that fits: none fits." in text
    assert "22.5 GB/s" in text
    assert page.bench_run.isEnabled()
