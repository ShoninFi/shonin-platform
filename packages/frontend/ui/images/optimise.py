"""Re-encode a PNG smaller: drop an alpha channel that is opaque everywhere,
pick each row's filter by the usual minimum-sum heuristic, and compress at
zlib's best level. Chrome writes its screenshots fast, not small.

    python3 optimise.py [--palette] [--crop WxH] <file.png>...

With `--palette` an opaque image with more than 256 colours is reduced to
the 256 that best cover it (median cut, weighted by area), which keeps a
smooth gradient smooth at a third of the size. An image that already has 256
colours or fewer is written with a palette either way, losslessly. With
`--crop WxH` only the top-left W by H pixels are kept (headless Chrome won't
make a window as small as an icon, so icons are drawn in a larger one).

Standard library only. Reads 8-bit RGB or RGBA, non-interlaced: what Chrome
writes.
"""

import struct
import sys
import zlib

SIGNATURE = b"\x89PNG\r\n\x1a\n"


def chunks(data):
    at = len(SIGNATURE)
    while at < len(data):
        (length,) = struct.unpack(">I", data[at : at + 4])
        kind = data[at + 4 : at + 8]
        yield kind, data[at + 8 : at + 8 + length]
        at += 12 + length


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def unfilter(raw, width, height, bpp):
    stride = width * bpp
    rows, previous, at = [], bytearray(stride), 0
    for _ in range(height):
        kind, line = raw[at], bytearray(raw[at + 1 : at + 1 + stride])
        at += 1 + stride
        for i in range(stride):
            left = line[i - bpp] if i >= bpp else 0
            up = previous[i]
            if kind == 1:
                line[i] = (line[i] + left) & 255
            elif kind == 2:
                line[i] = (line[i] + up) & 255
            elif kind == 3:
                line[i] = (line[i] + ((left + up) >> 1)) & 255
            elif kind == 4:
                corner = previous[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + paeth(left, up, corner)) & 255
        rows.append(line)
        previous = line
    return rows


def refilter(rows, bpp):
    out, previous = bytearray(), bytearray(len(rows[0]))
    for line in rows:
        best = None
        for kind in range(5):
            filtered = bytearray(len(line))
            for i, value in enumerate(line):
                left = line[i - bpp] if i >= bpp else 0
                up = previous[i]
                if kind == 0:
                    predicted = 0
                elif kind == 1:
                    predicted = left
                elif kind == 2:
                    predicted = up
                elif kind == 3:
                    predicted = (left + up) >> 1
                else:
                    predicted = paeth(left, up, previous[i - bpp] if i >= bpp else 0)
                filtered[i] = (value - predicted) & 255
            cost = sum(v if v < 128 else 256 - v for v in filtered)
            if best is None or cost < best[0]:
                best = (cost, kind, filtered)
        out.append(best[1])
        out += best[2]
        previous = line
    return bytes(out)


def median_cut(counts, size):
    """`size` colours that cover `counts` ({rgb: pixels}): split the box with
    the most pixels times its widest side, at its weighted median."""
    boxes = [list(counts.items())]

    def score(box):
        if len(box) < 2:
            return -1
        spread = max(max(c[0][k] for c in box) - min(c[0][k] for c in box) for k in range(3))
        return spread * sum(n for _, n in box)

    while len(boxes) < size:
        best = max(range(len(boxes)), key=lambda i: score(boxes[i]))
        box = boxes[best]
        if score(box) <= 0:
            break
        axis = max(range(3), key=lambda k: max(c[0][k] for c in box) - min(c[0][k] for c in box))
        box.sort(key=lambda c: c[0][axis])
        half, seen, cut = sum(n for _, n in box) / 2, 0, 1
        for i, (_, n) in enumerate(box):
            seen += n
            if seen >= half:
                cut = max(1, min(i, len(box) - 1))
                break
        boxes[best:best + 1] = [box[:cut], box[cut:]]
    palette = []
    for box in boxes:
        total = sum(n for _, n in box)
        palette.append(tuple(round(sum(rgb[k] * n for rgb, n in box) / total) for k in range(3)))
    return palette


def to_palette(rows, reduce):
    """Index rows of RGB pixels into a palette, or None if that would lose
    colours and `reduce` is off."""
    counts = {}
    for line in rows:
        for i in range(0, len(line), 3):
            key = bytes(line[i:i + 3])
            counts[key] = counts.get(key, 0) + 1
    if len(counts) <= 256:
        palette = [tuple(key) for key in counts]
    elif reduce:
        palette = median_cut({tuple(k): n for k, n in counts.items()}, 256)
    else:
        return None
    index = {}
    for key in counts:
        rgb = tuple(key)
        index[key] = min(
            range(len(palette)),
            key=lambda i: sum((a - b) ** 2 for a, b in zip(rgb, palette[i])),
        )
    indexed = [bytearray(index[bytes(line[i:i + 3])] for i in range(0, len(line), 3)) for line in rows]
    return palette, indexed


def chunk(kind, body):
    return (
        struct.pack(">I", len(body))
        + kind
        + body
        + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)
    )


def optimise(path, reduce, crop):
    data = open(path, "rb").read()
    if not data.startswith(SIGNATURE):
        raise SystemExit(f"{path}: not a PNG")
    header, idat = None, b""
    for kind, body in chunks(data):
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
    width, height, depth, colour, _, _, interlace = header
    if depth != 8 or colour not in (2, 6) or interlace != 0:
        raise SystemExit(f"{path}: only 8-bit, non-interlaced RGB or RGBA")
    bpp = 4 if colour == 6 else 3
    rows = unfilter(zlib.decompress(idat), width, height, bpp)
    if crop:
        if crop[0] > width or crop[1] > height:
            raise SystemExit(f"{path}: {width}x{height} is smaller than the crop")
        width, height = crop
        rows = [line[: width * bpp] for line in rows[:height]]
    if bpp == 4 and all(line[3::4] == b"\xff" * width for line in rows):
        rows = [bytearray(b for i, b in enumerate(line) if i % 4 != 3) for line in rows]
        bpp, colour = 3, 2
    extra = b""
    indexed = to_palette(rows, reduce) if bpp == 3 else None
    if indexed:
        palette, rows = indexed
        bpp, colour = 1, 3
        extra = chunk(b"PLTE", bytes(v for rgb in palette for v in rgb))
    body = zlib.compress(refilter(rows, bpp if bpp > 1 else 1), 9)
    out = (
        SIGNATURE
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, colour, 0, 0, 0))
        + extra
        + chunk(b"IDAT", body)
        + chunk(b"IEND", b"")
    )
    open(path, "wb").write(out)
    print(f"{path}: {len(data)} -> {len(out)} bytes")


if __name__ == "__main__":
    args, names, reduce, crop = sys.argv[1:], [], False, None
    while args:
        arg = args.pop(0)
        if arg == "--palette":
            reduce = True
        elif arg == "--crop":
            crop = tuple(int(n) for n in args.pop(0).split("x"))
        else:
            names.append(arg)
    for name in names:
        optimise(name, reduce, crop)
