"""The Misty page: chat with Misty, the agent inside the hub. She picks a model, searches your documents and uses tools; a
tool that changes something waits for the owner's answer in a dialog. Messages written during a reply queue, a reply can
be steered, and each reply can be rated (what she learns is in the dialog that opens from this page)."""

from __future__ import annotations

import json
import threading
from typing import Any

from PySide6.QtCore import Qt, QObject, QTimer, Signal
from PySide6.QtGui import QKeySequence, QShortcut
from PySide6.QtWidgets import (
    QCheckBox,
    QComboBox,
    QFileDialog,
    QHBoxLayout,
    QInputDialog,
    QLabel,
    QListWidget,
    QMessageBox,
    QPlainTextEdit,
    QPushButton,
    QTextBrowser,
    QVBoxLayout,
    QWidget,
)

from mm_gui.attachments import Attached, AttachmentBox, attach_paths
from mm_gui.chat_extras import OPEN_FILTER, steer_cut
from mm_gui.hub import Hub
from mm_gui.learn import LearnDialog
from mm_gui.pagebase import Page
from mm_gui.tasks import run_task

RATING_TEXT = {"good": "good", "bad": "bad", "no_tools": "no tools", "needs_tools": "needs tools", "corrected": "corrected"}


class _MistyBridge(QObject):
    """Carries each page of a run's events from the worker thread to the window."""

    page = Signal(dict)


def _compose(text: str, items: list[Attached]) -> dict:
    """The message as the model reads it (files' text as data) and as the transcript shows it (file names only)."""
    blocks = [text] if text else []
    blocks += [item.block for item in items if item.block]
    shown = text or "(no text)"
    if items:
        shown += "\n\n[attached: " + ", ".join(item.name for item in items) + "]"
    return {"text": "\n\n".join(blocks), "shown": shown}


