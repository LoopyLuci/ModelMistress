"""
ItemForge Desktop GUI - main window layer.
PySide6 / Qt.
"""
from __future__ import annotations

import sys

from PySide6.QtWidgets import QMainWindow, QWidget, QHBoxLayout, QStackedWidget

from .pages import (
    Sidebar,
    ItemsPage,
    ListingsPage,
    StatusCard,
    SettingCard,
    ICONS,
)


class MainWindow(QMainWindow):
    """Top-level window: left navigation + stacked content pages."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setWindowTitle("ItemForge")
        self.setMinimumSize(1150, 720)
        self._build_ui()

    def _build_ui(self):
        self.side = Sidebar()
        self.side.add_page("items", "Items", ICONS["items"])
        self.side.add_page("listings", "Listings", ICONS["listings"])
        self.side.add_page("status", "Status", ICONS["status"])
        self.side.add_page("settings", "Settings", ICONS["settings"])
        self.side.currentRowChanged.connect(self._on_nav)

        self.stack = QStackedWidget()
        self.stack.addWidget(ItemsPage())
        self.stack.addWidget(ListingsPage())
        self.stack.addWidget(StatusCard())
        self.stack.addWidget(SettingCard())

        self.side.setCurrentRow(0)
        central = QWidget()
        lay = QHBoxLayout(central)
        lay.addWidget(self.side)
        lay.addWidget(self.stack)
        self.setCentralWidget(central)

    def _on_nav(self, row: int):
        self.stack.setCurrentIndex(row)


def main():
    """Entry point: build the QApplication and run the window."""
    from PySide6.QtWidgets import QApplication
    import sys
    application = QApplication(sys.argv)
    window = MainWindow()
    window.show()
    sys.exit(application.exec())


def main_qapp():
    """Build and return the QApplication without entering the event loop.

    The caller (e.g. the desktop entry point) owns ``application.exec()``.
    """
    from PySide6.QtWidgets import QApplication
    application = QApplication(sys.argv)
    return application
