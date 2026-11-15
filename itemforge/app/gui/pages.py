"""
ItemForge Desktop GUI - Page widgets (PySide6 / Qt).
"""
from __future__ import annotations

import logging
from typing import Any, Dict, List, Optional

from PySide6.QtCore import Qt, QSize, QThread, QThreadPool
from PySide6.QtGui import QAction, QIcon
from PySide6.QtWidgets import (
    QAbstractItemView, QApplication, QDialog, QHBoxLayout, QLabel,
    QLCDNumber, QLineEdit, QListWidget, QListWidgetItem, QMainWindow,
    QMessageBox, QPushButton, QSizePolicy, QSpacerItem, QStackedWidget,
    QTabWidget, QFrame, QScrollArea, QGridLayout, QProgressBar, QWidget,
    QVBoxLayout,
)

from .forge_service import ForgeService

logger = logging.getLogger(__name__)
fs = ForgeService()

ICONS = {
    "items": "\U0001F468\U0001F3FB\U0001F308",
    "listings": "\U0001F4CE",
    "research": "\U0001F4DD",
    "status": "\U0001F4CA",
    "settings": "\U0001F6E0",
    "refresh": "\U0001F504",
    "play": "\U0001F3A4",
    "book": "\U0001F4D8",
    "trash": "\U0001F5D1",
    "camera": "\U0001F4F7",
    "plus": "\U0001F4F7",
}


def run_ai(coro):
    """Run an async AI coroutine on a background thread (non-blocking GUI)."""
    import asyncio

    class _Worker(QObject):
        done = Signal(object)

        def run(self):
            try:
                loop = asyncio.new_event_loop()
                asyncio.set_event_loop(loop)
                result = loop.run_until_complete(coro)
                self.done.emit(result)
            except Exception as e:
                self.done.emit(e)

    w = _Worker()
    QThreadPool.globalInstance().start(w)
    return w


def row_text(item, key, default="—"):
    v = item.get(key)
    return str(v) if v not in (None, "", [], {}) else default


def img_for(item, i=0):
    imgs = item.get("images") or []
    return imgs[i] if imgs and i < len(imgs) else None


def short(text, n=38):
    t = str(text or "").replace("\n", " ").replace("\r", " ").strip()
    return t if len(t) <= n else t[:n] + "…"


