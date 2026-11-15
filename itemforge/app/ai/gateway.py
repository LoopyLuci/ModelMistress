"""
ItemForge AI Gateway - ModelMistress Client
"""
import asyncio
import hashlib
import json
import time
from typing import Dict, Any, List, Optional
from dataclasses import dataclass, field
from collections import OrderedDict
import aiohttp

from app.config import settings

@dataclass
class CircuitBreaker:
    """Circuit breaker for API calls."""
    failures: int = 0
    last_failure_time: float = 0
    state: str = "closed"  # closed, open, half-open
    max_failures: int = 5
    reset_timeout: int = 60
    
    def record_success(self):
        self.failures = 0
        self.state = "closed"
    
    def record_failure(self):
        self.failures += 1
        self.last_failure_time = time.time()
        if self.failures >= self.max_failures:
            self.state = "open"
    
    def can_attempt(self) -> bool:
        if self.state == "closed":
            return True
        if self.state == "open":
            if time.time() - self.last_failure_time >= self.reset_timeout:
                self.state = "half-open"
                return True
            return False
        # half-open
        return True

class LRUCache:
    """LRU Cache with TTL."""
    def __init__(self, maxsize: int = 50, ttl: int = 300):
        self.maxsize = maxsize
        self.ttl = ttl
        self.cache: OrderedDict[str, tuple] = OrderedDict()
    
    def _make_key(self, *args, **kwargs) -> str:
        key_data = json.dumps([args, sorted(kwargs.items())], sort_keys=True)
        return hashlib.sha256(key_data.encode()).hexdigest()[:32]
    
    def get(self, key: str) -> Optional[Any]:
        if key in self.cache:
            value, timestamp = self.cache[key]
            if time.time() - timestamp < self.ttl:
                # Move to end (most recently used)
                self.cache.move_to_end(key)
                return value
            else:
                # Expired
                del self.cache[key]
        return None
    
    def set(self, key: str, value: Any):
        if key in self.cache:
            self.cache.move_to_end(key)
        elif len(self.cache) >= self.maxsize:
            self.cache.popitem(last=False)
        self.cache[key] = (value, time.time())

