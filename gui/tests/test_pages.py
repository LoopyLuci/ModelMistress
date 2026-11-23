"""The window and its pages, offscreen, against an in-memory hub."""

from __future__ import annotations

import os
import time

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import pytest  # noqa: E402
from PySide6.QtCore import QSettings  # noqa: E402
from PySide6.QtWidgets import QApplication  # noqa: E402

from mm_gui.app import Window  # noqa: E402
from mm_gui.hub import Hub, HubError  # noqa: E402
from mm_gui.pages import ChatPage, MistyPage, ModelsPage, SettingsPage, StatusPage  # noqa: E402
from mm_gui.prefs import Appearance, Preferences  # noqa: E402


class FakeHub(Hub):
    """A hub that answers from memory. Every call is recorded in `calls`."""

    def __init__(self, replies: dict | None = None, pieces: tuple[str, ...] = ()):
        super().__init__("http://127.0.0.1:9", "token")
        self.calls: list[tuple] = []
        self.pieces = pieces
        self.replies: dict = {
            "service.status": {"version": "0.3.0", "pid": 42, "uptime_s": 3725, "home": "/home/mm", "models_known": 2,
                               "llama_built_in": "b8000", "loaded": [{"model": "qwen3-8b"}],
                               "system": {"total_mb": 65536, "available_mb": 40960, "hub_rss_mb": 512}},
            "model.list": [],
            "model.loaded": [],
            "config.get": {},
        }
        self.replies.update(replies or {})

    def call(self, op, args=None, timeout=None):
        self.calls.append((op, dict(args or {})))
        if op not in self.replies:
            raise HubError(f"no operation {op}", 404)
        return self.replies[op]

    def chat_stream(self, model, messages, on_text, cancel=None):
        self.calls.append(("chat", model, list(messages)))
        for piece in self.pieces:
            on_text(piece)
        return "".join(self.pieces)


@pytest.fixture(scope="module")
def app():
    return QApplication.instance() or QApplication([])


@pytest.fixture
def appearance(app, tmp_path):
    """The window's look, kept in a temporary file; put back to the platform's look afterwards."""
    store = QSettings(str(tmp_path / "look.ini"), QSettings.Format.IniFormat)
    look = Appearance(app, Preferences(store))
    yield look
    look.set("text_size", "normal")
    look.set("theme", "system")
    look.set("style", "Fusion")


def wait_for(app: QApplication, ready, timeout: float = 5.0) -> None:
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        app.processEvents()
        if ready():
            return
        time.sleep(0.005)
    raise AssertionError("timed out waiting for the window")


def set_setting(page: SettingsPage, key: str, text: str) -> None:
    for row in range(page.table.rowCount()):
        if page.table.item(row, 0).text() == key:
            page.table.item(row, 1).setText(text)
            return
    raise AssertionError(f"no setting {key}")


def test_status_page_shows_the_hub_state(app):
    page = StatusPage()
    page.attach(FakeHub())

    wait_for(app, lambda: page.values["version"].text() == "0.3.0")
    assert "qwen3-8b" in page.values["loaded"].text()
    assert "built in (b8000)" == page.values["engine"].text()
    assert "free of 64.0 GB" in page.values["memory"].text()


def test_models_page_filters_loads_and_refreshes(app):
    hub = FakeHub({"model.list": [
        {"id": "qwen3-8b", "source": "hf", "size_gb": 4.7, "architecture": "qwen3", "loaded": False},
        {"id": "nomic-embed", "source": "hf"},
    ]})
    page = ModelsPage()
    page.attach(hub)
    wait_for(app, lambda: page.table.rowCount() == 2)

    page.filter.setText("qwen")
    assert page.table.rowCount() == 1
    page.table.selectRow(0)
    page._load()

    wait_for(app, lambda: ("model.load", {"model": "qwen3-8b"}) in hub.calls)
    assert page.table.rowCount() == 1 and page._visible[0]["id"] == "qwen3-8b"


