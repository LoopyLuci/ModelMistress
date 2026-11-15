"""
ItemForge Database Module
"""
import sqlite3
import json
import uuid
from typing import Optional, List, Dict, Any
from contextlib import contextmanager
from datetime import datetime
from pathlib import Path

from app.config import settings

DB_PATH = Path(settings.DATABASE_URL.replace("sqlite:///", ""))

def init_db():
    """Initialize the database with tables."""
    conn = sqlite3.connect(DB_PATH)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()
    
    # Items table
    cursor.execute("""
        CREATE TABLE IF NOT EXISTS items (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            type TEXT,
            subtype TEXT,
            brand TEXT,
            colors TEXT,
            material TEXT,
            condition TEXT,
            features TEXT,
            dimensions_cm TEXT,
            price_range TEXT,
            research_notes TEXT,
            documentation TEXT,
            facebook_listing TEXT,
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now'))
        )
    """)
    
    # Images table
    cursor.execute("""
        CREATE TABLE IF NOT EXISTS images (
            id TEXT PRIMARY KEY,
            item_id TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            role TEXT NOT NULL,
            embedding TEXT,
            perceptual_hash TEXT,
            uploaded_at TEXT DEFAULT (datetime('now')),
            FOREIGN KEY (item_id) REFERENCES items(id) ON DELETE CASCADE
        )
    """)
    
    # Listings table
    cursor.execute("""
        CREATE TABLE IF NOT EXISTS listings (
            id TEXT PRIMARY KEY,
            item_id TEXT NOT NULL,
            platform TEXT NOT NULL,
            title TEXT,
            description TEXT,
            price TEXT,
            shipping TEXT,
            keywords TEXT,
            photo_captions TEXT,
            status TEXT DEFAULT 'draft',
            published_at TEXT,
            error_log TEXT,
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now')),
            FOREIGN KEY (item_id) REFERENCES items(id) ON DELETE CASCADE
        )
    """)
    
    # AI logs table
    cursor.execute("""
        CREATE TABLE IF NOT EXISTS ai_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            model_name TEXT NOT NULL,
            task_type TEXT NOT NULL,
            endpoint TEXT NOT NULL,
            prompt_hash TEXT,
            response_hash TEXT,
            latency_ms INTEGER,
            tokens_prompt INTEGER,
            tokens_completion INTEGER,
            error TEXT,
            timestamp TEXT DEFAULT (datetime('now'))
        )
    """)
    
    # Create indexes
    cursor.execute("CREATE INDEX IF NOT EXISTS idx_items_type ON items(type)")
    cursor.execute("CREATE INDEX IF NOT EXISTS idx_images_item_id ON images(item_id)")
    cursor.execute("CREATE INDEX IF NOT EXISTS idx_listings_item_id ON listings(item_id)")
    cursor.execute("CREATE INDEX IF NOT EXISTS idx_listings_platform ON listings(platform)")
    
    conn.commit()
    conn.close()

@contextmanager
def get_db():
    """Get database connection."""
    conn = sqlite3.connect(DB_PATH)
    conn.row_factory = sqlite3.Row
    try:
        yield conn
    finally:
        conn.close()

def execute_query(query: str, params: tuple = ()) -> List[Dict[str, Any]]:
    """Execute a query and return results."""
    with get_db() as conn:
        cursor = conn.cursor()
        cursor.execute(query, params)
        conn.commit()
        return [dict(row) for row in cursor.fetchall()]

def execute_one(query: str, params: tuple = ()) -> Optional[Dict[str, Any]]:
    """Execute a query and return one result."""
    results = execute_query(query, params)
    return results[0] if results else None

def insert_item(item_data: Dict[str, Any]) -> str:
    """Insert an item and return its ID."""
    item_id = str(uuid.uuid4())
    now = datetime.utcnow().isoformat()
    
    with get_db() as conn:
        cursor = conn.cursor()
        cursor.execute("""
            INSERT INTO items (id, title, type, subtype, brand, colors, material, condition,
                             features, dimensions_cm, price_range, research_notes, documentation,
                             facebook_listing, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """, (
            item_id,
            item_data.get('title', ''),
            item_data.get('type'),
            item_data.get('subtype'),
            item_data.get('brand'),
            json.dumps(item_data.get('colors', [])),
            item_data.get('material'),
            item_data.get('condition'),
            json.dumps(item_data.get('features', [])),
            json.dumps(item_data.get('dimensions_cm', {})),
            item_data.get('price_range'),
            item_data.get('research_notes'),
            item_data.get('documentation'),
            item_data.get('facebook_listing'),
            now,
            now
        ))
        conn.commit()
    return item_id

def get_item(item_id: str) -> Optional[Dict[str, Any]]:
    """Get an item by ID."""
    row = execute_one("SELECT * FROM items WHERE id = ?", (item_id,))
    if row:
        row['colors'] = json.loads(row['colors']) if row['colors'] else []
        row['features'] = json.loads(row['features']) if row['features'] else []
        row['dimensions_cm'] = json.loads(row['dimensions_cm']) if row['dimensions_cm'] else {}
    return row

