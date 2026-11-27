"""The Chat page: streamed replies from a chat model, with sampling options, files and pictures, a queue for messages
written during a reply, steering a reply mid-sentence, dictation, read-aloud, and Live (a conversation by voice)."""

from __future__ import annotations

import threading
from typing import Any

from PySide6.QtCore import Qt, QObject, QTimer, Signal
from PySide6.QtGui import QKeySequence, QPixmap, QShortcut
from PySide6.QtWidgets import (
    QCheckBox,
    QComboBox,
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QListWidget,
    QPlainTextEdit,
    QPushButton,
    QSpinBox,
    QTextBrowser,
    QVBoxLayout,
    QWidget,
)

from mm_gui.attachments import Attached, AttachmentBox, attach_paths
from mm_gui.audio import Camera, Dictation, DeviceError, Speaker
from mm_gui.chat_extras import OPEN_FILTER, picture_data_url, steer_cut
from mm_gui.hub import Hub, HubError
from mm_gui.live import LivePanel
from mm_gui.models import ROUTER, is_chat_model, model_label, without_tags_in_last_user_message
from mm_gui.pagebase import Page
from mm_gui.speech import ERROR, OK, UNAVAILABLE, speak_result, transcribe_result
from mm_gui.tasks import run_task


class _Bridge(QObject):
    """Carries each piece of a streamed reply from the worker thread to the window."""

    delta = Signal(str)
    routed = Signal(str)  # "main: qwen3-9b": the router chose the model for this reply


def _wire(message: dict) -> dict:
    """A message in the OpenAI shape: a message with pictures becomes text and `image_url` parts."""
    if not message.get("images"):
        return {"role": message["role"], "content": message["content"]}
    parts: list[dict] = [{"type": "text", "text": message["content"]}] if message["content"] else []
    parts += [{"type": "image_url", "image_url": {"url": url}} for url in message["images"]]
    return {"role": message["role"], "content": parts}


