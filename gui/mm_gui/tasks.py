"""Hub calls run on a worker thread, so the window never waits for the hub. Each result comes back on the window's thread."""

from __future__ import annotations

from typing import Any, Callable

from PySide6.QtCore import QObject, QRunnable, QThreadPool, Signal


class _Outcome(QObject):
    done = Signal(object)
    failed = Signal(str)


class _Task(QRunnable):
    def __init__(self, fn: Callable[[], Any]):
        super().__init__()
        self.fn = fn
        self.outcome = _Outcome()
        self.setAutoDelete(False)  # kept alive in _running until its outcome has been delivered

    def run(self) -> None:
        try:
            value = self.fn()
        except Exception as e:  # every failure goes back to the page that asked for it
            self.outcome.failed.emit(str(e))
        else:
            self.outcome.done.emit(value)


_running: set[_Task] = set()


def run_task(fn: Callable[[], Any], on_done: Callable[[Any], None], on_failed: Callable[[str], None]) -> None:
    """Run `fn` on a worker thread; `on_done` or `on_failed` then runs on the window's thread."""
    task = _Task(fn)
    _running.add(task)

    def settle(callback: Callable[[Any], None], value: Any) -> None:
        _running.discard(task)
        callback(value)

    task.outcome.done.connect(lambda value: settle(on_done, value))
    task.outcome.failed.connect(lambda message: settle(on_failed, message))
    QThreadPool.globalInstance().start(task)