def get_all_items() -> List[Dict[str, Any]]:
    """Get all items."""
    rows = execute_query("SELECT * FROM items ORDER BY created_at DESC")
    for row in rows:
        row['colors'] = json.loads(row['colors']) if row['colors'] else []
        row['features'] = json.loads(row['features']) if row['features'] else []
        row['dimensions_cm'] = json.loads(row['dimensions_cm']) if row['dimensions_cm'] else {}
    return rows

def update_item(item_id: str, updates: Dict[str, Any]) -> bool:
    """Update an item."""
    now = datetime.utcnow().isoformat()
    fields = []
    params = []
    
    for key, value in updates.items():
        if key in ['colors', 'features', 'dimensions_cm']:
            fields.append(f"{key} = ?")
            params.append(json.dumps(value))
        else:
            fields.append(f"{key} = ?")
            params.append(value)
    
    if not fields:
        return False
    
    fields.append("updated_at = ?")
    params.append(now)
    params.append(item_id)
    
    with get_db() as conn:
        cursor = conn.cursor()
        cursor.execute(f"UPDATE items SET {', '.join(fields)} WHERE id = ?", params)
        conn.commit()
        return cursor.rowcount > 0

def insert_listing(listing_data: Dict[str, Any]) -> str:
    """Insert a listing and return its ID."""
    listing_id = str(uuid.uuid4())
    now = datetime.utcnow().isoformat()
    
    with get_db() as conn:
        cursor = conn.cursor()
        cursor.execute("""
            INSERT INTO listings (id, item_id, platform, title, description, price, shipping,
                                keywords, photo_captions, status, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """, (
            listing_id,
            listing_data['item_id'],
            listing_data['platform'],
            listing_data.get('title', ''),
            listing_data.get('description', ''),
            listing_data.get('price'),
            listing_data.get('shipping'),
            json.dumps(listing_data.get('keywords', [])),
            json.dumps(listing_data.get('photo_captions', [])),
            listing_data.get('status', 'draft'),
            now,
            now
        ))
        conn.commit()
    return listing_id

def get_listing(listing_id: str) -> Optional[Dict[str, Any]]:
    """Get a listing by ID."""
    row = execute_one("SELECT * FROM listings WHERE id = ?", (listing_id,))
    if row:
        row['keywords'] = json.loads(row['keywords']) if row['keywords'] else []
        row['photo_captions'] = json.loads(row['photo_captions']) if row['photo_captions'] else []
    return row

def get_listings_by_item(item_id: str) -> List[Dict[str, Any]]:
    """Get all listings for an item."""
    rows = execute_query("SELECT * FROM listings WHERE item_id = ? ORDER BY created_at DESC", (item_id,))
    for row in rows:
        row['keywords'] = json.loads(row['keywords']) if row['keywords'] else []
        row['photo_captions'] = json.loads(row['photo_captions']) if row['photo_captions'] else []
    return rows

def get_all_listings() -> List[Dict[str, Any]]:
    """Get all listings."""
    rows = execute_query("SELECT * FROM listings ORDER BY created_at DESC")
    for row in rows:
        row['keywords'] = json.loads(row['keywords']) if row['keywords'] else []
        row['photo_captions'] = json.loads(row['photo_captions']) if row['photo_captions'] else []
    return rows

def update_listing(listing_id: str, updates: Dict[str, Any]) -> bool:
    """Update a listing."""
    now = datetime.utcnow().isoformat()
    fields = []
    params = []
    
    for key, value in updates.items():
        if key in ['keywords', 'photo_captions']:
            fields.append(f"{key} = ?")
            params.append(json.dumps(value))
        else:
            fields.append(f"{key} = ?")
            params.append(value)
    
    if not fields:
        return False
    
    fields.append("updated_at = ?")
    params.append(now)
    params.append(listing_id)
    
    with get_db() as conn:
        cursor = conn.cursor()
        cursor.execute(f"UPDATE listings SET {', '.join(fields)} WHERE id = ?", params)
        conn.commit()
        return cursor.rowcount > 0

def log_ai_call(log_data: Dict[str, Any]) -> int:
    """Log an AI API call."""
    with get_db() as conn:
        cursor = conn.cursor()
        cursor.execute("""
            INSERT INTO ai_logs (model_name, task_type, endpoint, prompt_hash, response_hash,
                                latency_ms, tokens_prompt, tokens_completion, error)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        """, (
            log_data['model_name'],
            log_data['task_type'],
            log_data['endpoint'],
            log_data.get('prompt_hash'),
            log_data.get('response_hash'),
            log_data.get('latency_ms'),
            log_data.get('tokens_prompt'),
            log_data.get('tokens_completion'),
            log_data.get('error')
        ))
        conn.commit()
        return cursor.lastrowid

# Initialize database on import
init_db()