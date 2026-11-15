"""
ItemForge Pydantic Models
"""
from typing import Optional, List, Dict, Any
from pydantic import BaseModel, Field
from datetime import datetime
from enum import Enum

class ItemType(str, Enum):
    GLASSWARE = "glassware"
    TOOL = "tool"
    FURNITURE = "furniture"
    ELECTRONICS = "electronics"
    CLOTHING = "clothing"
    BOOK = "book"
    COLLECTIBLE = "collectible"
    OTHER = "other"

class ItemCondition(str, Enum):
    NEW = "new"
    LIKE_NEW = "like_new"
    GOOD = "good"
    FAIR = "fair"
    POOR = "poor"

class Platform(str, Enum):
    FACEBOOK = "facebook"
    INSTAGRAM = "instagram"
    EBAY = "ebay"
    ETSY = "etsy"

class ListingStatus(str, Enum):
    DRAFT = "draft"
    PUBLISHED = "published"
    FAILED = "failed"
    ARCHIVED = "archived"

class ItemBase(BaseModel):
    title: str = Field(..., min_length=1, max_length=200)
    type: Optional[ItemType] = None
    subtype: Optional[str] = None
    brand: Optional[str] = None
    colors: List[str] = Field(default_factory=list)
    material: Optional[str] = None
    condition: Optional[str] = None
    features: List[str] = Field(default_factory=list)
    dimensions_cm: Optional[Dict[str, float]] = None
    price_range: Optional[str] = None
    # Denormalized list of source image filenames for the item (also stored in the images table).
    images: List[str] = Field(default_factory=list)

class ItemCreate(ItemBase):
    pass

class ItemUpdate(BaseModel):
    title: Optional[str] = None
    type: Optional[ItemType] = None
    subtype: Optional[str] = None
    brand: Optional[str] = None
    colors: Optional[List[str]] = None
    material: Optional[str] = None
    condition: Optional[str] = None
    features: Optional[List[str]] = None
    dimensions_cm: Optional[Dict[str, float]] = None
    price_range: Optional[str] = None
    research_notes: Optional[str] = None
    documentation: Optional[str] = None
    facebook_listing: Optional[str] = None

class Item(ItemBase):
    id: str
    research_notes: Optional[str] = None
    documentation: Optional[str] = None
    facebook_listing: Optional[str] = None
    created_at: datetime = Field(default_factory=datetime.utcnow)
    updated_at: datetime = Field(default_factory=datetime.utcnow)
    
    class Config:
        from_attributes = True

class ListingBase(BaseModel):
    platform: Platform
    title: str
    description: str
    price: Optional[str] = None
    shipping: Optional[str] = None
    keywords: List[str] = Field(default_factory=list)
    photo_captions: List[str] = Field(default_factory=list)

class ListingCreate(ListingBase):
    item_id: str

class ListingUpdate(BaseModel):
    platform: Optional[Platform] = None
    title: Optional[str] = None
    description: Optional[str] = None
    price: Optional[str] = None
    shipping: Optional[str] = None
    keywords: Optional[List[str]] = None
    photo_captions: Optional[List[str]] = None
    status: Optional[ListingStatus] = None

class Listing(ListingBase):
    id: str
    item_id: str
    status: ListingStatus = ListingStatus.DRAFT
    published_at: Optional[datetime] = None
    error_log: Optional[str] = None
    created_at: datetime = Field(default_factory=datetime.utcnow)
    updated_at: datetime = Field(default_factory=datetime.utcnow)
    
    class Config:
        from_attributes = True

class IngestRequest(BaseModel):
    folder: str
    mode: str = "auto"
    skip_duplicates: bool = True
    tag_images: bool = True

class IngestResponse(BaseModel):
    folders_created: int
    images_processed: int
    duplicates_removed: int
    items_created: int
    errors: List[str] = Field(default_factory=list)

class ResearchRequest(BaseModel):
    depth: str = "deep"  # shallow, deep, exhaustive
    include_web: bool = False
    include_knowledge_modules: bool = True

class ResearchResponse(BaseModel):
    id: str
    title: str
    type: Optional[str] = None
    subtype: Optional[str] = None
    brand: Optional[str] = None
    colors: List[str] = Field(default_factory=list)
    material: Optional[str] = None
    condition: Optional[str] = None
    features: List[str] = Field(default_factory=list)
    dimensions_cm: Optional[Dict[str, float]] = None
    price_range: Optional[str] = None
    research_notes: Optional[str] = None
    created_at: datetime

class DocsResponse(BaseModel):
    id: str
    title: str
    documentation: str
    created_at: datetime

class ListingRequest(BaseModel):
    platforms: List[Platform] = [Platform.FACEBOOK, Platform.INSTAGRAM, Platform.EBAY, Platform.ETSY]
    title_override: Optional[str] = None
    description_override: Optional[str] = None

class ListingGenerateResponse(BaseModel):
    id: str
    product_id: str
    platforms: List[Dict[str, Any]]
    created_at: datetime

class PublishRequest(BaseModel):
    platform: Platform
    title: str
    description: str
    price: Optional[str] = None
    shipping: Optional[str] = None

class PublishResponse(BaseModel):
    id: str
    platform: Platform
    title: str
    description: str
    price: Optional[str] = None
    status: ListingStatus
    published_at: Optional[datetime] = None

class StatusResponse(BaseModel):
    status: str
    uptime_seconds: int
    modelmistress: Dict[str, Any]

class AIStatusResponse(BaseModel):
    connected: bool
    url: str
    token_provided: bool
    models: List[Dict[str, str]]

class ItemListResponse(BaseModel):
    items: List[Item]

class ListingListResponse(BaseModel):
    listings: List[Listing]