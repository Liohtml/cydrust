"""Tests for the GIF -> sprite blob converter.

Run: python3 -m pytest tools/test_gif2sprite.py -v
(or: python3 tools/test_gif2sprite.py)
"""
import re
import struct
import subprocess
import sys
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
BLOB = ROOT / "firmware" / "assets" / "sprites.bin"

MAGIC = b"CYDS"
HEADER_SIZE = 10
PALETTE_SIZE = 32
TABLE_ENTRY = 8


def read_blob():
    return BLOB.read_bytes()


def test_header_is_well_formed():
    b = read_blob()
    assert b[0:4] == MAGIC
    version, moods, frames, w, h, pal_len = struct.unpack_from("<BBBBBB", b, 4)
    assert version == 1
    assert moods == 5
    assert frames == 16
    assert w == 128
    assert h == 128
    assert pal_len == 16


def test_frame_table_entries_are_in_bounds():
    b = read_blob()
    payload_start = HEADER_SIZE + PALETTE_SIZE + 5 * 16 * TABLE_ENTRY
    assert payload_start == 682
    for i in range(5 * 16):
        off, ln = struct.unpack_from("<II", b, HEADER_SIZE + PALETTE_SIZE + i * TABLE_ENTRY)
        assert ln > 0, f"frame {i} is empty"
        assert ln % 2 == 0, f"frame {i} length is not a whole number of RLE pairs"
        assert payload_start + off + ln <= len(b), f"frame {i} runs past end of blob"


def test_every_frame_decodes_to_exactly_one_sprite():
    b = read_blob()
    payload_start = 682
    for i in range(5 * 16):
        off, ln = struct.unpack_from("<II", b, HEADER_SIZE + PALETTE_SIZE + i * TABLE_ENTRY)
        total = 0
        for p in range(off, off + ln, 2):
            run = b[payload_start + p]
            idx = b[payload_start + p + 1]
            assert run >= 1, f"frame {i} has a zero-length run"
            assert idx < 16, f"frame {i} index {idx} out of palette range"
            total += run
        assert total == 128 * 128, f"frame {i} decodes to {total}, expected 16384"


def test_frame_table_is_contiguous():
    b = read_blob()
    payload_start = 682
    expected_off = 0
    for i in range(5 * 16):
        off, ln = struct.unpack_from("<II", b, HEADER_SIZE + PALETTE_SIZE + i * TABLE_ENTRY)
        assert off == expected_off, (
            f"frame {i} offset {off} != expected {expected_off} "
            "(gap or overlap in frame table)"
        )
        expected_off += ln
    payload_len = len(b) - payload_start
    assert expected_off == payload_len, (
        f"sum of frame lengths {expected_off} != payload length {payload_len}"
    )


def test_mood_order_matches_the_firmware_wire_contract():
    """MOODS order is a wire contract: the blob's frame table is mood-major and
    the firmware indexes it with mascot::mood_index. If these disagree the blob
    is still structurally valid, so every other test passes while the device
    plays the wrong animation. This is the only guard against that."""
    mascot = (ROOT / "firmware" / "src" / "mascot.rs").read_text()
    # e.g. "Mood::Happy => 0,"
    arms = re.findall(r"Mood::(\w+)\s*=>\s*(\d+)", mascot)
    assert arms, "could not parse mood_index arms from mascot.rs — update this test"
    firmware_order = [name.lower() for name, _ in sorted(arms, key=lambda a: int(a[1]))]

    src = (ROOT / "tools" / "gif2sprite.py").read_text()
    m = re.search(r"^MOODS\s*=\s*\[(.*?)\]", src, re.M | re.S)
    assert m, "could not parse MOODS from gif2sprite.py — update this test"
    converter_order = re.findall(r'"(\w+)"', m.group(1))

    assert converter_order == firmware_order, (
        f"MOODS order {converter_order} != mascot.rs mood_index order {firmware_order}"
    )


def test_converter_is_deterministic():
    before = read_blob()
    subprocess.run(
        [sys.executable, str(ROOT / "tools" / "gif2sprite.py")],
        check=True, cwd=ROOT, capture_output=True,
    )
    assert read_blob() == before, "re-running the converter changed the output"


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print(f"ok  {name}")
    print("all passed")
