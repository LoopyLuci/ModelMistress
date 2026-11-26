"""The Chat page's own features, offscreen: the queue, steering a reply, the sampling options, attachments and pictures, and
Live opening beside the messages. The hub is a fake whose replies are scripted, one piece at a time."""

from __future__ import annotations

import os
import threading
import time

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import pytest  # noqa: E402
from PySide6.QtWidgets import QApplication  # noqa: E402

from mm_gui.attachments import Attached  # noqa: E402
from mm_gui.chat_page import ChatPage  # noqa: E402
from mm_gui.hub import Hub, HubError  # noqa: E402
from test_live_panel import FakeMic, FakeSpeaker  # noqa: E402


class ChatHub(Hub):
    """Each chat call takes the next script: its pieces of reply, in order. The first reply waits for the test after its
    first piece, so the test can queue a message or steer the reply while it is being written."""

    def __init__(self, models: list[dict], scripts: list[list[str]] | None = None):
        super().__init__("http://127.0.0.1:9", "token")
        self.model_rows = models  # not `models`: that name is the hub's method
        self.scripts = scripts or []
        self.calls: list[tuple[str, list[dict], dict]] = []
        self.go = threading.Event()

    def call(self, op, args=None, timeout=None):
        if op == "model.list":
            return self.model_rows
        raise HubError(f"no operation {op}", 404)

    def chat_stream(self, model, messages, on_text, cancel=None, options=None):
        index = len(self.calls)
        self.calls.append((model, [dict(m) for m in messages], dict(options or {})))
        pieces = self.scripts[index] if index < len(self.scripts) else ["ok"]
        for i, piece in enumerate(pieces):
            if index == 0 and i == 1:
                self.go.wait(5)
            if cancel is not None and cancel.is_set():
                break
            on_text(piece)
        return "".join(pieces)


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
    raise AssertionError("timed out waiting for the chat")


MODELS = [{"id": "qwen3-8b", "source": "hf"}]


def chat_page(hub: ChatHub, app: QApplication) -> ChatPage:
    page = ChatPage()
    page.attach(hub)
    wait_for(app, lambda: page.model.count() >= 1)
    return page


def test_a_message_written_during_a_reply_is_queued_and_sent_after_it(app):
    hub = ChatHub(MODELS, [["Hi", " there"]])
    page = chat_page(hub, app)
    page.input.setPlainText("first")
    page.send()
    wait_for(app, lambda: page.messages and page.messages[-1]["content"] == "Hi")

    page.input.setPlainText("second")
    page.send()
    assert [m["text"] for m in page._queue] == ["second"]
    assert page.queue_list.count() == 1
    hub.go.set()

    wait_for(app, lambda: len(hub.calls) == 2 and page._cancel is None)
    assert hub.calls[1][1][-1] == {"role": "user", "content": "second"}
    assert page._queue == []


def test_a_steering_note_cuts_the_reply_after_its_next_sentence_and_goes_next(app):
    hub = ChatHub(MODELS, [["Alpha. ", "Beta is here. Gamma"]])
    page = chat_page(hub, app)
    page.input.setPlainText("tell me")
    page.send()
    wait_for(app, lambda: page.messages and page.messages[-1]["content"] == "Alpha. ")

    page.input.setPlainText("change course")
    page.steer_now()
    assert page.steer_row.isHidden() is False
    hub.go.set()

    wait_for(app, lambda: len(hub.calls) == 2 and page._cancel is None)
    assert page.messages[1]["content"] == "Alpha. Beta is here."  # the text so far, cut after the sentence
    assert hub.calls[1][1][-1] == {"role": "user", "content": "change course"}


def test_the_sampling_options_are_sent_only_when_set(app):
    hub = ChatHub(MODELS)
    page = chat_page(hub, app)
    page.input.setPlainText("plain")
    page.send()
    wait_for(app, lambda: len(hub.calls) == 1 and page._cancel is None)
    assert hub.calls[0][2] == {}

    page.top_k.setValue(40)
    page.seed.setValue(7)
    page.input.setPlainText("set")
    page.send()
    wait_for(app, lambda: len(hub.calls) == 2 and page._cancel is None)
    assert hub.calls[1][2] == {"top_k": 40, "seed": 7}


def test_an_attached_file_goes_with_the_message_as_its_text(app):
    hub = ChatHub(MODELS)
    page = chat_page(hub, app)
    page.attachments.add(Attached("notes.md", block="NOTES BLOCK"))
    page.input.setPlainText("summarize")
    page.send()
    wait_for(app, lambda: len(hub.calls) == 1 and page._cancel is None)
    assert hub.calls[0][1][-1]["content"] == "summarize\n\nNOTES BLOCK"
    assert page.attachments.items() == []


def test_the_picture_buttons_follow_the_model_that_can_see(app):
    hub = ChatHub([{"id": "qwen3-8b", "source": "hf"}, {"id": "llava", "source": "hf", "vision": True}])
    page = chat_page(hub, app)
    wait_for(app, lambda: page.model.count() == 2)
    page.model.setCurrentIndex(page.model.findData("qwen3-8b"))
    assert page.picture_button.isEnabled() is False
    assert page.camera_button.isEnabled() is False

    page.model.setCurrentIndex(page.model.findData("llava"))
    assert page.picture_button.isEnabled() is True


def test_live_opens_beside_the_messages_and_its_sentences_are_chat_messages(app):
    hub = ChatHub(MODELS)
    page = chat_page(hub, app)
    mics: list[FakeMic] = []
    speaker = FakeSpeaker()

    def mic_factory(on_utterance):
        mics.append(FakeMic(on_utterance))
        return mics[-1]

    page.live_options = {"mic_factory": mic_factory, "speaker": speaker}
    page.live_button.setChecked(True)
    assert page._live is not None and mics[0].opened

    page._live._send("hello from live", [])  # what the owner said in Live goes to the chat as its message
    wait_for(app, lambda: len(hub.calls) == 1 and page._cancel is None)
    assert hub.calls[0][1][-1] == {"role": "user", "content": "hello from live"}

    page.live_button.setChecked(False)
    assert page._live is None
    assert mics[0].closed
