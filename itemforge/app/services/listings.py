"""
ItemForge Listings Service - Multi-Platform Listing Generator
"""
import json
from typing import List, Dict, Any, Optional
from datetime import datetime
from pathlib import Path

from app.config import settings
from app.database import get_item, insert_listing, get_listings_by_item, update_listing, log_ai_call
from app.ai.gateway import get_gateway
from app.models import Platform, ListingStatus

class ListingsService:
    """Generates and manages multi-platform listings."""
    
    def __init__(self):
        self.posts_dir = Path(settings.POSTS_DIR)
        self.posts_dir.mkdir(parents=True, exist_ok=True)
        (self.posts_dir / "facebook").mkdir(exist_ok=True)
        (self.posts_dir / "instagram").mkdir(exist_ok=True)
        (self.posts_dir / "ebay").mkdir(exist_ok=True)
        (self.posts_dir / "etsy").mkdir(exist_ok=True)
    
    def _build_fb_prompt(self, item_data: Dict[str, Any]) -> str:
        """Build Facebook Marketplace listing prompt."""
        return f"""
Generate a Facebook Marketplace listing for this product.

Product Information:
- Title: {item_data.get('title', 'Unknown')}
- Type: {item_data.get('type', 'unknown')}
- Brand: {item_data.get('brand', 'Unknown')}
- Colors: {', '.join(item_data.get('colors', [])) or 'Not specified'}
- Material: {item_data.get('material', 'Not specified')}
- Condition: {item_data.get('condition', 'Not specified')}
- Features: {', '.join(item_data.get('features', [])) or 'Not specified'}
- Price Range: {item_data.get('price_range', 'Not specified')}
- Research: {item_data.get('research_notes', 'None')[:500]}

Generate a complete Facebook Marketplace listing in JSON format:
{{
    "title": "Optimized title (max 65 characters, includes brand, type, key features)",
    "description": "Detailed description with bullet points, condition details, and key selling points. Use natural language that buyers search for.",
    "price": "Single price (e.g., $25)",
    "shipping": "Local pickup / Shipping available / Free shipping",
    "keywords": ["keyword1", "keyword2", "keyword3"],
    "photo_captions": [
        "Main photo showing full product",
        "Detail photo showing [specific feature]",
        "Photo showing [specific angle/feature]"
    ]
}}

Facebook Marketplace Optimization Rules:
- Title: Max 65 chars, front-load keywords
- Description: Use natural language, include condition, dimensions, use case
- Price: Single number, not a range
- Keywords: 5-10 relevant search terms
- Photo captions: Descriptive for accessibility
"""
    
    def _build_instagram_prompt(self, item_data: Dict[str, Any]) -> str:
        """Build Instagram listing prompt."""
        return f"""
Generate an Instagram post for this product.

Product Information:
- Title: {item_data.get('title', 'Unknown')}
- Type: {item_data.get('type', 'unknown')}
- Brand: {item_data.get('brand', 'Unknown')}
- Colors: {', '.join(item_data.get('colors', [])) or 'Not specified'}
- Condition: {item_data.get('condition', 'Not specified')}
- Price Range: {item_data.get('price_range', 'Not specified')}

Generate in JSON format:
{{
    "title": "Catchy title for Instagram (max 30 chars)",
    "description": "Instagram caption with emojis, hashtags, and call to action. Include condition, price, and contact info.",
    "price": "Single price",
    "shipping": "DM for shipping / Local pickup",
    "hashtags": ["#vintage", "#forsale", "#collectibles"],
    "photo_captions": ["Main photo", "Detail photo"]
}}
"""
    
    def _build_ebay_prompt(self, item_data: Dict[str, Any]) -> str:
        """Build eBay listing prompt."""
        return f"""
Generate an eBay listing for this product.

Product Information:
- Title: {item_data.get('title', 'Unknown')}
- Type: {item_data.get('type', 'unknown')}
- Brand: {item_data.get('brand', 'Unknown')}
- Colors: {', '.join(item_data.get('colors', [])) or 'Not specified'}
- Material: {item_data.get('material', 'Not specified')}
- Condition: {item_data.get('condition', 'Not specified')}
- Features: {', '.join(item_data.get('features', [])) or 'Not specified'}
- Price Range: {item_data.get('price_range', 'Not specified')}

Generate in JSON format:
{{
    "title": "eBay optimized title (max 80 chars, include brand, model, condition)",
    "description": "eBay description with item specifics, condition details, shipping info",
    "price": "Single price",
    "shipping": "Calculated shipping / Free shipping",
    "keywords": ["keyword1", "keyword2"],
    "photo_captions": ["Photo 1", "Photo 2"],
    "item_specifics": {{"Brand": "X", "Type": "Y", "Condition": "Z"}}
}}
"""
    
    def _build_etsy_prompt(self, item_data: Dict[str, Any]) -> str:
        """Build Etsy listing prompt."""
        return f"""
Generate an Etsy listing for this product.

Product Information:
- Title: {item_data.get('title', 'Unknown')}
- Type: {item_data.get('type', 'unknown')}
- Brand: {item_data.get('brand', 'Unknown')}
- Colors: {', '.join(item_data.get('colors', [])) or 'Not specified'}
- Material: {item_data.get('material', 'Not specified')}
- Condition: {item_data.get('condition', 'Not specified')}

Generate in JSON format:
{{
    "title": "Etsy optimized title (descriptive, keyword-rich, max 140 chars)",
    "description": "Etsy description with story, materials, dimensions, care instructions",
    "price": "Single price",
    "shipping": "Free shipping / Calculated",
    "keywords": ["handmade", "vintage", "material"],
    "photo_captions": ["Main", "Detail"],
    "tags": ["tag1", "tag2", "tag3", "tag4", "tag5", "tag6", "tag7", "tag8", "tag9", "tag10", "tag11", "tag12", "tag13"]
}}
"""
    
    async def generate_listings(self, item_id: str, platforms: List[Platform]) -> Dict[str, Any]:
        """Generate listings for multiple platforms."""
        item = get_item(item_id)
        if not item:
            raise ValueError(f"Item {item_id} not found")

        gateway = await get_gateway()
        results = []

        for platform in platforms:
            try:
                if platform == Platform.FACEBOOK:
                    prompt = self._build_fb_prompt(item)
                elif platform == Platform.INSTAGRAM:
                    prompt = self._build_instagram_prompt(item)
                elif platform == Platform.EBAY:
                    prompt = self._build_ebay_prompt(item)
                elif platform == Platform.ETSY:
                    prompt = self._build_etsy_prompt(item)
                else:
                    continue

                start_time = time.time()
                result = await gateway.chat_completion(
                    messages=[{"role": "user", "content": prompt}],
                    model=settings.DEFAULT_CHAT_MODEL,
                    temperature=0.4,
                    max_tokens=3000
                )
                latency_ms = int((time.time() - start_time) * 1000)

                content = result['choices'][0]['message']['content']

                # Log AI call
                log_ai_call({
                    'model_name': settings.DEFAULT_CHAT_MODEL,
                    'task_type': 'listing',
                    'endpoint': '/v1/chat/completions',
                    'latency_ms': latency_ms,
                    'tokens_prompt': result.get('usage', {}).get('prompt_tokens', 0),
                    'tokens_completion': result.get('usage', {}).get('completion_tokens', 0)
                })

                # Parse JSON
                listing_data = self._extract_json(content)
                if not isinstance(listing_data, dict):
                    listing_data = {
                        'title': item['title'],
                        'description': content[:2000],
                        'price': item.get('price_range', '').split('-')[0] if item.get('price_range') else '$25',
                        'shipping': 'Local pickup',
                        'keywords': [],
                        'photo_captions': []
                    }

                # Save to database
                listing_id = insert_listing({
                    'item_id': item_id,
                    'platform': platform.value,
                    'title': listing_data.get('title', item['title']),
                    'description': listing_data.get('description', ''),
                    'price': listing_data.get('price'),
                    'shipping': listing_data.get('shipping'),
                    'keywords': listing_data.get('keywords', []),
                    'photo_captions': listing_data.get('photo_captions', []),
                    'status': 'draft'
                })

                # Save to file
                platform_dir = self.posts_dir / platform.value
                platform_dir.mkdir(exist_ok=True)
                file_path = platform_dir / f"{item['title']}.json"

                with open(file_path, 'w', encoding='utf-8') as f:
                    json.dump(listing_data, f, indent=2)

                results.append({
                    'platform': platform.value,
                    'listing_id': listing_id,
                    'file_path': str(file_path),
                    **listing_data
                })

            except Exception as e:
                log_ai_call({
                    'model_name': settings.DEFAULT_CHAT_MODEL,
                    'task_type': 'listing',
                    'endpoint': '/v1/chat/completions',
                    'error': f"{platform.value}: {str(e)}"
                })
                results.append({
                    'platform': platform.value,
                    'error': str(e)
                })

        return {
            'id': str(uuid.uuid4()),
            'product_id': item_id,
            'platforms': results,
            'created_at': datetime.utcnow().isoformat()
        }

    def _extract_json(self, content: str) -> Dict[str, Any]:
        """Extract the first valid JSON object from free-form model output."""
        decoder = json.JSONDecoder()
        text = content.strip()
        try:
            obj, _ = decoder.raw_decode(text)
            return obj if isinstance(obj, dict) else {}
        except (json.JSONDecodeError, ValueError):
            pass
        start = text.find('{')
        if start != -1:
            try:
                obj, _ = decoder.raw_decode(text[start:])
                return obj if isinstance(obj, dict) else {}
            except (json.JSONDecodeError, ValueError):
                pass
        return {}


    async def publish_listing(self, listing_id: str, platform: Platform, 
                             title: str, description: str, 
                             price: Optional[str] = None,
                             shipping: Optional[str] = None) -> Dict[str, Any]:
        """Publish a listing (mark as published)."""
        listing = get_listing(listing_id)
        if not listing:
            raise ValueError(f"Listing {listing_id} not found")
        
        updates = {
            'title': title,
            'description': description,
            'status': ListingStatus.PUBLISHED.value,
            'published_at': datetime.utcnow().isoformat()
        }
        if price:
            updates['price'] = price
        if shipping:
            updates['shipping'] = shipping
        
        update_listing(listing_id, updates)
        
        return {
            'id': listing_id,
            'platform': platform.value,
            'title': title,
            'description': description,
            'price': price,
            'status': ListingStatus.PUBLISHED.value,
            'published_at': updates['published_at']
        }

def get_listing(listing_id: str):
    from app.database import get_listing
    return get_listing(listing_id)

import uuid
import time

# Service factory
_listings_service = None

def get_listings_service() -> ListingsService:
    global _listings_service
    if _listings_service is None:
        _listings_service = ListingsService()
    return _listings_service