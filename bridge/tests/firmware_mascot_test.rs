//! Host tests for the firmware's activity -> animation mapping.
//!
//! `firmware/src/mascot.rs` is std-only (no esp-idf / embedded-graphics deps)
//! so it compiles straight into the bridge's test binary via `#[path]`, the
//! same trick `firmware_proto_test.rs` uses for `proto.rs`.

#[path = "../../firmware/src/mascot.rs"]
mod mascot;
// This binary only exercises DisplayState/SessionRow/SessionStatus; the rest of
// proto.rs's public API is unused here but exercised by firmware_proto_test.rs,
// which #[path]-includes the same file into its own compilation unit.
#[allow(dead_code)]
#[path = "../../firmware/src/proto.rs"]
mod proto;

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
    DisplayState {
        sessions: rows,
        ..Default::default()
    }
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

/// Offline outranks Waiting: when the bridge is unreachable the session data is
/// stale, so claiming a session is waiting for you would be actively
/// misleading. Sleeping is the honest display for "state unknown".
#[test]
fn offline_outranks_waiting() {
    let mut ds = state(vec![row(SessionStatus::Waiting, 10)]);
    ds.offline = true;
    assert_eq!(mood_for(&ds), Mood::Sleeping);
}

#[test]
fn all_idle_is_sleeping() {
    let ds = state(vec![
        row(SessionStatus::Idle, 900),
        row(SessionStatus::Idle, 30),
    ]);
    assert_eq!(mood_for(&ds), Mood::Sleeping);
}

#[test]
fn single_recent_working_is_typing() {
    assert_eq!(
        mood_for(&state(vec![row(SessionStatus::Working, 119)])),
        Mood::Typing
    );
}

#[test]
fn building_boundary_is_strictly_greater_than_threshold() {
    // exactly at the threshold is still Typing; one second past it is Building
    assert_eq!(
        mood_for(&state(vec![row(SessionStatus::Working, 120)])),
        Mood::Typing
    );
    assert_eq!(
        mood_for(&state(vec![row(SessionStatus::Working, 121)])),
        Mood::Building
    );
}

#[test]
fn unknown_age_falls_through_to_typing() {
    assert_eq!(
        mood_for(&state(vec![row(SessionStatus::Working, -1)])),
        Mood::Typing
    );
}

#[test]
fn two_working_is_juggling() {
    let ds = state(vec![
        row(SessionStatus::Working, 5),
        row(SessionStatus::Working, 900),
    ]);
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
#[allow(clippy::assertions_on_constants)]
fn building_threshold_stays_above_bridge_working_sec() {
    const BRIDGE_WORKING_SEC: i32 = 60;
    assert!(ART_BUILDING_SEC > BRIDGE_WORKING_SEC);
}

#[test]
fn mood_index_is_stable_and_unique() {
    let all = [
        Mood::Happy,
        Mood::Juggling,
        Mood::Building,
        Mood::Typing,
        Mood::Sleeping,
    ];
    let idx: Vec<usize> = all.iter().map(|m| mood_index(*m)).collect();
    assert_eq!(idx, vec![0, 1, 2, 3, 4]);
}

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
    assert_eq!(
        snap_art(300, 0 /* never */),
        300,
        "sleep=Never keeps the value"
    );
    // Construct the no-room case directly: sleep_min so small every art value loses.
    // With SLEEP_VALS the smallest non-never sleep is 1m, and 30s fits, so the
    // only way to reach Off is an unknown/garbage art value below 30s.
    assert_eq!(
        snap_art(29, 1),
        0,
        "a value smaller than every valid option snaps Off"
    );
}

#[test]
fn snap_normalises_values_that_are_not_in_the_table() {
    assert_eq!(snap_art(45, 0), 30, "45s is not offered; snap down to 30s");
    assert_eq!(snap_art(9999, 0), 300, "clamp to the largest offered value");
}
