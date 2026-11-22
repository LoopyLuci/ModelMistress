"""The pages of the window. Each one shows what the hub says and runs its calls on a worker thread."""

from __future__ import annotations

import json
import threading
from typing import Any, Callable

from PySide6.QtCore import QObject, Qt, QTimer, Signal
from PySide6.QtGui import QKeySequence, QShortcut
from PySide6.QtWidgets import (
    QAbstractItemView,
    QComboBox,
    QFormLayout,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPlainTextEdit,
    QPushButton,
    QStyleFactory,
    QTableWidget,
    QTableWidgetItem,
    QTextBrowser,
    QVBoxLayout,
    QWidget,
)

from mm_gui.hub import Hub
from mm_gui.models import fmt_mb, fmt_uptime, is_chat_model, model_label
from mm_gui.prefs import TEXT_SIZES, THEMES, Appearance
from mm_gui.tasks import run_task

_TRUE = {"true", "yes", "on", "1"}
_FALSE = {"false", "no", "off", "0"}
_PLACEHOLDER = "(set)"  # config.get's stand-in for a secret the hub keeps


class Page(QWidget):
    """A page of the window. It works on the hub it is attached to and reports failures in its status line."""

    def __init__(self, title: str):
        super().__init__()
        self.title = title
        self.hub: Hub | None = None
        self.status = QLabel()
        self.status.setWordWrap(True)
        self.status.setAccessibleName(f"{title} status")

    def attach(self, hub: Hub) -> None:
        self.hub = hub
        self.refresh()

    def refresh(self) -> None:
        """Read what the page shows from the hub. Each page says how."""

    def shown(self) -> None:
        """Called when the window opens this page; a page whose numbers go stale refreshes here."""

    def ask(self, what: str, fn: Callable[[Hub], Any], on_done: Callable[[Any], None]) -> None:
        """Run `fn(hub)` on a worker thread; `what` names the call in the status line while it runs."""
        hub = self.hub
        if hub is None:
            return
        self.status.setText(f"{what}...")

        def done(value: Any) -> None:
            self.status.setText("")
            on_done(value)

        def failed(message: str) -> None:
            self.status.setText(f"{what} failed: {message}")

        run_task(lambda: fn(hub), done, failed)


class StatusPage(Page):
    FIELDS = [
        ("version", "Version"),
        ("pid", "Process"),
        ("uptime", "Up for"),
        ("home", "Home folder"),
        ("engine", "Engine"),
        ("models", "Models known"),
        ("loaded", "Loaded"),
        ("memory", "Memory"),
    ]

    GROUPS = [
        ("Hub", ["version", "pid", "uptime", "home", "engine"]),
        ("Models and memory", ["models", "loaded", "memory"]),
    ]

    def __init__(self):
        super().__init__("Status")
        labels = dict(self.FIELDS)
        self.values: dict[str, QLabel] = {}
        layout = QVBoxLayout(self)
        for title, keys in self.GROUPS:
            group = QGroupBox(title)
            form = QFormLayout(group)
            for key in keys:
                value = QLabel("-")
                value.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
                value.setWordWrap(True)
                value.setAccessibleName(labels[key])
                form.addRow(labels[key], value)
                self.values[key] = value
            layout.addWidget(group)
        refresh = QPushButton("Refresh")
        refresh.clicked.connect(self.refresh)
        layout.addWidget(refresh, alignment=Qt.AlignmentFlag.AlignLeft)
        layout.addWidget(self.status)
        layout.addStretch(1)

    def refresh(self) -> None:
        self.ask("Reading the hub status", lambda hub: hub.status(), self._show)

    def shown(self) -> None:
        if self.hub is not None:
            self.refresh()

    def _show(self, st: dict) -> None:
        system = st.get("system") or {}
        loaded = st.get("loaded") or []
        built = st.get("llama_built_in")
        engine = f"built in ({built})" if built else (st.get("llama_server") or "not found")
        values = {
            "version": st.get("version", "-"),
            "pid": st.get("pid", "-"),
            "uptime": fmt_uptime(st.get("uptime_s")),
            "home": st.get("home", "-"),
            "engine": engine,
            "models": st.get("models_known", "-"),
            "loaded": ", ".join(m.get("model", "?") for m in loaded) or "none",
            "memory": (
                f"{fmt_mb(system.get('available_mb'))} free of {fmt_mb(system.get('total_mb'))}; "
                f"the hub uses {fmt_mb(system.get('hub_rss_mb'))}"
            ),
        }
        for key, text in values.items():
            self.values[key].setText(str(text))
        warnings = st.get("config_warnings") or []
        if warnings:
            self.status.setText("Settings warnings: " + "; ".join(str(w) for w in warnings))


