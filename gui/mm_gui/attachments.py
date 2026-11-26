"""What the owner attaches to a message: the text of a file goes in as data (cut to 2000 characters), a PDF's text comes from
the hub, and a picture goes in with the message when the model can see pictures."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Callable

from PySide6.QtWidgets import QAbstractItemView, QHBoxLayout, QListWidget, QPushButton, QVBoxLayout, QWidget

from mm_gui.chat_extras import ATTACH_CHARS, attachment_block, is_pdf, is_picture, is_text_file, picture_data_url, read_text_file
from mm_gui.hub import Hub
from mm_gui.tasks import run_task


@dataclass(frozen=True)
class Attached:
    name: str
    block: str = ""  # a file's text, as a block the model reads as data
    picture: str = ""  # a picture, as a data URL
    cut: bool = False


class AttachmentBox(QWidget):
    """The files and pictures for the message being written. Hidden while there are none."""

    def __init__(self, parent: QWidget | None = None):
        super().__init__(parent)
        self._items: list[Attached] = []
        self.list = QListWidget()
        self.list.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        self.list.setMaximumHeight(72)
        self.list.setAccessibleName("Attached files")
        remove = QPushButton("Remove")
        remove.setAccessibleName("Remove the selected attachment")
        remove.clicked.connect(self.remove_selected)
        clear = QPushButton("Clear")
        clear.clicked.connect(self.clear)
        buttons = QVBoxLayout()
        buttons.addWidget(remove)
        buttons.addWidget(clear)
        buttons.addStretch(1)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(self.list, 1)
        layout.addLayout(buttons)
        self._refresh()

    def items(self) -> list[Attached]:
        return list(self._items)

    def add(self, item: Attached) -> None:
        self._items.append(item)
        self._refresh()

    def clear(self) -> None:
        self._items.clear()
        self._refresh()

    def take(self) -> list[Attached]:
        """What is attached, emptied: it goes with the message that is being sent."""
        items, self._items = self._items, []
        self._refresh()
        return items

    def remove_selected(self) -> None:
        row = self.list.currentRow()
        if 0 <= row < len(self._items):
            del self._items[row]
            self._refresh()

    def _refresh(self) -> None:
        self.list.clear()
        for item in self._items:
            label = item.name
            if item.cut:
                label += f" (cut to {ATTACH_CHARS} characters)"
            elif item.picture:
                label += " (picture)"
            self.list.addItem(label)
        self.setVisible(bool(self._items))


def attach_paths(
    box: AttachmentBox,
    paths: list[str],
    *,
    hub: Hub | None,
    can_picture: bool,
    report: Callable[[str], None],
) -> None:
    """Attaches each file: text and source files are read here, a PDF's text comes from the hub on a worker thread, and a
    picture is taken only when the model can see pictures. Anything that cannot be attached is reported, not dropped."""
    for raw in paths:
        path = Path(raw)
        name = path.name
        if is_picture(name):
            if not can_picture:
                report(f"{name}: this model cannot read pictures. Choose a model that can see, or the router.")
                continue
            try:
                box.add(Attached(name, picture=picture_data_url(name, path.read_bytes())))
            except OSError as e:
                report(f"{name}: {e}")
            continue
        if is_pdf(name):
            if hub is None:
                report(f"{name}: the hub is not connected.")
                continue
            run_task(
                lambda hub=hub, path=path: hub.document_text(path.read_bytes())[0],
                lambda text, name=name: _pdf_attached(box, name, text, report),
                lambda message, name=name: report(f"{name}: {message}"),
            )
            continue
        if is_text_file(name):
            try:
                block, cut = attachment_block(name, read_text_file(path))
            except OSError as e:
                report(f"{name}: {e}")
                continue
            box.add(Attached(name, block=block, cut=cut))
            continue
        report(f"{name}: only text, source, JSON, PDF and picture files can be attached.")


def _pdf_attached(box: AttachmentBox, name: str, text: str, report: Callable[[str], None]) -> None:
    if not text.strip():
        report(f"{name} has no text in it (a scanned PDF needs OCR, which this build does not have).")
        return
    block, cut = attachment_block(name, text)
    box.add(Attached(name, block=block, cut=cut))
