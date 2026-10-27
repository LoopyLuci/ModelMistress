"""The ModelMistress desktop window: pages on the left, the hub's connection at the top. Nothing here talks to a model;
every answer comes from the running hub over its API."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Callable

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
from mm_gui.tasks import run_task


class Window(QMainWindow):
    def __init__(self, home: Path, finder: Callable[[Path], Hub] = Hub.find):
        super().__init__()
        self.home = home
        self.finder = finder
        self.setWindowTitle("ModelMistress")
        self.resize(1100, 720)

        self.pages = [StatusPage(), ModelsPage(), ChatPage(), SettingsPage()]
        self.stack = QStackedWidget()
        self.nav = QListWidget()
        self.nav.setFixedWidth(150)
        self.nav.setAccessibleName("Pages")
        for page in self.pages:
            self.nav.addItem(page.title)
            self.stack.addWidget(page)
        self.nav.currentRowChanged.connect(self.stack.setCurrentIndex)
        self.nav.setCurrentRow(0)

        self.banner = QLabel()
        self.banner.setWordWrap(True)
        self.banner.setAccessibleName("Hub connection")
        self.retry = QPushButton("Connect again")
        self.retry.clicked.connect(self.connect_hub)
        top = QHBoxLayout()
        top.addWidget(self.banner, 1)
        top.addWidget(self.retry)
        body = QHBoxLayout()
        body.addWidget(self.nav)
        body.addWidget(self.stack, 1)
        layout = QVBoxLayout()
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


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="mm_gui",
        description="The ModelMistress desktop window. It talks to a running hub over its API.",
    )
    parser.add_argument("--home", type=Path, help="the hub's home folder (default: MM_HOME, else the platform folder)")
    args = parser.parse_args(argv)
    app = QApplication.instance() or QApplication(sys.argv[:1])
    app.setApplicationName("ModelMistress")
    window = Window(args.home or default_home())
    window.show()
    return app.exec()
