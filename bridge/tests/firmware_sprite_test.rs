//! Host tests for the pixel-art sprite blob decoder.
//!
//! `firmware/src/sprite.rs` depends only on embedded-graphics (which builds on
//! host), so it compiles into the bridge test binary via `#[path]`.

#[path = "../../firmware/src/sprite.rs"]
mod sprite;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::{RawData, RgbColor};
use sprite::{Sprites, BLOB, FRAMES, SPRITE_H, SPRITE_W};

const BG: Rgb565 = Rgb565::BLACK;

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