def test_chat_streams_the_reply_into_the_conversation(app):
    hub = FakeHub({"model.list": [{"id": "qwen3-8b", "source": "hf"}]}, pieces=("Hel", "lo"))
    page = ChatPage()
    page.attach(hub)
    wait_for(app, lambda: page.model.count() == 1)

    page.input.setPlainText("Hi")
    page.send()

    wait_for(app, lambda: page.messages[-1]["content"] == "Hello" and page._cancel is None)
    assert page.messages == [{"role": "user", "content": "Hi"}, {"role": "assistant", "content": "Hello"}]
    assert ("chat", "qwen3-8b", [{"role": "user", "content": "Hi"}]) in hub.calls


def test_chat_shows_a_model_s_markup_as_text(app):
    hub = FakeHub({"model.list": [{"id": "m", "source": "hf"}]}, pieces=('<img src="http://example.invalid/x.png">',))
    page = ChatPage()
    page.attach(hub)
    wait_for(app, lambda: page.model.count() == 1)

    page.input.setPlainText("show me")
    page.send()

    wait_for(app, lambda: page._cancel is None and page.messages[-1]["content"])
    assert "<img" in page.transcript.toPlainText()


def _role(role, model, status="ready", loaded=False):
    return {"role": role, "model": model, "status": status if model else "missing", "loaded": loaded}


ROUTER_STATUS = {
    "model": "auto", "enabled": True, "overrides": {}, "free_ram_mb": 8192,
    "roles": [_role("fast", "qwen3-2b"), _role("main", "qwen3-9b", loaded=True), _role("deep", None)],
    "support": [_role("embed", "nomic-embed-text"), _role("rerank", None)],
}
ROUTER_MODELS = [
    {"id": "qwen3-2b", "source": "hf", "size_gb": 1.5},
    {"id": "qwen3-9b", "source": "hf", "size_gb": 6},
    {"id": "nomic-embed-text", "source": "hf", "size_gb": 0.1},
]


def test_chat_starts_on_the_router_and_sends_to_the_model_it_picks(app):
    pick = {"role": "deep", "model": "qwen3-9b", "forced": "deep", "reasons": ["you asked for deep"], "candidates": []}
    hub = FakeHub({"model.list": ROUTER_MODELS, "router.status": ROUTER_STATUS, "router.pick": pick}, pieces=("Do", "ne"))
    page = ChatPage()
    page.attach(hub)
    wait_for(app, lambda: page.model.count() == 3)
    assert page.model.itemData(0) == "auto" and page.model.currentData() == "auto"

    page.input.setPlainText("@deep prove it")
    page.send()

    wait_for(app, lambda: page.messages[-1]["content"] == "Done" and page._cancel is None)
    assert ("router.pick", {"messages": [{"role": "user", "content": "@deep prove it"}]}) in hub.calls
    # the picked model gets the message without the tag
    assert ("chat", "qwen3-9b", [{"role": "user", "content": "prove it"}]) in hub.calls
    assert page.messages[-1]["model"] == "deep: qwen3-9b"
    assert "Model (deep: qwen3-9b)" in page.transcript.toPlainText()


def test_chat_does_not_start_on_a_router_with_nothing_to_send_to(app):
    status = {**ROUTER_STATUS, "roles": [_role("fast", None), _role("main", None), _role("deep", None)]}
    hub = FakeHub({"model.list": ROUTER_MODELS, "router.status": status})
    page = ChatPage()
    page.attach(hub)
    wait_for(app, lambda: page.model.count() == 3)
    assert page.model.currentData() == "qwen3-2b"  # a model, not Auto; Auto is still there to choose


