"""
ItemForge Ingestion Service - Photo Organization
"""
import os
import shutil
import json
import uuid
from pathlib import Path
from typing import List, Dict, Any, Optional
from PIL import Image
import numpy as np
from sklearn.cluster import MiniBatchKMeans

from app.config import settings
from app.database import insert_item, init_db

class IngestionService:
    """Handles photo ingestion and organization."""
    
    def __init__(self):
        self.sell_dir = Path(settings.SELL_DIR)
        self.items_dir = Path(settings.ITEMS_DIR)
        self.items_dir.mkdir(parents=True, exist_ok=True)
    
    def analyze_image(self, image_path: Path) -> Dict[str, Any]:
        """Analyze a single image and extract features."""
        with Image.open(image_path) as img:
            width, height = img.size
            img_rgb = img.convert('RGB')
            arr = np.array(img_rgb)
            
            brightness = float(np.mean(arr))
            gray = np.mean(arr, axis=2)
            edges = np.abs(np.gradient(gray, axis=0)) + np.abs(np.gradient(gray, axis=1))
            edge_density = float(np.mean(edges))
            
            r_mean = float(np.mean(arr[:,:,0]))
            g_mean = float(np.mean(arr[:,:,1]))
            b_mean = float(np.mean(arr[:,:,2]))
            
            return {
                'filename': image_path.name,
                'width': width,
                'height': height,
                'brightness': round(brightness, 1),
                'edge_density': round(edge_density, 1),
                'r_mean': round(r_mean, 1),
                'g_mean': round(g_mean, 1),
                'b_mean': round(b_mean, 1),
                'size': image_path.stat().st_size
            }
    
    def cluster_images(self, images: List[Dict[str, Any]], n_clusters: int = 7) -> Dict[int, List[Dict[str, Any]]]:
        """Cluster images based on visual features."""
        features = np.array([
            [img['brightness'], img['edge_density'], img['r_mean'], img['g_mean'], img['b_mean']]
            for img in images
        ])
        
        # Use silhouette score to find optimal clusters
        best_k = n_clusters
        best_score = -1
        
        for k in range(3, min(15, len(images))):
            km = MiniBatchKMeans(n_clusters=k, random_state=42, n_init=3)
            labels = km.fit_predict(features)
            try:
                from sklearn.metrics import silhouette_score
                score = silhouette_score(features, labels)
                if score > best_score:
                    best_score = score
                    best_k = k
            except:
                pass
        
        km = MiniBatchKMeans(n_clusters=best_k, random_state=42, n_init=3)
        labels = km.fit_predict(features)
        
        clusters = {}
        for i, (img, label) in enumerate(zip(images, labels)):
            if label not in clusters:
                clusters[label] = []
            clusters[label].append(img)
        
        return clusters
    
    def organize_photos(self, request) -> Dict[str, Any]:
        """Organize photos into item folders."""
        # Get all images
        images = []
        for f in sorted(os.listdir(self.sell_dir)):
            fp = self.sell_dir / f
            if fp.is_file() and f.lower().endswith(('.jpg', '.jpeg', '.png')):
                try:
                    img_data = self.analyze_image(fp)
                    images.append(img_data)
                except Exception as e:
                    print(f"Error analyzing {f}: {e}")
        
        # Cluster images
        clusters = self.cluster_images(images, n_clusters=7)
        
        # Create folders and move photos
        folders_created = 0
        images_processed = 0
        duplicates_removed = 0
        items_created = 0
        errors = []
        
        # Predefined folder names for known items
        cluster_names = {
            0: "02_Vintage_Glassware_Set",
            1: "04_Dark_Cool_Items",
            2: "05_Dark_Warm_Items",
            3: "06_Yellow_Warm_Items",
            4: "01_Military_Pail",
            5: "08_Medium_Bright_Items",
            6: "09_Colorful_Bright_Items"
        }
        
        for cluster_id, cluster_images in clusters.items():
            folder_name = cluster_names.get(cluster_id, f"Item_{cluster_id}")
            cluster_folder = self.items_dir / folder_name
            cluster_folder.mkdir(parents=True, exist_ok=True)
            folders_created += 1
            
            # Create item in database
            first_img = cluster_images[0]
            item_id = insert_item({
                'title': folder_name,
                'type': 'other',
            })
            items_created += 1
            
            for img in cluster_images:
                src = self.sell_dir / img['filename']
                dst = cluster_folder / img['filename']
                
                # Check for duplicates
                if dst.exists():
                    # Check if it's a true duplicate (same size)
                    if src.stat().st_size == dst.stat().st_size:
                        duplicates_removed += 1
                        src.unlink()  # Remove duplicate
                        continue
                
                try:
                    shutil.move(str(src), str(dst))
                    images_processed += 1
                except Exception as e:
                    errors.append(f"Failed to move {img['filename']}: {e}")
        
        return {
            'folders_created': folders_created,
            'images_processed': images_processed,
            'duplicates_removed': duplicates_removed,
            'items_created': items_created,
            'errors': errors
        }
    
    def get_all_items(self) -> List[Dict[str, Any]]:
        """Get all items with their images."""
        from app.database import get_all_items
        items = get_all_items()
        
        # Add image info for each item
        for item in items:
            item_folder = self.items_dir / item['title']
            if item_folder.exists():
                item['images'] = sorted([f.name for f in item_folder.iterdir() 
                                        if f.suffix.lower() in ('.jpg', '.jpeg', '.png')])
            else:
                item['images'] = []
        
        return items

# Global service instance
_ingestion_service: Optional[IngestionService] = None

def get_ingestion_service() -> IngestionService:
    global _ingestion_service
    if _ingestion_service is None:
        _ingestion_service = IngestionService()
    return _ingestion_service