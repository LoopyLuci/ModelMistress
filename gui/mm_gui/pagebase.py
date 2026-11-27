"""The base of every page of the window. Each page shows what the hub says and runs its calls on a worker thread."""

from __future__ import annotations

from typing import Any, Callable

from PySide6.QtWidgets import QLabel, QWidget

from mm_gui.hub import Hub
from mm_gui.tasks import run_task


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
