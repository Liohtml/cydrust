# Pixel-Art Screensaver Tab Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a fifth tab to the CYD firmware showing an animated pixel-art mascot driven by agent activity, which also auto-engages as a screensaver after a configurable idle period.

**Architecture:** Three new units. `mascot.rs` (std-only) decides *which* animation to show from the already-parsed `DisplayState`. `sprite.rs` (embedded-graphics only) decodes a committed, RLE-compressed asset blob. `main.rs` wires a `Tab::Pixel` into the existing tab bar, settings, and both transport loops. The first two are deliberately dependency-light so they compile into the bridge's test binary and run on plain stable — no ESP32 toolchain needed to test the logic.

**Tech Stack:** Rust (esp-idf-svc 0.50 / esp-idf-hal 0.45), embedded-graphics 0.8, mipidsi 0.8; Python 3 + Pillow for the one-off asset converter.

## Global Constraints

- **No esp/Xtensa toolchain is available locally.** `mascot.rs`, `sprite.rs` and any pure helper are tested with `cargo test` from `bridge/`. Changes to `firmware/src/main.rs` are verified only by CI firmware builds (~4½ min each). Put logic in the host-testable modules wherever there is a choice.
- **MSRV is 1.86** (`rust-version` in Cargo.toml). Do not use newer language features.
- **Clippy is `-D warnings`** across `--all-targets --all-features`. Code must be warning-clean.
- **`cargo fmt --check`** runs in CI. Format before committing.
- **The e-ink build must not gain this tab.** Everything new is gated `#[cfg(not(feature = "eink"))]`.
- **The default (USB) build is the corporate default.** It gains ~81 KB and must still fit the factory partition — Task 8 enforces this.
- Sprite geometry, fixed: **128×128 at `x=96, y=69`**, **16 frames**, **8 fps** (125 ms/frame).
- Palette is **16 entries; index 0 is reserved as transparent** and renders as the active theme background.
- Artwork is adapted from **KebeliSamet0/clawd (MIT)**; attribution must be preserved.
- Blob magic `"CYDS"`, version `1`, little-endian throughout.
- Conventional Commits are enforced on PR titles by `.github/workflows/pr-title.yml`.

---

### Task 1: Mood decision logic (`mascot.rs`)

Pure activity→animation mapping. No assets, no display, no esp-idf. Fully host-testable.

**Files:**
- Create: `firmware/src/mascot.rs`
- Create: `bridge/tests/firmware_mascot_test.rs`
- Modify: `firmware/src/main.rs` (add `mod mascot;` near the existing `mod proto;` at line 49)

**Interfaces:**
- Consumes: `proto::{DisplayState, SessionRow, SessionStatus}` (already exist).
- Produces:
  - `pub enum Mood { Happy, Juggling, Building, Typing, Sleeping }` — `Debug, Clone, Copy, PartialEq, Eq`
  - `pub const ART_BUILDING_SEC: i32 = 120;`
  - `pub fn mood_for(ds: &DisplayState) -> Mood`
  - `pub fn mood_index(m: Mood) -> usize` — blob frame-table order: Happy=0, Juggling=1, Building=2, Typing=3, Sleeping=4

- [ ] **Step 1: Write the failing test**

Create `bridge/tests/firmware_mascot_test.rs`:

```rust
//! Host tests for the firmware's activity -> animation mapping.
//!
//! `firmware/src/mascot.rs` is std-only (no esp-idf / embedded-graphics deps)
//! so it compiles straight into the bridge's test binary via `#[path]`, the
//! same trick `firmware_proto_test.rs` uses for `proto.rs`.

#[path = "../../firmware/src/proto.rs"]
mod proto;
#[path = "../../firmware/src/mascot.rs"]
mod mascot;

use mascot::{mood_for, mood_index, Mood, ART_BUILDING_SEC};
use proto::{DisplayState, SessionRow, SessionStatus};

fn row(status: SessionStatus, age_sec: i32) -> SessionRow {
    SessionRow {
        project: "proj".into(),
        status,
        tool: "claude".into(),
        id: "abc".into(),
        age_sec,
        wait_sec: -1,
        summary: String::new(),
    }
}

fn state(rows: Vec<SessionRow>) -> DisplayState {
    DisplayState { sessions: rows, ..Default::default() }
}

#[test]
fn no_sessions_is_sleeping() {
    assert_eq!(mood_for(&state(vec![])), Mood::Sleeping);
}

#[test]
fn offline_is_sleeping_even_with_working_sessions() {
    let mut ds = state(vec![row(SessionStatus::Working, 5)]);
    ds.offline = true;
    assert_eq!(mood_for(&ds), Mood::Sleeping);
}

#[test]
fn all_idle_is_sleeping() {
    let ds = state(vec![row(SessionStatus::Idle, 900), row(SessionStatus::Idle, 30)]);
    assert_eq!(mood_for(&ds), Mood::Sleeping);
}

#[test]
fn single_recent_working_is_typing() {
    assert_eq!(mood_for(&state(vec![row(SessionStatus::Working, 119)])), Mood::Typing);
}

#[test]
fn building_boundary_is_strictly_greater_than_threshold() {
    // exactly at the threshold is still Typing; one second past it is Building
    assert_eq!(mood_for(&state(vec![row(SessionStatus::Working, 120)])), Mood::Typing);
    assert_eq!(mood_for(&state(vec![row(SessionStatus::Working, 121)])), Mood::Building);
}

#[test]
fn unknown_age_falls_through_to_typing() {
    assert_eq!(mood_for(&state(vec![row(SessionStatus::Working, -1)])), Mood::Typing);
}

#[test]
fn two_working_is_juggling() {
    let ds = state(vec![row(SessionStatus::Working, 5), row(SessionStatus::Working, 900)]);
    assert_eq!(mood_for(&ds), Mood::Juggling);
}

#[test]
fn waiting_outranks_every_other_rule() {
    let ds = state(vec![
        row(SessionStatus::Working, 5),
        row(SessionStatus::Working, 900),
        row(SessionStatus::Working, 900),
        row(SessionStatus::Waiting, 10),
    ]);
    assert_eq!(mood_for(&ds), Mood::Happy);
}

#[test]
fn idle_sessions_do_not_count_toward_juggling() {
    let ds = state(vec![
        row(SessionStatus::Working, 5),
        row(SessionStatus::Idle, 5),
        row(SessionStatus::Idle, 5),
    ]);
    assert_eq!(mood_for(&ds), Mood::Typing);
}

/// Guards the reasoning in the design: the bridge marks a session Working when
/// `active_turn || age < WORKING_SEC` with WORKING_SEC = 60 (bridge/src/hub.rs:23).
/// Keeping the Building threshold above that means rule 3 cannot be reached by
/// mtime jitter alone — only by a genuinely stalled `active_turn`.
#[test]
fn building_threshold_stays_above_bridge_working_sec() {
    const BRIDGE_WORKING_SEC: i32 = 60;
    assert!(ART_BUILDING_SEC > BRIDGE_WORKING_SEC);
}

