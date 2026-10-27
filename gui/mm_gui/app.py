"""The ModelMistress desktop window: pages on the left, the hub's connection at the top. Nothing here talks to a model;
every answer comes from the running hub over its API."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Callable

from PySide6.QtGui import QCloseEvent, QFont, QKeySequence, QShortcut
from PySide6.QtWidgets import (
    QApplication,
    QHBoxLayout,
    QLabel,
    QListWidget,
    QMainWindow,
    QPushButton,
    QStackedWidget,
    QVBoxLayout,
    QWidget,
)

from mm_gui.hub import Hub, default_home
from mm_gui.pages import ChatPage, ModelsPage, SettingsPage, StatusPage
from mm_gui.prefs import Appearance, Preferences
from mm_gui.tasks import run_task


class Window(QMainWindow):
    def __init__(self, home: Path, finder: Callable[[Path], Hub] = Hub.find, appearance: Appearance | None = None):
        super().__init__()
        self.home = home
        self.finder = finder
        if appearance is None:
            appearance = Appearance(QApplication.instance(), Preferences())
        self.appearance = appearance
        self.prefs = appearance.prefs
        self.setWindowTitle("ModelMistress")
        geometry = self.prefs.window_geometry()
        if geometry is None or not self.restoreGeometry(geometry):
            self.resize(1100, 720)

        self.heading = QLabel()
        heading_font = QFont(self.heading.font())
        heading_font.setBold(True)
        if heading_font.pointSizeF() > 0:
            heading_font.setPointSizeF(heading_font.pointSizeF() * 1.3)
        self.heading.setFont(heading_font)
        self.heading.setAccessibleName("Page")
        self.pages = [StatusPage(), ModelsPage(), ChatPage(), SettingsPage(appearance)]
        self.stack = QStackedWidget()
        self.nav = QListWidget()
        self.nav.setFixedWidth(150)
        self.nav.setAccessibleName("Pages")
        for page in self.pages:
            self.nav.addItem(page.title)
            self.stack.addWidget(page)
        self.nav.currentRowChanged.connect(self._show_page)
        for row in range(len(self.pages)):
            shortcut = QShortcut(QKeySequence(f"Ctrl+{row + 1}"), self)
            shortcut.activated.connect(lambda row=row: self.nav.setCurrentRow(row))
        self.nav.setCurrentRow(0)

        self.banner = QLabel()
        self.banner.setWordWrap(True)
        self.banner.setAccessibleName("Hub connection")
        self.retry = QPushButton("Connect again")
        self.retry.clicked.connect(self.connect_hub)
        top = QHBoxLayout()
        top.addWidget(self.banner, 1)
        top.addWidget(self.retry)
        page = QVBoxLayout()
        page.addWidget(self.heading)
        page.addWidget(self.stack, 1)
        body = QHBoxLayout()
        body.setSpacing(12)
        body.addWidget(self.nav)
        body.addLayout(page, 1)
        layout = QVBoxLayout()
        layout.setContentsMargins(12, 10, 12, 12)
        layout.addLayout(top)
        layout.addLayout(body)
        center = QWidget()
        center.setLayout(layout)
        self.setCentralWidget(center)
        self.connect_hub()

    def connect_hub(self) -> None:
        self.banner.setText(f"Connecting to the hub in {self.home} ...")
        self.retry.setEnabled(False)
        run_task(lambda: self.finder(self.home), self._connected, self._not_connected)

    def _connected(self, hub: Hub) -> None:
        self.retry.setEnabled(True)
        self.banner.setText(f"Connected to the hub at {hub.url}")
        for page in self.pages:
            page.attach(hub)

    def _not_connected(self, message: str) -> None:
        self.retry.setEnabled(True)
        self.banner.setText(f"{message}. Start the hub (model-mistress serve), then press Connect again.")

    def _show_page(self, row: int) -> None:
        self.stack.setCurrentIndex(row)
        self.heading.setText(self.pages[row].title)
        if self.pages[row].hub is not None:
            self.pages[row].shown()

    def closeEvent(self, event: QCloseEvent) -> None:
        self.prefs.save_window_geometry(self.saveGeometry())
        super().closeEvent(event)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="mm_gui",
        description="The ModelMistress desktop window. It talks to a running hub over its API.",
    )
    parser.add_argument("--home", type=Path, help="the hub's home folder (default: MM_HOME, else the platform folder)")
    args = parser.parse_args(argv)
    app = QApplication.instance() or QApplication(sys.argv[:1])
    app.setApplicationName("ModelMistress")
    appearance = Appearance(app, Preferences())
    appearance.apply()
    window = Window(args.home or default_home(), appearance=appearance)
    window.show()
    return app.exec()