def test_chat_says_why_the_router_has_no_model_and_sends_nothing(app):
    pick = {"role": None, "model": None, "forced": None, "reasons": ["a picture, and no model can see it"], "candidates": []}
    hub = FakeHub({"model.list": ROUTER_MODELS, "router.status": ROUTER_STATUS, "router.pick": pick})
    page = ChatPage()
    page.attach(hub)
    wait_for(app, lambda: page.model.count() == 3)

    page.input.setPlainText("what is this?")
    page.send()

    wait_for(app, lambda: page._cancel is None and "router has no model" in page.status.text())
    assert "no model can see it" in page.status.text()
    assert not any(c[0] == "chat" for c in hub.calls)


def test_chat_on_a_hub_without_a_router_lists_only_models(app):
    hub = FakeHub({"model.list": ROUTER_MODELS})
    page = ChatPage()
    page.attach(hub)
    wait_for(app, lambda: page.model.count() == 2)
    assert "auto" not in [page.model.itemData(i) for i in range(page.model.count())]


def test_a_picked_model_is_sent_the_conversation_without_the_extra_keys_of_the_window(app):
    pick = {"role": "main", "model": "qwen3-9b", "forced": None, "reasons": [], "candidates": []}
    hub = FakeHub({"model.list": ROUTER_MODELS, "router.status": ROUTER_STATUS, "router.pick": pick}, pieces=("ok",))
    page = ChatPage()
    page.attach(hub)
    wait_for(app, lambda: page.model.count() == 3)
    for text in ("one", "two"):
        page.input.setPlainText(text)
        page.send()
        wait_for(app, lambda: page._cancel is None and page.messages[-1]["content"] == "ok")
        page.messages[-1]["content"] = "ok"
    chats = [c for c in hub.calls if c[0] == "chat"]
    assert chats[-1][2] == [
        {"role": "user", "content": "one"},
        {"role": "assistant", "content": "ok"},
        {"role": "user", "content": "two"},
    ]


def test_models_page_shows_and_changes_the_router_roles(app):
    hub = FakeHub({"model.list": ROUTER_MODELS, "router.status": ROUTER_STATUS, "misty.config.set": {}})
    page = ModelsPage()
    page.attach(hub)
    wait_for(app, lambda: page.table.rowCount() == 3)

    assert not page.roles_box.isHidden()
    roles = {page.table.item(r, 0).text(): page.table.item(r, 5).text() for r in range(3)}
    assert roles == {"qwen3-2b": "fast", "qwen3-9b": "main", "nomic-embed-text": "embed"}
    main = page.role_choice["main"]
    assert [main.itemData(i) for i in range(main.count())] == [None, "qwen3-2b", "qwen3-9b"]
    assert page.role_state["main"].text() == "ready, loaded"
    assert page.role_state["deep"].text() == "no model"
    embed = page.role_choice["embed"]
    assert [embed.itemData(i) for i in range(embed.count())] == [None, "nomic-embed-text"]

    fast = page.role_choice["fast"]
    fast.setCurrentIndex(2)  # qwen3-9b
    fast.activated.emit(2)
    wait_for(app, lambda: ("misty.config.set", {"patch": {"roles": {"fast": "qwen3-9b"}}}) in hub.calls)


def test_models_page_hides_the_roles_on_a_hub_without_a_router(app):
    page = ModelsPage()
    page.attach(FakeHub({"model.list": ROUTER_MODELS}))
    wait_for(app, lambda: page.table.rowCount() == 3)
    assert page.roles_box.isHidden()


class MistyFake(FakeHub):
    """A hub whose Misty answers from a script: `pages` are the pages misty.run.get returns, in order."""

    def __init__(self, pages: list[dict], status: dict | None = None):
        super().__init__()
        self.pages = list(pages)
        self.misty_calls: list[tuple] = []
        self.status_reply = status or {
            "ready": True,
            "roster": [{"role": "fast", "model": "bonsai-2b", "status": "ready"},
                       {"role": "main", "model": "bonsai-9b", "status": "ready"},
                       {"role": "deep", "model": None, "status": "missing"}],
            "next": [],
        }

    def misty_status(self):
        return self.status_reply

    def misty_start(self, args):
        self.misty_calls.append(("start", dict(args)))
        return {"run": "r1", "session": "s1"}

    def misty_page(self, run, start=0, wait_ms=0):
        self.misty_calls.append(("page", run, start))
        return self.pages.pop(0) if self.pages else {"events": [], "next": start, "done": True, "pending": []}

    def misty_cancel(self, run):
        self.misty_calls.append(("cancel", run))
        return {"run": run, "cancelled": True}

    def misty_approve(self, approval, allow):
        self.misty_calls.append(("approve", approval, allow))
        return {"approval": approval, "allowed": allow}


