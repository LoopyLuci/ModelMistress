"""The window's own look (theme, style and text size) and where it was last shown. They are kept per user with QSettings,
so they never reach the hub, and a missing or unknown value falls back to its default."""

from __future__ import annotations

from PySide6.QtCore import QByteArray, QSettings, Qt
from PySide6.QtGui import QFont, QGuiApplication
from PySide6.QtWidgets import QApplication, QStyleFactory

THEMES = {"system": "Follow the system", "light": "Light", "dark": "Dark"}
TEXT_SIZES = {"small": ("Small", 0.9), "normal": ("Normal", 1.0), "large": ("Large", 1.15)}
# Fusion is the default because it is the Qt style that follows the theme on every platform; the platform's own
# styles on Windows keep their look whatever the theme says.
DEFAULTS = {"theme": "system", "style": "Fusion", "text_size": "normal"}
_SCHEMES = {"system": Qt.ColorScheme.Unknown, "light": Qt.ColorScheme.Light, "dark": Qt.ColorScheme.Dark}


class Preferences:
    """The stored values, each checked against what the window offers."""

    def __init__(self, store: QSettings | None = None):
        self.store = store if store is not None else QSettings("ModelMistress", "ModelMistress")

    def get(self, key: str) -> str:
        value = str(self.store.value(f"appearance/{key}", DEFAULTS[key]))
        return value if self._allowed(key, value) else DEFAULTS[key]

    def set(self, key: str, value: str) -> None:
        if not self._allowed(key, value):
            raise ValueError(f"{key} cannot be {value!r}")
        self.store.setValue(f"appearance/{key}", value)

    def window_geometry(self) -> QByteArray | None:
        value = self.store.value("window/geometry")
        return value if isinstance(value, QByteArray) else None

    def save_window_geometry(self, geometry: QByteArray) -> None:
        self.store.setValue("window/geometry", geometry)

    @staticmethod
    def _allowed(key: str, value: str) -> bool:
        if key == "theme":
            return value in THEMES
        if key == "text_size":
            return value in TEXT_SIZES
        if key == "style":
            return value in QStyleFactory.keys()
        return False


class Appearance:
    """Applies the stored look to the application, and changes it when the Settings page asks. The platform's font is
    kept at the start, so the text sizes can always go back to it."""

    def __init__(self, app: QApplication, prefs: Preferences):
        self.app = app
        self.prefs = prefs
        self._base_font = QFont(app.font())

    def apply(self) -> None:
        self._apply_style(self.prefs.get("style"))
        self._apply_text_size(self.prefs.get("text_size"))
        self._apply_theme(self.prefs.get("theme"))

    def set(self, key: str, value: str) -> None:
        self.prefs.set(key, value)
        self.apply()

    def _apply_theme(self, theme: str) -> None:
        QGuiApplication.styleHints().setColorScheme(_SCHEMES[theme])

    def _apply_style(self, style: str) -> None:
        self.app.setStyle(style)

    def _apply_text_size(self, size: str) -> None:
        factor = TEXT_SIZES[size][1]
        font = QFont(self._base_font)
        if self._base_font.pointSizeF() > 0:
            font.setPointSizeF(self._base_font.pointSizeF() * factor)
        else:
            font.setPixelSize(round(self._base_font.pixelSize() * factor))
        self.app.setFont(font)
