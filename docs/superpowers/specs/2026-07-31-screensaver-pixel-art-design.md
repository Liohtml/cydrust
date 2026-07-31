# Pixel-art screensaver tab

**Date:** 2026-07-31
**Status:** Approved, not yet implemented
**Scope:** `firmware/` (new tab + assets), `README.md`, CI size guard

## Goal

Add a fifth tab to the CYD firmware that shows an animated pixel-art mascot
whose animation reflects current agent activity. It doubles as a screensaver:
after a configurable idle period it takes over the screen from any tab, and any
touch returns you to where you were.

Artwork is adapted from [KebeliSamet0/clawd](https://github.com/KebeliSamet0/clawd)
(MIT). Attribution is preserved in `README.md` and in the asset directory.

## Non-goals

- The e-ink variant does not get this tab (see "Variant handling").
- No new bridge/protocol fields. The mascot is derived entirely from the
  `DisplayState` the firmware already parses.
- No user-configurable mapping of activity to animation.

---

## 1. Module boundaries

| Unit | Purpose | Dependencies |
| --- | --- | --- |
| `firmware/src/mascot.rs` | `Mood` enum and `mood_for(&DisplayState) -> Mood`. Pure decision logic, no I/O. | std only |
| `firmware/src/sprite.rs` | Blob header parsing and RLE decoding to an `Iterator<Item = Rgb565>`. | `embedded-graphics` only |
| `firmware/assets/sprites.bin` | Generated asset blob, embedded with `include_bytes!`. | — |
| `tools/gif2sprite.py` | One-off GIF → blob converter. Output is committed. | Pillow |

`mascot.rs` is std-only for the same reason `proto.rs` is: it can be compiled
into the bridge's test binary with `#[path = "../../firmware/src/mascot.rs"]`
and exercised by `cargo test` on plain stable, with no ESP32 toolchain present.
`sprite.rs` depends only on `embedded-graphics`, which builds on host, so it is
testable the same way.

The converter is deliberately **not** a `build.rs` step. The artwork changes
approximately never, and making every builder install Pillow to compile the
firmware would be a poor trade.

---

## 2. Asset pipeline

`tools/gif2sprite.py` performs, for each of the five source GIFs:

1. Sample 16 frames evenly from the source's 44–45.
2. Downscale 302×300 → 128×128 with `NEAREST` (preserves pixel-art edges;
   bilinear would blur them).
3. Map pixels with alpha < 128 to palette index 0 (reserved: transparent).
4. Quantize remaining pixels against a **single shared palette** computed
   across all five animations, so one palette serves the whole blob and mood
   changes cause no palette swap. The palette holds 16 entries: index 0
   reserved for transparent, plus 15 real colours.
5. Emit 4bpp indices run-length encoded as `(run_len: u8, index: u8)` pairs.

### Blob format (`sprites.bin`, little-endian)

```
offset  size  field
0       4     magic "CYDS"
4       1     version = 1
5       1     mood_count = 5
6       1     frames_per_mood = 16
7       1     sprite_w = 128
8       1     sprite_h = 128
9       1     palette_len = 16
10      32    palette: 16 x u16 RGB565   (index 0 is reserved/transparent)
42      640   frame table: 80 x { offset: u32, len: u32 }  (mood-major)
682     ...   RLE payload
```

Frame table is mood-major: frame `f` of mood `m` is entry `m * 16 + f`.

Measured total: **~81 KB** at this configuration. Recorded alternatives if the
size guard (section 9) shows less headroom than expected:

| config | RLE size |
| --- | --- |
| 128×128 × 16f | 81 KB |
| 96×96 × 16f | 54 KB |
| 96×96 × 12f | 41 KB |

The script is deterministic: identical input produces byte-identical output.

### Transparency and theming

Palette index 0 is never emitted by the quantizer; the decoder substitutes the
**active theme background colour** for it. This is what allows the same blob to
render correctly in both dark and light themes. Baking a background colour into
the sprite would show a dark box in light theme.

---

## 3. Activity → animation

```rust
pub enum Mood { Happy, Juggling, Building, Typing, Sleeping }

pub fn mood_for(ds: &DisplayState) -> Mood
```

Priority ladder, first match wins:

| # | Condition | Mood |
| --- | --- | --- |
| 1 | any session has `status == Waiting` | `Happy` |
| 2 | count of `Working` sessions >= 2 | `Juggling` |
| 3 | exactly 1 `Working` and its `age_sec > 120` | `Building` |
| 4 | exactly 1 `Working` | `Typing` |
| 5 | otherwise — offline, no sessions, or all idle | `Sleeping` |

### What rule 3 actually detects

`age_sec` is **seconds since last activity** (`bridge/src/hub.rs:126`,
`age = now - s.last_activity`), not turn duration. The bridge marks a session
`Working` when `active_turn || age < WORKING_SEC`, and `WORKING_SEC = 60`
(`hub.rs:23`).

Therefore a `Working` session with `age_sec > 120` can only arise when
`active_turn == true` — the agent is mid-turn but has produced no output for
over two minutes. In practice that is a long build, test run, or tool call:
precisely what `Building` should convey. The threshold is 2× `WORKING_SEC`, so
it cannot be reached by mtime jitter alone.

If `WORKING_SEC` ever changes, this threshold should be revisited; a test
asserts the relationship holds (`ART_BUILDING_SEC > WORKING_SEC`).

`age_sec == -1` (unknown) does not satisfy `> 120`, so an unknown age falls
through to `Typing`.

`Waiting` ranks first deliberately: it is the state that wants a human, so it
gets the most visually distinct animation.

---

## 4. Rendering and timing

- Sprite is 128×128, drawn at `x = 96`, `y = 69` — horizontally centred, and
  vertically centred in the 214 px band below the 26 px tab bar.
- Each frame decodes lazily through `fill_contiguous` over the sprite
  rectangle: one bulk SPI transfer, no framebuffer. Working memory is a single
  row buffer (128 px × 2 B = 256 B).
- Frame rate is 8 fps (125 ms/frame); 16 frames gives a 2 s loop.
- On mood change the animation restarts at frame 0, so a transition reads as a
  deliberate change rather than a mid-motion jump.

### Presentation modes

| Mode | Tab bar | Entered by | Left by |
| --- | --- | --- | --- |
| Manual | visible | tapping the PIXEL tab | tapping another tab |
| Auto-engaged | hidden, full screen | `art_sec` of no touch, from any tab | any touch |

The touch that dismisses auto-engaged mode is **consumed**: it restores the
previous tab and view and is not additionally routed as a tab switch or a
session-card tap. Otherwise waking the device would trigger a random action.

Auto-engaging while already on the PIXEL tab is not a special case: the tab bar
hides, and the dismissing touch restores the PIXEL tab with its bar.

The idle timer is reset by **touch only**. Incoming state changes do not reset
it and do not wake the screen — a session going `Waiting` while the art is up
changes the animation to `Happy` but does not dismiss the screensaver. Waking
the device on remote events is a separate feature and deliberately out of scope.

---

## 5. Tab bar and settings

### Tab bar

Grows from 4 to 5 segments: `320 / 5 = 64 px` each. Labels shorten to
`SESS · USAGE · METRIC · PIXEL · SET`. Hit-testing changes from `sx / 80` to
`sx / 64` in both transport loops.

### Settings

A new **Idle art** chip row is added to the Settings tab:

```
Brightness  [======----] 60%      label y=48,  track y=56
Sleep after [Never][1m][5m][15m][30m]   label y=104, chips y=114
Theme: Dark                 [ o]  label y=176, switch y=166
Idle art    [Off][30s][1m][5m]    label y=200, chips y=206 (30 px, bottom 236)
```

- Values: `ART_VALS: [u16; 4] = [0, 30, 60, 300]` seconds, `0` = Off.
- Persisted in NVS under key `"art"`, alongside `"sleep"`, `"bright"`, `"dark"`.
- Default: `60` (1m).

### Interaction with the sleep timeout

Art engages at `art_sec`; `sleep_min` still blanks the screen afterwards. The
art is the stage *before* blanking, not a replacement for it.

To prevent a setting that silently does nothing, an art value is **invalid**
when `sleep_min != 0 && art_sec >= sleep_min * 60`. Invalid chips render dim
and are not selectable. If a change to `sleep_min` invalidates the stored
`art_sec`, it snaps down to the largest valid value, or to `Off` if none exists.

| `sleep_min` | selectable art values |
| --- | --- |
| Never (0) | Off, 30s, 1m, 5m |
| 1m | Off, 30s |
| 5m | Off, 30s, 1m |
| 15m / 30m | Off, 30s, 1m, 5m |

---

## 6. Variant handling and one targeted cleanup

**e-ink is excluded** via `#[cfg(not(feature = "eink"))]`. An e-paper full
refresh takes seconds, and animating it would both look wrong and shorten panel
life. That build keeps its existing 4 tabs; no sprite data is compiled into it.

**usb / wifi / ble** all get the tab — it is transport-independent.

### Cleanup

`main.rs` currently holds two near-duplicate render/touch loops: the wifi loop
(~line 901) and the usb/ble loop (~line 1050). They differ only in transport.
Adding an idle timer and animation clock to both would duplicate roughly 40
more lines and invite the two copies to drift.

This design extracts the shared per-tick UI logic into a single `UiState`
struct — holding `active_tab`, `view`, `settings`, `prev`, idle deadline, and
animation frame — which both transports drive. This is scoped to the loops this
feature already has to modify; it is not a general refactor of `main.rs`.

---

## 7. Error handling

The mascot must never take down the dashboard — monitoring is the device's job.

- The generator validates the blob at build time.
- At startup the firmware checks magic, version, and that the frame table is
  within bounds. On failure the PIXEL tab is **omitted** (4-tab layout, no
  auto-engage) rather than panicking.
- The decoder clamps every run to the remaining pixels in the sprite rectangle,
  so a corrupt or truncated blob cannot write outside its bounds or overrun.
- A frame whose decoded pixel count is short is padded with index 0
  (background); one that would overrun is truncated.

---

## 8. Testing

All host-run via the bridge test binary, no ESP32 toolchain:

**`mood_for` — table-driven over every ladder branch:**
- empty session list → `Sleeping`
- `offline == true` → `Sleeping`
- all sessions `Idle` → `Sleeping`
- 1 `Working`, `age_sec = 119` → `Typing`
- 1 `Working`, `age_sec = 120` → `Typing` (boundary is strictly `>`)
- 1 `Working`, `age_sec = 121` → `Building`
- 1 `Working`, `age_sec = -1` (unknown) → `Typing`
- 2 `Working` → `Juggling`
- 1 `Waiting` + 3 `Working` → `Happy` (rule 1 outranks rule 2)
- `ART_BUILDING_SEC > WORKING_SEC`, so rule 3 stays unreachable without
  `active_turn` (guards the reasoning in section 3)

**Decoder:**
- every frame of every mood decodes to exactly `128 × 128` pixels
- every emitted index is `< palette_len`
- header rejection: bad magic, wrong version, out-of-bounds frame table entry
- truncated payload does not panic and does not exceed the sprite rect

**Golden checksums:** a stored hash of decoded frame 0 for each mood, so a
regenerated blob cannot silently change the artwork.

**Converter determinism:** running `gif2sprite.py` twice on the same GIFs
produces identical bytes.

---

## 9. README and CI size guard

### README

A screensaver section showing the five animations and what each means, plus:

- the Idle art setting and its interaction with Sleep
- a credit line for [KebeliSamet0/clawd](https://github.com/KebeliSamet0/clawd) (MIT)

Source GIFs are copied into `assets/gif/` so the repository is self-contained
rather than hotlinking another repo's raw URLs. MIT permits this; the upstream
copyright notice is retained in `assets/gif/NOTICE`.

### CI size guard

A new step in the firmware build job asserts the app image fits the factory
partition with a stated margin. This addition costs ~81 KB, and the current
headroom figure (~200 KB) is **inferred** from the note in `partitions_ota.csv`,
not measured. The guard both pins the real number down and prevents any future
addition from silently overflowing.

If the measured headroom turns out to be tighter than 81 KB plus a safe margin,
fall back to the 96×96 configuration recorded in section 2.

---

## Open risk

The headroom figure is unverified until the size guard runs. This is the only
number in this design that could force a change, and section 2 records the
fallback configurations it would force.
