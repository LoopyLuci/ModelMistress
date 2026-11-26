"""The Live panel, offscreen. The microphone, the speaker and the hub are fakes, so a conversation can be driven one sentence
at a time: a sentence is written down, held, sent; a reply is said; a correction is recorded for Misty."""

from __future__ import annotations

import os
import time

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import pytest  # noqa: E402
from PySide6.QtWidgets import QApplication  # noqa: E402

from mm_gui import live  # noqa: E402
from mm_gui.audio import DeviceError  # noqa: E402
from mm_gui.hub import Hub  # noqa: E402
from mm_gui.live import LivePanel  # noqa: E402


class LiveHub(Hub):
    """Writes down whatever `text` says, speaks by returning a stand-in WAV, and keeps what it was asked to teach."""

    def __init__(self):
        super().__init__("http://127.0.0.1:9", "token")
        self.text = ""
        self.speeches: list[str] = []
        self.teaches: list[tuple[str, str, str]] = []

    def transcribe(self, wav: bytes) -> str:
        return self.text

    def speak(self, text: str) -> bytes:
        self.speeches.append(text)
        return b"RIFF"

    def misty_teach(self, question: str, answer: str, correction: str) -> dict:
        self.teaches.append((question, answer, correction))
        return {}


class FakeMic:
    def __init__(self, on_utterance):
        self.on_utterance = on_utterance
        self.opened = False
        self.enabled = False
        self.closed = False

    def open(self) -> None:
        self.opened = True

    def set_enabled(self, on: bool) -> None:
        self.enabled = on

    def close(self) -> None:
        self.closed = True

    def finish_sentence(self) -> None:
        """The owner has finished a sentence: the microphone hands over its audio."""
        self.on_utterance(b"\x01\x00" * 1600, 100)


class FakeSpeaker:
    def __init__(self):
        self.played: list[bytes] = []
        self.done = None
        self.stops = 0

    def play(self, wav: bytes, on_done) -> None:
        self.played.append(wav)
        self.done = on_done

    def stop(self) -> None:
        self.stops += 1

    def finish(self, error: str | None = None) -> None:
        done, self.done = self.done, None
        done(error)


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
    raise AssertionError("timed out waiting for Live")


class Rig:
    """A panel with its fakes, and the lists the panel's calls go into."""

    def __init__(self, hub: LiveHub, mic_error: str | None = None):
        self.hub = hub
        self.sent: list[str] = []
        self.stopped = 0
        self.closed = 0
        self.speaker = FakeSpeaker()
        self.mics: list[FakeMic] = []

        def mic_factory(on_utterance):
            if mic_error is not None:
                raise DeviceError(mic_error)
            mic = FakeMic(on_utterance)
            self.mics.append(mic)
            return mic

        self.panel = LivePanel(
            hub=hub,
            send=self._send,
            stop=self._stop,
            can_picture=False,
            mic_factory=mic_factory,
            speaker=self.speaker,
        )
        self.panel.closed.connect(lambda: setattr(self, "closed", self.closed + 1))

    def _send(self, text: str, pictures: list[str]) -> bool:
        self.sent.append(text)
        return True

    def _stop(self) -> None:
        self.stopped += 1

    @property
    def mic(self) -> FakeMic:
        return self.mics[0]

    def say(self, text: str) -> None:
        """The owner says one sentence, which the hub writes down."""
        self.hub.text = text
        self.mic.finish_sentence()


def test_a_sentence_is_written_down_held_and_sent(app, monkeypatch):
    monkeypatch.setattr(live, "HOLD_MS", 30)
    rig = Rig(LiveHub())
    assert rig.mic.opened
    rig.say("what time is it")
    wait_for(app, lambda: rig.sent == ["what time is it"])
    rig.panel.shutdown()


def test_send_now_sends_the_held_sentence_at_once(app, monkeypatch):
    monkeypatch.setattr(live, "HOLD_MS", 60_000)
    rig = Rig(LiveHub())
    rig.say("open the file")
    wait_for(app, lambda: not rig.panel.held_box.isHidden())
    assert rig.sent == []

    rig.panel.send_now.click()
    wait_for(app, lambda: rig.sent == ["open the file"])
    rig.panel.shutdown()


def test_cancel_send_takes_the_held_sentence_back(app, monkeypatch):
    monkeypatch.setattr(live, "HOLD_MS", 60_000)
    rig = Rig(LiveHub())
    rig.say("never mind this")
    wait_for(app, lambda: not rig.panel.held_box.isHidden())

    rig.panel._cancel_send()
    assert rig.panel.held_box.isHidden()
    assert rig.sent == []
    rig.panel.shutdown()


def test_a_reply_is_said_a_sentence_at_a_time_and_the_speaker_is_stopped_when_the_conversation_ends(app, monkeypatch):
    monkeypatch.setattr(live, "HOLD_MS", 30)
    hub = LiveHub()
    rig = Rig(hub)
    rig.say("hello")
    wait_for(app, lambda: rig.sent == ["hello"] and rig.panel.turns.replying)  # a reply is only read once it is asked for

    rig.panel.reply("Hello there.", busy=True)
    rig.panel.reply("Hello there. Bye", busy=False)
    wait_for(app, lambda: hub.speeches[:1] == ["Hello there."])
    wait_for(app, lambda: rig.speaker.done is not None)
    rig.speaker.finish()  # the first sentence is done; the next one is fetched and played
    wait_for(app, lambda: "Bye" in hub.speeches)
    wait_for(app, lambda: rig.speaker.done is not None)

    rig.panel.end_conversation()
    assert rig.mic.closed
    assert rig.closed == 1
    assert rig.speaker.stops >= 1
    rig.panel.shutdown()  # a second shutdown is harmless


def test_a_correction_is_recorded_against_the_last_reply(app, monkeypatch):
    monkeypatch.setattr(live, "HOLD_MS", 30)
    hub = LiveHub()
    rig = Rig(hub)
    rig.say("capital of France?")
    wait_for(app, lambda: rig.sent == ["capital of France?"] and rig.panel.turns.replying)

    rig.panel.reply("Lyo", busy=True)
    rig.panel.reply("Lyon", busy=False)
    rig.say("correction, it is Paris")
    wait_for(app, lambda: hub.teaches == [("capital of France?", "Lyon", "it is Paris")])
    assert rig.sent == ["capital of France?"]  # a correction is not sent to the chat
    rig.panel.shutdown()


def test_a_missing_microphone_says_so_and_the_camera_is_off_without_pictures(app):
    rig = Rig(LiveHub(), mic_error="no microphone found")
    assert "no microphone found" in rig.panel.problem.text()
    assert rig.panel.camera_button.isEnabled() is False
    rig.panel.shutdown()
