"""Live: a conversation by voice in the Chat page. The owner talks; each sentence is written down by the hub's speech to
text and sent as the next message; the reply is said aloud a sentence at a time, and talking over it stops it. A correction
spoken against a reply is saved for Misty. The rules are in mm_gui/turns.py, which this panel follows; it only moves sound
and calls, and every result comes back to the window's thread.

The computer has no echo cancellation, so its own voice can reach the microphone. The rules take out what sounds like the
reply being said, but on a loud speaker a headset keeps Live most accurate."""

from __future__ import annotations

from collections import deque
from typing import Callable

from PySide6.QtCore import Qt, QTimer, Signal
from PySide6.QtGui import QPixmap
from PySide6.QtWidgets import QCheckBox, QFrame, QHBoxLayout, QLabel, QLineEdit, QPushButton, QVBoxLayout, QWidget

from mm_gui.audio import Camera, DeviceError, Microphone, Speaker, wav_bytes
from mm_gui.chat_extras import picture_data_url
from mm_gui.hub import Hub
from mm_gui.speech import ERROR, NO_SPEECH_TEXT, OK, UNAVAILABLE, UNAVAILABLE_TEXT, speak_result, transcribe_result
from mm_gui.tasks import run_task
from mm_gui import turns as _turns
from mm_gui.turns import Effect, LiveTurns

# A finished sentence waits this long so the owner can take it back. Read at each use, so a test can shorten it.
HOLD_MS = _turns.HOLD_MS
PHASE_TEXT = {
    "starting": "Starting",
    "listening": "Listening",
    "thinking": "Thinking",
    "speaking": "Speaking",
    "stopped": "Stopped",
}


class _Voice:
    """The computer's voice: each sentence is fetched from the hub and played, one after another. `done(error)` runs once a
    sentence has been said, or with the reason it could not be."""

    def __init__(self, hub_of: Callable[[], Hub | None], speaker):
        self._hub_of = hub_of
        self._speaker = speaker
        self._queue: deque[tuple[str, Callable[[str | None], None]]] = deque()
        self._busy = False
        self._gen = 0

    def say(self, sentence: str, done: Callable[[str | None], None]) -> None:
        self._queue.append((sentence, done))
        self._pump()

    def stop(self) -> None:
        self._gen += 1
        self._queue.clear()
        self._busy = False
        self._speaker.stop()

    def _pump(self) -> None:
        if self._busy or not self._queue:
            return
        hub = self._hub_of()
        gen = self._gen
        if hub is None:
            self._finish(gen, "The hub is not connected.")
            return
        self._busy = True
        text = self._queue[0][0]
        run_task(lambda: speak_result(hub, text), lambda result: self._fetched(gen, result), lambda message: self._finish(gen, message))

    def _fetched(self, gen: int, result: tuple[str, object]) -> None:
        if gen != self._gen:
            return
        kind, value = result
        if kind != OK:
            self._finish(gen, NO_SPEECH_TEXT if kind == UNAVAILABLE else str(value))
            return
        self._speaker.play(value, lambda error: self._finish(gen, error))

    def _finish(self, gen: int, error: str | None) -> None:
        if gen != self._gen or not self._queue:
            return
        _text, done = self._queue.popleft()
        self._busy = False
        done(error)
        self._pump()