class Sidebar(QListWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setMaximumWidth(200)
        self.setFixedWidth(200)
        self.setFocusPolicy(Qt.NoFocus)
        self.setStyleSheet("QListWidget{background:#f4f6f8; border-right:1px solid #ccc;}")
        self.setObjectName("SidebarMenu")

    def add_page(self, name, label, icon=None):
        item = QListWidgetItem(label)
        item.setData(Qt.UserRole, name)
        if icon:
            item.setIcon(QIcon(icon))
        self.addItem(item)
        return item


class SectionCard(QFrame):
    def __init__(self, title, parent=None):
        super().__init__(parent)
        self.setObjectName("Card")
        lay = QVBoxLayout(self)
        lay.setContentsMargins(10, 10, 10, 10)
        lay.setSpacing(6)
        head = QHBoxLayout()
        t = QLabel(title)
        t.setStyleSheet("font-weight: bold; font-size: 11pt; color:#222;")
        head.addWidget(t)
        head.addStretch(1)
        lay.addLayout(head)
        self.body = QVBoxLayout()
        self.body.setContentsMargins(0, 0, 0, 0)
        self.body.setSpacing(6)
        lay.addLayout(self.body)

    def add(self, widget):
        self.body.addWidget(widget)


class StatusBadge(QLabel):
    def __init__(self, ok, parent=None):
        super().__init__(parent)
        self.setAlignment(Qt.AlignCenter)
        self.setStyleSheet(
            "background:#2e7d32; color:white; border-radius:4px; padding:2px 8px; font-weight:bold;"
            if ok else
            "background:#c62828; color:white; border-radius:4px; padding:2px 8px; font-weight:bold;")
        self.setText("ONLINE" if ok else "OFFLINE")


class ItemTableWidget(QListWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setStyleSheet("QListWidget{background:#fff;} QListWidget::item{padding:6px; border-bottom:1px solid #eee;}")
        self.setSelectionMode(QAbstractItemView.ExtendedSelection)
        self.setVerticalScrollMode(QAbstractItemView.ScrollPerPixel)

    def set_items(self, items):
        self.clear()
        for it in items:
            title = row_text(it, "title", it.get("id", ""))
            li = QListWidgetItem(f"<b>{title}</b>", self)
            li.setData(Qt.UserRole, it.get("id", ""))
            li.setIcon(QIcon(ICONS["items"]))

    def set_filter(self, q):
        for i in range(self.count()):
            item = self.item(i)
            txt = item.toolTip() or item.text()
            item.setHidden(q not in txt.lower())


class ItemDetailWidget(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.item_id = None
        self.setStyleSheet("background:#fafafa; border:1px solid #ddd; border-radius:8px;")
        lay = QVBoxLayout(self)
        lay.setSpacing(8)
        self.headers = QHBoxLayout()
        self.headers.setContentsMargins(0, 0, 0, 0)
        self.title_lbl = QLabel("Item detail")
        self.title_lbl.setStyleSheet("font-weight: bold; font-size: 12pt;")
        self.headers.addWidget(self.title_lbl)
        self.emoji = QLabel("\U0001F468\U0001F3FB\U0001F308")
        self.emoji.setFixedSize(18, 18)
        self.headers.addWidget(self.emoji, 0, Qt.AlignVCenter)
        self.headers.addStretch(1)
        lay.addLayout(self.headers)

        body = QVBoxLayout()
        body.setSpacing(6)
        body.setContentsMargins(0, 6, 0, 0)
        self.meta = QGridLayout()
        self.meta.setSpacing(3)
        self.meta.setHorizontalSpacing(80)
        self._rows = {}
        for key, label in [("Type", "Type"), ("Brand", "Brand"), ("Condition", "Condition"), ("Material", "Material"), ("Price", "Price")]:
            r = self.meta.rowCount()
            self.meta.addWidget(QLabel(label), r, 0)
            self._rows[key] = QLabel("—")
            self.meta.addWidget(self._rows[key], r, 1)
        body.addLayout(self.meta)
        self.features_list = QListWidget()
        self.features_list.setStyleSheet("QListWidget{background:white;}")
        self.features_list.setFixedHeight(80)
        body.addWidget(QLabel("Features"), 0)
        body.addWidget(self.features_list)
        self.photos = QListWidget()
        self.photos.setStyleSheet("QListWidget{background:white;}")
        self.photos.setIconSize(QSize(64, 64))
        self.photos.setFlow(QListWidget.LeftToRight)
        self.photos.setWrapping(True)
        body.addWidget(QLabel("Photos"), 0)
        body.addWidget(self.photos)
        lay.addLayout(body)
        self.actions = QHBoxLayout()
        self.actions.setSpacing(6)
        self.research_btn = QPushButton("Research")
        self.research_btn.setToolTip("Run AI research on this item")
        self.docs_btn = QPushButton("Docs")
        self.docs_btn.setToolTip("Generate documentation")
        self.listings_btn = QPushButton("Listings")
        self.listings_btn.setToolTip("Generate multi-platform listings")
        self.delete_btn = QPushButton("Delete", self)
        self.delete_btn.setStyleSheet("background:#c62828;color:white;")
        self.actions.addWidget(self.research_btn)
        self.actions.addWidget(self.docs_btn)
        self.actions.addWidget(self.listings_btn)
        self.actions.addWidget(self.delete_btn)
        lay.addLayout(self.actions)
        self.setLayout(lay)

    def bind(self, item):
        if item is None:
            for key in self._rows:
                self._rows[key].setText("—")
            self.features_list.clear()
            self.photos.clear()
            self.research_btn.setEnabled(False)
            self.docs_btn.setEnabled(False)
            self.listings_btn.setEnabled(False)
            self.delete_btn.setEnabled(False)
            self.title_lbl.setText("Select an item")
            return
        self.item_id = item.get("id")
        self.title_lbl.setText(row_text(item, "title", item.get("id", "")))
        self._rows["Type"].setText(row_text(item, "type"))
        self._rows["Brand"].setText(row_text(item, "brand"))
        self._rows["Condition"].setText(row_text(item, "condition"))
        self._rows["Material"].setText(row_text(item, "material"))
        self._rows["Price"].setText(row_text(item, "price_range"))
        feats = item.get("features") or []
        self.features_list.clear()
        for f in feats:
            self.features_list.addItem(f)
        imgs = item.get("images") or []
        self.photos.clear()
        for img in imgs[:8]:
            self.photos.addItem(QListWidgetItem(QIcon(), img))
        self.research_btn.setEnabled(True)
        self.docs_btn.setEnabled(True)
        self.listings_btn.setEnabled(True)
        self.delete_btn.setEnabled(True)

    def save(self, updates):
        if not self.item_id:
            return


class ItemsPage(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        lay = QHBoxLayout(self)
        lay.setSpacing(8)
        lay.setContentsMargins(0, 0, 0, 0)
        self.side = Sidebar()
        self.side.add_page("items", "Items", ICONS["items"])
        self.side.add_page("listings", "Listings", ICONS["listings"])
        self.side.add_page("status", "Status", ICONS["status"])
        self.side.add_page("settings", "Settings", ICONS["settings"])
        self.side.currentRowChanged.connect(self.on_nav)
        lay.addWidget(self.side, 0)
        lay.setStretch(0, 0)

        self.stack = QStackedWidget()
        lay.addWidget(self.stack, 1)

        self.items_page = self._build_items_page()
        self.listings_page = self._build_listings_page()
        self.status_page = self._build_status_page()
        self.settings_page = self._build_settings_page()
        self.stack.addWidget(self.items_page)
        self.stack.addWidget(self.listings_page)
        self.stack.addWidget(self.status_page)
        self.stack.addWidget(self.settings_page)
        self.side.setCurrentRow(0)
        self.on_nav(0)

    def _build_items_page(self):
        p = QWidget()
        lay = QVBoxLayout(p)
        lay.setSpacing(8)
        lay.setContentsMargins(10, 10, 10, 10)
        top = QHBoxLayout()
        self.filter = QLineEdit()
        self.filter.setPlaceholderText("Search items...")
        self.filter.setClearButtonEnabled(True)
        self.filter.setMaximumWidth(300)
        top.addWidget(self.filter)
        top.addStretch(1)
        self.search_btn = QPushButton("Search")
        self.search_btn.clicked.connect(self._apply_filter)
        top.addWidget(self.search_btn)
        lay.addLayout(top)

        self.list = ItemTableWidget()
        self.list.itemClicked.connect(self._on_item_selected)
        self.detail = ItemDetailWidget()
        self.detail.research_btn.clicked.connect(self._research_selected)
        self.detail.docs_btn.clicked.connect(self._generate_docs)
        self.detail.listings_btn.clicked.connect(self._generate_listings)
        self.detail.delete_btn.clicked.connect(self._delete_selected)
        h = QHBoxLayout()
        h.setSpacing(8)
        h.addWidget(self.list, 1)
        h.addWidget(self.detail, 0)
        lay.addLayout(h)
        self._load_items()
        return p

    def _build_listings_page(self):
        p = QWidget()
        lay = QVBoxLayout(p)
        lay.setSpacing(8)
        lay.setContentsMargins(10, 10, 10, 10)
        top = QHBoxLayout()
        self.list_filter = QLineEdit()
        self.list_filter.setPlaceholderText("Filter listings...")
        top.addWidget(self.list_filter)
        top.addStretch(1)
        lay.addLayout(top)
        self.list_view = ItemTableWidget()
        self.list_view.itemClicked.connect(self._on_listing_selected)
        lay.addWidget(self.list_view)
        self.detail = ItemDetailWidget()
        self.detail.research_btn.hide()
        self.detail.docs_btn.hide()
        self.detail.listings_btn.hide()
        self.detail.delete_btn.hide()
        h = QHBoxLayout()
        h.setSpacing(8)
        h.addWidget(self.list_view, 1)
        h.addWidget(self.detail, 0)
        lay.addLayout(h)
        self._load_listings()
        return p

    def _on_listing_selected(self, item):
        # Listings page detail panel (no-op; detail is managed by the window's
        # own ItemDetailWidget only when browsing items).
        pass

    def _build_status_page(self):
        p = QWidget()
        lay = QVBoxLayout(p)
        lay.setSpacing(8)
        lay.setContentsMargins(10, 10, 10, 10)
        self.status_card = StatusCard()
        lay.addWidget(self.status_card)
        return p

    def _build_settings_page(self):
        p = QWidget()
        lay = QVBoxLayout(p)
        lay.setSpacing(8)
        lay.setContentsMargins(10, 10, 10, 10)
        self.setting_card = SettingCard()
        lay.addWidget(self.setting_card)
        return p

    def _load_items(self):
        items = fs.items_with_images()
        self.list.set_items(items)
        if items:
            self.list.setCurrentRow(0)
            self.detail.bind(items[0])

    def _apply_filter(self):
        q = self.filter.text().lower()
        self.list.set_filter(q)

    def _on_item_selected(self, item):
        self.detail.bind(item.data(Qt.UserRole))

    def _research_selected(self):
        item = self.detail.item_id and fs.get_item(self.detail.item_id)
        if not item:
            QMessageBox.warning(self, "Research", "No item selected.")
            return
        QMessageBox.information(self, "Research", f"Researching: {row_text(item,'title')}...")
        import asyncio
        try:
            loop = asyncio.get_running_loop()
            asyncio.run_coroutine_threadsafe(fs.research_item(item.get("id"), "deep"), loop)
        except RuntimeError:
            run_ai(fs.research_item(item.get("id"), "deep"))

    def _generate_docs(self):
        item = self.detail.item_id and fs.get_item(self.detail.item_id)
        if not item:
            QMessageBox.warning(self, "Docs", "No item selected.")
            return
        QMessageBox.information(self, "Docs", f"Generating docs: {row_text(item,'title')}...")
        import asyncio
        try:
            loop = asyncio.get_running_loop()
            asyncio.run_coroutine_threadsafe(fs.generate_docs(item.get("id")), loop)
        except RuntimeError:
            run_ai(fs.generate_docs(item.get("id")))

    def _generate_listings(self):
        item = self.detail.item_id and fs.get_item(self.detail.item_id)
        if not item:
            QMessageBox.warning(self, "Listings", "No item selected.")
            return
        QMessageBox.information(self, "Listings", f"Generating listings: {row_text(item,'title')}...")
        import asyncio
        try:
            loop = asyncio.get_running_loop()
            asyncio.run_coroutine_threadsafe(fs.generate_listings(item.get("id"), ["facebook", "instagram", "ebay", "etsy"]), loop)
        except RuntimeError:
            run_ai(fs.generate_listings(item.get("id"), ["facebook", "instagram", "ebay", "etsy"]))

    def _delete_selected(self):
        item = self.detail.item_id and fs.get_item(self.detail.item_id)
        if not item:
            return
        if QMessageBox.warning(self, "Delete", f"Delete '{row_text(item,'title')}'?", QMessageBox.Yes | QMessageBox.No) != QMessageBox.Yes:
            return

    def on_nav(self, row):
        self.stack.setCurrentIndex(row)

    def _load_listings(self):
        for l in fs.get_listings():
            self.list_view.addItem(QListWidgetItem(f"<b>{row_text(l,'title')}</b>", self.list_view))
            self.list_view.item(self.list_view.count()-1).setData(Qt.UserRole, l)


class ListingsPage(QWidget):
    def __init__(self):
        super().__init__()
        lay = QVBoxLayout(self)
        lay.setSpacing(8); lay.setContentsMargins(10,10,10,10)
        top = QHBoxLayout(); self.list_filter = QLineEdit(); top.addWidget(self.list_filter); top.addStretch(1); lay.addLayout(top)
        self.list_view = ItemTableWidget()
        self.list_view.itemClicked.connect(self._on_listing_selected)
        lay.addWidget(self.list_view)
        self.detail = ItemDetailWidget()
        self.detail.research_btn.hide(); self.detail.docs_btn.hide(); self.detail.listings_btn.hide()
        self.detail.delete_btn.hide()
        h = QHBoxLayout(); h.setSpacing(8); h.addWidget(self.list_view, 1); h.addWidget(self.detail, 0); lay.addLayout(h)
        self._load_listings()

    def _load_listings(self):
        for l in fs.get_listings():
            self.list_view.addItem(QListWidgetItem(f"<b>{row_text(l,'title')}</b>", self.list_view))
            self.list_view.item(self.list_view.count()-1).setData(Qt.UserRole, l)

    def _on_listing_selected(self, it):
        pass


class StatusCard(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        lay = QVBoxLayout(self); lay.setSpacing(8); lay.setContentsMargins(10,10,10,10)
        header = QHBoxLayout()
        self.badge = StatusBadge(True)
        self.badge.setFixedWidth(110)
        header.addWidget(self.badge, 0)
        self.title = QLabel("ModelMistress"); self.title.setStyleSheet("font-weight:bold;font-size:15pt;")
        header.addStretch(1)
        lay.addLayout(header)
        vb = QVBoxLayout(); vb.setSpacing(6)
        self.model_list = QListWidget(); self.model_list.setFixedHeight(180); self.model_list.setSelectionMode(QListWidget.NoSelection)
        vb.addWidget(QLabel("Loaded models"), 0); vb.addWidget(self.model_list, 1)
        self.empty = QLabel("No model loaded."); self.empty.setWordWrap(True); self.empty.setStyleSheet("color:#888;")
        vb.addWidget(self.empty, 0)
        lay.addLayout(vb)
        self.refresh_btn = QPushButton("Refresh"); self.refresh_btn.clicked.connect(self.refresh)
        lay.addWidget(self.refresh_btn)
        self.refresh()

    def refresh(self):
        import asyncio
        async def _run():
            try:
                from app.ai.gateway import get_gateway
                g = await get_gateway()
                ok = True
                try:
                    await g.health_check()
                except Exception:
                    ok = False
                self.badge.setText("ONLINE" if ok else "OFFLINE")
                self.badge.setStyleSheet(
                    "background:#2e7d32; color:white; border-radius:4px; padding:2px 8px; font-weight:bold;"
                    if ok else "background:#c62828; color:white; border-radius:4px; padding:2px 8px; font-weight:bold;")
                models = []
                try:
                    m = await g.list_models()
                    models = m.get("data", []) if isinstance(m, dict) else []
                except Exception:
                    models = []
                self.model_list.clear()
                for m in models:
                    self.model_list.addItem(m.get("id", ""))
                self.empty.setText(f"No model loaded. Loaded models: {len(models)}")
            except Exception as e:
                self.badge.setText("OFFLINE")
                self.badge.setStyleSheet("background:#c62828; color:white; border-radius:4px; padding:2px 8px; font-weight:bold;")
                self.empty.setText(str(e))
        asyncio.run(_run())


class SettingCard(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        lay = QVBoxLayout(self); lay.setSpacing(8); lay.setContentsMargins(10,10,10,10)
        self.modelmistress_url = QLineEdit()
        try:
            from app.config import settings
            self.modelmistress_url.setText(settings.MODELMISTRESS_URL)
        except Exception:
            self.modelmistress_url.setText("http://127.0.0.1:61419")
        self.modelmistress_url.setPlaceholderText("ModelMistress URL")
        self.save_btn = QPushButton("Save")
        lay.addWidget(QLabel("ModelMistress URL")); lay.addWidget(self.modelmistress_url); lay.addWidget(self.save_btn)