def test_misty_page_says_who_is_ready(app):
    page = MistyPage()
    page.attach(MistyFake([]))

    wait_for(app, lambda: page.ready.text().startswith("Ready"))
    assert "fast = bonsai-2b" in page.ready.text() and "main = bonsai-9b" in page.ready.text()
    assert "deep" not in page.ready.text()


def test_misty_page_says_what_to_do_when_nothing_is_ready(app):
    page = MistyPage()
    page.attach(MistyFake([], {"ready": False, "roster": [], "next": ["no language model is ready: run `model-mistress misty setup`"]}))

    wait_for(app, lambda: page.ready.text().startswith("Not ready"))
    assert "misty setup" in page.ready.text()


def test_misty_answers_in_the_conversation_and_keeps_the_session(app):
    hub = MistyFake([
        {"events": [{"type": "route", "role": "fast"}, {"type": "model", "role": "fast", "model": "bonsai-2b"},
                    {"type": "token", "text": "Hel"}, {"type": "token", "text": "lo"}], "next": 4, "done": False, "pending": []},
        {"events": [{"type": "final", "status": "done", "text": "Hello", "role": "fast", "citations": [{"n": 1, "source": "notes/a.md"}]}],
         "next": 5, "done": True, "pending": []},
    ])
    page = MistyPage()
    page.attach(hub)
    page.role.setCurrentIndex(1)  # fast
    page.input.setPlainText("Hi")
    page.send()

    wait_for(app, lambda: page._cancel is None and page.turns and page.turns[-1]["text"] == "Hello")
    assert page.session == "s1"
    assert page.turns[-1]["cites"] == [{"n": 1, "source": "notes/a.md"}]
    start = [c for c in hub.misty_calls if c[0] == "start"][0][1]
    assert start == {"input": "Hi", "approvals": "ask", "new_session": True, "role": "fast"}
    assert "Hello" in page.transcript.toPlainText() and "notes/a.md" in page.transcript.toPlainText()

    page.input.setPlainText("again")
    hub.pages = [{"events": [{"type": "final", "text": "Again", "status": "done"}], "next": 1, "done": True, "pending": []}]
    page.send()
    wait_for(app, lambda: page._cancel is None and len(page.turns) == 2 and page.turns[-1]["text"] == "Again")
    second = [c for c in hub.misty_calls if c[0] == "start"][1][1]
    assert second["session"] == "s1" and "new_session" not in second


def test_misty_waits_for_the_owner_before_a_tool_that_changes_something(app, monkeypatch):
    approval = {"approval": "ap1", "run": "r1", "tool": "fs_write", "class": "write", "args": {"path": "a.txt"}}
    hub = MistyFake([
        {"events": [], "next": 0, "done": False, "pending": [approval]},
        {"events": [{"type": "final", "text": "Written.", "status": "done"}], "next": 1, "done": True, "pending": []},
    ])
    page = MistyPage()
    asked = []
    monkeypatch.setattr(page, "_confirm", lambda a: asked.append(a["tool"]) or True)
    page.attach(hub)
    page.input.setPlainText("write a.txt")
    page.send()

    wait_for(app, lambda: page._cancel is None and page.turns[-1]["text"] == "Written.")
    wait_for(app, lambda: ("approve", "ap1", True) in hub.misty_calls)
    assert asked == ["fs_write"]