#[test]
fn mood_index_is_stable_and_unique() {
    let all = [Mood::Happy, Mood::Juggling, Mood::Building, Mood::Typing, Mood::Sleeping];
    let idx: Vec<usize> = all.iter().map(|m| mood_index(*m)).collect();
    assert_eq!(idx, vec![0, 1, 2, 3, 4]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd bridge && cargo test --test firmware_mascot_test`
Expected: FAIL — `couldn't read ../../firmware/src/mascot.rs` (file does not exist yet).

- [ ] **Step 3: Write minimal implementation**

Create `firmware/src/mascot.rs`:

```rust
//! Activity -> animation mapping for the pixel-art screensaver tab.
//!
//! Deliberately std-only (no esp-idf, no embedded-graphics) so it compiles into
//! the bridge's test binary via `#[path]` and runs on plain stable — the same
//! arrangement as `proto.rs`. See bridge/tests/firmware_mascot_test.rs.

use crate::proto::{DisplayState, SessionStatus};

/// Which animation the mascot is playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    Happy,
    Juggling,
    Building,
    Typing,
    Sleeping,
}

/// A single Working session quiet for longer than this reads as a long build or
/// tool call rather than active generation. Must stay above the bridge's
/// WORKING_SEC (60) — see the test that guards this.
pub const ART_BUILDING_SEC: i32 = 120;

/// Frame-table order in `sprites.bin`. Must match `tools/gif2sprite.py`.
pub fn mood_index(m: Mood) -> usize {
    match m {
        Mood::Happy => 0,
        Mood::Juggling => 1,
        Mood::Building => 2,
        Mood::Typing => 3,
        Mood::Sleeping => 4,
    }
}

/// Priority ladder — first match wins. See the design doc for the rationale.
pub fn mood_for(ds: &DisplayState) -> Mood {
    if ds.offline {
        return Mood::Sleeping;
    }
    if ds.sessions.iter().any(|s| s.status == SessionStatus::Waiting) {
        return Mood::Happy;
    }
    let working: Vec<&crate::proto::SessionRow> = ds
        .sessions
        .iter()
        .filter(|s| s.status == SessionStatus::Working)
        .collect();
    match working.len() {
        0 => Mood::Sleeping,
        1 if working[0].age_sec > ART_BUILDING_SEC => Mood::Building,
        1 => Mood::Typing,
        _ => Mood::Juggling,
    }
}
```

Then add to `firmware/src/main.rs`, immediately after `mod proto;` (line 49):

```rust
// Activity -> animation mapping for the pixel tab. Std-only, host-tested (see
// bridge/tests/firmware_mascot_test.rs).
#[cfg(not(feature = "eink"))]
mod mascot;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd bridge && cargo test --test firmware_mascot_test`
Expected: PASS — 11 tests.

Note: the test file uses `crate::proto` from inside `mascot.rs`. Because both are declared as sibling `mod`s in the test binary, `crate::proto` resolves correctly there **and** in `main.rs`. If the compiler objects, change the `use` in `mascot.rs` to `use super::proto::{...}` — verify with the same command.

- [ ] **Step 5: Commit**

```bash
git add firmware/src/mascot.rs bridge/tests/firmware_mascot_test.rs firmware/src/main.rs
git commit -m "feat(firmware): add activity to animation mood mapping"
```

---

### Task 2: Asset converter and blob (`gif2sprite.py`)

Produces the committed `sprites.bin`. Run once; output is checked in.

**Files:**
- Create: `tools/gif2sprite.py`
- Create: `firmware/assets/sprites.bin` (generated)
- Create: `assets/gif/clawd-{typing,building,juggling,happy,sleeping}.gif` (sources)
- Create: `assets/gif/NOTICE`
- Create: `tools/test_gif2sprite.py`

**Interfaces:**
- Produces: `firmware/assets/sprites.bin` in the format below. Task 3 parses it; the mood order must match `mascot::mood_index`.

Blob format (little-endian):

```
offset  size  field
0       4     magic "CYDS"
4       1     version = 1
5       1     mood_count = 5
6       1     frames_per_mood = 16
7       1     sprite_w = 128
8       1     sprite_h = 128
9       1     palette_len = 16
10      32    palette: 16 x u16 RGB565   (index 0 reserved/transparent)
42      640   frame table: 80 x { offset: u32, len: u32 }, mood-major
682     ...   RLE payload: (run_len: u8, index: u8) pairs
```

Frame `f` of mood `m` is table entry `m * 16 + f`. `offset` is relative to the start of the payload (byte 682).

- [ ] **Step 1: Fetch the source GIFs and record attribution**

```bash
mkdir -p assets/gif firmware/assets tools
for n in typing building juggling happy sleeping; do
  curl -sSL -o "assets/gif/clawd-$n.gif" \
    "https://raw.githubusercontent.com/KebeliSamet0/clawd/master/assets/gif/clawd-$n.gif"
done
ls -la assets/gif/
```

Expected: five files, roughly 39–98 KB each.

Create `assets/gif/NOTICE`:

```
The clawd-*.gif pixel-art animations in this directory are taken from:

    https://github.com/KebeliSamet0/clawd

Copyright (c) KebeliSamet0, licensed under the MIT License.

They are redistributed here unmodified so this repository is self-contained.
firmware/assets/sprites.bin is a derived work: downscaled to 128x128, reduced
to 16 frames, and quantized to a shared 16-colour palette by tools/gif2sprite.py.
```

- [ ] **Step 2: Write the failing test**

Create `tools/test_gif2sprite.py`:

```python
"""Tests for the GIF -> sprite blob converter.

Run: python3 -m pytest tools/test_gif2sprite.py -v
(or: python3 tools/test_gif2sprite.py)
"""
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
```

- [ ] **Step 3: Run test to verify it fails**

Run: `python3 tools/test_gif2sprite.py`
Expected: FAIL — `FileNotFoundError: firmware/assets/sprites.bin`.

- [ ] **Step 4: Write the converter**

Create `tools/gif2sprite.py`:

```python
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
```

- [ ] **Step 5: Generate the blob and run the tests**

```bash
python3 tools/gif2sprite.py
python3 tools/test_gif2sprite.py
ls -la firmware/assets/sprites.bin
```

Expected: converter prints roughly `82.0 KB, 80 frames`; all tests print `ok`.

If the blob exceeds **110 KB**, stop and reduce `SIZE` to `(96, 96)` (design doc records this fallback), regenerate, and note the change in the commit message.

- [ ] **Step 6: Commit**

```bash
git add tools/gif2sprite.py tools/test_gif2sprite.py firmware/assets/sprites.bin assets/gif/
git commit -m "feat(firmware): add pixel-art sprite blob and converter"
```

---

### Task 3: Sprite decoder (`sprite.rs`)

**Files:**
- Create: `firmware/src/sprite.rs`
- Create: `bridge/tests/firmware_sprite_test.rs`
- Modify: `bridge/Cargo.toml` (`[dev-dependencies]`)
- Modify: `firmware/src/main.rs` (add `mod sprite;` after `mod mascot;`)

**Interfaces:**
- Consumes: `firmware/assets/sprites.bin` (Task 2), `mascot::mood_index` (Task 1).
- Produces:
  - `pub const SPRITE_W: u32 = 128;` / `pub const SPRITE_H: u32 = 128;` / `pub const FRAMES: usize = 16;`
  - `pub struct Sprites<'a>` with `pub fn parse(blob: &'a [u8]) -> Option<Sprites<'a>>`
  - `pub fn frame(&self, mood_idx: usize, frame_idx: usize, bg: Rgb565) -> Option<FramePixels<'a>>`
  - `pub struct FramePixels<'a>` implementing `Iterator<Item = Rgb565>`, yielding exactly `SPRITE_W * SPRITE_H` items
  - `pub const BLOB: &[u8] = include_bytes!("../assets/sprites.bin");`

`parse` returns `None` on bad magic, wrong version, short buffer, or any frame-table entry out of bounds. `frame` returns `None` for out-of-range indices. `FramePixels` substitutes `bg` for palette index 0, clamps total output to exactly `SPRITE_W * SPRITE_H` (padding short frames with `bg`, truncating long ones), so a corrupt blob can never overrun the target rectangle.

- [ ] **Step 1: Add the test-only dependency**

