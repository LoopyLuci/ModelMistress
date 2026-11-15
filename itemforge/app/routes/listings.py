"""
ItemForge API Routes - Listings
"""
from typing import List, Optional
from fastapi import APIRouter, HTTPException

from app.models import (
    Listing, ListingCreate, ListingUpdate, ListingListResponse,
    PublishRequest, PublishResponse
)
from app.database import get_listing, get_listings_by_item, get_all_listings, update_listing as db_update_listing
from app.services.listings import get_listings_service, get_listing as svc_get_listing
from app.models import Platform, ListingStatus

router = APIRouter(prefix="/v1/forge/listings", tags=["listings"])

@router.get("", response_model=ListingListResponse)
async def list_listings(item_id: Optional[str] = None):
    """List all listings, optionally filtered by item."""
    if item_id:
        listings = get_listings_by_item(item_id)
    else:
        listings = get_all_listings()
    return {"listings": listings}

@router.get("/{listing_id}", response_model=Listing)
async def get_listing_endpoint(listing_id: str):
    """Get a single listing by ID."""
    listing = svc_get_listing(listing_id)
    if not listing:
        raise HTTPException(status_code=404, detail="Listing not found")
    return listing

@router.post("/{listing_id}/publish", response_model=PublishResponse)
async def publish_listing(listing_id: str, request: PublishRequest):
    """Publish a listing to a platform."""
    listings = get_listings_service()
    result = await listings.publish_listing(
        listing_id=listing_id,
        platform=request.platform,
        title=request.title,
        description=request.description,
        price=request.price,
        shipping=request.shipping
    )
    return result

@router.put("/{listing_id}", response_model=Listing)
async def update_listing_endpoint(listing_id: str, updates: dict):
    """Update a listing."""
    success = db_update_listing(listing_id, updates)
    if not success:
        raise HTTPException(status_code=404, detail="Listing not found or no changes")
    listing = svc_get_listing(listing_id)
    return listing