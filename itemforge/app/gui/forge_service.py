"""
ItemForge Desktop GUI - Data access layer.

Wraps the backend services (app.services + app.database) so the desktop GUI can
read items/listings and run research/docs/listings against the local ModelMistress
hub, without depending on the FastAPI server being up.
"""

from __future__ import annotations

import logging
from typing import Any, Dict, List, Optional

from app.config import settings
from app.database import (
    get_all_items,
    get_item,
    get_all_listings,
    get_listing,
    get_listings_by_item,
)
from app.services.ingestion import get_ingestion_service
from app.services.research import get_research_service
from app.services.docs import DocumentationService
from app.services.listings import get_listings_service
from app.ai.gateway import get_gateway

# Resolve the DB path against the repo root so the GUI data layer reads the
# same database the backend serves (regardless of the CWD the GUI was launched
# from). The backend changes directory to the repo root at startup.
import app.database
from pathlib import Path
_app_root = Path(__file__).resolve().parent.parent.parent
app.database.DB_PATH = _app_root / "itemforge.db"

logger = logging.getLogger(__name__)


class ForgeService:
    """Single point of access to ItemForge data & AI services."""

    _instance: Optional["ForgeService"] = None
    _initialized = False

    def __new__(cls):
        if cls._instance is None or not cls._initialized:
            cls._instance = super().__new__(cls)
            cls._initialized = True
            cls._instance._init()
        return cls._instance

    def _init(self) -> None:
        self.ingestion = get_ingestion_service()
        self.research = get_research_service()
        self.docs = DocumentationService()
        self.listings = get_listings_service()

    async def ai_connected(self) -> bool:
        try:
            g = await get_gateway()
            try:
                await g.health_check()
                return True
            except Exception:
                return False
        except Exception:
            return False

    def items_with_images(self) -> List[Dict[str, Any]]:
        return self.ingestion.get_all_items()

    def get_item(self, item_id: str) -> Optional[Dict[str, Any]]:
        return get_item(item_id)

    def get_listings(self) -> List[Dict[str, Any]]:
        return get_all_listings()

    def get_listings_by_item(self, item_id: str) -> List[Dict[str, Any]]:
        return get_listings_by_item(item_id)

    def get_listing(self, listing_id: str) -> Optional[Dict[str, Any]]:
        return get_listing(listing_id)

    async def research_item(self, item_id: str, depth: str = "deep") -> Dict[str, Any]:
        R = type("Req", (), {"depth": depth})()
        return await self.research.research_item(item_id, R)

    async def generate_docs(self, item_id: str) -> Dict[str, Any]:
        return await self.docs.generate_documentation(item_id)

    async def generate_listings(self, item_id: str, platforms: List[str]) -> Dict[str, Any]:
        from app.models import Platform
        p = [Platform(x) for x in platforms]
        return await self.listings.generate_listings(item_id, p)

    async def publish_listing(self, listing_id: str, platform: str,
                              title: str, description: str,
                              price: Optional[str], shipping: Optional[str]) -> Dict[str, Any]:
        from app.models import Platform
        p = Platform(platform)
        return await self.listings.publish_listing(listing_id, p, title, description, price, shipping)
