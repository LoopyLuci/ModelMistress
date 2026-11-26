"""The pages of the window. Each one shows what the hub says and runs its calls on a worker thread. The chat pages live in
their own modules and are re-exported here, so the window imports them from one place."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from PySide6.QtCore import QObject, Qt, Signal
from PySide6.QtWidgets import (
    QAbstractItemView,
    QComboBox,
    QFileDialog,
    QFormLayout,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QStyleFactory,
    QTableWidget,
    QTableWidgetItem,
    QVBoxLayout,
)

from mm_gui.chat_page import ChatPage
from mm_gui.misty_page import MistyPage
from mm_gui.models import (
    ROLE_HELP,
    ROLES,
    STATUS_TEXT,
    fmt_mb,
    fmt_uptime,
    is_chat_model,
    model_label,
    roles_of,
    suits_role,
)
from mm_gui.pagebase import Page
from mm_gui.prefs import TEXT_SIZES, THEMES, Appearance
from mm_gui.tasks import run_task

__all__ = ["ChatPage", "MistyPage", "ModelsPage", "Page", "SettingsPage", "StatusPage"]

_TRUE = {"true", "yes", "on", "1"}
_FALSE = {"false", "no", "off", "0"}
_PLACEHOLDER = "(set)"  # config.get's stand-in for a secret the hub keeps


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


class _ImportBridge(QObject):
    """Carries the bytes sent of a model import from the worker thread to the window."""

    progress = Signal(int)


class ModelsPage(Page):
    COLUMNS = ["Model", "Source", "Size", "Architecture", "Context", "Router role", "State"]
    BENCH_COLUMNS = ["Model", "Size", "Tokens per second", "Note"]

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
        self._router: dict | None = None  # router.status, or None on a hub with no router
        self._note: str | None = None  # a line to show once the next list has been read (after an import)
        self._importing = False
        self._import_name = ""
        self._import_total = 1
        self._import_bridge = _ImportBridge()
        self._import_bridge.progress.connect(self._import_progress)
        self._benching = False

        # Which model fills each role of the router: the choices that make the model `auto` usable in any chat.
        self.roles_box = QGroupBox("Router: which model answers")
        roles_form = QFormLayout(self.roles_box)
        self.role_choice: dict[str, QComboBox] = {}
        self.role_state: dict[str, QLabel] = {}
        for role in ROLES:
            choice = QComboBox()
            choice.setAccessibleName(f"Model for {role}")
            choice.setToolTip(ROLE_HELP[role])
            choice.activated.connect(lambda _i, r=role: self._choose_role(r))
            state = QLabel("-")
            state.setWordWrap(True)
            state.setAccessibleName(f"State of {role}")
            line = QHBoxLayout()
            line.addWidget(choice, 2)
            line.addWidget(state, 3)
            roles_form.addRow(f"{role}", line)
            self.role_choice[role] = choice
            self.role_state[role] = state
        self.roles_box.setVisible(False)

        scan = QPushButton("Rescan disk")
        scan.clicked.connect(lambda: self.refresh(scan=True))
        load = QPushButton("Load")
        load.clicked.connect(self._load)
        unload = QPushButton("Unload")
        unload.clicked.connect(self._unload)
        self.importer = QPushButton("Import model file...")
        self.importer.setToolTip("Copy a .gguf model file into the hub's models folder. A large file can take minutes.")
        self.importer.clicked.connect(self._import)
        buttons = QHBoxLayout()
        for button in (scan, load, unload, self.importer):
            buttons.addWidget(button)
        buttons.addStretch(1)

        # The speed test: how fast this machine runs each installed chat model, and which of them suit it.
        self.bench_box = QGroupBox("Speed test")
        bench_layout = QVBoxLayout(self.bench_box)
        bench_text = QLabel("Times this machine and each installed chat model. It takes minutes on a CPU.")
        bench_text.setWordWrap(True)
        self.bench_run = QPushButton("Run speed test")
        self.bench_run.clicked.connect(self._bench)
        self.bench_table = QTableWidget(0, len(self.BENCH_COLUMNS))
        self.bench_table.setHorizontalHeaderLabels(self.BENCH_COLUMNS)
        self.bench_table.setEditTriggers(QAbstractItemView.EditTrigger.NoEditTriggers)
        self.bench_table.verticalHeader().setVisible(False)
        self.bench_table.horizontalHeader().setStretchLastSection(True)
        self.bench_table.setMaximumHeight(160)
        self.bench_table.setAccessibleName("Speed test results")
        self.bench_table.setVisible(False)
        self.bench_result = QLabel()
        self.bench_result.setWordWrap(True)
        self.bench_result.setAccessibleName("Speed test summary")
        bench_layout.addWidget(bench_text)
        bench_layout.addWidget(self.bench_run, alignment=Qt.AlignmentFlag.AlignLeft)
        bench_layout.addWidget(self.bench_table)
        bench_layout.addWidget(self.bench_result)

        layout = QVBoxLayout(self)
        layout.addWidget(self.filter)
        layout.addWidget(self.table, 1)
        layout.addWidget(self.roles_box)
        layout.addLayout(buttons)
        layout.addWidget(self.bench_box)
        layout.addWidget(self.status)

    def refresh(self, scan: bool = False) -> None:
        self.ask(
            "Reading the model list",
            lambda hub: (hub.models(refresh=scan), hub.router_status_or_none()),
            self._show,
        )

    def _show(self, result: tuple[list[dict], dict | None]) -> None:
        models, router = result
        self._models = list(models)
        self._router = router
        self._fill()
        self._fill_roles()
        if self._note:
            self.status.setText(self._note)
            self._note = None

    def _fill_roles(self) -> None:
        """The role choosers: Automatic, then every installed model that suits the role."""
        status = self._router
        self.roles_box.setVisible(status is not None)
        if status is None:
            return
        rows = {r.get("role"): r for r in [*(status.get("roles") or []), *(status.get("support") or [])]}
        overrides = status.get("overrides") or {}
        for role in ROLES:
            choice, row = self.role_choice[role], rows.get(role) or {}
            choice.clear()
            automatic = f"Automatic: {row['model']}" if row.get("model") and role not in overrides else "Automatic"
            choice.addItem(automatic, None)
            for m in sorted(self._models, key=lambda x: str(x.get("id"))):
                if suits_role(role, str(m.get("id", "")), is_chat_model(m)):
                    choice.addItem(model_label(m), m.get("id"))
            chosen = overrides.get(role)
            if chosen and choice.findData(chosen) < 0:
                choice.addItem(f"{chosen} (not installed)", chosen)
            choice.setCurrentIndex(max(choice.findData(chosen), 0) if chosen else 0)
            text = STATUS_TEXT.get(str(row.get("status")), str(row.get("status") or "no model"))
            self.role_state[role].setText(text + (", loaded" if row.get("loaded") else ""))

    def _choose_role(self, role: str) -> None:
        model = self.role_choice[role].currentData()
        what = f"Using {model} for {role}" if model else f"Letting the router choose {role}"
        self.ask(what, lambda hub: hub.set_role(role, model), self._changed)

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
                ", ".join(roles_of(self._router, str(m.get("id", "")))),
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

    # ---- importing a model file -------------------------------------------------------------------------------------

    def _import(self) -> None:
        if self.hub is None or self._importing:
            return
        path, _ = QFileDialog.getOpenFileName(self, "Import a model", "", "Model files (*.gguf);;All files (*)")
        if not path:
            return
        source = Path(path)
        self._importing = True
        self._import_name = source.name
        self._import_total = max(source.stat().st_size, 1)
        self.importer.setEnabled(False)
        self.status.setText(f"Importing {source.name} (a large model can take minutes)...")
        hub, bridge = self.hub, self._import_bridge
        run_task(
            lambda: hub.import_model(source, bridge.progress.emit),
            self._imported,
            self._import_failed,
        )

    def _import_progress(self, sent: int) -> None:
        percent = min(100, sent * 100 // self._import_total)
        self.status.setText(f"Importing {self._import_name}: {percent}%")

    def _imported(self, doc: dict) -> None:
        self._importing = False
        self.importer.setEnabled(True)
        self._note = f"Imported {doc.get('name', self._import_name)}. It is in the list now."
        self.refresh()

    def _import_failed(self, message: str) -> None:
        self._importing = False
        self.importer.setEnabled(True)
        self.status.setText(f"Importing {self._import_name} failed: {message}")

    # ---- the speed test ---------------------------------------------------------------------------------------------

    def _bench(self) -> None:
        if self.hub is None or self._benching:
            return
        self._benching = True
        self.bench_run.setEnabled(False)
        self.status.setText("Running the speed test (a CPU takes minutes)...")
        hub = self.hub
        run_task(lambda: hub.bench(), self._benched, self._bench_failed)

    def _benched(self, doc: dict) -> None:
        self._benching = False
        self.bench_run.setEnabled(True)
        self.status.setText("")
        self._show_bench(doc)

    def _bench_failed(self, message: str) -> None:
        self._benching = False
        self.bench_run.setEnabled(True)
        self.status.setText(f"The speed test failed: {message}")

    def _show_bench(self, doc: dict) -> None:
        rows = doc.get("models") or []
        self.bench_table.setRowCount(len(rows))
        self.bench_table.setVisible(bool(rows))
        for r, row in enumerate(rows):
            if row.get("error"):
                speed, note = "-", f"not timed: {row['error']}"
            else:
                speed = "-" if row.get("tokens_per_s") is None else f"{row['tokens_per_s']:.1f}"
                note = "a cold start: left out of the suggestions" if row.get("cold_start") else ""
            size = f"{row['size_gb']} GB" if row.get("size_gb") else "-"
            for c, text in enumerate([row.get("model", "?"), size, speed, note]):
                self.bench_table.setItem(r, c, QTableWidgetItem(str(text)))
        suggestions = doc.get("suggestions") or {}

        def pick(key: str) -> str:
            return str(suggestions.get(key) or "none fits")

        memory = doc.get("memory_gbps")
        lines = [
            f"RAM {doc.get('ram_gb', '-')} GB; memory copy {'-' if memory is None else f'{memory} GB/s'}.",
            f"Fastest: {pick('fastest')}. Keeps up with reading: {pick('balanced')}. Best quality that fits: {pick('best')}.",
        ]
        if doc.get("note"):
            lines.append(str(doc["note"]))
        self.bench_result.setText(" ".join(lines))


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