`bridge/Cargo.toml` does **not** currently depend on embedded-graphics, so the
test below would not compile without this. Add to `[dev-dependencies]`:

```toml
# Host-side decoding of the firmware sprite blob (bridge/tests/firmware_sprite_test.rs).
# Must match the firmware's embedded-graphics major version.
embedded-graphics = "0.8"
```

Verify it resolves: `cd bridge && cargo fetch`

- [ ] **Step 2: Write the failing test**

Create `bridge/tests/firmware_sprite_test.rs`:

```rust
//! Host tests for the pixel-art sprite blob decoder.
//!
//! `firmware/src/sprite.rs` depends only on embedded-graphics (which builds on
//! host), so it compiles into the bridge test binary via `#[path]`.

#[path = "../../firmware/src/sprite.rs"]
mod sprite;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::RgbColor;
use sprite::{Sprites, BLOB, FRAMES, SPRITE_H, SPRITE_W};

const BG: Rgb565 = Rgb565::BLACK;

#[test]
fn parses_the_committed_blob() {
    assert!(Sprites::parse(BLOB).is_some(), "committed sprites.bin failed to parse");
}

#[test]
fn every_frame_of_every_mood_decodes_to_a_full_sprite() {
    let s = Sprites::parse(BLOB).expect("parse");
    let expected = (SPRITE_W * SPRITE_H) as usize;
    for mood in 0..5 {
        for f in 0..FRAMES {
            let px = s.frame(mood, f, BG).expect("frame present");
            assert_eq!(px.count(), expected, "mood {mood} frame {f} wrong pixel count");
        }
    }
}

#[test]
fn rejects_bad_magic() {
    let mut bad = BLOB.to_vec();
    bad[0] = b'X';
    assert!(Sprites::parse(&bad).is_none());
}

#[test]
fn rejects_wrong_version() {
    let mut bad = BLOB.to_vec();
    bad[4] = 99;
    assert!(Sprites::parse(&bad).is_none());
}

#[test]
fn rejects_a_truncated_blob() {
    assert!(Sprites::parse(&BLOB[..600]).is_none());
    assert!(Sprites::parse(&[]).is_none());
}

#[test]
fn rejects_a_frame_table_entry_past_the_end() {
    let mut bad = BLOB.to_vec();
    // First frame-table entry sits at offset 42; blow up its length field.
    bad[46..50].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Sprites::parse(&bad).is_none());
}

#[test]
fn out_of_range_indices_return_none() {
    let s = Sprites::parse(BLOB).expect("parse");
    assert!(s.frame(5, 0, BG).is_none(), "mood index 5 does not exist");
    assert!(s.frame(0, FRAMES, BG).is_none(), "frame index past the end");
}

#[test]
fn transparent_index_renders_as_the_supplied_background() {
    let s = Sprites::parse(BLOB).expect("parse");
    // Corner pixels of pixel art are background; assert at least one pixel in
    // frame 0 of every mood comes back as the substituted colour.
    for mood in 0..5 {
        let any_bg = s.frame(mood, 0, Rgb565::GREEN).unwrap().any(|c| c == Rgb565::GREEN);
        assert!(any_bg, "mood {mood} frame 0 had no transparent pixels");
    }
}

#[test]
fn decoding_is_stable_across_calls() {
    let s = Sprites::parse(BLOB).expect("parse");
    let a: Vec<Rgb565> = s.frame(0, 0, BG).unwrap().collect();
    let b: Vec<Rgb565> = s.frame(0, 0, BG).unwrap().collect();
    assert_eq!(a, b);
}

// NOTE: the golden-checksum test is added in Step 5, once the real values are
// known. Do NOT commit a placeholder version of it — see Step 5.
```

Note: `RawU16::from(c).into_inner()` needs `use embedded_graphics::prelude::RawData;`
in scope for `into_inner`. Add it to the test's imports if the compiler asks.

- [ ] **Step 3: Run test to verify it fails**

Run: `cd bridge && cargo test --test firmware_sprite_test`
Expected: FAIL — `couldn't read ../../firmware/src/sprite.rs`.

- [ ] **Step 4: Write the implementation**

Create `firmware/src/sprite.rs`:

```rust
//! Decoder for the RLE pixel-art sprite blob (firmware/assets/sprites.bin).
//!
//! Depends only on embedded-graphics so it can be host-tested — see
//! bridge/tests/firmware_sprite_test.rs. The blob is produced by
//! tools/gif2sprite.py; format documented there and in the plan.
//!
//! Defensive by construction: a malformed blob makes `parse` return None (the
//! caller then omits the tab) and a malformed frame can never emit more than
//! SPRITE_W * SPRITE_H pixels, so it cannot overrun the target rectangle.

use embedded_graphics::pixelcolor::{raw::RawU16, Rgb565};

pub const BLOB: &[u8] = include_bytes!("../assets/sprites.bin");

pub const SPRITE_W: u32 = 128;
pub const SPRITE_H: u32 = 128;
pub const FRAMES: usize = 16;

const MAGIC: &[u8; 4] = b"CYDS";
const VERSION: u8 = 1;
const MOODS: usize = 5;
const PALETTE_LEN: usize = 16;
const HEADER: usize = 10;
const TABLE_AT: usize = HEADER + PALETTE_LEN * 2; // 42
const TABLE_BYTES: usize = MOODS * FRAMES * 8;    // 640
const PAYLOAD_AT: usize = TABLE_AT + TABLE_BYTES; // 682

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

pub struct Sprites<'a> {
    payload: &'a [u8],
    table: [(u32, u32); MOODS * FRAMES],
    palette: [Rgb565; PALETTE_LEN],
}

impl<'a> Sprites<'a> {
    pub fn parse(blob: &'a [u8]) -> Option<Sprites<'a>> {
        if blob.len() < PAYLOAD_AT || &blob[0..4] != MAGIC {
            return None;
        }
        if blob[4] != VERSION
            || blob[5] as usize != MOODS
            || blob[6] as usize != FRAMES
            || blob[7] as u32 != SPRITE_W
            || blob[8] as u32 != SPRITE_H
            || blob[9] as usize != PALETTE_LEN
        {
            return None;
        }

        let mut palette = [Rgb565::from(RawU16::new(0)); PALETTE_LEN];
        for (i, slot) in palette.iter_mut().enumerate() {
            *slot = Rgb565::from(RawU16::new(u16le(blob, HEADER + i * 2)));
        }

        let payload = &blob[PAYLOAD_AT..];
        let mut table = [(0u32, 0u32); MOODS * FRAMES];
        for (i, slot) in table.iter_mut().enumerate() {
            let off = u32le(blob, TABLE_AT + i * 8);
            let len = u32le(blob, TABLE_AT + i * 8 + 4);
            let end = (off as usize).checked_add(len as usize)?;
            if end > payload.len() || len == 0 || len % 2 != 0 {
                return None;
            }
            *slot = (off, len);
        }
        Some(Sprites { payload, table, palette })
    }

    pub fn frame(&self, mood_idx: usize, frame_idx: usize, bg: Rgb565) -> Option<FramePixels<'a>> {
        if mood_idx >= MOODS || frame_idx >= FRAMES {
            return None;
        }
        let (off, len) = self.table[mood_idx * FRAMES + frame_idx];
        let mut palette = self.palette;
        palette[0] = bg; // index 0 is transparent -> active theme background
        Some(FramePixels {
            data: &self.payload[off as usize..(off + len) as usize],
            at: 0,
            run: 0,
            colour: bg,
            emitted: 0,
            bg,
            palette,
        })
    }
}

pub struct FramePixels<'a> {
    data: &'a [u8],
    at: usize,
    run: u8,
    colour: Rgb565,
    emitted: u32,
    bg: Rgb565,
    palette: [Rgb565; PALETTE_LEN],
}

impl Iterator for FramePixels<'_> {
    type Item = Rgb565;

    fn next(&mut self) -> Option<Rgb565> {
        // Hard cap: never emit more than one sprite's worth, whatever the data says.
        if self.emitted >= SPRITE_W * SPRITE_H {
            return None;
        }
        if self.run == 0 {
            if self.at + 1 < self.data.len() {
                self.run = self.data[self.at];
                let idx = self.data[self.at + 1] as usize;
                self.colour = *self.palette.get(idx).unwrap_or(&self.bg);
                self.at += 2;
                if self.run == 0 {
                    // Defensive: a zero run would loop forever; pad instead.
                    self.emitted += 1;
                    return Some(self.bg);
                }
            } else {
                // Short frame: pad out to a full sprite with background.
                self.emitted += 1;
                return Some(self.bg);
            }
        }
        self.run -= 1;
        self.emitted += 1;
        Some(self.colour)
    }
}
```

