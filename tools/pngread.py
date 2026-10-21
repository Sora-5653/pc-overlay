"""Minimal PNG reader (standard library only) for the source diagrams.

Supports 8-bit grayscale, RGB, palette, grayscale+alpha, and RGBA images
without interlacing, which covers every image embedded in the source
document. Returns rows of (r, g, b, a) tuples.
"""

import struct
import zlib

CHANNELS = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}


def read_png(data):
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    pos = 8
    idat = []
    palette = None
    transparency = None
    width = height = depth = colour = interlace = None
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, depth, colour, _, _, interlace = struct.unpack(">IIBBBBB", body)
        elif kind == b"PLTE":
            palette = [tuple(body[i:i + 3]) for i in range(0, len(body), 3)]
        elif kind == b"tRNS":
            transparency = body
        elif kind == b"IDAT":
            idat.append(body)
        elif kind == b"IEND":
            break
    if depth != 8 or interlace:
        raise ValueError(f"unsupported PNG: depth={depth} interlace={interlace}")
    channels = CHANNELS[colour]
    stride = width * channels
    raw = zlib.decompress(b"".join(idat))
    rows = []
    previous = bytearray(stride)
    offset = 0
    for _ in range(height):
        kind = raw[offset]
        line = bytearray(raw[offset + 1:offset + 1 + stride])
        offset += 1 + stride
        for i in range(stride):
            left = line[i - channels] if i >= channels else 0
            up = previous[i]
            corner = previous[i - channels] if i >= channels else 0
            if kind == 1:
                line[i] = (line[i] + left) & 255
            elif kind == 2:
                line[i] = (line[i] + up) & 255
            elif kind == 3:
                line[i] = (line[i] + (left + up) // 2) & 255
            elif kind == 4:
                estimate = left + up - corner
                pa, pb, pc = abs(estimate - left), abs(estimate - up), abs(estimate - corner)
                predictor = left if pa <= pb and pa <= pc else up if pb <= pc else corner
                line[i] = (line[i] + predictor) & 255
        rows.append(_pixels(line, colour, channels, palette, transparency))
        previous = line
    return width, height, rows


def _pixels(line, colour, channels, palette, transparency):
    out = []
    for i in range(0, len(line), channels):
        if colour == 0:
            v = line[i]
            out.append((v, v, v, 255))
        elif colour == 2:
            out.append((line[i], line[i + 1], line[i + 2], 255))
        elif colour == 3:
            index = line[i]
            r, g, b = palette[index]
            a = transparency[index] if transparency and index < len(transparency) else 255
            out.append((r, g, b, a))
        elif colour == 4:
            v = line[i]
            out.append((v, v, v, line[i + 1]))
        else:
            out.append(tuple(line[i:i + 4]))
    return out
