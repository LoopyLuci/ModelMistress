"""The window and its pages, offscreen, against an in-memory hub."""

from __future__ import annotations

import os
import time

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import pytest  # noqa: E402
from PySide6.QtWidgets import QApplication  # noqa: E402

from mm_gui.app import Window  # noqa: E402
from mm_gui.hub import Hub, HubError  # noqa: E402
from mm_gui.pages import ChatPage, ModelsPage, SettingsPage, StatusPage  # noqa: E402


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


def test_settings_save_sends_only_what_changed(app):
    hub = FakeHub({"config.get": {"max_loaded": 2, "allow_network": False, "download_dir": "E:/x",
                                  "hf_token": "(set)", "ports": [1, 2]}})
    page = SettingsPage()
    page.attach(hub)
    wait_for(app, lambda: page.table.rowCount() == 5)

    set_setting(page, "max_loaded", "3")
    set_setting(page, "allow_network", "true")
    page._save()

    wait_for(app, lambda: any(op == "config.set" for op, *_ in hub.calls))
    assert ("config.set", {"settings": {"max_loaded": 3, "allow_network": True}}) in hub.calls


def test_settings_refuse_a_value_of_the_wrong_type(app):
    hub = FakeHub({"config.get": {"max_loaded": 2}})
    page = SettingsPage()
    page.attach(hub)
    wait_for(app, lambda: page.table.rowCount() == 1)

    set_setting(page, "max_loaded", "many")
    page._save()

    assert "whole number" in page.status.text()
    assert not any(op == "config.set" for op, *_ in hub.calls)


def test_window_reports_a_missing_hub(app, tmp_path):
    def finder(_home):
        raise HubError("ModelMistress is not running")

    window = Window(tmp_path, finder=finder)
    wait_for(app, lambda: "not running" in window.banner.text())

    assert window.retry.isEnabled()
    window.close()


def test_window_connects_and_gives_the_hub_to_every_page(app, tmp_path):
    window = Window(tmp_path, finder=lambda _home: FakeHub())

    wait_for(app, lambda: window.pages[0].values["version"].text() == "0.3.0")
    assert window.banner.text().startswith("Connected to the hub at")
    window.close()