class ModelsPage(Page):
    COLUMNS = ["Model", "Source", "Size", "Architecture", "Context", "State"]

    def __init__(self):
        super().__init__("Models")
        self.filter = QLineEdit()
        self.filter.setPlaceholderText("Filter models")
        self.filter.setAccessibleName("Filter models")
        self.filter.textChanged.connect(self._fill)
        self.table = QTableWidget(0, len(self.COLUMNS))
        self.table.setHorizontalHeaderLabels(self.COLUMNS)
        self.table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.table.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        self.table.setEditTriggers(QAbstractItemView.EditTrigger.NoEditTriggers)
        self.table.verticalHeader().setVisible(False)
        self.table.horizontalHeader().setStretchLastSection(True)
        self.table.setAccessibleName("Models")
        self._models: list[dict] = []
        self._visible: list[dict] = []

        scan = QPushButton("Rescan disk")
        scan.clicked.connect(lambda: self.refresh(scan=True))
        load = QPushButton("Load")
        load.clicked.connect(self._load)
        unload = QPushButton("Unload")
        unload.clicked.connect(self._unload)
        buttons = QHBoxLayout()
        for button in (scan, load, unload):
            buttons.addWidget(button)
        buttons.addStretch(1)

        layout = QVBoxLayout(self)
        layout.addWidget(self.filter)
        layout.addWidget(self.table, 1)
        layout.addLayout(buttons)
        layout.addWidget(self.status)

    def refresh(self, scan: bool = False) -> None:
        self.ask("Reading the model list", lambda hub: hub.models(refresh=scan), self._show)

    def _show(self, models: list[dict]) -> None:
        self._models = list(models)
        self._fill()

    def _fill(self) -> None:
        text = self.filter.text().strip().lower()
        self._visible = [m for m in self._models if text in str(m.get("id", "")).lower()]
        self.table.setRowCount(len(self._visible))
        for row, m in enumerate(self._visible):
            size = f"{m['size_gb']} GB" if m.get("size_gb") else "-"
            cells = [
                m.get("id", ""),
                m.get("source", ""),
                size,
                m.get("architecture") or "-",
                m.get("context_length") or "-",
                "loaded" if m.get("loaded") else "",
            ]
            for col, value in enumerate(cells):
                self.table.setItem(row, col, QTableWidgetItem(str(value)))

    def _selected(self) -> str | None:
        row = self.table.currentRow()
        return str(self._visible[row].get("id")) if 0 <= row < len(self._visible) else None

    def _load(self) -> None:
        model = self._selected()
        if not model:
            self.status.setText("Choose a model first.")
            return
        self.ask(f"Loading {model} (a large model can take minutes)", lambda hub: hub.load(model), self._changed)

    def _unload(self) -> None:
        model = self._selected()
        if not model:
            self.status.setText("Choose a model first.")
            return
        self.ask(f"Unloading {model}", lambda hub: hub.unload(model), self._changed)

    def _changed(self, _result: Any) -> None:
        self.refresh()


class _Bridge(QObject):
    """Carries each piece of a streamed reply from the worker thread to the window."""

    delta = Signal(str)


