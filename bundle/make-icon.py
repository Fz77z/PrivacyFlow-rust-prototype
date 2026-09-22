#!/usr/bin/env python3
"""Render PrivacyFlow's app icon: the capsule, given thickness and tilted.

The icon is the widget itself as an object. The capsule is the thing on
screen all day, so the icon is that shape with depth, floating over its own
light, and deliberately empty: the mark that normally sits inside the
capsule is a state readout, and state belongs to the widget rather than to
an icon sitting still in a dock.

Written with the standard library only. There is no SVG renderer on a stock
macOS, and a gradient and four polygons do not justify a dependency, so this
rasterises them directly: polygons are filled by a scanline crossing test,
gradients are interpolated per pixel, and the whole image is drawn at four
times the requested size and box filtered down, which is what gives the
isometric edges clean antialiasing.
"""
import math
import struct
import sys
import zlib

SIZE = 1024
SUPERSAMPLE = 4

BACKGROUND = (10, 10, 10)
ACCENT = (95, 211, 155)

# Geometry in a 0..100 space, scaled to the output. The slab is an isometric
# capsule: a top face, and a front wall giving it thickness.
TOP_FACE = [(50, 22), (87, 42), (50, 62), (13, 42)]
FRONT_WALL = [(13, 42), (50, 62), (87, 42), (87, 51), (50, 71), (13, 51)]
INNER = [(50, 31), (69, 42), (50, 53), (31, 42)]
INNER_HALF_WIDTH = 19.0
INNER_HALF_HEIGHT = 11.0
ACCENT_STROKE = 2.6

GLOW_CENTRE = (50, 74)
GLOW_RADIUS = 40.0

# macOS draws app icons inside a rounded square that does not fill the
# canvas: 824 points of a 1024 grid, with a corner radius of 185. Painting
# to every edge instead produced a hard-cornered square sitting among the
# squircles in Finder, the microphone prompt and the Privacy lists.
ICON_INSET = (1024 - 824) / 2 / 1024 * 100
ICON_RADIUS = 185 / 1024 * 100


def inner_hole():
    """The inner edge of the accent outline, as its own polygon.

    Insetting each scanline horizontally, which is what this used to do, is
    not an inset outline: on an edge at angle t it leaves a perpendicular
    thickness of amount * sin(t), so the stroke rendered at about half its
    intended width, and any scanline narrower than twice the inset was
    dropped and painted solid, which blunted the diamond's top and bottom
    points. Scaling the whole diamond toward its centre is the actual
    offset curve for a shape like this.
    """
    half_width, half_height = INNER_HALF_WIDTH, INNER_HALF_HEIGHT
    # Perpendicular distance from the centre to an edge of the diamond.
    reach = half_width * half_height / math.hypot(half_width, half_height)
    scale = (reach - ACCENT_STROKE) / reach
    cx, cy = 50, 42
    return [
        (cx, cy - half_height * scale),
        (cx + half_width * scale, cy),
        (cx, cy + half_height * scale),
        (cx - half_width * scale, cy),
    ]


def rounded_rect_span(unit_y):
    """The x range of the icon's rounded square on one scanline, in units.

    Returned per scanline rather than tested per pixel, because the shape is
    convex and this keeps the mask free.
    """
    low, high = ICON_INSET, 100 - ICON_INSET
    if unit_y < low or unit_y > high:
        return None
    if unit_y < low + ICON_RADIUS:
        offset = low + ICON_RADIUS - unit_y
    elif unit_y > high - ICON_RADIUS:
        offset = unit_y - (high - ICON_RADIUS)
    else:
        return (low, high)
    inset = ICON_RADIUS - math.sqrt(max(ICON_RADIUS ** 2 - offset ** 2, 0.0))
    return (low + inset, high - inset)


def main(out_path):
    scale = SIZE * SUPERSAMPLE / 100.0
    big = SIZE * SUPERSAMPLE

    top = [(x * scale, y * scale) for x, y in TOP_FACE]
    wall = [(x * scale, y * scale) for x, y in FRONT_WALL]
    inner = [(x * scale, y * scale) for x, y in INNER]
    hole = [(x * scale, y * scale) for x, y in inner_hole()]
    glow_centre = (GLOW_CENTRE[0] * scale, GLOW_CENTRE[1] * scale)
    glow_radius = GLOW_RADIUS * scale
    top_edges = edges(top)
    wall_edges = edges(wall)
    inner_edges = edges(inner)
    hole_edges = edges(hole)

    rows = []
    for y in range(big):
        row = bytearray()
        cy = y + 0.5
        top_spans = spans(top_edges, cy)
        wall_spans = spans(wall_edges, cy)
        inner_spans = spans(inner_edges, cy)
        hole_spans = spans(hole_edges, cy)
        mask = rounded_rect_span(cy / scale)
        mask_span = None if mask is None else (mask[0] * scale, mask[1] * scale)
        for x in range(big):
            cx = x + 0.5
            opaque = mask_span is not None and mask_span[0] <= cx < mask_span[1]
            row += bytes(shade(cx, cy, top_spans, wall_spans, inner_spans,
                               hole_spans, glow_centre, glow_radius, scale))
            row.append(255 if opaque else 0)
        rows.append(bytes(row))

    write_png(out_path, downsample(rows, big, SUPERSAMPLE))


