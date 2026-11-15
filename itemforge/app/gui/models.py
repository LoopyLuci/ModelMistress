"""
ItemForge Desktop GUI - Qt model helpers.
"""
from __future__ import annotations

from PySide6.QtCore import QAbstractTableModel, QModelIndex, Qt


class ItemsTableModel:
    """Static helper: build a QStandardItemModel for the items list."""

    def __init__(self, items):
        self._items = items

    def rowCount(self, parent=None):
        return len(self._items)

    def columnCount(self, parent=None):
        return 5

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid() or role != Qt.DisplayRole:
            return None
        i = index.row()
        item = self._items[i]
        if i >= len(self._items):
            return None
        col = index.column()
        fields = ["title", "type", "brand", "condition", "images"]
        if col >= len(fields):
            return None
        key = fields[col]
        val = item.get(key, "")
        if isinstance(val, list):
            val = ", ".join(str(x) for x in val)
        return str(val) if val is not None else ""

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if role != Qt.DisplayRole or orientation != Qt.Horizontal:
            return None
        headers = ["Title", "Type", "Brand", "Condition", "Images"]
        if section < len(headers):
            return headers[section]
        return None