class ChatPage(Page):
    def __init__(self):
        super().__init__("Chat")
        self.model = QComboBox()
        self.model.setAccessibleName("Model")
        self.system = QLineEdit()
        self.system.setPlaceholderText("System prompt (optional)")
        self.system.setAccessibleName("System prompt")
        self.transcript = QTextBrowser()
        self.transcript.setOpenExternalLinks(True)
        self.transcript.setAccessibleName("Conversation")
        self.input = QPlainTextEdit()
        self.input.setPlaceholderText("Message (Ctrl+Enter sends)")
        self.input.setAccessibleName("Message")
        self.input.setMaximumHeight(110)
        self.send_button = QPushButton("Send")
        self.send_button.clicked.connect(self.send)
        self.stop_button = QPushButton("Stop")
        self.stop_button.clicked.connect(self.stop)
        self.stop_button.setEnabled(False)
        new_chat = QPushButton("New chat")
        new_chat.clicked.connect(self.new_chat)
        QShortcut(QKeySequence("Escape"), self).activated.connect(self.stop)
        for sequence in ("Ctrl+Return", "Ctrl+Enter"):
            QShortcut(QKeySequence(sequence), self.input).activated.connect(self.send)

        self.messages: list[dict] = []  # {"role": ..., "content": ...}, the last one is the reply being written
        self._cancel: threading.Event | None = None
        self._bridge = _Bridge()
        self._bridge.delta.connect(self._on_delta)
        self._render_timer = QTimer(self)
        self._render_timer.setSingleShot(True)
        self._render_timer.setInterval(80)
        self._render_timer.timeout.connect(self._render)

        top = QHBoxLayout()
        top.addWidget(self.model, 1)
        top.addWidget(self.system, 2)
        bottom = QHBoxLayout()
        bottom.addWidget(self.input, 1)
        buttons = QVBoxLayout()
        for button in (self.send_button, self.stop_button, new_chat):
            buttons.addWidget(button)
        bottom.addLayout(buttons)
        layout = QVBoxLayout(self)
        layout.addLayout(top)
        layout.addWidget(self.transcript, 1)
        layout.addLayout(bottom)
        layout.addWidget(self.status)

    def refresh(self) -> None:
        self.ask("Loading the models", lambda hub: hub.models(), self._models_listed)

    def _models_listed(self, models: list[dict]) -> None:
        chosen = self.model.currentData()
        self.model.clear()
        for m in models:
            if is_chat_model(m):
                self.model.addItem(model_label(m), m.get("id"))
        index = self.model.findData(chosen)
        if index >= 0:
            self.model.setCurrentIndex(index)
        if self.model.count() == 0:
            self.status.setText("No chat model is listed. Download one first; the hub's dashboard can do it.")

    def send(self) -> None:
        text = self.input.toPlainText().strip()
        model = self.model.currentData()
        if self.hub is None or self._cancel is not None or not text:
            return
        if not model:
            self.status.setText("Choose a model first.")
            return
        hub = self.hub
        self.messages += [{"role": "user", "content": text}, {"role": "assistant", "content": ""}]
        history = self._history()
        self.input.clear()
        cancel = threading.Event()
        self._cancel = cancel
        self._busy(True)
        self.status.setText("Writing...")
        self._render()
        run_task(
            lambda: hub.chat_stream(model, history, self._bridge.delta.emit, cancel=cancel),
            lambda _answer: self._reply_done(cancel),
            self._reply_failed,
        )

    def stop(self) -> None:
        if self._cancel is not None:
            self._cancel.set()
            self.status.setText("Stopping...")

    def new_chat(self) -> None:
        if self._cancel is None:
            self.messages = []
            self._render()
            self.status.setText("")

    def _history(self) -> list[dict]:
        history = [{"role": "system", "content": self.system.text().strip()}] if self.system.text().strip() else []
        return history + self.messages[:-1]

    def _on_delta(self, text: str) -> None:
        self.messages[-1]["content"] += text
        if not self._render_timer.isActive():
            self._render_timer.start()

    def _reply_done(self, cancel: threading.Event) -> None:
        self.status.setText("Stopped." if cancel.is_set() else "")
        self._finish()

    def _reply_failed(self, message: str) -> None:
        if not self.messages[-1]["content"]:
            self.messages.pop()  # the reply never started: keep only the question
        self.status.setText(f"The reply failed: {message}")
        self._finish()

    def _finish(self) -> None:
        self._cancel = None
        self._busy(False)
        self._render()

    def _busy(self, busy: bool) -> None:
        self.send_button.setEnabled(not busy)
        self.stop_button.setEnabled(busy)
        self.model.setEnabled(not busy)

    def _render(self) -> None:
        self._render_timer.stop()
        parts = []
        for m in self.messages:
            who = "You" if m["role"] == "user" else "Model"
            # A backslash before < keeps a model's markup as plain text instead of letting the viewer render it.
            body = (m["content"] or "...").replace("<", "\\<")
            parts.append(f"**{who}**\n\n{body}")
        self.transcript.setMarkdown("\n\n---\n\n".join(parts))
        bar = self.transcript.verticalScrollBar()
        bar.setValue(bar.maximum())