Then add to `firmware/src/main.rs`, immediately after the `mod mascot;` block from Task 1:

```rust
// RLE decoder for the pixel-art blob. embedded-graphics only, host-tested (see
// bridge/tests/firmware_sprite_test.rs).
#[cfg(not(feature = "eink"))]
mod sprite;
```

- [ ] **Step 5: Run tests, then add the golden test with real values**

Run: `cd bridge && cargo test --test firmware_sprite_test`
Expected: PASS — 9 tests.

Now add the golden test (design doc section 8). First compute the values by
temporarily adding this test and running it:

```rust
#[test]
fn print_goldens() {
    let s = Sprites::parse(BLOB).expect("parse");
    let mut out = [0u64; 5];
    for (mood, slot) in out.iter_mut().enumerate() {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;      // FNV-1a, dependency-free
        for c in s.frame(mood, 0, BG).unwrap() {
            let raw: u16 = embedded_graphics::pixelcolor::raw::RawU16::from(c).into_inner();
            for byte in raw.to_le_bytes() {
                h ^= byte as u64;
                h = h.wrapping_mul(0x100_0000_01b3);
            }
        }
        *slot = h;
    }
    println!("{out:?}");
    panic!("scratch");
}
```

Run `cargo test --test firmware_sprite_test print_goldens -- --nocapture`, note
the five values, then **delete `print_goldens`** and replace it with the real
test, substituting the values you captured for `<v0>`..`<v4>`:

```rust
/// Golden test (design doc section 8): pins the decoded artwork so a
/// regenerated blob cannot silently change what the device draws.
///
/// After an INTENTIONAL artwork change these values will fail. Recompute them
/// with the same FNV-1a fold, update them here, and say so in the commit message.
#[test]
fn frame_zero_of_each_mood_matches_its_golden_checksum() {
    const GOLDEN: [u64; 5] = [<v0>, <v1>, <v2>, <v3>, <v4>];

    let s = Sprites::parse(BLOB).expect("parse");
    let mut actual = [0u64; 5];
    for (mood, slot) in actual.iter_mut().enumerate() {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for c in s.frame(mood, 0, BG).unwrap() {
            let raw: u16 = embedded_graphics::pixelcolor::raw::RawU16::from(c).into_inner();
            for byte in raw.to_le_bytes() {
                h ^= byte as u64;
                h = h.wrapping_mul(0x100_0000_01b3);
            }
        }
        *slot = h;
    }
    assert_eq!(actual, GOLDEN, "decoded artwork changed");
}
```

Re-run: `cd bridge && cargo test --test firmware_sprite_test`
Expected: PASS — 10 tests. `print_goldens` must NOT appear in the commit.

Then confirm Task 1 still passes: `cd bridge && cargo test --test firmware_mascot_test`

- [ ] **Step 6: Commit**

```bash
git add firmware/src/sprite.rs bridge/tests/firmware_sprite_test.rs \
        bridge/Cargo.toml Cargo.lock firmware/src/main.rs
git commit -m "feat(firmware): add RLE sprite blob decoder"
```

---

### Task 4: Idle-art setting and validity rules

The `art_sec` setting plus the pure snap/validity logic, which lives in `mascot.rs` so it can be host-tested rather than stranded in `main.rs`.

**Files:**
- Modify: `firmware/src/mascot.rs` (append)
- Modify: `bridge/tests/firmware_mascot_test.rs` (append)
- Modify: `firmware/src/main.rs:85-99` (Settings struct, defaults), `:697-709` (NVS load/save)

**Interfaces:**
- Produces:
  - `pub const ART_VALS: [u16; 4] = [0, 30, 60, 300];` (seconds; 0 = Off)
  - `pub const ART_LBL: [&str; 4] = ["Off", "30s", "1m", "5m"];`
  - `pub fn art_is_valid(art_sec: u16, sleep_min: u16) -> bool`
  - `pub fn snap_art(art_sec: u16, sleep_min: u16) -> u16`
- Consumed by Task 6 (settings UI) and Task 7 (loops).

- [ ] **Step 1: Write the failing test**

Append to `bridge/tests/firmware_mascot_test.rs`:

```rust
use mascot::{art_is_valid, snap_art, ART_LBL, ART_VALS};

#[test]
fn art_tables_line_up() {
    assert_eq!(ART_VALS.len(), ART_LBL.len());
    assert_eq!(ART_VALS[0], 0, "index 0 must be the Off value");
}

#[test]
fn every_art_value_is_valid_when_sleep_is_never() {
    for v in ART_VALS {
        assert!(art_is_valid(v, 0), "{v} should be valid with sleep=Never");
    }
}

#[test]
fn off_is_always_valid() {
    for sleep in [0u16, 1, 5, 15, 30] {
        assert!(art_is_valid(0, sleep));
    }
}

#[test]
fn art_must_be_strictly_shorter_than_the_sleep_timeout() {
    // sleep = 1m (60s): 30s fits, 60s does not (it would never fire), 5m does not
    assert!(art_is_valid(30, 1));
    assert!(!art_is_valid(60, 1));
    assert!(!art_is_valid(300, 1));

    // sleep = 5m (300s): 30s and 60s fit, 300s does not
    assert!(art_is_valid(30, 5));
    assert!(art_is_valid(60, 5));
    assert!(!art_is_valid(300, 5));

    // sleep = 15m (900s): everything fits
    assert!(art_is_valid(300, 15));
}

#[test]
fn snap_keeps_a_valid_value_untouched() {
    assert_eq!(snap_art(60, 0), 60);
    assert_eq!(snap_art(30, 1), 30);
    assert_eq!(snap_art(300, 30), 300);
}

#[test]
fn snap_drops_to_the_largest_valid_value() {
    // sleep 1m invalidates 60 and 300 -> largest valid non-Off is 30
    assert_eq!(snap_art(300, 1), 30);
    assert_eq!(snap_art(60, 1), 30);
    // sleep 5m invalidates only 300 -> falls back to 60
    assert_eq!(snap_art(300, 5), 60);
}

#[test]
fn snap_falls_back_to_off_when_nothing_fits() {
    // A hypothetical very short sleep leaves no room for any art delay.
    assert_eq!(snap_art(300, 0 /* never */), 300, "sleep=Never keeps the value");
    // Construct the no-room case directly: sleep_min so small every art value loses.
    // With SLEEP_VALS the smallest non-never sleep is 1m, and 30s fits, so the
    // only way to reach Off is an unknown/garbage art value below 30s.
    assert_eq!(snap_art(29, 1), 0, "a value smaller than every valid option snaps Off");
}

#[test]
fn snap_normalises_values_that_are_not_in_the_table() {
    assert_eq!(snap_art(45, 0), 30, "45s is not offered; snap down to 30s");
    assert_eq!(snap_art(9999, 0), 300, "clamp to the largest offered value");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd bridge && cargo test --test firmware_mascot_test`
