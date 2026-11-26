"""What Misty has learned from the owner's verdicts: the counts, the verdicts (newest first), forgetting some of them, and the
training data for export. Opened from the Misty page."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Callable

from PySide6.QtCore import Qt
from PySide6.QtWidgets import (
    QAbstractItemView,
    QDialog,
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QMessageBox,
    QPushButton,
    QTableWidget,
    QTableWidgetItem,
    QVBoxLayout,
)

from mm_gui.hub import Hub
from mm_gui.tasks import run_task

COLUMNS = ["Verdict", "Question", "Answer", "Correction"]
LABELS = {"good": "good", "bad": "bad", "no_tools": "no tools", "needs_tools": "needs tools", "corrected": "corrected"}


def counts_text(counts: dict | None) -> str:
    """The counts in words: each verdict kind and the total, or that there are none yet."""
    counts = counts or {}
    total = counts.get("total", 0)
    if not total:
        return "Misty has no verdicts yet. Rate her replies, and she learns from them."
    kinds = ", ".join(f"{LABELS.get(k, k)} {v}" for k, v in counts.items() if k != "total" and v)
    return f"{total} verdicts: {kinds}."


class LearnDialog(QDialog):
    def __init__(self, hub: Hub | None, parent=None):
        super().__init__(parent)
        self.hub = hub
        self.setWindowTitle("What Misty learns")
        self.setMinimumWidth(640)
        self.summary = QLabel("Reading the verdicts...")
        self.summary.setWordWrap(True)
        self.summary.setAccessibleName("Verdict counts")
        self.table = QTableWidget(0, len(COLUMNS))
        self.table.setHorizontalHeaderLabels(COLUMNS)
        self.table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.table.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        self.table.setEditTriggers(QAbstractItemView.EditTrigger.NoEditTriggers)
        self.table.verticalHeader().setVisible(False)
        self.table.horizontalHeader().setStretchLastSection(True)
        self.table.setAccessibleName("Verdicts")
        self.status = QLabel()
        self.status.setWordWrap(True)
        self.status.setAccessibleName("Learning status")

        refresh = QPushButton("Refresh")
        refresh.clicked.connect(self.refresh)
        forget = QPushButton("Forget selected")
        forget.clicked.connect(self._forget_selected)
        forget_all = QPushButton("Forget all")
        forget_all.clicked.connect(self._forget_all)
        export = QPushButton("Export training data...")
        export.clicked.connect(self._export)
        close = QPushButton("Close")
        close.clicked.connect(self.accept)
        buttons = QHBoxLayout()
        for button in (refresh, forget, forget_all, export):
            buttons.addWidget(button)
        buttons.addStretch(1)
        buttons.addWidget(close)

        layout = QVBoxLayout(self)
        layout.addWidget(self.summary)
        layout.addWidget(self.table, 1)
        layout.addLayout(buttons)
        layout.addWidget(self.status)
        self.refresh()

    # ---- the calls -------------------------------------------------------------------------------------------------

    def refresh(self) -> None:
        self._ask("Reading the verdicts", lambda hub: hub.misty_feedback(200), self._show)

    def _ask(self, what: str, fn: Callable[[Hub], Any], on_done: Callable[[Any], None], then: Callable[[], None] | None = None) -> None:
        hub = self.hub
        if hub is None:
            self.status.setText("The hub is not connected.")
            return
        self.status.setText(f"{what}...")

        def done(value: Any) -> None:
            self.status.setText("")
            on_done(value)
            if then is not None:
                then()

        run_task(lambda: fn(hub), done, lambda message: self.status.setText(f"{what} failed: {message}"))

    def _show(self, doc: dict) -> None:
        self.summary.setText(counts_text(doc.get("counts")))
        verdicts = doc.get("verdicts") or []
        self.table.setRowCount(len(verdicts))
        for row, v in enumerate(verdicts):
            cells = [LABELS.get(str(v.get("label")), str(v.get("label") or "")), v.get("question") or "", v.get("answer") or "", v.get("correction") or ""]
            for col, text in enumerate(cells):
                item = QTableWidgetItem(str(text))
                if col == 0:
                    item.setData(Qt.ItemDataRole.UserRole, v.get("id"))
                self.table.setItem(row, col, item)

    def _selected_id(self) -> int | None:
        row = self.table.currentRow()
        if row < 0:
            return None
        return self.table.item(row, 0).data(Qt.ItemDataRole.UserRole)

    def _forget_selected(self) -> None:
        verdict = self._selected_id()
        if verdict is None:
            self.status.setText("Choose a verdict first.")
            return
        if QMessageBox.question(self, "Forget this verdict", "Misty stops using this verdict. It cannot be undone.") != QMessageBox.StandardButton.Yes:
            return
        self._ask("Forgetting the verdict", lambda hub: hub.misty_forget(verdict=int(verdict)), lambda _r: None, self.refresh)

    def _forget_all(self) -> None:
        if QMessageBox.question(self, "Forget everything", "Misty forgets every verdict she was given. It cannot be undone.") != QMessageBox.StandardButton.Yes:
            return
        self._ask("Forgetting every verdict", lambda hub: hub.misty_forget(everything=True), lambda _r: None, self.refresh)

    def _export(self) -> None:
        path, _ = QFileDialog.getSaveFileName(self, "Save the training data", "misty-training.jsonl", "JSON Lines (*.jsonl)")
        if not path:
            return
        target = Path(path)

        def written(doc: dict) -> None:
            target.write_text(str(doc.get("jsonl") or ""), encoding="utf-8")
            self.status.setText(f"Saved {doc.get('count', 0)} example(s) to {target}.")

        self._ask("Exporting the training data", lambda hub: hub.misty_export(), written)
