import struct
import zlib
import os

def create_png(path, w=32, h=32):
    # PNG signature
    data = b'\x89PNG\r\n\x1a\n'
    
    # IHDR
    ihdr = struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)  # 8-bit RGB
    crc = zlib.crc32(b'IHDR' + ihdr) & 0xffffffff
    data += struct.pack('>I', 13) + b'IHDR' + ihdr + struct.pack('>I', crc)
    
    # IDAT - minimal compressed white image
    raw = b'\x00\xff\xff\xff' * w  # filter byte + RGB white
    compressed = zlib.compress(raw * h)
    crc = zlib.crc32(b'IDAT' + compressed) & 0xffffffff
    data += struct.pack('>I', len(compressed)) + b'IDAT' + compressed + struct.pack('>I', crc)
    
    # IEND
    crc = zlib.crc32(b'IEND') & 0xffffffff
    data += struct.pack('>I', 0) + b'IEND' + struct.pack('>I', crc)
    
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'wb') as f:
        f.write(data)

# Create icons directory first
os.makedirs('/Z/Projects/ModelMistress/desktop-app/icons', exist_ok=True)

# Create PNG icons
create_png('/Z/Projects/ModelMistress/desktop-app/icons/32x32.png', 32, 32)
create_png('/Z/Projects/ModelMistress/desktop-app/icons/128x128.png', 128, 128)
create_png('/Z/Projects/ModelMistress/desktop-app/icons/128x128@2x.png', 256, 256)

print("PNG icons created successfully")