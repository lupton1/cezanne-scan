"""Generate the Cezanne Scan icons (PNG, ICO, ICNS) with no third party libraries.

The design is a dark rounded square with a film frame (sprocket holes) and a scan line.
Run: python scripts/make_icons.py
"""
import math, os, struct, zlib

OUT = os.path.join(os.path.dirname(__file__), '..', 'packaging', 'icons')


def render(size):
    s = size / 512.0
    px = bytearray(size * size * 4)

    def blend(x, y, rgb, a):
        if a <= 0:
            return
        i = (y * size + x) * 4
        a = min(1.0, a)
        for k in range(3):
            px[i + k] = int(px[i + k] * (1 - a) + rgb[k] * a)
        px[i + 3] = int(max(px[i + 3], a * 255))

    def rounded(x, y, x0, y0, x1, y1, r):
        # signed coverage of a rounded rectangle, 1 inside, 0 outside, smooth edge
        cx = min(max(x, x0 + r), x1 - r)
        cy = min(max(y, y0 + r), y1 - r)
        d = math.hypot(x - cx, y - cy) - r
        return max(0.0, min(1.0, 0.5 - d))

    for y in range(size):
        for x in range(size):
            fx, fy = (x + 0.5) / s, (y + 0.5) / s
            # background tile
            a = rounded(fx, fy, 16, 16, 496, 496, 96) * min(1.0, s * 2)
            if a > 0:
                t = fy / 512
                bg = (int(28 + 20 * t), int(34 + 24 * t), int(48 + 30 * t))
                blend(x, y, bg, a)
            # film strip
            a = rounded(fx, fy, 72, 132, 440, 380, 18)
            if a > 0:
                blend(x, y, (232, 140, 52), a)
            # frame window
            a = rounded(fx, fy, 128, 172, 384, 340, 10)
            if a > 0:
                g = 0.55 + 0.45 * (1 - (fy - 172) / 168)
                blend(x, y, (int(70 * g), int(150 * g), int(220 * g)), a)
            # hills in the frame
            hill = 290 - 30 * math.sin((fx - 128) / 256 * math.pi * 1.4)
            if 128 <= fx <= 384 and hill <= fy <= 340:
                blend(x, y, (60, 150, 80), rounded(fx, fy, 128, 172, 384, 340, 10))
            # sprocket holes
            for hx in range(92, 430, 44):
                for hy in (146, 352):
                    a = rounded(fx, fy, hx, hy, hx + 22, hy + 18, 4)
                    if a > 0:
                        blend(x, y, (28, 34, 48), a)
            # scan line
            if 236 <= fy <= 248 and 56 <= fx <= 456:
                blend(x, y, (250, 250, 210), 0.9)
    return bytes(px)


def png(size, rgba):
    rows = b''.join(b'\0' + rgba[y * size * 4:(y + 1) * size * 4] for y in range(size))

    def chunk(k, d):
        return struct.pack('>I', len(d)) + k + d + struct.pack('>I', zlib.crc32(k + d) & 0xFFFFFFFF)

    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', size, size, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(rows, 9)) + chunk(b'IEND', b''))


def main():
    os.makedirs(OUT, exist_ok=True)
    pngs = {}
    for size in (16, 32, 48, 64, 128, 256, 512, 1024):
        pngs[size] = png(size, render(size))
    for size in (32, 128, 256, 512):
        open(os.path.join(OUT, f'{size}x{size}.png'), 'wb').write(pngs[size])
    open(os.path.join(OUT, 'icon.png'), 'wb').write(pngs[512])
    # ICO with PNG payloads
    sizes = (16, 32, 48, 64, 128, 256)
    header = struct.pack('<HHH', 0, 1, len(sizes))
    offset = 6 + 16 * len(sizes)
    entries, blobs = b'', b''
    for sz in sizes:
        d = pngs[sz]
        entries += struct.pack('<BBBBHHII', sz % 256, sz % 256, 0, 0, 1, 32, len(d), offset)
        offset += len(d)
        blobs += d
    open(os.path.join(OUT, 'icon.ico'), 'wb').write(header + entries + blobs)
    # ICNS with PNG payloads
    parts = b''
    for kind, sz in ((b'icp4', 16), (b'icp5', 32), (b'ic07', 128), (b'ic08', 256), (b'ic09', 512), (b'ic10', 1024)):
        d = pngs[sz]
        parts += kind + struct.pack('>I', len(d) + 8) + d
    open(os.path.join(OUT, 'icon.icns'), 'wb').write(b'icns' + struct.pack('>I', len(parts) + 8) + parts)
    print('icons written to', os.path.abspath(OUT))


if __name__ == '__main__':
    main()