class MistyPage(Page):
    ROLES = [("Chosen by the router", ""), ("Fast", "fast"), ("Main", "main"), ("Deep", "deep")]
    DOCS = [("Router decides", None), ("Always search my documents", True), ("Never search", False)]

    def __init__(self):
        super().__init__("Misty")
        self.ready = QLabel("Checking...")
        self.ready.setWordWrap(True)
        self.ready.setAccessibleName("Misty readiness")
        self.role = QComboBox()
        self.role.setAccessibleName("Model")
        for label, value in self.ROLES:
            self.role.addItem(label, value)
        self.docs = QComboBox()
        self.docs.setAccessibleName("Documents")
        for label, value in self.DOCS:
            self.docs.addItem(label, value)
        self.no_tools = QCheckBox("No tools")
        self.no_tools.setToolTip("Misty answers without using her tools for this message.")
        self.transcript = QTextBrowser()
        self.transcript.setOpenExternalLinks(False)
        self.transcript.setAccessibleName("Conversation with Misty")
        self.input = QPlainTextEdit()
        self.input.setPlaceholderText("Ask Misty (Ctrl+Enter sends, or queues while she answers)")
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
        attach = QPushButton("Attach file...")
        attach.setToolTip("Attach a text, source, JSON or PDF file. Its text goes in with the message, cut to 2000 characters.")
        attach.clicked.connect(self._pick_files)
        self.learn_button = QPushButton("What she learns...")
        self.learn_button.setToolTip("The verdicts you gave her, what she learned from them, and the training data.")
        self.learn_button.clicked.connect(self._learn)

        self.queue_list = QListWidget()
        self.queue_list.setMaximumHeight(80)
        self.queue_list.setVisible(False)
        self.remove_queued = QPushButton("Remove queued")
        self.remove_queued.clicked.connect(self._remove_queued)
        self.remove_queued.setVisible(False)
        self.steer_label = QLabel()
        self.steer_label.setWordWrap(True)
        cancel_steer = QPushButton("Cancel steer")
        cancel_steer.clicked.connect(self._cancel_steer)
        self.steer_row = QWidget()
        steer_layout = QHBoxLayout(self.steer_row)
        steer_layout.setContentsMargins(0, 0, 0, 0)
        steer_layout.addWidget(self.steer_label, 1)
        steer_layout.addWidget(cancel_steer)
        self.steer_row.setVisible(False)

        # Ratings: the owner's verdict on one of her replies (the reply is picked here; the newest is the default).
        self.rate_pick = QComboBox()
        self.rate_pick.setAccessibleName("Reply to rate")
        self.rate_pick.currentIndexChanged.connect(lambda _index: self._show_rated())
        self.rate_buttons: list[QPushButton] = []
        rate_row = QHBoxLayout()
        rate_row.addWidget(QLabel("Rate:"))
        rate_row.addWidget(self.rate_pick, 1)
        for label, text in (*self.RATINGS, ("corrected", "Correct it...")):
            button = QPushButton(text)
            button.setAccessibleName(f"Rate the reply {text.lower()}")
            button.clicked.connect(lambda _checked=False, label=label: self._correct() if label == "corrected" else self._rate(label))
            rate_row.addWidget(button)
            self.rate_buttons.append(button)
        self.rate_state = QLabel()
        self.rate_state.setAccessibleName("Rating")
        self.rate_state.setWordWrap(True)

        QShortcut(QKeySequence("Escape"), self).activated.connect(self.stop)
        for sequence in ("Ctrl+Return", "Ctrl+Enter"):
            QShortcut(QKeySequence(sequence), self.input).activated.connect(self.send)

        self.session: str | None = None
        self.turns: list[dict] = []  # {"you", "text", "notes", "role", "cites", "error", "run", "rated", "steered"}
        self._cancel: threading.Event | None = None
        self._asked: set[str] = set()
        self._queue: list[dict] = []
        self._steer: dict | None = None  # a steering note waiting for the next sentence end of the reply
        self._steer_note: str | None = None  # the note whose reply was cut: it goes next
        self._bridge = _MistyBridge()
        self._bridge.page.connect(self._on_page)
        self._render_timer = QTimer(self)
        self._render_timer.setSingleShot(True)
        self._render_timer.setInterval(80)
        self._render_timer.timeout.connect(self._render)

        top = QHBoxLayout()
        top.addWidget(self.role, 1)
        top.addWidget(self.docs, 1)
        top.addWidget(self.no_tools)
        top.addWidget(self.learn_button)
        bottom = QHBoxLayout()
        bottom.addWidget(self.input, 1)
        buttons = QVBoxLayout()
        for button in (self.send_button, self.steer_button, self.stop_button, new_chat):
            buttons.addWidget(button)
        bottom.addLayout(buttons)
        tools = QHBoxLayout()
        tools.addWidget(attach)
        tools.addStretch(1)
        layout = QVBoxLayout(self)
        layout.addWidget(self.ready)
        layout.addLayout(top)
        layout.addWidget(self.transcript, 1)
        layout.addLayout(rate_row)
        layout.addWidget(self.rate_state)
        layout.addWidget(self.queue_list)
        layout.addWidget(self.remove_queued)
        layout.addWidget(self.steer_row)
        layout.addWidget(self.attachments)
        layout.addLayout(bottom)
        layout.addLayout(tools)
        layout.addWidget(self.status)
        self._update_buttons()

    RATINGS = [("good", "Good"), ("bad", "Bad"), ("no_tools", "No tools"), ("needs_tools", "Needs tools")]

    # ---- readiness -------------------------------------------------------------------------------------------------

    def refresh(self) -> None:
        self.ask("Checking Misty", lambda hub: hub.misty_status(), self._status)

    def shown(self) -> None:
        if self.hub is not None and self._cancel is None:
            self.refresh()

    def _status(self, st: dict) -> None:
        roster = {r.get("role"): r for r in st.get("roster") or []}
        names = [f"{role} = {roster[role].get('model')}" for role in ("fast", "main", "deep") if roster.get(role, {}).get("status") == "ready"]
        if st.get("ready"):
            self.ready.setText("Ready: " + ", ".join(names))
        else:
            hint = (st.get("next") or ["No language model is ready."])[0]
            self.ready.setText(f"Not ready. {hint}")

    # ---- sending ---------------------------------------------------------------------------------------------------

    def send(self) -> None:
        text = self.input.toPlainText().strip()
        items = self.attachments.items()
        if self.hub is None or (not text and not items):
            return
        message = _compose(text, items)
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

    def _start(self, message: dict) -> bool:
        if self.hub is None:
            return False
        hub = self.hub
        args: dict[str, Any] = {"input": message["text"], "approvals": "ask"}
        if self.session:
            args["session"] = self.session
        else:
            args["new_session"] = True
        if self.role.currentData():
            args["role"] = self.role.currentData()
        if self.docs.currentData() is not None:
            args["rag"] = self.docs.currentData()
        if self.no_tools.isChecked():
            args["no_tools"] = True
        self.turns.append({"you": message.get("shown", message["text"]), "text": "", "notes": [], "role": "", "cites": [], "error": "", "run": "", "rated": "", "steered": False})
        cancel = threading.Event()
        self._cancel = cancel
        self._asked = set()
        self._steer = None
        self.steer_row.setVisible(False)
        self._update_buttons()
        self.status.setText("Working...")
        self._render()
        run_task(
            lambda: hub.misty_run(args, self._bridge.page.emit, cancel),
            lambda _page: self._finished(cancel),
            self._failed,
        )
        return True

    def stop(self) -> None:
        if self._cancel is not None:
            self._cancel.set()
            self.status.setText("Stopping...")

    def new_chat(self) -> None:
        if self._cancel is None:
            self.session = None
            self.turns = []
            self._queue = []
            self._show_queue()
            self._refresh_rate_choices()
            self._render()
            self.status.setText("")

    def _show_queue(self) -> None:
        self.queue_list.clear()
        for message in self._queue:
            self.queue_list.addItem(f"Next: {message.get('shown', message['text'])[:80]}")
        has = bool(self._queue)
        self.queue_list.setVisible(has)
        self.remove_queued.setVisible(has)

    def _remove_queued(self) -> None:
        row = self.queue_list.currentRow()
        if 0 <= row < len(self._queue):
            del self._queue[row]
            self._show_queue()

    def steer_now(self) -> None:
        text = self.input.toPlainText().strip()
        if self._cancel is None or not text or self._steer is not None or not self.turns:
            return
        self._steer = {"text": text, "from": len(self.turns[-1]["text"])}
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

    def _drain(self) -> None:
        """Sends the next queued message once no reply is running."""
        if self._cancel is not None or not self._queue or self.hub is None:
            return
        if self._start(self._queue[0]):
            self._queue.pop(0)
            self._show_queue()

    def _confirm(self, approval: dict) -> bool:
        """Ask the owner about one tool call. A separate method so a test can answer without a dialog."""
        args = json.dumps(approval.get("args"), indent=2, ensure_ascii=False)[:1500]
        box = QMessageBox(self)
        box.setWindowTitle("Misty asks")
        box.setText(f"Misty wants to use {approval.get('tool')} ({approval.get('class')}).")
        box.setInformativeText("Allow it?")
        box.setDetailedText(args)
        box.setStandardButtons(QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No)
        box.setDefaultButton(QMessageBox.StandardButton.No)
        return box.exec() == QMessageBox.StandardButton.Yes

    def _on_page(self, page: dict) -> None:
        if not self.turns:
            return
        turn = self.turns[-1]
        if "start" in page:
            started = page["start"]
            self.session = started.get("session") or self.session
            turn["run"] = str(started.get("run") or "")
            return
        for e in page.get("events") or []:
            kind = e.get("type")
            if kind == "token":
                if not turn["steered"]:
                    turn["text"] += e.get("text") or ""
            elif kind == "model":
                turn["role"] = e.get("role") or turn["role"]
            elif kind == "tool":
                turn["notes"].append(f"{e.get('name')}: {e.get('status')}")
            elif kind == "escalate":
                if not turn["steered"]:
                    turn["text"] = ""
                turn["notes"].append(f"moved up from {e.get('from')} to {e.get('to')}: {e.get('why')}")
            elif kind == "retrieve":
                turn["notes"].append(f"searched your documents: {e.get('used')} of {e.get('hits')} passages used")
            elif kind == "transcript":
                turn["notes"].append(f"heard: {e.get('text')}")
            elif kind == "error":
                turn["error"] = e.get("message") or "the run failed"
            elif kind == "final" and not turn["steered"]:
                turn["text"] = e.get("text") if e.get("text") is not None else turn["text"]
                turn["cites"] = e.get("citations") or []
                turn["role"] = e.get("role") or turn["role"]
                if e.get("error") and not turn["error"]:
                    turn["error"] = e["error"]
        self._steer_if_cut(turn)
        if not self._render_timer.isActive():
            self._render_timer.start()
        for approval in page.get("pending") or []:
            key = approval.get("approval")
            if key and key not in self._asked and self.hub is not None:
                self._asked.add(key)
                allow = self._confirm(approval)
                hub = self.hub
                run_task(lambda hub=hub, key=key, allow=allow: hub.misty_approve(key, allow), lambda _r: None, self._approval_failed)

    def _steer_if_cut(self, turn: dict) -> None:
        """A steering note cuts the reply after its next sentence: the text so far is kept and the run is stopped."""
        steer = self._steer
        if steer is None or turn["steered"] or self._cancel is None:
            return
        cut = steer_cut(turn["text"], steer["from"])
        if cut is None:
            return
        turn["text"] = turn["text"][:cut].rstrip()
        turn["steered"] = True
        turn["run"] = ""  # a cut reply is not the whole reply, so it cannot be rated
        self._steer_note = steer["text"]
        self._steer = None
        self.steer_row.setVisible(False)
        self._cancel.set()  # misty_run tells the hub to stop; the last events still arrive
        self.status.setText("Steering: the reply is cut after this sentence.")

    def _approval_failed(self, message: str) -> None:
        self.status.setText(f"Your answer did not reach Misty: {message}")

    def _take_steer_note(self) -> bool:
        """A note that cut its reply goes out next, ahead of the queue. A note still waiting for a sentence goes back
        into the box, so nothing the owner typed is lost."""
        if self._steer is not None:
            self._cancel_steer()
        note, self._steer_note = self._steer_note, None
        if note is None:
            return False
        self._queue.insert(0, {"text": note})
        self._show_queue()
        return True

    def _finished(self, cancel: threading.Event) -> None:
        if self._take_steer_note():
            self.status.setText("Your steering note goes in next.")
        else:
            self.status.setText("Stopped." if cancel.is_set() else "")
        self._end()

    def _failed(self, message: str) -> None:
        if self.turns and not self.turns[-1]["text"]:
            self.turns[-1]["error"] = message
        self._take_steer_note()
        self.status.setText(f"Misty could not answer: {message}")
        self._end()

    def _end(self) -> None:
        self._cancel = None
        self._update_buttons()
        self._refresh_rate_choices()
        self._render()
        QTimer.singleShot(0, self._drain)

    def _update_buttons(self) -> None:
        busy = self._cancel is not None
        has_text = bool(self.input.toPlainText().strip())
        self.send_button.setText("Queue" if busy else "Send")
        self.send_button.setEnabled(True)  # send() ignores an empty message, so the button needs no other state
        self.steer_button.setEnabled(busy and has_text and self._steer is None)
        self.stop_button.setEnabled(busy)
        self.role.setEnabled(not busy)
        self.docs.setEnabled(not busy)
        self.no_tools.setEnabled(not busy)
        rateable = not busy and self.rate_pick.count() > 0
        for button in self.rate_buttons:
            button.setEnabled(rateable)
        self.rate_pick.setEnabled(rateable)

    # ---- ratings ---------------------------------------------------------------------------------------------------

    def _refresh_rate_choices(self) -> None:
        self.rate_pick.blockSignals(True)
        self.rate_pick.clear()
        for index, turn in enumerate(self.turns):
            if turn.get("run"):
                self.rate_pick.addItem(f"Reply {index + 1}: {(turn['text'] or '...')[:60]}", index)
        if self.rate_pick.count():
            self.rate_pick.setCurrentIndex(self.rate_pick.count() - 1)
        self.rate_pick.blockSignals(False)
        self._show_rated()
        self._update_buttons()

    def _show_rated(self) -> None:
        index = self.rate_pick.currentData()
        if index is None or not (0 <= int(index) < len(self.turns)):
            self.rate_state.setText("Finished replies can be rated here. Misty learns from your verdicts.")
            return
        rated = self.turns[int(index)].get("rated")
        self.rate_state.setText(f"Rated: {RATING_TEXT.get(rated, rated)}." if rated else "Not rated yet.")

    def _rate(self, label: str, correction: str | None = None) -> None:
        index = self.rate_pick.currentData()
        hub = self.hub
        if index is None or hub is None:
            return
        turn = self.turns[int(index)]
        run = turn.get("run")
        if not run:
            self.status.setText("This reply cannot be rated.")
            return
        run_task(
            lambda: hub.misty_rate(run, label, correction),
            lambda _result, turn=turn, label=label: self._rated(turn, label),
            lambda message: self.status.setText(f"The rating was not saved: {message}"),
        )

    def _rated(self, turn: dict, label: str) -> None:
        turn["rated"] = label
        self._show_rated()
        self.status.setText("Rating saved. Misty learns from it.")
        self._render()

    def _correct(self) -> None:
        text, ok = QInputDialog.getMultiLineText(self, "Correct the reply", "What is the right answer? Misty uses it for similar questions.")
        if ok and text.strip():
            self._rate("corrected", text.strip())

    def _learn(self) -> None:
        dialog = LearnDialog(self.hub, self)
        dialog.setAttribute(Qt.WidgetAttribute.WA_DeleteOnClose)
        dialog.open()

    # ---- files and rendering ---------------------------------------------------------------------------------------

    def _pick_files(self) -> None:
        paths, _ = QFileDialog.getOpenFileNames(self, "Attach files", "", OPEN_FILTER)
        if paths:
            attach_paths(
                self.attachments,
                paths,
                hub=self.hub,
                can_picture=False,
                report=lambda message: self.status.setText(message),
            )
            self._update_buttons()

    def _render(self) -> None:
        self._render_timer.stop()
        # A backslash before < keeps a model's markup as plain text instead of letting the viewer render it.
        safe = lambda s: str(s).replace("<", "\\<")  # noqa: E731
        parts = []
        for t in self.turns:
            who = f"Misty ({t['role']})" if t["role"] else "Misty"
            lines = [f"**You**\n\n{safe(t['you'])}", f"**{who}**"]
            lines += [f"*{safe(n)}*" for n in t["notes"]]
            lines.append(safe(t["text"] or "..."))
            if t["error"]:
                lines.append(f"**Error:** {safe(t['error'])}")
            if t["cites"]:
                lines.append("Sources: " + "; ".join(safe(c.get("source", "?")) for c in t["cites"]))
            if t.get("rated"):
                lines.append(f"*rated: {RATING_TEXT.get(t['rated'], t['rated'])}*")
            parts.append("\n\n".join(lines))
        self.transcript.setMarkdown("\n\n---\n\n".join(parts))
        bar = self.transcript.verticalScrollBar()
        bar.setValue(bar.maximum())