class _MistyBridge(QObject):
    """Carries each page of a run's events from the worker thread to the window."""

    page = Signal(dict)


class MistyPage(Page):
    """Chat with Misty, the agent inside the hub: she picks a model, searches your documents and uses tools. A tool that
    changes something waits for your answer in a dialog."""

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
        self.transcript = QTextBrowser()
        self.transcript.setOpenExternalLinks(False)
        self.transcript.setAccessibleName("Conversation with Misty")
        self.input = QPlainTextEdit()
        self.input.setPlaceholderText("Ask Misty (Ctrl+Enter sends)")
        self.input.setAccessibleName("Message")
        self.input.setMaximumHeight(110)
        self.send_button = QPushButton("Send")
        self.send_button.clicked.connect(self.send)
        self.stop_button = QPushButton("Stop")
        self.stop_button.clicked.connect(self.stop)
        self.stop_button.setEnabled(False)
        new_chat = QPushButton("New chat")
        new_chat.clicked.connect(self.new_chat)
        for sequence in ("Ctrl+Return", "Ctrl+Enter"):
            QShortcut(QKeySequence(sequence), self.input).activated.connect(self.send)

        self.session: str | None = None
        self.turns: list[dict] = []  # {"you", "text", "notes", "role", "cites", "error"}; the last one may be running
        self._cancel: threading.Event | None = None
        self._asked: set[str] = set()
        self._bridge = _MistyBridge()
        self._bridge.page.connect(self._on_page)
        self._render_timer = QTimer(self)
        self._render_timer.setSingleShot(True)
        self._render_timer.setInterval(80)
        self._render_timer.timeout.connect(self._render)

        top = QHBoxLayout()
        top.addWidget(self.role, 1)
        top.addWidget(self.docs, 1)
        bottom = QHBoxLayout()
        bottom.addWidget(self.input, 1)
        buttons = QVBoxLayout()
        for button in (self.send_button, self.stop_button, new_chat):
            buttons.addWidget(button)
        bottom.addLayout(buttons)
        layout = QVBoxLayout(self)
        layout.addWidget(self.ready)
        layout.addLayout(top)
        layout.addWidget(self.transcript, 1)
        layout.addLayout(bottom)
        layout.addWidget(self.status)

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

    def send(self) -> None:
        text = self.input.toPlainText().strip()
        if self.hub is None or self._cancel is not None or not text:
            return
        hub = self.hub
        args: dict = {"input": text, "approvals": "ask"}
        if self.session:
            args["session"] = self.session
        else:
            args["new_session"] = True
        if self.role.currentData():
            args["role"] = self.role.currentData()
        if self.docs.currentData() is not None:
            args["rag"] = self.docs.currentData()
        self.turns.append({"you": text, "text": "", "notes": [], "role": "", "cites": [], "error": ""})
        self.input.clear()
        cancel = threading.Event()
        self._cancel = cancel
        self._asked = set()
        self._busy(True)
        self.status.setText("Working...")
        self._render()
        run_task(
            lambda: hub.misty_run(args, self._bridge.page.emit, cancel),
            lambda _page: self._finished(cancel),
            self._failed,
        )

    def stop(self) -> None:
        if self._cancel is not None:
            self._cancel.set()
            self.status.setText("Stopping...")

    def new_chat(self) -> None:
        if self._cancel is None:
            self.session = None
            self.turns = []
            self._render()
            self.status.setText("")

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
            self.session = page["start"].get("session") or self.session
            return
        for e in page.get("events") or []:
            kind = e.get("type")
            if kind == "token":
                turn["text"] += e.get("text") or ""
            elif kind == "model":
                turn["role"] = e.get("role") or turn["role"]
            elif kind == "tool":
                turn["notes"].append(f"{e.get('name')}: {e.get('status')}")
            elif kind == "escalate":
                turn["text"] = ""
                turn["notes"].append(f"moved up from {e.get('from')} to {e.get('to')}: {e.get('why')}")
            elif kind == "retrieve":
                turn["notes"].append(f"searched your documents: {e.get('used')} of {e.get('hits')} passages used")
            elif kind == "transcript":
                turn["notes"].append(f"heard: {e.get('text')}")
            elif kind == "error":
                turn["error"] = e.get("message") or "the run failed"
            elif kind == "final":
                turn["text"] = e.get("text") if e.get("text") is not None else turn["text"]
                turn["cites"] = e.get("citations") or []
                turn["role"] = e.get("role") or turn["role"]
                if e.get("error") and not turn["error"]:
                    turn["error"] = e["error"]
        if not self._render_timer.isActive():
            self._render_timer.start()
        for approval in page.get("pending") or []:
            key = approval.get("approval")
            if key and key not in self._asked and self.hub is not None:
                self._asked.add(key)
                allow = self._confirm(approval)
                hub = self.hub
                run_task(lambda hub=hub, key=key, allow=allow: hub.misty_approve(key, allow), lambda _r: None, self._approval_failed)

    def _approval_failed(self, message: str) -> None:
        self.status.setText(f"Your answer did not reach Misty: {message}")

    def _finished(self, cancel: threading.Event) -> None:
        self.status.setText("Stopped." if cancel.is_set() else "")
        self._end()

    def _failed(self, message: str) -> None:
        if self.turns and not self.turns[-1]["text"]:
            self.turns[-1]["error"] = message
        self.status.setText(f"Misty could not answer: {message}")
        self._end()

    def _end(self) -> None:
        self._cancel = None
        self._busy(False)
        self._render()

    def _busy(self, busy: bool) -> None:
        self.send_button.setEnabled(not busy)
        self.stop_button.setEnabled(busy)
        self.role.setEnabled(not busy)
        self.docs.setEnabled(not busy)

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
            parts.append("\n\n".join(lines))
        self.transcript.setMarkdown("\n\n---\n\n".join(parts))
        bar = self.transcript.verticalScrollBar()
        bar.setValue(bar.maximum())