def shade(cx, cy, top_spans, wall_spans, inner_spans, hole_spans,
          glow_centre, glow_radius, scale):
    """The colour of one supersampled pixel, painted back to front."""
    colour = BACKGROUND

    # The glow the slab floats over. Squashed vertically so it reads as light
    # cast on a surface rather than as a halo around a ball.
    dx = (cx - glow_centre[0]) / glow_radius
    dy = (cy - glow_centre[1]) / (glow_radius * 0.34)
    distance = math.hypot(dx, dy)
    if distance < 1.0:
        falloff = (1.0 - distance) ** 2
        colour = mix(colour, ACCENT, falloff * 0.5)

    # The front wall, darkest, shading down so the slab has a bottom.
    if inside(wall_spans, cx):
        t = clamp((cy / (scale * 100.0) - 0.42) / 0.30)
        colour = lerp((32, 35, 41), (17, 19, 23), t)

    # The top face, lit from the upper left across to a cooler far corner.
    if inside(top_spans, cx):
        t = clamp(((cx + cy) / (scale * 100.0) - 0.55) / 0.45)
        colour = lerp((70, 75, 85), (38, 42, 49), t)

    # The accent outline, drawn as the region between the inner diamond and
    # its own smaller copy, because a scanline filler has no notion of
    # stroke width.
    if inside(inner_spans, cx) and not inside(hole_spans, cx):
        colour = ACCENT

    return colour


def edges(points):
    """Polygon edges as (x0, y0, x1, y1), skipping horizontal ones."""
    result = []
    for index, start in enumerate(points):
        end = points[(index + 1) % len(points)]
        if start[1] != end[1]:
            result.append((start[0], start[1], end[0], end[1]))
    return result


def spans(polygon_edges, cy):
    """Sorted x crossings of one scanline, paired into filled spans."""
    crossings = []
    for x0, y0, x1, y1 in polygon_edges:
        if (y0 <= cy < y1) or (y1 <= cy < y0):
            crossings.append(x0 + (cy - y0) / (y1 - y0) * (x1 - x0))
    crossings.sort()
    return [(crossings[i], crossings[i + 1]) for i in range(0, len(crossings) - 1, 2)]


def inside(pairs, cx):
    for a, b in pairs:
        if a <= cx < b:
            return True
    return False


def lerp(a, b, t):
    return tuple(int(round(a[i] + (b[i] - a[i]) * t)) for i in range(3))


def mix(base, over, weight):
    return tuple(int(round(base[i] + (over[i] - base[i]) * weight)) for i in range(3))


def clamp(value):
    return 0.0 if value < 0.0 else 1.0 if value > 1.0 else value


def downsample(rows, big, factor):
    """Box filter the supersampled RGBA image down to the output size.

    Rounded rather than truncated: integer division alone biases every
    output channel downwards by up to (area - 1) / area of a level.
    """
    out = []
    area = factor * factor
    half = area // 2
    for y in range(0, big, factor):
        block = rows[y:y + factor]
        row = bytearray()
        for x in range(0, big, factor):
            totals = [0, 0, 0, 0]
            for source in block:
                base = x * 4
                for step in range(factor):
                    offset = base + step * 4
                    totals[0] += source[offset]
                    totals[1] += source[offset + 1]
                    totals[2] += source[offset + 2]
                    totals[3] += source[offset + 3]
            row += bytes((total + half) // area for total in totals)
        out.append(bytes(row))
    return out


def write_png(path, rows):
    raw = b"".join(b"\x00" + row for row in rows)
    png = b"\x89PNG\r\n\x1a\n"
    # Colour type 6: RGBA. The alpha channel is what lets the icon be a
    # squircle rather than an opaque square.
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", len(rows[0]) // 4, len(rows), 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as handle:
        handle.write(png)


def chunk(tag, data):
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


if __name__ == "__main__":
    main(sys.argv[1])
