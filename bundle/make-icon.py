#!/usr/bin/env python3
"""Generate a placeholder AppIcon.png: the capsule's own mark, four bars.

This is a TODO. It exists so the bundle is well-formed, and replacing it is
a single file change plus a rebuild. Written with the standard library only,
because a placeholder icon does not justify a dependency.
"""
import struct
import sys
import zlib

SIZE = 1024
BACKGROUND = (10, 10, 10)
BAR = (110, 110, 110)
# x fraction, height fraction: the same resting silhouette the capsule paints.
BARS = [(0.30, 0.22), (0.42, 0.46), (0.54, 0.30), (0.66, 0.40)]
BAR_WIDTH = 0.055


def main(out_path):
    rows = []
    for y in range(SIZE):
        row = bytearray()
        for x in range(SIZE):
            row += bytes(pixel(x / SIZE, y / SIZE))
        rows.append(bytes(row))

    raw = b"".join(b"\x00" + row for row in rows)
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", SIZE, SIZE, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 9))
    png += chunk(b"IEND", b"")
    with open(out_path, "wb") as handle:
        handle.write(png)


def pixel(u, v):
    for left, height in BARS:
        if left <= u < left + BAR_WIDTH and abs(v - 0.5) < height / 2:
            return BAR
    return BACKGROUND


def chunk(tag, data):
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


if __name__ == "__main__":
    main(sys.argv[1])