Expected: FAIL — `cannot find function art_is_valid` / `snap_art` / unresolved `ART_VALS`.

- [ ] **Step 3: Write the implementation**

Append to `firmware/src/mascot.rs`:

```rust
/// Idle delay before the art auto-engages, in seconds. 0 = Off.
pub const ART_VALS: [u16; 4] = [0, 30, 60, 300];
pub const ART_LBL: [&str; 4] = ["Off", "30s", "1m", "5m"];

/// An art delay is only meaningful if the screen has not already blanked.
/// `sleep_min == 0` means Never, so everything is valid then.
pub fn art_is_valid(art_sec: u16, sleep_min: u16) -> bool {
    if art_sec == 0 || sleep_min == 0 {
        return true;
    }
    (art_sec as u32) < (sleep_min as u32) * 60
}

/// Normalise `art_sec` to the largest offered value that is <= it and valid for
/// the current `sleep_min`, or Off when none qualifies.
pub fn snap_art(art_sec: u16, sleep_min: u16) -> u16 {
    ART_VALS
        .iter()
        .copied()
        .filter(|&v| v <= art_sec && art_is_valid(v, sleep_min))
        .max()
        .unwrap_or(0)
}
```

Modify `firmware/src/main.rs`. Replace the `Settings` struct and its `Default` impl (lines 84-92) with:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
struct Settings {
    brightness: u8,    // 10..=100 (%)
    sleep_min:  u16,   // 0=never, else minutes until screen-off
    dark:       bool,  // theme: true=dark, false=light
    #[cfg(not(feature = "eink"))]
    art_sec:    u16,   // 0=off, else seconds idle before the pixel art engages
}
impl Default for Settings {
    fn default() -> Self {
        Settings {
            brightness: 100,
            sleep_min: 0,
            dark: true,
            #[cfg(not(feature = "eink"))]
            art_sec: 60,
        }
    }
}
```

Replace `settings_load` / `settings_save` (lines 697-709) with:

```rust
fn settings_load(nvs: &EspNvs<NvsDefault>) -> Settings {
    let brightness = nvs.get_u8("bright").ok().flatten().unwrap_or(100).clamp(10, 100);
    let sleep_min  = snap_sleep(nvs.get_u16("sleep").ok().flatten().unwrap_or(0));
    let dark       = nvs.get_u8("dark").ok().flatten().unwrap_or(1) != 0;
    DARK.store(dark, Ordering::Relaxed);
    #[cfg(not(feature = "eink"))]
    let art_sec = mascot::snap_art(nvs.get_u16("art").ok().flatten().unwrap_or(60), sleep_min);
    Settings {
        brightness,
        sleep_min,
        dark,
        #[cfg(not(feature = "eink"))]
        art_sec,
    }
}

