#!/usr/bin/env python3
"""Convert the clawd pixel-art GIFs into firmware/assets/sprites.bin.

One-off tool: the output blob is committed, so building the firmware does NOT
require Pillow. Re-run only when the artwork changes.

    pip install Pillow
    python3 tools/gif2sprite.py

Output format is documented in docs/superpowers/plans/2026-07-31-pixel-screensaver.md
and parsed by firmware/src/sprite.rs.
"""
import pathlib
import struct

from PIL import Image, ImageSequence

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = ROOT / "assets" / "gif"
OUT = ROOT / "firmware" / "assets" / "sprites.bin"

# Order MUST match mascot::mood_index (firmware/src/mascot.rs).
MOODS = ["happy", "juggling", "building", "typing", "sleeping"]

SIZE = (128, 128)
FRAMES = 16
PALETTE_LEN = 16      # index 0 reserved for transparent
ALPHA_CUTOFF = 128


def load_frames(path):
    """Sample FRAMES evenly-spaced frames, downscaled with NEAREST."""
    im = Image.open(path)
    src = [f.convert("RGBA") for f in ImageSequence.Iterator(im)]
    step = len(src) / FRAMES
    picked = [src[int(i * step)] for i in range(FRAMES)]
    return [f.resize(SIZE, Image.NEAREST) for f in picked]


def build_shared_palette(all_frames):
    """One palette across every frame of every mood, so moods never swap it.

    Index 0 is reserved for transparent, so only PALETTE_LEN-1 real colours are
    quantized; the reserved slot is prepended afterwards.
    """
    w, h = SIZE
    sheet = Image.new("RGB", (w, h * len(all_frames)), (0, 0, 0))
    for i, fr in enumerate(all_frames):
        flat = Image.new("RGB", fr.size, (0, 0, 0))
        flat.paste(fr, (0, 0), fr)
        sheet.paste(flat, (0, i * h))
    return sheet.quantize(colors=PALETTE_LEN - 1, method=Image.MEDIANCUT)


def rgb565(r, g, b):
    return ((r & 0xF8) << 8) | ((g & 0xFC) << 3) | (b >> 3)


def encode(indices):
    """Run-length encode to (run_len u8, index u8) pairs; runs cap at 255."""
    out = bytearray()
    prev = indices[0]
    n = 0
    for v in indices:
        if v == prev and n < 255:
            n += 1
        else:
            out += bytes((n, prev))
            prev = v
            n = 1
    out += bytes((n, prev))
    return bytes(out)


def main():
    per_mood = {m: load_frames(SRC / f"clawd-{m}.gif") for m in MOODS}
    every = [fr for m in MOODS for fr in per_mood[m]]
    pal_img = build_shared_palette(every)

    raw = pal_img.getpalette()[: (PALETTE_LEN - 1) * 3]
    # Index 0 is transparent; the firmware substitutes the theme background.
    palette = [0] + [rgb565(raw[i * 3], raw[i * 3 + 1], raw[i * 3 + 2])
                     for i in range(PALETTE_LEN - 1)]

    table, payload = [], bytearray()
    for m in MOODS:
        for fr in per_mood[m]:
            flat = Image.new("RGB", fr.size, (0, 0, 0))
            flat.paste(fr, (0, 0), fr)
            q = flat.quantize(palette=pal_img, dither=Image.NONE)
            alpha = fr.getchannel("A").load()
            px = q.load()
            w, h = SIZE
            idx = []
            for y in range(h):
                for x in range(w):
                    # +1 shifts quantized colours past the reserved slot 0
                    idx.append(0 if alpha[x, y] < ALPHA_CUTOFF else px[x, y] + 1)
            enc = encode(idx)
            table.append((len(payload), len(enc)))
            payload += enc

    blob = bytearray()
    blob += b"CYDS"
    blob += struct.pack("<BBBBBB", 1, len(MOODS), FRAMES, SIZE[0], SIZE[1], PALETTE_LEN)
    for c in palette:
        blob += struct.pack("<H", c)
    for off, ln in table:
        blob += struct.pack("<II", off, ln)
    blob += payload

    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_bytes(blob)
    print(f"wrote {OUT} ({len(blob) / 1024:.1f} KB, {len(table)} frames)")


if __name__ == "__main__":
    main()