class LivePanel(QFrame):
    """The Live conversation, shown above the message box of the Chat page. `closed` says the owner ended it."""

    closed = Signal()

    def __init__(
        self,
        *,
        hub: Hub | None,
        send: Callable[[str, list[str]], bool],
        stop: Callable[[], None],
        can_picture: bool,
        mic_factory: Callable[[Callable[[bytes, int], None]], object] | None = None,
        speaker=None,
        camera_factory: Callable[[], object] | None = None,
        parent: QWidget | None = None,
    ):
        super().__init__(parent)
        self.setFrameShape(QFrame.Shape.StyledPanel)
        self.setAccessibleName("Live conversation")
        self.turns = LiveTurns()
        self._hub = hub
        self._send = send
        self._stop_reply = stop
        self._can_picture = can_picture
        self._mic_factory = mic_factory or Microphone
        self._camera_factory = camera_factory or Camera
        self._speaker = speaker if speaker is not None else Speaker(self)
        self._voice = _Voice(lambda: self._hub, self._speaker)
        self._mic = None
        self._camera = None
        self._camera_on = False
        self._clips: deque[bytes] = deque()
        self._transcribing = False
        self._problem = ""
        self._closed = False

        self._hold_timer = QTimer(self)
        self._hold_timer.setSingleShot(True)
        self._hold_timer.timeout.connect(lambda: self._dispatch(self.turns.hold_ended()))
        self._listen_timer = QTimer(self)
        self._listen_timer.setSingleShot(True)
        self._listen_timer.timeout.connect(self._listen_later)

        self._build()
        self._dispatch(self.turns.start())
        try:
            self._mic = self._mic_factory(self._utterance)
            self._mic.open()
        except DeviceError as e:
            self._dispatch(self.turns.failed(False, str(e)))
        except Exception as e:  # a device that fails in another way is still a reason the conversation cannot start
            self._dispatch(self.turns.failed(False, f"The microphone could not be opened: {e}"))

    # ---- the screen -------------------------------------------------------------------------------------------------

    def _build(self) -> None:
        self.phase = QLabel("Starting")
        self.phase.setAccessibleName("Live phase")
        self.mic_button = QPushButton("Mic on")
        self.mic_button.setCheckable(True)
        self.mic_button.setChecked(True)
        self.mic_button.setAccessibleName("Microphone")
        self.mic_button.toggled.connect(lambda on: self._dispatch(self.turns.set_mic(on)))
        self.edit_check = QCheckBox("Edit before send")
        self.edit_check.toggled.connect(lambda on: self._dispatch(self.turns.set_edit_before_send(on)))
        self.camera_button = QPushButton("Camera on")
        self.camera_button.setCheckable(True)
        self.camera_button.setEnabled(self._can_picture)
        self.camera_button.setToolTip("Each sentence goes with the picture the camera has now, to a model that can read pictures.")
        self.camera_button.toggled.connect(self._camera_toggled)
        end = QPushButton("End")
        end.setAccessibleName("End Live")
        end.clicked.connect(self.end_conversation)

        head = QHBoxLayout()
        head.addWidget(self.phase)
        head.addWidget(self.mic_button)
        head.addWidget(self.edit_check)
        head.addWidget(self.camera_button)
        head.addStretch(1)
        head.addWidget(end)

        self.preview = QLabel()
        self.preview.setFixedSize(320, 240)
        self.preview.setAccessibleName("Camera preview")
        self.preview.setVisible(False)
        self.camera_error = QLabel()
        self.camera_error.setWordWrap(True)

        self.heard = QLabel("You: ...")
        self.heard.setWordWrap(True)
        self.heard.setAccessibleName("What you said")

        self.held_label = QLabel()
        self.held_label.setWordWrap(True)
        self.held_edit = QLineEdit()
        self.held_edit.setAccessibleName("Sentence about to be sent")
        self.held_edit.textEdited.connect(lambda text: self._dispatch(self.turns.edit_held(text)))
        self.send_now = QPushButton("Send now")
        self.send_now.clicked.connect(self._send_now)
        cancel_send = QPushButton("Cancel send")
        cancel_send.clicked.connect(self._cancel_send)
        self.held_box = QFrame()
        held = QVBoxLayout(self.held_box)
        held.setContentsMargins(0, 0, 0, 0)
        held.addWidget(self.held_label)
        held.addWidget(self.held_edit)
        held_row = QHBoxLayout()
        held_row.addWidget(self.send_now)
        held_row.addWidget(cancel_send)
        held_row.addStretch(1)
        held.addLayout(held_row)
        self.held_box.setVisible(False)

        self.awaiting = QLabel("Say the correction.")
        self.awaiting.setVisible(False)
        self.notice = QLabel()
        self.notice.setAccessibleName("Live notice")
        self.problem = QLabel()
        self.problem.setWordWrap(True)
        self.problem.setAccessibleName("Live problem")
        self.problem.setStyleSheet("color: #b00020;")
        self.problem.setVisible(False)
        self.interrupt_button = QPushButton("Interrupt")
        self.interrupt_button.clicked.connect(lambda: self._dispatch(self.turns.interrupt()))
        hint = QLabel(
            'Talk as you would to a person. Start with "correction" or "that\'s wrong", then say what is right, to correct the '
            'last reply. Say "cancel" to take back a sentence.'
        )
        hint.setWordWrap(True)

        layout = QVBoxLayout(self)
        layout.addLayout(head)
        layout.addWidget(self.camera_error)
        layout.addWidget(self.preview)
        layout.addWidget(self.heard)
        layout.addWidget(self.held_box)
        layout.addWidget(self.awaiting)
        layout.addWidget(self.notice)
        layout.addWidget(self.problem)
        layout.addWidget(self.interrupt_button, alignment=Qt.AlignmentFlag.AlignLeft)
        layout.addWidget(hint)

    def _sync(self) -> None:
        t = self.turns
        self.phase.setText(PHASE_TEXT.get(t.phase, t.phase))
        self._quietly(self.mic_button, t.mic_on, "Mic on" if t.mic_on else "Mic off")
        self._quietly(self.edit_check, t.edit_before_send)
        self.heard.setText(f"You: {t.heard_text or '...'}")
        held = t.held
        self.held_box.setVisible(held is not None)
        if held is not None:
            if self.held_edit.text() != held:
                self.held_edit.setText(held)
            self.held_label.setText(
                "Sentence to send: change it, then press Send now."
                if t.edit_before_send
                else 'Sending soon. Cancel send, or say "cancel", takes it back.'
            )
        self.send_now.setEnabled(held is not None and held.strip() != "")
        self.awaiting.setVisible(t.awaiting_correction)
        self.notice.setText(t.notice or "")
        error = t.error or self._problem
        self.problem.setText(error)
        self.problem.setVisible(bool(error))
        self.interrupt_button.setEnabled(t.phase in ("speaking", "thinking"))

    @staticmethod
    def _quietly(button, checked: bool, text: str | None = None) -> None:
        """Sets a check box or toggle to the conversation's state without telling the conversation about it."""
        button.blockSignals(True)
        button.setChecked(checked)
        if text is not None:
            button.setText(text)
        button.blockSignals(False)

    # ---- the conversation ------------------------------------------------------------------------------------------

    def reply(self, text: str | None, busy: bool, error: str | None = None) -> None:
        """The chat's reply as it stands. The chat page calls this as the reply is written."""
        self._dispatch(self.turns.reply(text, busy, error))

    def end_conversation(self) -> None:
        self._dispatch(self.turns.end())
        self.shutdown()
        self.closed.emit()

    def shutdown(self) -> None:
        """Stops everything Live holds: the reply, the speech, the microphone and the camera. Safe to call twice."""
        if self._closed:
            return
        self._closed = True
        self._hold_timer.stop()
        self._listen_timer.stop()
        self._voice.stop()
        if self._mic is not None:
            self._mic.close()
        self._stop_camera()

    def _dispatch(self, effects: list[Effect]) -> None:
        if self._closed:
            return
        self._sync()
        for effect in effects:
            self._run(effect)

    def _run(self, e: Effect) -> None:
        if e.kind == "listen":
            self._request_listen(e.delay_ms)
        elif e.kind == "cancelListening":
            self._listen_timer.stop()
            self._set_mic(False)
        elif e.kind == "hold":
            self._hold_timer.stop()
            if e.timed:
                self._hold_timer.start(HOLD_MS)
        elif e.kind == "send":
            try:
                started = bool(self._send(e.text, self._pictures_now()))
            except Exception:
                started = False
            # The turn's own state changes after this effect is done, never inside it.
            QTimer.singleShot(0, lambda started=started: self._dispatch(self.turns.sent(started)))
        elif e.kind == "stopReply":
            self._stop_reply()
        elif e.kind == "say":
            self._voice.say(e.text, self._spoken)
        elif e.kind == "stopSpeech":
            self._voice.stop()
        elif e.kind == "record":
            self._save_correction(e.question, e.answer, e.correction)

    def _spoken(self, error: str | None) -> None:
        if error:
            self._problem = f"The reply could not be spoken: {error}"
        self._dispatch(self.turns.spoken_one())

    def _request_listen(self, delay_ms: int) -> None:
        if not self.turns.mic_on:
            return
        if delay_ms > 0:
            self._listen_timer.start(delay_ms)
        else:
            self._set_mic(True)

    def _listen_later(self) -> None:
        if self.turns.active and self.turns.mic_on:
            self._set_mic(True)

    def _set_mic(self, on: bool) -> None:
        if self._mic is not None:
            self._mic.set_enabled(on)

    def _send_now(self) -> None:
        self._hold_timer.stop()
        self._dispatch(self.turns.send_held())

    def _cancel_send(self) -> None:
        self._hold_timer.stop()
        self._dispatch(self.turns.cancel_hold())

    # ---- what the owner says ---------------------------------------------------------------------------------------

    def _utterance(self, pcm: bytes, _ms: int) -> None:
        """A sentence the owner finished, from the microphone. It is written down in the order it was said."""
        self._clips.append(wav_bytes(pcm))
        self._next_clip()

    def _next_clip(self) -> None:
        if self._transcribing or not self._clips:
            return
        hub = self._hub
        if hub is None:
            self._clips.clear()
            self._problem = "The hub is not connected, so what you say cannot be written down."
            self._dispatch(self.turns.failed(True))
            return
        self._transcribing = True
        clip = self._clips.popleft()
        run_task(lambda: transcribe_result(hub, clip), self._heard, lambda message: self._heard((ERROR, message)))

    def _heard(self, result: tuple[str, str]) -> None:
        self._transcribing = False
        kind, value = result
        if kind == OK:
            self._dispatch(self.turns.said(value))
        elif kind == UNAVAILABLE:
            self._dispatch(self.turns.failed(False, UNAVAILABLE_TEXT))
        else:
            self._problem = f"The sentence could not be written down: {value}"
            self._dispatch(self.turns.failed(True))
        self._next_clip()

    def _save_correction(self, question: str, answer: str, correction: str) -> None:
        hub = self._hub
        if hub is None:
            return
        run_task(
            lambda: hub.misty_teach(question, answer, correction),
            lambda _result: None,
            lambda message: self._set_problem(f"The correction was not saved: {message}"),
        )

    def _set_problem(self, message: str) -> None:
        self._problem = message
        self._sync()

    # ---- the camera ------------------------------------------------------------------------------------------------

    def _camera_toggled(self, on: bool) -> None:
        if not on:
            self._stop_camera()
            return
        try:
            if self._camera is None:
                self._camera = self._camera_factory()
            self._camera.start()
        except DeviceError as e:
            self.camera_error.setText(f"The camera is not available: {e}")
            self._quietly(self.camera_button, False)
            return
        except Exception as e:
            self.camera_error.setText(f"The camera is not available: {e}")
            self._quietly(self.camera_button, False)
            return
        self.camera_error.setText("")
        self._camera_on = True
        self.preview.setVisible(True)
        frame = getattr(self._camera, "frame", None)
        if frame is not None:
            frame.connect(self._show_frame)

    def _stop_camera(self) -> None:
        self._camera_on = False
        self.preview.setVisible(False)
        if self._camera is not None:
            self._camera.stop()

    def _show_frame(self, image) -> None:
        self.preview.setPixmap(QPixmap.fromImage(image).scaled(320, 240, Qt.AspectRatioMode.KeepAspectRatio))

    def _pictures_now(self) -> list[str]:
        if not self._camera_on or not self._can_picture or self._camera is None:
            return []
        png = self._camera.latest_png()
        return [picture_data_url("picture.png", png)] if png else []
