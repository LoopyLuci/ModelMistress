"""
ItemForge Configuration
"""
import os
from typing import Optional
from pydantic_settings import BaseSettings
from functools import lru_cache

class Settings(BaseSettings):
    # ModelMistress
    MODELMISTRESS_URL: str = "http://127.0.0.1:61419"
    MODELMISTRESS_TOKEN: str = ""
    
    # API
    API_HOST: str = "0.0.0.0"
    API_PORT: int = 8000
    API_WORKERS: int = 1
    
    # Database
    DATABASE_URL: str = "sqlite:///./itemforge.db"
    
    # File storage
    SELL_DIR: str = "C:/Users/limpi/OneDrive/Documents/Sell"
    ITEMS_DIR: str = "C:/Users/limpi/OneDrive/Documents/Sell/Items"
    DOCS_DIR: str = "C:/Users/limpi/OneDrive/Documents/Sell/Documentation"
    POSTS_DIR: str = "C:/Users/limpi/OneDrive/Documents/Sell/Platform_Posts"
    
    # AI
    DEFAULT_CHAT_MODEL: str = "qwen3.5:9b"
    DEFAULT_EMBED_MODEL: str = "nomic-embed-text-v1.5"
    
    # Rate limiting
    RATE_LIMIT_PER_MINUTE: int = 60
    RATE_LIMIT_PER_DAY: int = 1000
    
    # Circuit breaker
    CIRCUIT_BREAKER_FAILURES: int = 5
    CIRCUIT_BREAKER_RESET_SECONDS: int = 60
    
    class Config:
        env_file = ".env"
        case_sensitive = True

@lru_cache()
def get_settings() -> Settings:
    return Settings()

settings = get_settings()