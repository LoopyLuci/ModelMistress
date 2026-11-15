"""
ItemForge API Routes - Items
"""
from typing import List, Optional, Dict, Any
from fastapi import APIRouter, HTTPException, Query
from fastapi.responses import JSONResponse

from app.models import (
    Item, ItemCreate, ItemUpdate, ItemListResponse,
    ResearchRequest, ResearchResponse,
    DocsResponse,
    ListingRequest, ListingGenerateResponse,
    IngestRequest, IngestResponse
)
from app.services.ingestion import get_ingestion_service
from app.services.research import get_research_service
from app.services.docs import DocumentationService
from app.services.listings import get_listings_service
from app.database import get_item, get_all_items, update_item

router = APIRouter(prefix="/v1/forge/items", tags=["items"])

@router.post("/ingest", response_model=IngestResponse)
async def ingest_photos(request: IngestRequest):
    """Ingest photos from a folder and organize into item folders."""
    ingestion = get_ingestion_service()
    result = ingestion.organize_photos(request)
    return result

@router.get("/catalog", response_model=List[Dict[str, Any]])
async def catalog_items():
    """Get a catalog of all organized items with their images."""
    from pathlib import Path
    items_dir = Path("C:/Users/limpi/OneDrive/Documents/Sell/Items")
    items = []
    if items_dir.exists():
        for item_folder in sorted(items_dir.iterdir()):
            if item_folder.is_dir():
                images = [f.name for f in item_folder.iterdir() 
                         if f.suffix.lower() in ('.jpg', '.jpeg', '.png')]
                items.append({
                    'id': item_folder.name,
                    'title': item_folder.name,
                    'images': images,
                    'type': 'unknown',
                    'images': images
                })
    return items

@router.get("", response_model=ItemListResponse)
async def list_items():
    """List all items with their images."""
    ingestion = get_ingestion_service()
    items = ingestion.get_all_items()
    return {"items": items}

@router.get("/{item_id}", response_model=Item)
async def get_item_endpoint(item_id: str):
    """Get a single item by ID."""
    item = get_item(item_id)
    if not item:
        raise HTTPException(status_code=404, detail="Item not found")
    return item

@router.put("/{item_id}", response_model=Item)
async def update_item_endpoint(item_id: str, updates: ItemUpdate):
    """Update an item."""
    from app.database import update_item as db_update_item
    update_data = updates.model_dump(exclude_unset=True)
    if 'colors' in update_data:
        update_data['colors'] = update_data['colors']
    if 'features' in update_data:
        update_data['features'] = update_data['features']
    if 'dimensions_cm' in update_data:
        update_data['dimensions_cm'] = update_data['dimensions_cm']
    
    success = db_update_item(item_id, update_data)
    if not success:
        raise HTTPException(status_code=404, detail="Item not found or no changes")
    
    item = get_item(item_id)
    return item

@router.post("/{item_id}/research", response_model=ResearchResponse)
async def research_item(item_id: str, request: ResearchRequest):
    """Run AI research on an item."""
    research = get_research_service()
    result = await research.research_item(item_id, request)
    return result

@router.post("/{item_id}/docs", response_model=DocsResponse)
async def generate_docs(item_id: str):
    """Generate markdown documentation for an item."""
    docs = DocumentationService()
    result = await docs.generate_documentation(item_id)
    return result

@router.post("/{item_id}/listings", response_model=ListingGenerateResponse)
async def generate_listings(item_id: str, request: ListingRequest):
    """Generate multi-platform listings for an item."""
    listings = get_listings_service()
    result = await listings.generate_listings(item_id, request.platforms)
    return result