"""
ItemForge FastAPI Application
"""
from contextlib import asynccontextmanager
from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware
from fastapi.staticfiles import StaticFiles
import logging

from app.config import settings
from app.routes import items, listings, status
from app.ai.gateway import get_gateway, close_gateway
from app.database import init_db

# Configure logging
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

@asynccontextmanager
async def lifespan(app: FastAPI):
    # Startup
    logger.info("Starting ItemForge API...")
    init_db()
    logger.info("Database initialized")
    
    # Initialize AI gateway
    try:
        await get_gateway()
        logger.info("AI Gateway connected")
    except Exception as e:
        logger.warning(f"AI Gateway not available: {e}")
    
    yield
    
    # Shutdown
    logger.info("Shutting down ItemForge API...")
    await close_gateway()
    logger.info("AI Gateway closed")

app = FastAPI(
    title="ItemForge API",
    description="All-in-One Item Listing & Sales Platform",
    version="2.0.0",
    lifespan=lifespan
)

# CORS
app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

# Include routers
app.include_router(items.router)
app.include_router(listings.router)
app.include_router(status.router)

# Health check
@app.get("/health")
async def health():
    return {"status": "healthy", "service": "itemforge"}

# Root
@app.get("/")
async def root():
    return {
        "name": "ItemForge",
        "version": "2.0.0",
        "description": "All-in-One Item Listing & Sales Platform",
        "docs": "/docs",
        "health": "/health"
    }

if __name__ == "__main__":
    import uvicorn
    uvicorn.run(
        "app.main:app",
        host=settings.API_HOST,
        port=settings.API_PORT,
        reload=True
    )