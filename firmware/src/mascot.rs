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
    // Checked before Waiting: offline means the session data is stale, so
    // claiming a session is Waiting for you here would be actively misleading.
    // Sleeping is the honest display for "state unknown".
    if ds.offline {
        return Mood::Sleeping;
    }
    if ds
        .sessions
        .iter()
        .any(|s| s.status == SessionStatus::Waiting)
    {
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
