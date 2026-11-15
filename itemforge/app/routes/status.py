"""
ItemForge API Routes - Status
"""
from fastapi import APIRouter
import time
import os

from app.models import StatusResponse, AIStatusResponse
from app.config import settings
from app.ai.gateway import get_gateway
from app.database import init_db

router = APIRouter(prefix="/v1/forge", tags=["status"])

# Track start time
START_TIME = time.time()

@router.get("/status", response_model=StatusResponse)
async def status():
    """Health check endpoint."""
    try:
        gateway = await get_gateway()
        health = await gateway.health_check()
        models = await gateway.list_models()
        return {
            "status": "healthy",
            "uptime_seconds": int(time.time() - START_TIME),
            "modelmistress": {
                "connected": True,
                "url": settings.MODELMISTRESS_URL,
                "models": [m.get("id", "") for m in models.get("data", [])]
            }
        }
    except Exception as e:
        return {
            "status": "healthy",
            "uptime_seconds": int(time.time() - START_TIME),
            "modelmistress": {
                "connected": False,
                "url": settings.MODELMISTRESS_URL,
                "models": [],
                "error": str(e)
            }
        }

@router.get("/ai/status", response_model=AIStatusResponse)
async def ai_status():
    """Check ModelMistress connection and loaded models."""
    try:
        gateway = await get_gateway()
        health = await gateway.health_check()
        models = await gateway.list_models()
        
        return {
            "connected": True,
            "url": settings.MODELMISTRESS_URL,
            "token_provided": bool(settings.MODELMISTRESS_TOKEN),
            "models": [
                {"id": m.get("id", ""), "name": m.get("object", ""), "status": "loaded"}
                for m in models.get("data", [])
            ]
        }
    except Exception as e:
        return {
            "connected": False,
            "url": settings.MODELMISTRESS_URL,
            "token_provided": bool(settings.MODELMISTRESS_TOKEN),
            "models": [],
            "error": str(e)
        }