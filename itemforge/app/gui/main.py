"""
ItemForge Desktop GUI - entry point.
"""
from __future__ import annotations

import sys
from pathlib import Path

# Ensure the repository root is on sys.path so the `app` package resolves,
# regardless of the current working directory the desktop shell was launched
# from.
_repo_root = Path(__file__).resolve().parent.parent.parent
if str(_repo_root) not in sys.path:
    sys.path.insert(0, str(_repo_root))

from app.gui import forge_window  # noqa: E402


def run():
    application = forge_window.main_qapp()
    window = forge_window.MainWindow()
    window.show()
    sys.exit(application.exec())


if __name__ == "__main__":
    run()