fn settings_save(nvs: &mut EspNvs<NvsDefault>, s: &Settings) {
    let _ = nvs.set_u8("bright", s.brightness);
    let _ = nvs.set_u16("sleep", s.sleep_min);
    let _ = nvs.set_u8("dark", s.dark as u8);
    #[cfg(not(feature = "eink"))]
    let _ = nvs.set_u16("art", s.art_sec);
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd bridge && cargo test --test firmware_mascot_test`
Expected: PASS — 19 tests.

- [ ] **Step 5: Commit**

```bash
git add firmware/src/mascot.rs bridge/tests/firmware_mascot_test.rs firmware/src/main.rs
git commit -m "feat(firmware): add idle-art setting with sleep-aware validity rules"
```

---

### Task 5: Fifth tab — bar, hit-testing, render wiring

First `main.rs` task. Not locally compilable; verified by CI.

**Files:**
- Modify: `firmware/src/main.rs:67-68` (Tab enum), `:231-245` (tab bar), `:259-277` (render), `:929-933` and `:1081-1085` (hit-testing)

**Interfaces:**
- Consumes: `mascot::{mood_for, mood_index}`, `sprite::{Sprites, BLOB, SPRITE_W, SPRITE_H, FRAMES}`.
- Produces: `Tab::Pixel` variant; `fn render_pixel<D>(display, ds, frame_idx, full_clear)`.

- [ ] **Step 1: Add the Tab variant**

Replace line 68:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
enum Tab {
    Sessions,
    Usage,
    Metrics,
    #[cfg(not(feature = "eink"))]
    Pixel,
    Settings,
}
```

- [ ] **Step 2: Rewrite the tab bar for five segments**

Replace `draw_tab_bar` (lines 231-245):

```rust
// 320 px / 5 tabs = 64 px each (1 px gutters). Labels are shortened to fit
// FONT_7X13_BOLD inside 64 px. The e-ink build keeps the original four.
fn draw_tab_bar<D: DrawTarget<Color = Rgb565>>(d: &mut D, active: Tab) {
    #[cfg(not(feature = "eink"))]
    let tabs: &[(&str, Tab)] = &[
        ("SESS",   Tab::Sessions),
        ("USAGE",  Tab::Usage),
        ("METRIC", Tab::Metrics),
        ("PIXEL",  Tab::Pixel),
        ("SET",    Tab::Settings),
    ];
    #[cfg(feature = "eink")]
    let tabs: &[(&str, Tab)] = &[
        ("SESSIONS", Tab::Sessions),
        ("USAGE",    Tab::Usage),
        ("METRICS",  Tab::Metrics),
        ("SETTINGS", Tab::Settings),
    ];

    let n = tabs.len() as i32;
    let w = (320 - (n + 1)) / n;         // 5 tabs -> 62 px; 4 tabs -> 78 px
    let mut x = 1i32;
    for (label, tab) in tabs {
        let (bg, fg) = if *tab == active { (c_claude(), c_bg()) } else { (c_panel(), c_dim()) };
        rfill(d, x, 1, w as u32, 24, 5, bg);
        txt(d, &FONT_7X13_BOLD, label, x + w / 2, 17, Alignment::Center, fg);
        x += w + 1;
    }
}

// Screen-x -> Tab, matching the geometry above. Kept next to draw_tab_bar so
// the two cannot drift apart.
fn tab_at(sx: i32) -> Tab {
    #[cfg(not(feature = "eink"))]
    let tabs = [Tab::Sessions, Tab::Usage, Tab::Metrics, Tab::Pixel, Tab::Settings];
    #[cfg(feature = "eink")]
    let tabs = [Tab::Sessions, Tab::Usage, Tab::Metrics, Tab::Settings];

    let n = tabs.len() as i32;
    let w = (320 - (n + 1)) / n;
    let i = ((sx - 1) / (w + 1)).clamp(0, n - 1) as usize;
    tabs[i]
}
```

- [ ] **Step 3: Add the pixel renderer**

Add after `render_metrics` (i.e. after line 277's `render` function; place it before `render_metrics` or after — anywhere in the render section):

```rust
// Pixel-art mascot. `full_screen` hides the tab bar (auto-engaged screensaver
// mode); otherwise the sprite sits below the 26 px bar.
#[cfg(not(feature = "eink"))]
fn render_pixel<D: DrawTarget<Color = Rgb565>>(
    display: &mut D, ds: &DisplayState, frame_idx: usize, full_clear: bool,
) {
    // `Rectangle`, `Point` and `Size` are already in scope from main.rs's
    // top-level imports (lines 2-11) — do not re-import, clippy denies that.
    if full_clear { fill(display, 0, 26, 320, 214, c_bg()); }

    let Some(sprites) = sprite::Sprites::parse(sprite::BLOB) else { return };
    let mood = mascot::mood_for(ds);
    let Some(px) = sprites.frame(
        mascot::mood_index(mood),
        frame_idx % sprite::FRAMES,
        c_bg(),
    ) else { return };

    let area = Rectangle::new(
        Point::new(96, 69),
        Size::new(sprite::SPRITE_W, sprite::SPRITE_H),
    );
    let _ = display.fill_contiguous(&area, px);
}
```

- [ ] **Step 4: Wire it into `render`**

Replace the `render` signature and body (lines 259-277):

```rust
fn render<D: DrawTarget<Color = Rgb565>>(
    display: &mut D, ds: &DisplayState, active: Tab, view: View, set: &Settings,
    full_clear: bool, frame_idx: usize, full_screen: bool,
) {
    if full_clear { fill(display, 0, 0, 320, 240, c_bg()); }
    if !full_screen { draw_tab_bar(display, active); }

    match active {
        Tab::Sessions => match view {
            View::List             => render_sessions(display, ds),
            View::Detail { index } => match ds.sessions.get(index) {
                Some(row) => render_detail(display, row),
                None      => render_sessions(display, ds),
            },
        },
        Tab::Usage    => render_usage(display, ds),
        Tab::Metrics  => render_metrics(display, &ds.metrics),
        #[cfg(not(feature = "eink"))]
        Tab::Pixel    => render_pixel(display, ds, frame_idx, full_clear),
        Tab::Settings => render_settings(display, set),
    }
}
```

`frame_idx` and `full_screen` are genuinely unused in the e-ink build (no
`Tab::Pixel` arm compiles, and the tab bar always draws). Clippy runs with
`-D warnings`, so silence them unconditionally by adding this as the first line
of `render`'s body — it is a no-op in every build and avoids a cfg-dependent
signature:

```rust
    #[cfg(feature = "eink")]
    let _ = (frame_idx, full_screen);
```

- [ ] **Step 5: Update both hit-test sites**

In the wifi loop, replace lines 929-933:

```rust
                } else if sy < 34 {
                    active_tab = tab_at(sx);
                    view = View::List;
```

Apply the identical replacement in the usb/ble loop (lines 1081-1085).

Update both `render(...)` call sites (lines 971 and 1125) to pass the two new arguments — for now `render(&mut display, &ds, active_tab, view, &settings, layout_changed, 0, false)`. Task 7 replaces the `0`/`false` with real values.

- [ ] **Step 6: Verify formatting and lints on the host-testable parts**

```bash
cd bridge && cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings
cd bridge && cargo test
```

Expected: clean; all firmware host tests still pass.

- [ ] **Step 7: Commit**

```bash
git add firmware/src/main.rs
git commit -m "feat(firmware): add PIXEL tab to the tab bar and renderer"
```

Note: firmware compilation is verified in CI. Push and confirm `Firmware · Build (usb)`, `(wifi-ota)` and `(eink)` all pass before starting Task 6.

---

### Task 6: Idle-art settings row

**Files:**
- Modify: `firmware/src/main.rs:562-595` (`render_settings`), `:598-618` (`settings_touch`)

**Interfaces:**
- Consumes: `mascot::{ART_VALS, ART_LBL, art_is_valid, snap_art}` (Task 4).

- [ ] **Step 1: Add the chip row to `render_settings`**

Append inside `render_settings`, after the Theme switch block (before the closing brace at line 595):

```rust
    // ── Idle art ──
    // Chips whose delay would never fire (>= the sleep timeout) render dim and
    // are rejected by settings_touch, so the two settings cannot contradict.
    #[cfg(not(feature = "eink"))]
    {
        txt(display, &FONT_7X13, "Idle art", 8, 200, Alignment::Left, c_fg());
        let sel = mascot::ART_VALS.iter().position(|&v| v == set.art_sec).unwrap_or(0);
        for i in 0..mascot::ART_VALS.len() {
            let cx = 4 + (i as i32) * 78;
            let on = i == sel;
            let ok = mascot::art_is_valid(mascot::ART_VALS[i], set.sleep_min);
            // Invalid chips drop their background to the page bg so they visually
            // recede (no raised panel), while keeping a c_dim() label — that label
            // colour has strong contrast against c_bg() in both palettes, unlike
            // c_panel(), which is indistinguishable from itself as a label on a
            // c_panel() background.
            let chip_bg = if on { c_claude() } else if ok { c_panel() } else { c_bg() };
            rfill(display, cx, 206, 76, 30, 5, chip_bg);
            let label_fg = if on { c_bg() } else { c_dim() };
            txt(display, &FONT_7X13, mascot::ART_LBL[i], cx + 38, 225,
                Alignment::Center, label_fg);
        }
    }
```

- [ ] **Step 2: Handle touches on the new row**

In `settings_touch`, insert before the final `false` (line 617):

```rust
    // Idle-art chips
    #[cfg(not(feature = "eink"))]
    if (206..=236).contains(&sy) && (4..=316).contains(&sx) {
        let i = (((sx - 4) / 78).clamp(0, mascot::ART_VALS.len() as i32 - 1)) as usize;
        let v = mascot::ART_VALS[i];
        if mascot::art_is_valid(v, set.sleep_min) && v != set.art_sec {
            set.art_sec = v;
            return true;
        }
        return false;
    }
```

- [ ] **Step 3: Re-snap the art value when sleep changes**

In `settings_touch`, replace the Sleep chips block (lines 606-611):

```rust
    // Sleep chips
    if (114..=148).contains(&sy) && (4..=314).contains(&sx) {
        let i = (((sx - 4) / 62).clamp(0, 4)) as usize;
        let v = SLEEP_VALS[i];
        if v != set.sleep_min {
            set.sleep_min = v;
            // A shorter sleep timeout can invalidate the stored art delay.
            #[cfg(not(feature = "eink"))]
            {
                set.art_sec = mascot::snap_art(set.art_sec, v);
            }
            return true;
        }
    }
```

- [ ] **Step 4: Verify**

```bash
cd bridge && cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test
```

Expected: clean, all tests pass. (These settings functions are not host-tested directly — the pure logic they call is, from Task 4.)

- [ ] **Step 5: Commit**

```bash
git add firmware/src/main.rs
git commit -m "feat(firmware): add Idle art row to the settings tab"
```

---

### Task 7: Idle auto-engage and shared loop state

The behavioural core, and the only task that touches existing loop logic.

**Files:**
- Modify: `firmware/src/main.rs:897-976` (wifi loop), `:1046-1130` (usb/ble loop)

**Interfaces:**
- Consumes: everything from Tasks 1, 3, 4, 5.
- Produces: `struct ArtState` with `fn tick(&mut self, now, art_sec, ds) -> bool` and `fn wake(&mut self)`.

Critical constraint from the design: the art timer is a **separate clock** from `last_touch`. `main.rs:950-953` already resets `last_touch` whenever a session is `Waiting`, so reusing it would prevent the art from ever auto-engaging while something waits — making `Happy` unreachable from the screensaver.

Scope note (design doc section 6): all *new* behaviour is encapsulated in one `ArtState` used by both loops, so only wiring is duplicated. The pre-existing `active_tab` / `view` / `settings` / `prev` locals are **not** hoisted into a shared struct — that is working code this feature does not otherwise touch, unverifiable locally, and belongs in its own change.

- [ ] **Step 1: Add the shared art-state helper**

Add near `Settings` (after `snap_sleep`, around line 99):

```rust
// Screensaver state, shared verbatim by both transport loops.
//
// NB: `last_input` is deliberately NOT the loops' `last_touch`. That one is
// also reset whenever a session is Waiting (so the screen stays awake for you),
// which would stop the art from ever auto-engaging while anything waits — and
// Waiting is exactly what drives the Happy animation.
#[cfg(not(feature = "eink"))]
struct ArtState {
    last_input:  Instant,
    engaged:     bool,
    frame:       usize,
    last_advance: Instant,
}

#[cfg(not(feature = "eink"))]
const ART_FRAME_MS: u64 = 125; // 8 fps

#[cfg(not(feature = "eink"))]
impl ArtState {
    fn new() -> Self {
        ArtState {
            last_input: Instant::now(),
            engaged: false,
            frame: 0,
            last_advance: Instant::now(),
        }
    }

    /// Called on every touch: dismisses the screensaver and restarts the clock.
    /// Returns true if a touch was consumed by dismissing (caller must then NOT
    /// route it as a tab / card tap).
    fn wake(&mut self) -> bool {
        self.last_input = Instant::now();
        if self.engaged {
            self.engaged = false;
            self.frame = 0;
            return true;
        }
        false
    }

    /// Advance the clock. Returns true if the display needs repainting.
    fn tick(&mut self, art_sec: u16, mood_changed: bool) -> bool {
        if art_sec > 0
            && !self.engaged
            && self.last_input.elapsed() >= std::time::Duration::from_secs(art_sec as u64)
        {
            self.engaged = true;
            self.frame = 0;
            self.last_advance = Instant::now();
            return true;
        }
        if mood_changed {
            self.frame = 0;              // a mood change reads as a fresh start
            self.last_advance = Instant::now();
            return true;
        }
        if self.last_advance.elapsed() >= std::time::Duration::from_millis(ART_FRAME_MS) {
            self.last_advance = Instant::now();
            self.frame = self.frame.wrapping_add(1);
            return true;
        }
        false
    }
}
```

- [ ] **Step 2: Wire it into the wifi loop**

In the wifi loop, add to the declarations after line 906:

```rust
        #[cfg(not(feature = "eink"))]
        let mut art = ArtState::new();
        #[cfg(not(feature = "eink"))]
        let mut last_mood = mascot::mood_for(&ds);
```

Replace the touch-handling block (lines 924-946) so a dismissing touch is consumed:

```rust
                if sleeping {
                    sleeping = false;
                    set_brightness(&mut bl, settings.brightness);
                    prev = None;
                    art.wake();
                } else if art.wake() {
                    // Touch dismissed the screensaver — consume it so waking the
                    // device never also switches tabs or opens a session card.
                    prev = None;
                } else if sy < 34 {
                    active_tab = tab_at(sx);
                    view = View::List;
                } else if active_tab == Tab::Sessions {
                    view = sessions_touch(sx, sy, view, &ds);
                } else if active_tab == Tab::Settings {
                    let prev_dark = settings.dark;
                    if settings_touch(sx, sy, &mut settings) {
                        if settings.dark != prev_dark {
                            DARK.store(settings.dark, Ordering::Relaxed);
                            prev = None;
                        }
                        set_brightness(&mut bl, settings.brightness);
                        settings_save(&mut nvs, &settings);
                    }
                }
```

Replace the render block (lines 962-974):

```rust
            if !sleeping {
                #[cfg(not(feature = "eink"))]
                let mood = mascot::mood_for(&ds);
                #[cfg(not(feature = "eink"))]
                let mood_changed = mood != last_mood;
                #[cfg(not(feature = "eink"))]
                { last_mood = mood; }

                #[cfg(not(feature = "eink"))]
                let showing_art = art.engaged || active_tab == Tab::Pixel;
                #[cfg(not(feature = "eink"))]
                let art_dirty = art.tick(settings.art_sec, mood_changed) && showing_art;
                #[cfg(feature = "eink")]
                let art_dirty = false;

                let layout_changed = prev.as_ref()
                    .map(|p| p.1 != active_tab || p.2 != view).unwrap_or(true);
                let content_changed = prev.as_ref().map(|p| match active_tab {
                    Tab::Settings => p.3 != settings,
                    Tab::Metrics  => p.0.metrics != ds.metrics,
                    _             => p.0 != ds,
                }).unwrap_or(true);

                if layout_changed || content_changed || art_dirty {
                    #[cfg(not(feature = "eink"))]
                    let (tab_to_draw, frame_idx, full_screen) = if art.engaged {
                        (Tab::Pixel, art.frame, true)
                    } else {
                        (active_tab, art.frame, false)
                    };
                    #[cfg(feature = "eink")]
                    let (tab_to_draw, frame_idx, full_screen) = (active_tab, 0usize, false);

                    // Engaging/leaving the screensaver changes layout identity,
                    // so force a full clear on that transition.
                    let clear = layout_changed
                        || prev.as_ref().map(|p| p.1 != tab_to_draw).unwrap_or(true);

                    render(&mut display, &ds, tab_to_draw, view, &settings,
                           clear, frame_idx, full_screen);
                    prev = Some((ds.clone(), tab_to_draw, view, settings));
                }
            }
```

- [ ] **Step 3: Apply the identical changes to the usb/ble loop**

Repeat Step 2 verbatim in the second loop (declarations after line 1056; touch block at lines 1076-1098; render block at lines 1117-1128), substituting the loop's local state variable name (`state` rather than `ds`) where it differs. Read both loops side by side before editing to confirm the only differences are the transport and that variable name.

- [ ] **Step 4: Reduce the loop delay so 8 fps is achievable**

Both loops end with `FreeRtos::delay_ms(50)`. 50 ms polling cannot hit a 125 ms frame boundary accurately but is close enough (frames land at 150 ms, ~6.7 fps). Leave it at 50 ms — dropping it would increase touch-poll CPU for a barely perceptible gain. Add a comment at both sites:

```rust
            // 50 ms poll: art frames land on ~150 ms (≈6.7 fps) rather than the
            // nominal 125 ms. Deliberate — a tighter loop costs touch-poll CPU
            // for an imperceptible smoothness gain.
            FreeRtos::delay_ms(50);
```

- [ ] **Step 5: Verify**

```bash
cd bridge && cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test
```

Expected: clean. Firmware compilation is CI-verified.

- [ ] **Step 6: Commit and push for CI verification**

```bash
git add firmware/src/main.rs
git commit -m "feat(firmware): auto-engage the pixel art after an idle period"
git push
```

Wait for `Firmware · Build (usb)`, `(wifi-ota)` and `(eink)` to pass before continuing. The e-ink build is the one most likely to break here, since every new block is cfg-gated out of it.

---

### Task 8: CI flash-size guard

**Files:**
- Modify: `.github/workflows/ci.yml` (firmware-build job, after the "Verify ELF output exists" step)

- [ ] **Step 1: Add the size-report and guard step**

Add after the existing "Verify ELF output exists" step:

```yaml
      # The pixel-art blob adds ~81 KB. The default build targets the built-in
      # single-`factory` layout (1 MiB app partition), so record the real
      # margin on every run and fail before an addition can silently overflow it.
      - name: Check app image fits the factory partition
        run: |
          # Measure the flash-resident sections of the app ELF directly. A
          # stray `.bin` (e.g. a partition-table binary) is NOT used here: an
          # earlier version of this step preferred any `.bin` it found, which
          # silently measured the wrong file and reported a fake, identical
          # size across every matrix leg. The ELF at this path is reliable.
          # NB the toolchain binary is `xtensa-esp-elf-size` (esp-15.x), NOT
          # `xtensa-esp32-elf-size` — the latter does not exist on the runner.
          ELF=/tmp/fw-target/xtensa-esp32-espidf/release/vibe-firmware
          SIZER=$(command -v xtensa-esp-elf-size || command -v xtensa-esp32-elf-size || true)
          if [ -z "$SIZER" ]; then
            echo "::warning::no esp size tool on PATH; falling back to ELF file size (overestimates)"
            SIZE=$(stat -c%s "$ELF" 2>/dev/null || echo "")
          else
            SIZER_OUTPUT=$("$SIZER" -A "$ELF" 2>&1 || true)
            echo "----- $SIZER -A $ELF -----"
            echo "$SIZER_OUTPUT"
            echo "---------------------------------------------------------"
            {
              echo "<details><summary>Raw <code>$(basename "$SIZER") -A</code> output (${{ matrix.name }})</summary>"
              echo ""
              echo '```'
              echo "$SIZER_OUTPUT"
              echo '```'
              echo "</details>"
            } >> "$GITHUB_STEP_SUMMARY"
            # Sum every loaded section (nonzero address) that isn't a
            # zero-initialized RAM section (bss/noinit never occupies space
            # in the flashed image). This is deliberately not a hardcoded
            # list of esp-idf section-name prefixes — the raw table printed
            # above is what makes this arithmetic auditable, not a regex.
            SIZE=$(echo "$SIZER_OUTPUT" | awk '
              $1 ~ /^\./ && $2 ~ /^[0-9]+$/ {
                name = tolower($1); sz = $2 + 0; addr = $3 + 0
                if (name ~ /bss|noinit/) next
                if (addr == 0) next
                s += sz
              }
              END { print s+0 }
            ')
          fi
          if [ -z "$SIZE" ] || [ "$SIZE" -le 0 ]; then
            echo "::error::could not determine app image size"; exit 1
          fi
          # A real firmware image embedding the ~82 KB pixel-art asset cannot
          # be anywhere near this small. Landing here means the measurement
          # itself is broken (wrong file, wrong tool, wrong sections) — a
          # guard that can't detect its own misconfiguration is worthless,
          # so fail loudly instead of reporting a fictitious margin.
          PLAUSIBILITY_FLOOR_BYTES=204800
          if [ "$SIZE" -lt "$PLAUSIBILITY_FLOOR_BYTES" ]; then
            echo "::error::measured app image size ($SIZE B) is implausibly small (< $PLAUSIBILITY_FLOOR_BYTES B) — the size guard is measuring the wrong thing, not a genuinely tiny firmware"
            exit 1
          fi
          # Partition limit depends on which layout this matrix leg targets:
          # usb/eink keep the built-in single-`factory` layout (1 MiB app
          # partition); wifi-ota targets the dual-slot OTA layout defined in
          # firmware/partitions_ota.csv, whose `ota_0` slot is 0x1A0000 =
          # 1,703,936 B. This checks each build against its INTENDED layout;
          # whether the OTA sdkconfig layer is actually applied at build time
          # is tracked separately and is out of scope for this guard.
          case "${{ matrix.name }}" in
            wifi-ota)
              LIMIT=1703936
              LIMIT_DESC="ota_0 slot, partitions_ota.csv"
              ;;
            *)
              LIMIT=1048576
              LIMIT_DESC="factory partition"
              ;;
          esac
          MARGIN=$((LIMIT - SIZE))
          echo "app image: $SIZE bytes, limit: $LIMIT B ($LIMIT_DESC), margin: $MARGIN"
          {
            echo "### Firmware size (${{ matrix.name }})"
            echo ""
            echo "| metric | value |"
            echo "| --- | ---: |"
            echo "| app image | $SIZE B |"
            echo "| partition limit | $LIMIT B ($LIMIT_DESC) |"
            echo "| margin | $MARGIN B |"
          } >> "$GITHUB_STEP_SUMMARY"
          if [ "$SIZE" -ge "$LIMIT" ]; then
            echo "::error::app image ($SIZE B) does not fit the $LIMIT_DESC ($LIMIT B)"
            exit 1
          fi
          if [ "$MARGIN" -lt 51200 ]; then
            echo "::warning::only $MARGIN bytes of $LIMIT_DESC headroom remain (<50 KB)"
          fi
