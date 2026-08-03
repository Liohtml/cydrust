//! Host tests for the pixel-art sprite blob decoder.
//!
//! `firmware/src/sprite.rs` depends only on embedded-graphics (which builds on
//! host), so it compiles into the bridge test binary via `#[path]`.

#[path = "../../firmware/src/sprite.rs"]
mod sprite;

use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::{RawData, RgbColor};
use sprite::{Sprites, BLOB, FRAMES, SPRITE_H, SPRITE_W};

const BG: Rgb565 = Rgb565::BLACK;

// ── Synthetic-blob builder (for FramePixels defensive-path coverage) ───────
//
// The real committed sprites.bin is well-formed by construction, so it never
// exercises FramePixels's short-frame/long-frame/zero-run/out-of-range-index
// branches. These constants mirror the private layout constants in
// `firmware/src/sprite.rs` (not `pub`, so re-declared here) to build minimal,
// otherwise-valid blobs whose single frame-under-test payload we control.
const MOODS: usize = 5;
const PALETTE_LEN: usize = 16;
const HEADER: usize = 10;
const TABLE_AT: usize = HEADER + PALETTE_LEN * 2; // 42
const TABLE_ENTRIES: usize = MOODS * FRAMES; // 80
const TABLE_BYTES: usize = TABLE_ENTRIES * 8; // 640
const PAYLOAD_AT: usize = TABLE_AT + TABLE_BYTES; // 682

/// Builds a minimal, otherwise-valid synthetic blob whose only frame under
/// test is `(mood, frame)`, with `payload` as its raw RLE bytes. Every other
/// frame-table slot points at a small filler run so `Sprites::parse` accepts
/// the whole blob — but no test here ever calls `.frame()` on a filler slot,
/// so its content is irrelevant. Palette entry `i` is given raw value `i`
/// (arbitrary but distinct and deterministic), so `palette_color(i)` tells a
/// test what colour a given index must decode to.
fn synthetic_blob(mood: usize, frame: usize, payload: &[u8]) -> Vec<u8> {
    assert_eq!(
        payload.len() % 2,
        0,
        "RLE payload must be an even number of bytes"
    );
    let mut blob = Vec::new();
    blob.extend_from_slice(b"CYDS");
    blob.push(1); // version
    blob.push(MOODS as u8);
    blob.push(FRAMES as u8);
    blob.push(SPRITE_W as u8);
    blob.push(SPRITE_H as u8);
    blob.push(PALETTE_LEN as u8);
    assert_eq!(blob.len(), HEADER);
    for i in 0..PALETTE_LEN {
        blob.extend_from_slice(&(i as u16).to_le_bytes());
    }
    assert_eq!(blob.len(), TABLE_AT);

    const FILLER: [u8; 2] = [1, 0]; // run=1, idx=0 — never decoded in these tests
    let filler_off = payload.len() as u32;
    for i in 0..TABLE_ENTRIES {
        let (off, len) = if i == mood * FRAMES + frame {
            (0u32, payload.len() as u32)
        } else {
            (filler_off, FILLER.len() as u32)
        };
        blob.extend_from_slice(&off.to_le_bytes());
        blob.extend_from_slice(&len.to_le_bytes());
    }
    assert_eq!(blob.len(), PAYLOAD_AT);

    blob.extend_from_slice(payload);
    blob.extend_from_slice(&FILLER);
    blob
}

/// The colour a raw palette index `i` (as written by `synthetic_blob`) must
/// decode to — mirrors the production `Rgb565::from(RawU16::new(..))`
/// conversion so tests assert against the same mapping the decoder uses.
fn palette_color(idx: u8) -> Rgb565 {
    Rgb565::from(RawU16::new(idx as u16))
}

#[test]
fn parses_the_committed_blob() {
    assert!(
        Sprites::parse(BLOB).is_some(),
        "committed sprites.bin failed to parse"
    );
}