class AIGateway:
    """ModelMistress API Gateway with caching and circuit breaker."""
    
    def __init__(self):
        self.settings = settings
        self.session: Optional[aiohttp.ClientSession] = None
        self.cache = LRUCache(maxsize=50, ttl=300)
        self.embedding_cache = LRUCache(maxsize=1000, ttl=86400)  # 24 hours
        self.circuit_breaker = CircuitBreaker(
            max_failures=settings.CIRCUIT_BREAKER_FAILURES,
            reset_timeout=settings.CIRCUIT_BREAKER_RESET_SECONDS
        )
        self.base_url = settings.MODELMISTRESS_URL
        self.token = settings.MODELMISTRESS_TOKEN
    
    async def __aenter__(self):
        self.session = aiohttp.ClientSession(
            headers={"Authorization": f"Bearer {self.token}"} if self.token else {},
            timeout=aiohttp.ClientTimeout(total=60)
        )
        return self
    
    async def __aexit__(self, exc_type, exc_val, exc_tb):
        if self.session:
            await self.session.close()
    
    def _get_headers(self) -> Dict[str, str]:
        return {"Authorization": f"Bearer {self.token}"} if self.token else {}
    
    async def _request(self, method: str, endpoint: str, **kwargs) -> Dict[str, Any]:
        """Make a request with circuit breaker."""
        if not self.circuit_breaker.can_attempt():
            raise Exception("Circuit breaker open - service unavailable")
        
        if not self.session:
            self.session = aiohttp.ClientSession(
                headers=self._get_headers(),
                timeout=aiohttp.ClientTimeout(total=60)
            )
        
        url = f"{self.base_url}{endpoint}"
        
        try:
            async with self.session.request(method, url, **kwargs) as resp:
                if resp.status >= 400:
                    text = await resp.text()
                    raise Exception(f"API error {resp.status}: {text}")
                result = await resp.json()
                self.circuit_breaker.record_success()
                return result
        except Exception as e:
            self.circuit_breaker.record_failure()
            raise
    
    async def health_check(self) -> Dict[str, Any]:
        """Check ModelMistress health."""
        return await self._request("GET", "/health")
    
    async def list_models(self) -> List[Dict[str, Any]]:
        """List available models."""
        return await self._request("GET", "/v1/models")
    
    async def chat_completion(
        self,
        messages: List[Dict[str, Any]],
        model: Optional[str] = None,
        temperature: float = 0.7,
        max_tokens: int = 2000,
        stream: bool = False
    ) -> Dict[str, Any]:
        """Chat completion endpoint."""
        model = model or self.settings.DEFAULT_CHAT_MODEL
        cache_key = self.cache._make_key("chat", model, json.dumps(messages, sort_keys=True))
        
        if cached := self.cache.get(cache_key):
            return cached
        
        payload = {
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": stream
        }
        
        result = await self._request("POST", "/v1/chat/completions", json=payload)
        self.cache.set(cache_key, result)
        return result
    
    async def vision_classify(self, image_b64: str) -> Dict[str, Any]:
        """Classify an image."""
        payload = {"image": f"data:image/jpeg;base64,{image_b64}"}
        return await self._request("POST", "/v1/vision/classify", json=payload)
    
    async def vision_detect(self, image_b64: str) -> Dict[str, Any]:
        """Detect objects in an image."""
        payload = {"image": f"data:image/jpeg;base64,{image_b64}"}
        return await self._request("POST", "/v1/vision/detect", json=payload)
    
    async def vision_embed(self, image_b64: str) -> Dict[str, Any]:
        """Get image embedding."""
        payload = {"image": f"data:image/jpeg;base64,{image_b64}"}
        return await self._request("POST", "/v1/vision/embed", json=payload)
    
    async def embeddings(self, texts: List[str], model: Optional[str] = None) -> List[List[float]]:
        """Generate embeddings."""
        model = model or self.settings.DEFAULT_EMBED_MODEL
        cache_key = self.embedding_cache._make_key("embeddings", model, json.dumps(texts, sort_keys=True))
        
        if cached := self.embedding_cache.get(cache_key):
            return cached
        
        payload = {"model": model, "input": texts}
        result = await self._request("POST", "/v1/embeddings", json=payload)
        
        embeddings = [item["embedding"] for item in result.get("data", [])]
        self.embedding_cache.set(cache_key, embeddings)
        return embeddings
    
    async def rerank(self, query: str, documents: List[str], model: str = "rerank") -> List[float]:
        """Rerank documents."""
        payload = {"model": model, "query": query, "documents": documents}
        result = await self._request("POST", "/v1/rerank", json=payload)
        return [item.get("score", 0) for item in result.get("results", [])]
    
    async def load_knowledge_module(self, km_id: str) -> Dict[str, Any]:
        """Load a knowledge module."""
        return await self._request("POST", f"/v1/km/load", json={"id": km_id})
    
    async def list_knowledge_modules(self) -> Dict[str, Any]:
        """List knowledge modules."""
        return await self._request("GET", "/v1/km/list")
    
    async def speech_to_text(self, audio_b64: str) -> Dict[str, Any]:
        """Speech to text."""
        payload = {"file": f"data:audio/wav;base64,{audio_b64}"}
        return await self._request("POST", "/v1/audio/transcriptions", json=payload)
    
    async def text_to_speech(self, text: str, voice: str = "alloy") -> Dict[str, Any]:
        """Text to speech."""
        payload = {"input": text, "voice": voice}
        return await self._request("POST", "/v1/audio/speech", json=payload)

# Global gateway instance
_gateway: Optional[AIGateway] = None

async def get_gateway() -> AIGateway:
    """Get or create the AI gateway."""
    global _gateway
    if _gateway is None:
        _gateway = AIGateway()
        await _gateway.__aenter__()
    return _gateway

async def close_gateway():
    """Close the AI gateway."""
    global _gateway
    if _gateway:
        await _gateway.__aexit__(None, None, None)
        _gateway = None