import struct
import zlib

def create_minimal_png(filename, width=32, height=32):
    # PNG signature
    signature = b'\x89PNG\r\n\x1a\n'
    
    # IHDR chunk
    ihdr_data = struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0)
    ihdr_crc = zlib.crc32(b'IHDR' + ihdr_data)
    ihdr = struct.pack('>I', 13) + b'IHDR' + ihdr_data + struct.pack('>I', ihdr_crc & 0xffffffff)
    
    # IDAT chunk (solid white pixel)
    raw_data = b'\x00\xff\xff\xff' * (width * 4)  # Filter byte + RGB for white
    compressed = zlib.compress(raw_data)
    idat_crc = zlib.crc32(b'IDAT' + compressed)
    idat = struct.pack('>I', len(compressed)) + b'IDAT' + compressed + struct.pack('>I', idat_crc & 0xffffffff)
    
    # IEND chunk
    iend_crc = zlib.crc32(b'IEND')
    iend = struct.pack('>I', 0) + b'IEND' + struct.pack('>I', iend_crc & 0xffffffff)
    
    with open(filename, 'wb') as f:
        f.write(signature + ihdr + idat + iend)

create_minimal_png('/Z/Projects/ModelMistress/desktop-app/icons/32x32.png')
create_minimal_png('/Z/Projects/ModelMistress/desktop-app/icons/128x128.png')
create_minimal_png('/Z/Projects/ModelMistress/desktop-app/icons/128x128@2x.png')

# Create a minimal ICO file
with open('/Z/Projects/ModelMistress/desktop-app/icons/app.ico', 'wb') as f:
    # ICONDIR
    f.write(b'\x00\x00')  # Reserved
    f.write(b'\x01\x00')  # Type: 1 = ICO
    f.write(b'\x01\x00')  # Count: 1
    # ICONDIRENTRY
    f.write(b'\x20\x00')  # Width: 32
    f.write(b'\x20\x00')  # Height: 32
    f.write(b'\x00')      # Color palette
    f.write(b'\x00')      # Reserved
    f.write(b'\x01\x00')  # Color planes
    f.write(b'\x18\x00')  # Bits per pixel: 24
    f.write(struct.pack('<I', 22))  # Size of image data (will be corrected)
    # Image data offset placeholder
    f.write(b'\x16\x00\x00\x00')  # Offset to image data

print("Icons created successfully")

import struct, zlib