#[test]
fn every_frame_of_every_mood_decodes_to_a_full_sprite() {
    let s = Sprites::parse(BLOB).expect("parse");
    let expected = (SPRITE_W * SPRITE_H) as usize;
    for mood in 0..5 {
        for f in 0..FRAMES {
            let px = s.frame(mood, f, BG).expect("frame present");
            assert_eq!(
                px.count(),
                expected,
                "mood {mood} frame {f} wrong pixel count"
            );
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
        let any_bg = s
            .frame(mood, 0, Rgb565::GREEN)
            .unwrap()
            .any(|c| c == Rgb565::GREEN);
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

// ── Hostile-input coverage for FramePixels's defensive branches ────────────
//
// Each test bounds its iterator with `.take(20_000)` before collecting: if a
// future regression reintroduced an unbounded loop, these would fail with a
// length mismatch instead of hanging the whole suite.

#[test]
fn short_frame_pads_the_remainder_with_background() {
    // Runs sum to only 5 pixels; the decoder must pad the other 16379 with bg.
    let payload = [5u8, 3u8];
    let blob = synthetic_blob(0, 0, &payload);
    let s = Sprites::parse(&blob).expect("synthetic blob should parse");
    let bg = Rgb565::new(1, 2, 3); // distinct from every palette colour (raw 0..16)
    let px: Vec<Rgb565> = s.frame(0, 0, bg).unwrap().take(20_000).collect();
    assert_eq!(px.len(), (SPRITE_W * SPRITE_H) as usize);
    let want = palette_color(3);
    assert_eq!(
        &px[..5],
        [want; 5],
        "the 5 real pixels must use the run's colour"
    );
    assert!(
        px[5..].iter().all(|&c| c == bg),
        "everything past the real runs must be padded with bg"
    );
}

#[test]
fn long_frame_truncates_the_overrun_without_emitting_extra_pixels() {
    // 70 runs of 255 = 17850 pixels of demand, well past the 16384-pixel cap.
    let mut payload = Vec::new();
    for _ in 0..70 {
        payload.push(255u8);
        payload.push(4u8);
    }
    let blob = synthetic_blob(1, 2, &payload);
    let s = Sprites::parse(&blob).expect("synthetic blob should parse");
    let bg = Rgb565::new(1, 2, 3);
    let px: Vec<Rgb565> = s.frame(1, 2, bg).unwrap().take(20_000).collect();
    assert_eq!(
        px.len(),
        (SPRITE_W * SPRITE_H) as usize,
        "iterator must stop exactly at SPRITE_W*SPRITE_H, not emit the full 17850"
    );
    let want = palette_color(4);
    assert!(
        px.iter().all(|&c| c == want),
        "every emitted pixel should be the run's colour — truncation, not corruption"
    );
}

#[test]
fn zero_length_run_is_skipped_without_hanging() {
    // Two defensive zero-run pairs (each still emits one bg pixel and always
    // advances past its 2 bytes), then one real run of 5 pixels at index 2.
    let payload = [0u8, 9u8, 0u8, 9u8, 5u8, 2u8];
    let blob = synthetic_blob(2, 3, &payload);
    let s = Sprites::parse(&blob).expect("synthetic blob should parse");
    let bg = Rgb565::new(1, 2, 3);
    let px: Vec<Rgb565> = s.frame(2, 3, bg).unwrap().take(20_000).collect();
    assert_eq!(px.len(), (SPRITE_W * SPRITE_H) as usize);
    assert_eq!(px[0], bg, "first zero-run pair must pad with bg, not hang");
    assert_eq!(px[1], bg, "second zero-run pair must also pad with bg");
    let want = palette_color(2);
    assert_eq!(
        &px[2..7],
        [want; 5],
        "the real run must still decode correctly"
    );
    assert!(
        px[7..].iter().all(|&c| c == bg),
        "remainder must be padded with bg (short frame)"
    );
}

#[test]
fn frame_table_entry_with_max_offset_is_rejected() {
    let mut bad = BLOB.to_vec();
    // Offset field of the first frame-table entry (mood0/frame0) sits at 42..46.
    bad[42..46].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Sprites::parse(&bad).is_none());
}

#[test]
fn frame_table_entry_with_max_offset_and_length_is_rejected() {
    let mut bad = BLOB.to_vec();
    bad[42..46].copy_from_slice(&u32::MAX.to_le_bytes());
    bad[46..50].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Sprites::parse(&bad).is_none());
}

#[test]
fn rejects_a_blob_truncated_mid_palette() {
    // Header (10 bytes) present but the 32-byte palette (ending at offset 42)
    // is cut short.
    assert!(Sprites::parse(&BLOB[..20]).is_none());
}

#[test]
fn rejects_a_blob_truncated_mid_frame_table() {
    // Palette complete (ends at offset 42) but the 640-byte frame table
    // (ending at offset 682) is cut short.
    assert!(Sprites::parse(&BLOB[..300]).is_none());
}

#[test]
fn palette_index_beyond_the_palette_falls_back_to_background() {
    // The palette has 16 entries (0..=15); index 200 is out of range.
    let payload = [50u8, 200u8];
    let blob = synthetic_blob(3, 5, &payload);
    let s = Sprites::parse(&blob).expect("synthetic blob should parse");
    let bg = Rgb565::new(1, 2, 3);
    let px: Vec<Rgb565> = s.frame(3, 5, bg).unwrap().take(20_000).collect();
    assert_eq!(px.len(), (SPRITE_W * SPRITE_H) as usize);
    assert!(
        px.iter().all(|&c| c == bg),
        "out-of-range palette index must fall back to bg, never panic"
    );
}

/// Golden test (design doc section 8): pins the decoded artwork so a
/// regenerated blob cannot silently change what the device draws.
///
/// After an INTENTIONAL artwork change these values will fail. Recompute them
/// with the same FNV-1a fold, update them here, and say so in the commit message.
#[test]
fn frame_zero_of_each_mood_matches_its_golden_checksum() {
    const GOLDEN: [u64; 5] = [
        1706513953753925952,
        1805407772405720938,
        8094789397269111184,
        2704131101864094809,
        1844850168112885821,
    ];

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
