"""
ItemForge Research Service - AI Research
"""
import json
import uuid
from typing import List, Dict, Any, Optional
from datetime import datetime

from app.config import settings
from app.database import get_item, update_item, log_ai_call
from app.ai.gateway import get_gateway

class ResearchService:
    """Handles AI-powered item research."""
    
    def __init__(self):
        pass
    
    def _build_research_prompt(self, item_data: Dict[str, Any]) -> str:
        """Build the research prompt."""
        return f"""
You are an expert product researcher and appraiser. Analyze this product and provide detailed research.

Product Information:
- Title: {item_data.get('title', 'Unknown')}
- Type: {item_data.get('type', 'unknown')}
- Brand: {item_data.get('brand', 'Unknown')}
- Colors: {', '.join(item_data.get('colors', [])) or 'Not specified'}
- Material: {item_data.get('material', 'Not specified')}
- Condition: {item_data.get('condition', 'Not specified')}
- Features: {', '.join(item_data.get('features', [])) or 'Not specified'}
- Current Price Range: {item_data.get('price_range', 'Not specified')}

Provide a comprehensive research response in the following JSON format:
{{
    "title": "Optimized product title",
    "type": "product category",
    "subtype": "specific subcategory",
    "brand": "brand name or 'Unknown'",
    "colors": ["color1", "color2"],
    "material": "material",
    "condition": "condition description",
    "features": ["feature1", "feature2", "feature3"],
    "dimensions_cm": {{"height": 0, "width": 0, "depth": 0}},
    "price_range": "$X-$Y",
    "research_notes": "Detailed research notes about the product including history, typical value, market demand, comparable sales, and any notable details."
}}

Be specific and accurate. If you cannot determine something, use your best estimate based on the available information.
"""
    
    async def research_item(self, item_id: str, request) -> Dict[str, Any]:
        """Research an item using AI."""
        item = get_item(item_id)
        if not item:
            raise ValueError(f"Item {item_id} not found")
        
        gateway = await get_gateway()
        prompt = self._build_research_prompt(item)
        
        start_time = time.time()
        try:
            result = await gateway.chat_completion(
                messages=[{"role": "user", "content": prompt}],
                model=settings.DEFAULT_CHAT_MODEL,
                temperature=0.3,
                max_tokens=3000
            )
            latency_ms = int((time.time() - start_time) * 1000)
            
            # Extract response
            content = result['choices'][0]['message']['content']
            
            # Log AI call
            log_ai_call({
                'model_name': settings.DEFAULT_CHAT_MODEL,
                'task_type': 'research',
                'endpoint': '/v1/chat/completions',
                'latency_ms': latency_ms,
                'tokens_prompt': result.get('usage', {}).get('prompt_tokens', 0),
                'tokens_completion': result.get('usage', {}).get('completion_tokens', 0)
            })
            
            # Parse JSON from response
            try:
                # Find JSON in response
                import re
                json_match = re.search(r'\{.*\}', content, re.DOTALL)
                if json_match:
                    research_data = json.loads(json_match.group())
                else:
                    research_data = json.loads(content)
            except json.JSONDecodeError:
                # Fallback: create structured data from text
                research_data = self._parse_text_research(content, item)
            
            # Update item with research
            updates = {
                'title': research_data.get('title', item['title']),
                'type': research_data.get('type', item['type']),
                'subtype': research_data.get('subtype', item['subtype']),
                'brand': research_data.get('brand', item['brand']),
                'colors': research_data.get('colors', item['colors']),
                'material': research_data.get('material', item['material']),
                'condition': research_data.get('condition', item['condition']),
                'features': research_data.get('features', item['features']),
                'dimensions_cm': research_data.get('dimensions_cm', item['dimensions_cm']),
                'price_range': research_data.get('price_range', item['price_range']),
                'research_notes': research_data.get('research_notes', '')
            }
            
            update_item(item_id, updates)
            
            return {
                'id': item_id,
                **updates,
                'created_at': item['created_at']
            }
            
        except Exception as e:
            log_ai_call({
                'model_name': settings.DEFAULT_CHAT_MODEL,
                'task_type': 'research',
                'endpoint': '/v1/chat/completions',
                'error': str(e)
            })
            raise
    
    def _parse_text_research(self, content: str, item: Dict[str, Any]) -> Dict[str, Any]:
        """Parse research from text when JSON parsing fails."""
        return {
            'title': item['title'],
            'type': item['type'],
            'subtype': item['subtype'],
            'brand': item['brand'],
            'colors': item['colors'],
            'material': item['material'],
            'condition': item['condition'],
            'features': item['features'],
            'dimensions_cm': item['dimensions_cm'],
            'price_range': item['price_range'],
            'research_notes': content[:5000]  # Truncate if too long
        }

# Global service instance
_research_service: Optional[ResearchService] = None

def get_research_service() -> ResearchService:
    global _research_service
    if _research_service is None:
        _research_service = ResearchService()
    return _research_service

# Import time at module level
import time