def _to_text(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (list, dict)):
        return json.dumps(value)
    return str(value)


def _from_text(text: str, original: Any) -> Any:
    """The value typed in a cell, in the type the setting has now; ValueError when it does not fit."""
    if isinstance(original, bool):
        if text.strip().lower() in _TRUE:
            return True
        if text.strip().lower() in _FALSE:
            return False
        raise ValueError("use true or false")
    if isinstance(original, int):
        try:
            return int(text)
        except ValueError:
            raise ValueError("a whole number is needed") from None
    if isinstance(original, float):
        try:
            return float(text)
        except ValueError:
            raise ValueError("a number is needed") from None
    if isinstance(original, (list, dict)):
        try:
            value = json.loads(text)
        except ValueError:
            raise ValueError("write it as JSON") from None
        if not isinstance(value, type(original)):
            raise ValueError(f"write a JSON {type(original).__name__}")
        return value
    if original is None:
        return text or None
    return text


class SettingsPage(Page):
    def __init__(self, appearance: Appearance):
        super().__init__("Settings")
        self.appearance = appearance
        self._looks: dict[str, QComboBox] = {}
        look = QGroupBox("Appearance")
        look_form = QFormLayout(look)
        styles = ["Fusion", *(name for name in QStyleFactory.keys() if name != "Fusion")]
        choices = {
            "theme": ("Theme", list(THEMES.items())),
            "style": ("Style", [(name, name) for name in styles]),
            "text_size": ("Text size", [(key, label) for key, (label, _) in TEXT_SIZES.items()]),
        }
        for key, (label, options) in choices.items():
            box = QComboBox()
            box.setAccessibleName(label)
            for value, text in options:
                box.addItem(text, value)
            if key == "style":
                box.setToolTip("Fusion follows the theme on every platform. The Windows styles keep their own look.")
            box.setCurrentIndex(max(0, box.findData(appearance.prefs.get(key))))
            box.currentIndexChanged.connect(lambda _index, key=key, box=box: self._look_changed(key, box))
            look_form.addRow(label, box)
            self._looks[key] = box
        note = QLabel("Kept on this computer and applied at once; the hub is not changed.")
        note.setWordWrap(True)
        look_form.addRow(note)

        self.filter = QLineEdit()
        self.filter.setPlaceholderText("Filter hub settings")
        self.filter.setAccessibleName("Filter hub settings")
        self.filter.textChanged.connect(self._filter_rows)
        self.table = QTableWidget(0, 2)
        self.table.setHorizontalHeaderLabels(["Setting", "Value"])
        self.table.verticalHeader().setVisible(False)
        self.table.horizontalHeader().setStretchLastSection(True)
        self.table.setAccessibleName("Settings")
        self._original: dict[str, Any] = {}
        reload = QPushButton("Reload")
        reload.clicked.connect(self.refresh)
        save = QPushButton("Save changes")
        save.clicked.connect(self._save)
        buttons = QHBoxLayout()
        buttons.addWidget(reload)
        buttons.addWidget(save)
        buttons.addStretch(1)
        layout = QVBoxLayout(self)
        layout.addWidget(look)
        layout.addWidget(self.filter)
        layout.addWidget(self.table, 1)
        layout.addLayout(buttons)
        layout.addWidget(self.status)

    def refresh(self) -> None:
        self.ask("Reading the settings", lambda hub: hub.config(), self._show)

    def _look_changed(self, key: str, box: QComboBox) -> None:
        self.appearance.set(key, box.currentData())
        self.status.setText("Appearance saved.")

    def _show(self, cfg: dict) -> None:
        cfg = dict(cfg)
        warnings = cfg.pop("config_warnings", None) or []
        self._original = cfg
        self.table.setRowCount(0)
        for key in sorted(cfg):
            row = self.table.rowCount()
            self.table.insertRow(row)
            name = QTableWidgetItem(key)
            name.setFlags(name.flags() & ~Qt.ItemFlag.ItemIsEditable)
            value = QTableWidgetItem(_to_text(cfg[key]))
            if cfg[key] == _PLACEHOLDER:
                value.setFlags(value.flags() & ~Qt.ItemFlag.ItemIsEditable)  # a secret is not shown or edited here
            self.table.setItem(row, 0, name)
            self.table.setItem(row, 1, value)
        self.table.resizeColumnToContents(0)
        self._filter_rows()
        if warnings:
            self.status.setText("Settings warnings: " + "; ".join(str(w) for w in warnings))

    def _filter_rows(self) -> None:
        text = self.filter.text().strip().lower()
        for row in range(self.table.rowCount()):
            self.table.setRowHidden(row, text not in self.table.item(row, 0).text().lower())

    def _save(self) -> None:
        changes: dict[str, Any] = {}
        for row in range(self.table.rowCount()):
            key = self.table.item(row, 0).text()
            text = self.table.item(row, 1).text()
            original = self._original.get(key)
            if text == _to_text(original):
                continue
            try:
                changes[key] = _from_text(text, original)
            except ValueError as e:
                self.status.setText(f"{key}: {e}.")
                return
        if not changes:
            self.status.setText("Nothing changed.")
            return
        self.ask(f"Saving {len(changes)} setting(s)", lambda hub: hub.set_config(changes), self._saved)

    def _saved(self, _result: Any) -> None:
        self.status.setText("Saved.")
        self.refresh()
