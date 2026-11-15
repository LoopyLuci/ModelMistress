"""
ItemForge Documentation Service - Markdown Documentation Generator
"""
import json
import uuid
from typing import List, Dict, Any, Optional
from datetime import datetime

from app.config import settings
from app.database import get_item, update_item, log_ai_call
from app.ai.gateway import get_gateway

class DocumentationService:
    """Generates markdown documentation for items."""
    
    def __init__(self):
        pass
    
    def _build_doc_prompt(self, item_data: Dict[str, Any]) -> str:
        """Build the documentation generation prompt."""
        return f"""
Generate comprehensive markdown documentation for this product.

Product Information:
- Title: {item_data.get('title', 'Unknown')}
- Type: {item_data.get('type', 'unknown')}
- Brand: {item_data.get('brand', 'Unknown')}
- Colors: {', '.join(item_data.get('colors', [])) or 'Not specified'}
- Material: {item_data.get('material', 'Not specified')}
- Condition: {item_data.get('condition', 'Not specified')}
- Features: {', '.join(item_data.get('features', [])) or 'Not specified'}
- Dimensions: {json.dumps(item_data.get('dimensions_cm', {}))}
- Price Range: {item_data.get('price_range', 'Not specified')}
- Research Notes: {item_data.get('research_notes', 'None provided')}

Generate a complete markdown document with the following sections:

# {item_data.get('title', 'Product Documentation')}

## Overview
[3-4 sentences describing the product, its purpose, and key characteristics]

## Key Features
- [Feature 1]
- [Feature 2]
- [Feature 3]
...

## Condition Details
[Detailed description of the product's condition, any wear, damage, or notable characteristics]

## Specifications
| Property | Value |
|----------|-------|
| Type | [type] |
| Subtype | [subtype] |
| Brand | [brand] |
| Material | [material] |
| Colors | [colors] |
| Condition | [condition] |
| Dimensions (cm) | [dimensions] |
| Weight | [if known] |

## Research Notes
[Include the research notes from the research phase]

## Photo Captions
[Describe what each photo shows, e.g., "Main view showing the full product", "Close-up of the manufacturer's mark", "Detail of the condition"]

## Suggested Price
[Price range with rationale]

## SEO Tags
[Comma-separated list of search terms: vintage, collectible, brand, category, etc.]
"""
    
    async def generate_documentation(self, item_id: str) -> Dict[str, Any]:
        """Generate markdown documentation for an item."""
        item = get_item(item_id)
        if not item:
            raise ValueError(f"Item {item_id} not found")
        
        gateway = await get_gateway()
        prompt = self._build_doc_prompt(item)
        
        start_time = time.time()
        try:
            result = await gateway.chat_completion(
                messages=[{"role": "user", "content": prompt}],
                model=settings.DEFAULT_CHAT_MODEL,
                temperature=0.3,
                max_tokens=4000
            )
            latency_ms = int((time.time() - start_time) * 1000)
            
            content = result['choices'][0]['message']['content']
            
            # Log AI call
            log_ai_call({
                'model_name': settings.DEFAULT_CHAT_MODEL,
                'task_type': 'documentation',
                'endpoint': '/v1/chat/completions',
                'latency_ms': latency_ms,
                'tokens_prompt': result.get('usage', {}).get('prompt_tokens', 0),
                'tokens_completion': result.get('usage', {}).get('completion_tokens', 0)
            })
            
            # Update item
            update_item(item_id, {'documentation': content})
            
            # Save to file
            docs_dir = Path("C:/Users/limpi/OneDrive/Documents/Sell/Documentation")
            docs_dir.mkdir(parents=True, exist_ok=True)
            doc_path = docs_dir / f"{item['title']}.md"
            
            with open(doc_path, 'w', encoding='utf-8') as f:
                f.write(content)
            
            return {
                'id': item_id,
                'title': item['title'],
                'documentation': content,
                'file_path': str(doc_path),
                'created_at': datetime.utcnow().isoformat()
            }
            
        except Exception as e:
            log_ai_call({
                'model_name': settings.DEFAULT_CHAT_MODEL,
                'task_type': 'documentation',
                'endpoint': '/v1/chat/completions',
                'error': str(e)
            })
            raise

from pathlib import Path
import time
from datetime import datetime