def test_misty_refusal_reaches_the_hub(app, monkeypatch):
    approval = {"approval": "ap2", "run": "r1", "tool": "fs_write", "class": "write", "args": {}}
    hub = MistyFake([
        {"events": [], "next": 0, "done": False, "pending": [approval]},
        {"events": [], "next": 0, "done": False, "pending": [approval]},  # still listed: asked once
        {"events": [{"type": "final", "text": "I did not write it.", "status": "done"}], "next": 1, "done": True, "pending": []},
    ])
    page = MistyPage()
    asked = []
    monkeypatch.setattr(page, "_confirm", lambda a: asked.append(1) or False)
    page.attach(hub)
    page.input.setPlainText("write")
    page.send()

    wait_for(app, lambda: page._cancel is None and page.turns[-1]["text"].startswith("I did not"))
    wait_for(app, lambda: ("approve", "ap2", False) in hub.misty_calls)
    assert len(asked) == 1


def test_misty_stop_cancels_the_run(app):
    hub = MistyFake([{"events": [{"type": "token", "text": "wor"}], "next": 1, "done": False, "pending": []}] * 3
                    + [{"events": [{"type": "final", "text": "", "status": "cancelled"}], "next": 2, "done": True, "pending": []}])
    page = MistyPage()
    page.attach(hub)
    page.input.setPlainText("long task")
    page.send()
    wait_for(app, lambda: page.turns and page.turns[-1]["text"] == "wor")
    page.stop()

    wait_for(app, lambda: page._cancel is None)
    assert ("cancel", "r1") in hub.misty_calls
    assert page.status.text() == "Stopped."


def test_misty_shows_a_model_s_markup_as_text(app):
    hub = MistyFake([{"events": [{"type": "final", "text": '<img src="http://example.invalid/x.png">', "status": "done"}],
                      "next": 1, "done": True, "pending": []}])
    page = MistyPage()
    page.attach(hub)
    page.input.setPlainText("show me")
    page.send()

    wait_for(app, lambda: page._cancel is None and page.turns[-1]["text"])
    assert "<img" in page.transcript.toPlainText()


def test_settings_save_sends_only_what_changed(app, appearance):
    hub = FakeHub({"config.get": {"max_loaded": 2, "allow_network": False, "download_dir": "E:/x",
                                  "hf_token": "(set)", "ports": [1, 2]}})
    page = SettingsPage(appearance)
    page.attach(hub)
    wait_for(app, lambda: page.table.rowCount() == 5)

    set_setting(page, "max_loaded", "3")
    set_setting(page, "allow_network", "true")
    page._save()

    wait_for(app, lambda: any(op == "config.set" for op, *_ in hub.calls))
    assert ("config.set", {"settings": {"max_loaded": 3, "allow_network": True}}) in hub.calls


def test_settings_refuse_a_value_of_the_wrong_type(app, appearance):
    hub = FakeHub({"config.get": {"max_loaded": 2}})
    page = SettingsPage(appearance)
    page.attach(hub)
    wait_for(app, lambda: page.table.rowCount() == 1)

    set_setting(page, "max_loaded", "many")
    page._save()

    assert "whole number" in page.status.text()
    assert not any(op == "config.set" for op, *_ in hub.calls)


def test_window_reports_a_missing_hub(app, tmp_path, appearance):
    def finder(_home):
        raise HubError("ModelMistress is not running")

    window = Window(tmp_path, finder=finder, appearance=appearance)
    wait_for(app, lambda: "not running" in window.banner.text())

    assert window.retry.isEnabled()
    window.close()


def test_window_connects_and_gives_the_hub_to_every_page(app, tmp_path, appearance):
    window = Window(tmp_path, finder=lambda _home: FakeHub(), appearance=appearance)

    wait_for(app, lambda: window.pages[0].values["version"].text() == "0.3.0")
    assert window.banner.text().startswith("Connected to the hub at")
    window.close()