```

- [ ] **Step 2: Commit and push**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: guard the firmware app image against factory-partition overflow"
git push
```

- [ ] **Step 3: Read the real numbers**

Open the CI run's job summary and record the actual `margin` for the `usb` build.

**If the margin is negative or under ~30 KB**, the 128×128 configuration does not fit. Fall back per the design doc: edit `SIZE = (96, 96)` in `tools/gif2sprite.py`, and `SPRITE_W`/`SPRITE_H` in `firmware/src/sprite.rs`, plus the `Rectangle` origin in `render_pixel` (centre of the 214 px band: `y = 26 + (214 - 96) / 2 = 85`, `x = (320 - 96) / 2 = 112`). Regenerate, re-run Task 2 and 3 tests, and commit as `fix(firmware): fall back to 96x96 sprites to fit the factory partition`.

---

### Task 9: README and attribution

**Files:**
- Modify: `README.md`
- Verify: `assets/gif/NOTICE` exists from Task 2

- [ ] **Step 1: Add the screensaver section**

Insert a section after the existing feature/tab description in `README.md`:

```markdown
## Pixel-art screensaver

The **PIXEL** tab shows an animated mascot whose animation reflects what your
agents are doing. After an idle period it takes over the whole screen as a
screensaver; any touch returns you to the tab you were on.

| Animation | Shown when |
| --- | --- |
| ![happy](assets/gif/clawd-happy.gif) | a session is **waiting** for you |
| ![juggling](assets/gif/clawd-juggling.gif) | **two or more** sessions are working |
| ![building](assets/gif/clawd-building.gif) | one session has been working but quiet for over 2 minutes — a long build or tool call |
| ![typing](assets/gif/clawd-typing.gif) | exactly **one** session is working |
| ![sleeping](assets/gif/clawd-sleeping.gif) | nothing is running, everything is idle, or the bridge is offline |

Rules are checked top-down; the first match wins, so "waiting" always wins —
it is the state that wants a human.

Configure it under **SET → Idle art**: `Off`, `30s`, `1m`, `5m`. Values that
would never fire because the screen blanks first (see **Sleep after**) are shown
dimmed and cannot be selected.

The artwork is adapted from [clawd](https://github.com/KebeliSamet0/clawd)
by KebeliSamet0, used under the MIT License. See `assets/gif/NOTICE`.
```

- [ ] **Step 2: Verify the images resolve**

```bash
grep -o 'assets/gif/[a-z-]*\.gif' README.md | sort -u | while read -r f; do
  [ -f "$f" ] && echo "ok   $f" || echo "MISSING $f"
done
```

Expected: five `ok` lines, no `MISSING`.

- [ ] **Step 3: Commit**

```bash
git add README.md assets/gif/NOTICE
git commit -m "docs: document the pixel-art screensaver tab"
```

---

## Verification checklist

Before opening the PR:

- [ ] `cd bridge && cargo test` — all host tests pass (proto + mascot + sprite)
- [ ] `cd bridge && cargo fmt --check` — clean
- [ ] `cd bridge && cargo clippy --all-targets --all-features -- -D warnings` — clean
- [ ] `python3 tools/test_gif2sprite.py` — all pass
- [ ] CI: `Firmware · Build (usb)` passes
- [ ] CI: `Firmware · Build (wifi-ota)` passes
- [ ] CI: `Firmware · Build (eink)` passes **and the e-ink build still shows four tabs** (no `Tab::Pixel` compiled in)
- [ ] CI: size-guard step reports a positive margin for every build
- [ ] PR title follows Conventional Commits (`feat(firmware): ...`)
