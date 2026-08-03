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
const TABLE_BYTES: usize = MOODS * FRAMES * 8; // 640
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
        Some(Sprites {
            payload,
            table,
            palette,
        })
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
