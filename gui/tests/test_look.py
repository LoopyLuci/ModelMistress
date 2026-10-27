"""The window's look and its polish: theme, style and text size, the Settings filter, the status refresh, sending with
Ctrl+Enter, and the remembered window size. Offscreen; the look is kept in a temporary file."""

from __future__ import annotations

import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import pytest  # noqa: E402
from PySide6.QtCore import QSettings  # noqa: E402
from PySide6.QtGui import QShortcut  # noqa: E402
from PySide6.QtWidgets import QApplication, QStyleFactory  # noqa: E402

from mm_gui.app import Window  # noqa: E402
from mm_gui.pages import ChatPage, SettingsPage, StatusPage  # noqa: E402
from mm_gui.prefs import Appearance, Preferences  # noqa: E402
from test_pages import FakeHub, wait_for  # noqa: E402


@pytest.fixture(scope="module")
def app():
    return QApplication.instance() or QApplication([])


@pytest.fixture
def prefs(tmp_path):
    return Preferences(QSettings(str(tmp_path / "look.ini"), QSettings.Format.IniFormat))


@pytest.fixture
def appearance(app, prefs):
    look = Appearance(app, prefs)
    yield look
    look.set("text_size", "normal")
    look.set("theme", "system")
    look.set("style", "Fusion")


def test_unknown_or_missing_values_fall_back_to_the_defaults(prefs):
    prefs.store.setValue("appearance/theme", "neon")
    prefs.store.setValue("appearance/text_size", "huge")
    prefs.store.setValue("appearance/style", "NoSuchStyle")

    assert (prefs.get("theme"), prefs.get("text_size"), prefs.get("style")) == ("system", "normal", "Fusion")


def test_setting_an_unknown_value_is_refused(prefs):
    with pytest.raises(ValueError):
        prefs.set("theme", "neon")
    assert prefs.get("theme") == "system"


def test_the_look_applies_and_goes_back(app, appearance):
    base_font = app.font().pointSizeF()

    appearance.set("text_size", "large")
    if base_font > 0:
        assert app.font().pointSizeF() == pytest.approx(base_font * 1.15)
    appearance.set("text_size", "normal")
    if base_font > 0:
        assert app.font().pointSizeF() == pytest.approx(base_font)

    assert appearance.prefs.get("style") == "Fusion"
    assert app.style().name().lower() == "fusion"
    other = next((name for name in QStyleFactory.keys() if name != "Fusion"), None)
    if other:
        appearance.set("style", other)
        assert app.style().name().lower() == other.lower()
    appearance.set("style", "Fusion")
    assert app.style().name().lower() == "fusion"


def test_the_theme_is_kept_and_each_choice_applies(appearance):
    # The offscreen platform has no colour scheme to switch, so this checks the stored choice; the look itself is
    # checked on a real screen.
    for theme in ("dark", "light", "system"):
        appearance.set("theme", theme)
        assert appearance.prefs.get("theme") == theme


def test_settings_page_saves_the_look_and_filters_the_hub_settings(app, appearance):
    hub = FakeHub({"config.get": {"max_loaded": 2, "allow_network": False, "download_dir": "E:/x"}})
    page = SettingsPage(appearance)
    page.attach(hub)
    wait_for(app, lambda: page.table.rowCount() == 3)

    theme = page._looks["theme"]
    theme.setCurrentIndex(theme.findData("dark"))
    assert appearance.prefs.get("theme") == "dark"
    assert page.status.text() == "Appearance saved."

    page.filter.setText("max")
    shown = [page.table.item(row, 0).text() for row in range(page.table.rowCount()) if not page.table.isRowHidden(row)]
    assert shown == ["max_loaded"]

    page.filter.setText("")
    assert not any(page.table.isRowHidden(row) for row in range(page.table.rowCount()))


def test_the_status_page_refreshes_when_it_is_opened(app):
    hub = FakeHub()
    page = StatusPage()
    page.attach(hub)
    wait_for(app, lambda: page.values["version"].text() == "0.3.0")

    def status_calls() -> int:
        return sum(1 for call in hub.calls if call[0] == "service.status")

    before = status_calls()
    page.shown()
    wait_for(app, lambda: status_calls() > before)


def test_ctrl_enter_sends_the_message(app):
    hub = FakeHub({"model.list": [{"id": "m", "source": "hf"}]}, pieces=("ok",))
    page = ChatPage()
    page.attach(hub)
    wait_for(app, lambda: page.model.count() == 1)

    page.input.setPlainText("hello")
    # Focus depends on the platform, so fire the shortcuts that belong to the message box instead of a key press.
    shortcuts = {s.key().toString(): s for s in page.input.findChildren(QShortcut)}
    assert {"Ctrl+Return", "Ctrl+Enter"} <= set(shortcuts)
    shortcuts["Ctrl+Return"].activated.emit()

    wait_for(app, lambda: page.messages[-1:] == [{"role": "assistant", "content": "ok"}] and page._cancel is None)


def test_the_window_remembers_its_size_and_opens_its_first_page(app, tmp_path, appearance):
    window = Window(tmp_path, finder=lambda _home: FakeHub(), appearance=appearance)
    window.resize(700, 500)  # the offscreen screen is 800 pixels wide, so a wider window would be clamped
    window.close()
    assert appearance.prefs.window_geometry() is not None

    again = Window(tmp_path, finder=lambda _home: FakeHub(), appearance=appearance)
    again.show()
    app.processEvents()
    # The width is at least what the pages' content needs, so the height (the default is 720) shows the restore.
    assert again.height() == 500
    assert again.stack.currentWidget() is again.pages[0]
    assert again.heading.text() == "Status"
    again.nav.setCurrentRow(3)
    assert again.heading.text() == "Settings"
    again.close()