class ChatPage(Page):
    def __init__(self):
        super().__init__("Chat")
        self.model = QComboBox()
        self.model.setAccessibleName("Model")
        self.model.currentIndexChanged.connect(lambda _index: self._update_buttons())
        self.system = QLineEdit()
        self.system.setPlaceholderText("System prompt (optional)")
        self.system.setAccessibleName("System prompt")
        self.top_k = QSpinBox()
        self.top_k.setRange(0, 1000)
        self.top_k.setSpecialValueText("off")
        self.top_k.setAccessibleName("Top-k")
        self.top_k.setToolTip("Keep only the k most likely tokens. Off is 0.")
        self.seed = QSpinBox()
        self.seed.setRange(-1, 2_147_483_647)
        self.seed.setSpecialValueText("random")
        self.seed.setValue(-1)
        self.seed.setAccessibleName("Seed")
        self.seed.setToolTip("A fixed seed makes a reply repeat. Random is the default.")
        self.transcript = QTextBrowser()
        self.transcript.setOpenExternalLinks(True)
        self.transcript.setAccessibleName("Conversation")
        self.input = QPlainTextEdit()
        self.input.setPlaceholderText("Message (Ctrl+Enter sends, or queues while the model answers)")
        self.input.setAccessibleName("Message")
        self.input.setMaximumHeight(110)
        self.input.textChanged.connect(self._update_buttons)
        self.attachments = AttachmentBox()

        self.send_button = QPushButton("Send")
        self.send_button.clicked.connect(self.send)
        self.steer_button = QPushButton("Steer")
        self.steer_button.setToolTip("Write a note now. It goes in after the next sentence of this reply; the text so far is kept.")
        self.steer_button.clicked.connect(self.steer_now)
        self.stop_button = QPushButton("Stop")
        self.stop_button.clicked.connect(self.stop)
        self.stop_button.setEnabled(False)
        new_chat = QPushButton("New chat")
        new_chat.clicked.connect(self.new_chat)
        self.attach_button = QPushButton("Attach file...")
        self.attach_button.setToolTip("Attach a text, source, JSON or PDF file. Its text goes in with the message, cut to 2000 characters.")
        self.attach_button.clicked.connect(lambda: self._pick_files(pictures=False))
        self.picture_button = QPushButton("Picture...")
        self.picture_button.setToolTip("Attach a picture. It needs a model that can see pictures (the router then needs one too).")
        self.picture_button.clicked.connect(lambda: self._pick_files(pictures=True))
        self.camera_button = QPushButton("Camera")
        self.camera_button.setCheckable(True)
        self.camera_button.toggled.connect(self._camera_toggled)
        self.snap_button = QPushButton("Take picture")
        self.snap_button.setVisible(False)
        self.snap_button.clicked.connect(self._take_picture)
        self.preview = QLabel()
        self.preview.setFixedSize(320, 240)
        self.preview.setAccessibleName("Camera preview")
        self.preview.setVisible(False)
        self.dictate_button = QPushButton("Dictate")
        self.dictate_button.setToolTip("Say the message instead of typing it.")
        self.dictate_button.clicked.connect(self._toggle_dictation)
        self.live_button = QPushButton("Live")
        self.live_button.setCheckable(True)
        self.live_button.setToolTip("Talk with the model: each sentence you say is sent, and the reply is read aloud.")
        self.live_button.toggled.connect(self._toggle_live)
        self.read_aloud = QCheckBox("Read replies aloud")
        self.read_last = QPushButton("Read last reply")
        self.read_last.clicked.connect(lambda: self._speak(self._last_reply_text()))
        self.stop_speaking = QPushButton("Stop speaking")
        self.stop_speaking.clicked.connect(self._stop_speaking)

        self.queue_list = QListWidget()
        self.queue_list.setMaximumHeight(80)
        self.queue_list.setAccessibleName("Queued messages")
        self.queue_list.setVisible(False)
        self.remove_queued = QPushButton("Remove queued")
        self.remove_queued.clicked.connect(self._remove_queued)
        self.remove_queued.setVisible(False)
        self.steer_label = QLabel()
        self.steer_label.setWordWrap(True)
        self.cancel_steer_button = QPushButton("Cancel steer")
        self.cancel_steer_button.clicked.connect(self._cancel_steer)
        self.steer_row = QWidget()
        steer_layout = QHBoxLayout(self.steer_row)
        steer_layout.setContentsMargins(0, 0, 0, 0)
        steer_layout.addWidget(self.steer_label, 1)
        steer_layout.addWidget(self.cancel_steer_button)
        self.steer_row.setVisible(False)

        QShortcut(QKeySequence("Escape"), self).activated.connect(self.stop)
        for sequence in ("Ctrl+Return", "Ctrl+Enter"):
            QShortcut(QKeySequence(sequence), self.input).activated.connect(self.send)

        self.messages: list[dict] = []  # {"role", "content", and "images" or "model"}; the last may be the reply being written
        self._cancel: threading.Event | None = None
        self._queue: list[dict] = []  # messages written during a reply: {"text", "images"?}
        self._steer: dict | None = None  # a steering note waiting for the next sentence end of the reply
        self._steer_note: str | None = None  # the note whose reply was cut: it goes next
        self._info: dict[str, dict] = {}  # the models of the list, by id
        self._live: LivePanel | None = None
        self.live_options: dict[str, Any] = {}  # passed to the Live panel (its microphone, voice and camera)
        self._dictation: Dictation | None = None
        self._dictating = False
        self._speaker_obj: Speaker | None = None
        self._camera_obj: Camera | None = None
        self._camera_connected = False
        self._bridge = _Bridge()
        self._bridge.delta.connect(self._on_delta)
        self._bridge.routed.connect(self._on_routed)
        self._render_timer = QTimer(self)
        self._render_timer.setSingleShot(True)
        self._render_timer.setInterval(80)
        self._render_timer.timeout.connect(self._render)

        top = QHBoxLayout()
        top.addWidget(self.model, 2)
        top.addWidget(self.system, 3)
        options = QHBoxLayout()
        options.addWidget(QLabel("Top-k"))
        options.addWidget(self.top_k)
        options.addWidget(QLabel("Seed"))
        options.addWidget(self.seed)
        options.addStretch(1)
        bottom = QHBoxLayout()
        bottom.addWidget(self.input, 1)
        buttons = QVBoxLayout()
        for button in (self.send_button, self.steer_button, self.stop_button, new_chat):
            buttons.addWidget(button)
        bottom.addLayout(buttons)
        tools = QHBoxLayout()
        for widget in (self.attach_button, self.picture_button, self.camera_button, self.snap_button, self.dictate_button, self.live_button):
            tools.addWidget(widget)
        tools.addStretch(1)
        speech = QHBoxLayout()
        for widget in (self.read_aloud, self.read_last, self.stop_speaking):
            speech.addWidget(widget)
        speech.addStretch(1)
        self._live_holder = QVBoxLayout()
        layout = QVBoxLayout(self)
        layout.addLayout(top)
        layout.addLayout(options)
        layout.addWidget(self.transcript, 1)
        layout.addLayout(self._live_holder)
        layout.addWidget(self.preview)
        layout.addWidget(self.queue_list)
        layout.addWidget(self.remove_queued, alignment=Qt.AlignmentFlag.AlignLeft)
        layout.addWidget(self.steer_row)
        layout.addWidget(self.attachments)
        layout.addLayout(bottom)
        layout.addLayout(tools)
        layout.addLayout(speech)
        layout.addWidget(self.status)
        self._update_buttons()

    # ---- the models ------------------------------------------------------------------------------------------------

    def refresh(self) -> None:
        self.ask("Loading the models", lambda hub: hub.chat_models(), self._models_listed)

    def _models_listed(self, models: list[dict]) -> None:
        chosen = self.model.currentData()
        self.model.clear()
        items = [m for m in models if is_chat_model(m)]
        self._info = {str(m.get("id")): m for m in items}
        for m in items:
            self.model.addItem(model_label(m), m.get("id"))
        # A new chat starts on the router when it has a model that can answer; until then on the first real model.
        default = next(
            (i for i, m in enumerate(items) if m.get("router") and m.get("ready")),
            next((i for i, m in enumerate(items) if not m.get("router")), 0),
        )
        index = self.model.findData(chosen) if chosen else -1
        self.model.setCurrentIndex(index if index >= 0 else default)
        if self.model.count() == 0:
            self.status.setText("No chat model is listed. Download one first; the hub's dashboard can do it.")
        self._update_buttons()

    def _can_picture(self) -> bool:
        info = self._info.get(str(self.model.currentData()))
        return bool(info and (info.get("vision") or info.get("router")))

    # ---- sending ---------------------------------------------------------------------------------------------------

    def send(self) -> None:
        text = self.input.toPlainText().strip()
        items = self.attachments.items()
        if self.hub is None or (not text and not items):
            return
        message = self._compose(text, items)
        if self._cancel is not None:
            # A message written during a reply waits for it, and goes next.
            self._queue.append(message)
            self._clear_composer()
            self._show_queue()
            self.status.setText(f"Queued: it goes after this reply ({len(self._queue)} waiting).")
            return
        if self._start(message):
            self._clear_composer()

    def _clear_composer(self) -> None:
        self.input.clear()
        self.attachments.take()

    @staticmethod
    def _compose(text: str, items: list[Attached]) -> dict:
        blocks = [text] if text else []
        blocks += [item.block for item in items if item.block]
        message: dict[str, Any] = {"text": "\n\n".join(blocks)}
        images = [item.picture for item in items if item.picture]
        if images:
            message["images"] = images
        return message

    def _start(self, message: dict) -> bool:
        """Starts the reply to `message` on a worker thread. False when it cannot start (no model, no hub)."""
        if self.hub is None:
            return False
        model = self.model.currentData()
        if not model:
            self.status.setText("Choose a model first.")
            return False
        hub = self.hub
        user: dict[str, Any] = {"role": "user", "content": message["text"]}
        if message.get("images"):
            user["images"] = list(message["images"])
        self.messages += [user, {"role": "assistant", "content": ""}]
        history = self._history()
        options = self._options()
        cancel = threading.Event()
        self._cancel = cancel
        self._steer = None
        self._steer_note = None
        self.steer_row.setVisible(False)
        self._update_buttons()
        self.status.setText("Writing...")
        self._render()
        self._notify_live(True)
        run_task(
            lambda: self._answer(hub, model, history, cancel, options),
            lambda _answer: self._reply_done(cancel),
            self._reply_failed,
        )
        return True

    def _answer(self, hub: Hub, model: str, history: list[dict], cancel: threading.Event, options: dict) -> str:
        """On the worker thread. For the router: ask it which model suits the conversation, say so, then send the
        conversation (without the role tags) to that model."""
        if model == ROUTER:
            pick = hub.router_pick(history)
            if not pick.get("model"):
                why = "; ".join(str(r) for r in pick.get("reasons") or []) or "no model is ready"
                raise HubError(f"The router has no model to send this to: {why}. Choose models on the Models page.")
            self._bridge.routed.emit(f"{pick.get('role') or '?'}: {pick['model']}")
            return hub.chat_stream(
                str(pick["model"]),
                without_tags_in_last_user_message(history),
                self._bridge.delta.emit,
                cancel=cancel,
                options=options,
            )
        return hub.chat_stream(model, history, self._bridge.delta.emit, cancel=cancel, options=options)

    def _history(self) -> list[dict]:
        system = self.system.text().strip()
        out = [{"role": "system", "content": system}] if system else []
        return out + [_wire(m) for m in self.messages[:-1]]

    def _options(self) -> dict:
        """The sampling settings the owner set; a setting left at off or random is not sent."""
        options: dict[str, int] = {}
        if self.top_k.value() > 0:
            options["top_k"] = self.top_k.value()
        if self.seed.value() >= 0:
            options["seed"] = self.seed.value()
        return options

    def stop(self) -> None:
        if self._cancel is not None:
            self._cancel.set()
            self.status.setText("Stopping...")

    def new_chat(self) -> None:
        if self._cancel is None:
            self.messages = []
            self._queue = []
            self._show_queue()
            self._render()
            self.status.setText("")

    # ---- the queue and steering ------------------------------------------------------------------------------------

    def _show_queue(self) -> None:
        self.queue_list.clear()
        for message in self._queue:
            self.queue_list.addItem(f"Next: {message['text'][:80]}")
        has = bool(self._queue)
        self.queue_list.setVisible(has)
        self.remove_queued.setVisible(has)

    def _remove_queued(self) -> None:
        row = self.queue_list.currentRow()
        if 0 <= row < len(self._queue):
            del self._queue[row]
            self._show_queue()

    def steer_now(self) -> None:
        """A steering note: the reply is cut after its next sentence, and the note goes in next."""
        text = self.input.toPlainText().strip()
        if self._cancel is None or not text or self._steer is not None:
            return
        self._steer = {"text": text, "from": len(self._last_reply_text())}
        self.input.clear()
        self.steer_label.setText(f'Steer: "{text}" goes in after the next sentence. The text so far is kept.')
        self.steer_row.setVisible(True)
        self._update_buttons()

    def _cancel_steer(self) -> None:
        if self._steer is None:
            return
        if not self.input.toPlainText().strip():
            self.input.setPlainText(self._steer["text"])  # the note goes back into the box
        self._steer = None
        self.steer_row.setVisible(False)
        self._update_buttons()

    # ---- replies ---------------------------------------------------------------------------------------------------

    def _last_reply_text(self) -> str:
        if self.messages and self.messages[-1]["role"] == "assistant":
            return str(self.messages[-1]["content"])
        return ""

    def _on_routed(self, label: str) -> None:
        self.messages[-1]["model"] = label
        self._render()

    def _on_delta(self, text: str) -> None:
        if not self.messages or self._steer_note is not None:
            return  # a reply cut by a steering note takes no more of its text
        self.messages[-1]["content"] += text
        steer = self._steer
        if steer is not None and self._cancel is not None:
            content = self.messages[-1]["content"]
            cut = steer_cut(content, steer["from"])
            if cut is not None:
                self.messages[-1]["content"] = content[:cut].rstrip()
                self._steer_note = steer["text"]
                self._steer = None
                self.steer_row.setVisible(False)
                self._cancel.set()
                self.status.setText("Steering: the reply is cut after this sentence.")
        if not self._render_timer.isActive():
            self._render_timer.start()
        self._notify_live(True)

    def _reply_done(self, cancel: threading.Event) -> None:
        note, self._steer_note = self._steer_note, None
        self._steer = None
        self.steer_row.setVisible(False)
        if note is not None:
            self._queue.insert(0, {"text": note})
            self._show_queue()
            self.status.setText("Your steering note goes in next.")
        else:
            self.status.setText("Stopped." if cancel.is_set() else "")
        reply = self._last_reply_text()
        self._finish()
        self._notify_live(False)
        if reply and self.read_aloud.isChecked() and not cancel.is_set() and note is None:
            self._speak(reply)

    def _reply_failed(self, message: str) -> None:
        if self.messages and self.messages[-1]["role"] == "assistant" and not self.messages[-1]["content"]:
            self.messages.pop()  # the reply never started: keep only the question
        self._steer = None
        self._steer_note = None
        self.steer_row.setVisible(False)
        self.status.setText(f"The reply failed: {message}")
        self._finish()
        self._notify_live(False, message)

    def _finish(self) -> None:
        self._cancel = None
        self._render()
        self._update_buttons()
        QTimer.singleShot(0, self._drain)

    def _drain(self) -> None:
        """Sends the next queued message once no reply is running."""
        if self._cancel is not None or not self._queue or self.hub is None:
            return
        if self._start(self._queue[0]):
            self._queue.pop(0)
            self._show_queue()

    def _update_buttons(self) -> None:
        busy = self._cancel is not None
        has_text = bool(self.input.toPlainText().strip())
        self.send_button.setText("Queue" if busy else "Send")
        self.send_button.setEnabled(True)  # send() ignores an empty message; a file that is still being read cannot leave it stale
        self.steer_button.setEnabled(busy and has_text and self._steer is None)
        self.stop_button.setEnabled(busy)
        self.model.setEnabled(not busy)
        can_picture = self._can_picture()
        self.picture_button.setEnabled(can_picture)
        self.camera_button.setEnabled(can_picture)
        self.read_last.setEnabled(bool(self._last_reply_text()))

    def _render(self) -> None:
        self._render_timer.stop()
        parts = []
        for m in self.messages:
            who = "You" if m["role"] == "user" else (f"Model ({m['model']})" if m.get("model") else "Model")
            # A backslash before < keeps a model's markup as plain text instead of letting the viewer render it.
            body = (m["content"] or "...").replace("<", "\\<")
            if m.get("images"):
                body += f"\n\n*{len(m['images'])} picture(s) attached*"
            parts.append(f"**{who}**\n\n{body}")
        self.transcript.setMarkdown("\n\n---\n\n".join(parts))
        bar = self.transcript.verticalScrollBar()
        bar.setValue(bar.maximum())

    # ---- files and pictures ----------------------------------------------------------------------------------------

    def _pick_files(self, pictures: bool) -> None:
        filter_text = "Pictures (*.png *.jpg *.jpeg *.gif *.webp *.bmp)" if pictures else OPEN_FILTER
        paths, _ = QFileDialog.getOpenFileNames(self, "Attach files", "", filter_text)
        if paths:
            attach_paths(
                self.attachments,
                paths,
                hub=self.hub,
                can_picture=self._can_picture(),
                report=lambda message: self.status.setText(message),
            )
            self._update_buttons()

    def _camera_toggled(self, on: bool) -> None:
        if not on:
            self._stop_camera()
            return
        try:
            if self._camera_obj is None:
                self._camera_obj = Camera(self)
            self._camera_obj.start()
        except DeviceError as e:
            self.status.setText(f"The camera is not available: {e}")
            self.camera_button.setChecked(False)
            return
        if not self._camera_connected:
            self._camera_obj.frame.connect(self._show_frame)
            self._camera_connected = True
        self.snap_button.setVisible(True)
        self.preview.setVisible(True)

    def _stop_camera(self) -> None:
        self.snap_button.setVisible(False)
        self.preview.setVisible(False)
        if self._camera_obj is not None:
            self._camera_obj.stop()

    def _show_frame(self, image) -> None:
        self.preview.setPixmap(QPixmap.fromImage(image).scaled(320, 240, Qt.AspectRatioMode.KeepAspectRatio))

    def _take_picture(self) -> None:
        if not self._can_picture():
            self.status.setText("This model cannot read pictures. Choose a model that can see, or the router.")
            return
        png = self._camera_obj.latest_png() if self._camera_obj is not None else None
        if not png:
            self.status.setText("No picture yet: the camera is still starting.")
            return
        self.attachments.add(Attached("picture.png", picture=picture_data_url("picture.png", png)))
        self._update_buttons()

    # ---- dictation and read-aloud ----------------------------------------------------------------------------------

    def _toggle_dictation(self) -> None:
        if not self._dictating:
            if self._dictation is None:
                self._dictation = Dictation(self)
            try:
                self._dictation.start()
            except DeviceError as e:
                self.status.setText(f"Dictation: {e}")
                return
            self._dictating = True
            self.dictate_button.setText("Stop listening")
            self.status.setText("Listening. Press Stop listening to write down what was said.")
            return
        wav = self._dictation.stop() if self._dictation is not None else b""
        self._dictating = False
        self.dictate_button.setText("Dictate")
        hub = self.hub
        if hub is None:
            return
        self.status.setText("Transcribing...")
        run_task(lambda: transcribe_result(hub, wav), self._dictated, lambda message: self._dictated((ERROR, message)))

    def _dictated(self, result: tuple[str, str]) -> None:
        kind, value = result
        if kind == OK:
            current = self.input.toPlainText().strip()
            self.input.setPlainText(f"{current} {value}".strip() if current else value)
            self.status.setText("")
        elif kind == UNAVAILABLE:
            self.status.setText("Dictation is not available in this build.")
        else:
            self.status.setText(f"Dictation: {value}")

    def _speak(self, text: str) -> None:
        hub = self.hub
        if hub is None or not text.strip():
            return
        run_task(lambda: speak_result(hub, text), self._spoken, lambda message: self.status.setText(f"Read aloud failed: {message}"))

    def _spoken(self, result: tuple[str, Any]) -> None:
        kind, value = result
        if kind != OK:
            self.status.setText(f"Read aloud failed: {value}")
            return
        if self._speaker_obj is None:
            self._speaker_obj = Speaker(self)
        self._speaker_obj.play(value, lambda error: self.status.setText(f"Read aloud failed: {error}") if error else None)

    def _stop_speaking(self) -> None:
        if self._speaker_obj is not None:
            self._speaker_obj.stop()

    # ---- Live -----------------------------------------------------------------------------------------------------

    def _toggle_live(self, on: bool) -> None:
        if not on:
            self._close_live()
            return
        if self._live is not None:
            return
        panel = LivePanel(
            hub=self.hub,
            send=self._live_send,
            stop=self.stop,
            can_picture=self._can_picture(),
            parent=self,
            **self.live_options,
        )
        panel.closed.connect(self._close_live)
        self._live = panel
        self._live_holder.addWidget(panel)

    def _close_live(self) -> None:
        panel, self._live = self._live, None
        self.live_button.blockSignals(True)
        self.live_button.setChecked(False)
        self.live_button.blockSignals(False)
        if panel is not None:
            panel.shutdown()
            self._live_holder.removeWidget(panel)
            panel.setParent(None)
            panel.deleteLater()

    def _live_send(self, text: str, images: list[str]) -> bool:
        """What the owner said in Live, sent as the next message of this chat. False when no reply can start now."""
        if self._cancel is not None:
            return False
        message: dict[str, Any] = {"text": text}
        if images:
            message["images"] = images
        return self._start(message)

    def _notify_live(self, busy: bool, error: str | None = None) -> None:
        if self._live is not None:
            self._live.reply(self._last_reply_text(), busy, error)
