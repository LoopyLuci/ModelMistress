import struct
import zlib

def create_png(filename, w=32, h=32):
    sig = b'\x89PNG\r\n\x1a\n'
    ihdr = struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)
    ihdr_crc = zlib.crc32(b'IHDR' + ihdr)
    chunk1 = struct.pack('>I', 13) + b'IHDR' + ihdr + struct.pack('>I', ihdr_crc & 0xffffffff)
    raw = b'\x00' + b'\xff\xff\xff' * w  # white row with filter byte
    data = zlib.compress(raw * h)
    idat_crc = zlib.crc32(b'IDAT' + data)
    chunk2 = struct.pack('>I', len(data)) + b'IDAT' + data + struct.pack('>I', idat_crc & 0xffffffff)
    iend_crc = zlib.crc32(b'IEND')
    chunk3 = struct.pack('>I', 0) + b'IEND' + struct.pack('>I', iend_crc & 0xffffffff)
    open(filename, 'wb').write(sig + chunk1 + chunk2 + chunk3)

import os
os.makedirs('/Z/Projects/ModelMistress/desktop-app/icons', exist_ok=True)

for f in ['32x32.png', '128x128.png', '128x128@2x.png', 'icons/32x32.png']:
    path = '/Z/Projects/ModelMistress/desktop-app/' + f
    create_png(path)
    print(f'Created: {path}')

print('Done!')