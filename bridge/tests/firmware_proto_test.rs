//! Host tests for the CYD firmware's `/state` mini-JSON parser and pure
//! display-formatting helpers.
//!
//! `firmware/src/proto.rs` is deliberately std-only (no esp-idf / embedded
//! dependencies — see its module doc comment) so it can be compiled directly
//! into the bridge's test binary via `#[path]` and exercised with `cargo test`
//! from a plain stable toolchain, without an ESP32 toolchain anywhere in
//! sight. The wire format it parses is the compact "mini" JSON the bridge's
//! `make_mini` (bridge/src/bin/serial_bridge.rs) derives from the full
//! `/state` response documented in docs/api.md — short single/double-letter
//! keys to fit inside the ESP32 UART FIFO.

#[path = "../../firmware/src/proto.rs"]
mod proto;

use proto::{
    extract_str, fmt_long, fmt_reset, fmt_tokens, fmt_usd, humanize_age, humanize_dur, num_field,
    parse_metrics, parse_provider, parse_state, trunc_bytes, wrap_lines, SessionStatus, MAX_MODELS,
    MAX_SESSIONS,
};

// ── parse_state: happy path ──────────────────────────────────────────────────

#[test]
fn parses_a_full_mini_state_payload() {
    // Shape mirrors make_mini's output for the docs/api.md example: two
    // sessions (one plain, one waiting-with-summary), both providers, and a
    // metrics block with two model rows.
    let text = r#"{"sessions":[{"project":"cydrust","status":"working","tool":"claude","i":"abc123def456","a":12},{"project":"myapp","status":"waiting","tool":"codex","i":"fed987cba654","a":305,"s":"waiting on review","ws":42}],"claude":{"ok":true,"p":0.42,"r":7200,"wp":0.18,"wr":432000,"b":0.031,"lo":0.58,"e":"14:30"},"codex":{"ok":false},"metrics":{"m":[{"p":"claude","n":"opus","t":152700000.0,"u":12.34},{"p":"codex","n":"gpt-4","t":84000.0}],"tt":236700000.0,"tu":12.34,"ts":5,"uc":true}}"#;

    let ds = parse_state(text).expect("parse_state never returns None");

    assert_eq!(ds.sessions.len(), 2);
    assert_eq!(ds.dropped_sessions, 0);

    let s0 = &ds.sessions[0];
    assert_eq!(s0.project, "cydrust");
    assert_eq!(s0.status, SessionStatus::Working);
    assert_eq!(s0.tool, "claude");
    assert_eq!(s0.id, "abc123def456");
    assert_eq!(s0.age_sec, 12);
    assert_eq!(s0.wait_sec, -1); // "ws" absent -> sentinel
    assert_eq!(s0.summary, "");

    let s1 = &ds.sessions[1];
    assert_eq!(s1.project, "myapp");
    assert_eq!(s1.status, SessionStatus::Waiting);
    assert_eq!(s1.tool, "codex");
    assert_eq!(s1.id, "fed987cba654");
    assert_eq!(s1.age_sec, 305);
    assert_eq!(s1.wait_sec, 42);
    assert_eq!(s1.summary, "waiting on review");

    assert!(ds.claude.ok);
    assert!((ds.claude.pct - 0.42).abs() < f32::EPSILON);
    assert_eq!(ds.claude.reset_sec, 7200);
    assert!((ds.claude.week_pct - 0.18).abs() < f32::EPSILON);
    assert_eq!(ds.claude.week_reset_sec, 432000);
    assert!(!ds.claude.will_exhaust);
    assert!((ds.claude.burn_per_hr - 0.031).abs() < f32::EPSILON);
    assert!((ds.claude.leftover_pct - 0.58).abs() < f32::EPSILON);
    assert_eq!(ds.claude.eta_clock, "14:30");

    // codex reported ok:false -> every other field stays at its sentinel
    // default (parse_provider short-circuits once `ok` is false).
    assert!(!ds.codex.ok);
    assert_eq!(ds.codex.pct, 0.0);
    assert_eq!(ds.codex.week_pct, -1.0);
    assert_eq!(ds.codex.week_reset_sec, -1);

    assert_eq!(ds.metrics.models.len(), 2);
    assert_eq!(ds.metrics.dropped_models, 0);
    assert_eq!(ds.metrics.models[0].provider, "claude");
    assert_eq!(ds.metrics.models[0].model, "opus");
    assert_eq!(ds.metrics.models[0].tokens, 152_700_000.0);
    assert!(ds.metrics.models[0].has_usd);
    assert_eq!(ds.metrics.models[0].usd, 12.34);
    assert_eq!(ds.metrics.models[1].provider, "codex");
    assert!(!ds.metrics.models[1].has_usd);
    assert_eq!(ds.metrics.total_tokens, 236_700_000.0);
    assert_eq!(ds.metrics.total_usd, 12.34);
    assert!(ds.metrics.has_usd);
    assert!(ds.metrics.usd_complete);
    assert_eq!(ds.metrics.total_sessions, 5);
}

#[test]
fn parses_full_state_response_shape_from_docs_api_md() {
    // The human-facing /state response documented in docs/api.md uses long
    // field names (ageSec, resetSec, ...) rather than the mini wire format's
    // short keys — the firmware parser only understands the mini format. The
    // scanner also requires its literal markers (`"sessions":[`, `"claude":{`)
    // with no intervening whitespace, so this is written compact (matching
    // what a real serde_json::to_string payload looks like on the wire, since
    // docs/api.md's own pretty-printed example — with spaces after every
    // colon — would not match at all, i.e. `parse_state` would see no
    // sessions and no usage whatsoever). project/status/tool still match
    // (same key names in both shapes), but the numeric mini keys ("a", "p",
    // "r", ...) never collide with the long-form names (this is the exact
    // scenario the boundary-prefixed `num_field` needle guards against — see
    // its doc comment) and so fall back to sentinels.
    let text = r#"{"ts":1750000000,"sessions":[{"id":"abc123def456","tool":"claude","project":"cydrust","status":"working","ageSec":12,"waiting":false,"waitingSec":null}],"usage":{"claude":{"ok":true,"pct":0.42,"resetSec":7200},"codex":{"ok":false}},"capacity":{"verdict":"go"},"staleSec":1}"#;

    let ds = parse_state(text).unwrap();
    assert_eq!(ds.sessions.len(), 1);
    assert_eq!(ds.sessions[0].project, "cydrust");
    assert_eq!(ds.sessions[0].status, SessionStatus::Working);
    // "ageSec" isn't the mini "a" key -> age stays at the unknown sentinel.
    assert_eq!(ds.sessions[0].age_sec, -1);
    // claude's "ok":true is found, but "pct"/"resetSec" don't match the mini
    // "p"/"r" needles, so those fields stay at their zero defaults.
    assert!(ds.claude.ok);
    assert_eq!(ds.claude.pct, 0.0);
    assert_eq!(ds.claude.reset_sec, 0);
}

#[test]
fn docs_api_md_pretty_printed_example_does_not_match_at_all() {
    // Sibling to the test above: prove the whitespace sensitivity explicitly.
    // A pretty-printed /state response (spaces after colons, newlines — the
    // exact style docs/api.md uses for readability) never matches the mini
    // scanner's literal markers, so everything falls back to defaults. This
    // is expected: production always sends the compact `make_mini` output,
    // never this human-readable shape, over the wire.
    let text = r#"{
      "sessions": [
        {"id": "abc123def456", "tool": "claude", "project": "cydrust", "status": "working"}
      ],
      "usage": { "claude": {"ok": true}, "codex": {"ok": false} }
    }"#;
    let ds = parse_state(text).unwrap();
    assert!(ds.sessions.is_empty());
    assert!(!ds.claude.ok);
    assert!(!ds.codex.ok);
}

// ── parse_state: edge cases ──────────────────────────────────────────────────

#[test]
fn empty_sessions_array_yields_no_sessions_and_no_panic() {
    let ds = parse_state(r#"{"sessions":[],"claude":{"ok":false},"codex":{"ok":false}}"#).unwrap();
    assert!(ds.sessions.is_empty());
    assert_eq!(ds.dropped_sessions, 0);
}

#[test]
fn missing_sessions_key_yields_defaults() {
    let ds = parse_state(r#"{"claude":{"ok":true,"p":0.5}}"#).unwrap();
    assert!(ds.sessions.is_empty());
    assert!(ds.claude.ok);
    assert!(!ds.codex.ok); // "codex" marker entirely absent -> default
}

#[test]
fn missing_optional_session_fields_fall_back_to_sentinels() {
    // Only "project" is present; status/tool/i/a/ws/s are all absent.
    let ds = parse_state(r#"{"sessions":[{"project":"solo"}]}"#).unwrap();
    let row = &ds.sessions[0];
    assert_eq!(row.project, "solo");
    assert_eq!(row.status, SessionStatus::Idle); // default when "status" missing
    assert_eq!(row.tool, "claude"); // default when "tool" missing
    assert_eq!(row.id, "");
    assert_eq!(row.age_sec, -1);
    assert_eq!(row.wait_sec, -1);
    assert_eq!(row.summary, "");
}

#[test]
fn garbage_input_never_panics_and_yields_defaults() {
    for junk in [
        "",
        "not json at all",
        "{",
        "{{{{",
        r#"{"sessions":[{"project":"unterminated"#,
        "\u{0}\u{0}\u{0}",
        "🦀🦀🦀 not json 🦀🦀🦀",
    ] {
        let ds = parse_state(junk).expect("parse_state always returns Some(_), even on garbage");
        assert!(ds.sessions.is_empty());
        assert!(!ds.claude.ok);
        assert!(!ds.codex.ok);
    }
}

#[test]
fn escaped_quote_in_a_string_field_truncates_at_the_escape() {
    // proto's scanner is deliberately NOT a full JSON implementation (see its
    // module doc comment): it has no escape handling, so a `\"` inside a
    // string value is read as the string's closing quote. This test locks in
    // that documented, non-panicking (if display-imperfect) behaviour rather
    // than asserting full JSON compliance.
    let text =
        r#"{"sessions":[{"project":"p","status":"idle","tool":"claude","s":"he said \"hi\""}]}"#;
    let ds = parse_state(text).unwrap();
    assert_eq!(ds.sessions[0].summary, "he said \\");
}

#[test]
fn escaped_backslash_is_also_read_literally() {
    let text = r#"{"sessions":[{"project":"p","status":"idle","tool":"claude","s":"a\\b"}]}"#;
    let ds = parse_state(text).unwrap();
    // The literal bytes between the quotes are `a\\b`; the scanner stops at
    // the first raw `"` byte, so nothing here is unescaped — the whole
    // (backslash-doubled) literal comes through untouched.
    assert_eq!(ds.sessions[0].summary, "a\\\\b");
}

// ── Session-list overflow (MAX_SESSIONS) ────────────────────────────────────

fn mini_session(id: usize) -> String {
    format!(r#"{{"project":"p{id}","status":"idle","tool":"claude"}}"#)
}

#[test]
fn sessions_up_to_capacity_are_all_kept() {
    let sessions: Vec<String> = (0..MAX_SESSIONS).map(mini_session).collect();
    let text = format!(r#"{{"sessions":[{}]}}"#, sessions.join(","));
    let ds = parse_state(&text).unwrap();
    assert_eq!(ds.sessions.len(), MAX_SESSIONS);
    assert_eq!(ds.dropped_sessions, 0);
}

#[test]
fn sessions_past_capacity_are_counted_not_silently_dropped() {
    let extra = 3;
    let sessions: Vec<String> = (0..MAX_SESSIONS + extra).map(mini_session).collect();
    let text = format!(r#"{{"sessions":[{}]}}"#, sessions.join(","));
    let ds = parse_state(&text).unwrap();
    assert_eq!(ds.sessions.len(), MAX_SESSIONS);
    assert_eq!(ds.dropped_sessions, extra);
    // The kept sessions are the first MAX_SESSIONS in payload order.
    assert_eq!(ds.sessions[0].project, "p0");
    assert_eq!(
        ds.sessions[MAX_SESSIONS - 1].project,
        format!("p{}", MAX_SESSIONS - 1)
    );
}

// ── Metrics model-row overflow (MAX_MODELS) ─────────────────────────────────

fn mini_model(i: usize) -> String {
    format!(r#"{{"p":"claude","n":"m{i}","t":{}.0}}"#, 100 - i)
}

#[test]
fn models_past_capacity_are_counted_not_silently_dropped() {
    let extra = 2;
    let models: Vec<String> = (0..MAX_MODELS + extra).map(mini_model).collect();
    let text = format!(r#"{{"metrics":{{"m":[{}]}}}}"#, models.join(","));
    let m = parse_metrics(&text);
    assert_eq!(m.models.len(), MAX_MODELS);
    assert_eq!(m.dropped_models, extra);
}

#[test]
fn metrics_missing_entirely_yields_defaults() {
    let m = parse_metrics(r#"{"sessions":[]}"#);
    assert!(m.models.is_empty());
    assert_eq!(m.dropped_models, 0);
    assert_eq!(m.total_tokens, 0.0);
    assert!(!m.has_usd);
    assert!(!m.usd_complete);
}

// ── num_field: boundary-prefixed needle (short key vs. longer key) ─────────

#[test]
fn num_field_does_not_confuse_short_key_with_longer_suffix_match() {
    // "p" must not match inside "wp", and must find the right value even
    // when a longer key sharing the same prefix comes first.
    let obj = r#"{"wp":0.18,"p":0.42}"#;
    assert_eq!(num_field(obj, "p"), Some(0.42));
    assert_eq!(num_field(obj, "wp"), Some(0.18));

    let obj2 = r#"{"p":0.5}"#;
    assert_eq!(num_field(obj2, "wp"), None);
}

#[test]
fn num_field_missing_key_is_none() {
    assert_eq!(num_field(r#"{"a":1}"#, "b"), None);
}

#[test]
fn num_field_reads_across_the_comma_or_closing_brace() {
    assert_eq!(num_field(r#"{"a":1,"b":2}"#, "a"), Some(1.0));
    assert_eq!(num_field(r#"{"a":1,"b":2}"#, "b"), Some(2.0));
    assert_eq!(num_field(r#"{"a":-3.5}"#, "a"), Some(-3.5));
}

// ── parse_provider ────────────────────────────────────────────────────────────

#[test]
fn provider_not_ok_short_circuits_to_defaults() {
    let u = parse_provider(r#"{"ok":false,"p":0.9}"#, "\"x\":{");
    // marker not even found -> default
    assert!(!u.ok);
    assert_eq!(u.pct, 0.0);
}

#[test]
fn provider_ok_false_ignores_other_fields_even_when_present() {
    let text = r#""claude":{"ok":false,"p":0.9,"r":100}"#;
    let u = parse_provider(text, "\"claude\":{");
    assert!(!u.ok);
    // Real bridge output never sends extra fields when ok:false, but the
    // parser should still ignore them defensively rather than trust them.
    assert_eq!(u.pct, 0.0);
    assert_eq!(u.reset_sec, 0);
}

#[test]
fn provider_marker_absent_yields_default() {
    let u = parse_provider(r#"{"nope":true}"#, "\"claude\":{");
    assert!(!u.ok);
    assert_eq!(u.week_pct, -1.0);
    assert_eq!(u.week_reset_sec, -1);
}

// ── extract_str / trunc_bytes ─────────────────────────────────────────────────

#[test]
fn extract_str_finds_and_bounds_the_value() {
    assert_eq!(extract_str(r#"{"k":"v"}"#, "k"), Some("v"));
    assert_eq!(extract_str(r#"{"k":"v","j":"w"}"#, "j"), Some("w"));
    assert_eq!(extract_str(r#"{"k":"v"}"#, "missing"), None);
}

#[test]
fn trunc_bytes_never_splits_a_multibyte_char() {
    // "é" is 2 bytes in UTF-8; cutting at byte 1 would split it.
    let s = "aé"; // 'a' (1 byte) + 'é' (2 bytes) = 3 bytes total
    assert_eq!(trunc_bytes(s, 2), "a"); // backs off to the char boundary at 1
    assert_eq!(trunc_bytes(s, 3), "aé"); // exactly fits
    assert_eq!(trunc_bytes(s, 10), "aé"); // shorter than cap -> unchanged
    assert_eq!(trunc_bytes("", 5), "");
}

#[test]
fn trunc_bytes_handles_emoji_boundaries() {
    let s = "🦀abc"; // crab emoji is 4 bytes
    for n in 0..4 {
        // Cutting anywhere inside the 4-byte emoji must back off to 0, not panic.
        assert_eq!(trunc_bytes(s, n), "");
    }
    assert_eq!(trunc_bytes(s, 4), "🦀");
}

// ── Display-formatting helpers ────────────────────────────────────────────────

#[test]
fn fmt_tokens_scales_by_magnitude() {
    assert_eq!(fmt_tokens(0.0), "0");
    assert_eq!(fmt_tokens(512.0), "512");
    assert_eq!(fmt_tokens(84_000.0), "84k");
    assert_eq!(fmt_tokens(152_700_000.0), "152.7M");
}

#[test]
fn fmt_usd_always_shows_two_decimals() {
    assert_eq!(fmt_usd(0.0), "$0.00");
    assert_eq!(fmt_usd(12.3), "$12.30");
    assert_eq!(fmt_usd(12.346), "$12.35"); // rounds
}

#[test]
fn humanize_age_buckets_are_correct_at_the_boundaries() {
    assert_eq!(humanize_age(-1), "");
    assert_eq!(humanize_age(0), "now");
    assert_eq!(humanize_age(4), "now");
    assert_eq!(humanize_age(5), "5s ago");
    assert_eq!(humanize_age(59), "59s ago");
    assert_eq!(humanize_age(60), "1m ago");
    assert_eq!(humanize_age(3599), "59m ago");
    assert_eq!(humanize_age(3600), "1h ago");
    assert_eq!(humanize_age(86399), "23h ago");
    assert_eq!(humanize_age(86400), "1d ago");
}

#[test]
fn humanize_dur_has_no_now_bucket() {
    assert_eq!(humanize_dur(-1), "");
    assert_eq!(humanize_dur(0), "0s");
    assert_eq!(humanize_dur(59), "59s");
    assert_eq!(humanize_dur(60), "1m");
    assert_eq!(humanize_dur(3600), "1h");
}

#[test]
fn fmt_reset_switches_to_hours_at_sixty_minutes() {
    assert_eq!(fmt_reset(0), "0m");
    assert_eq!(fmt_reset(300), "5m");
    assert_eq!(fmt_reset(3540), "59m");
    assert_eq!(fmt_reset(7200), "2h 0m");
    assert_eq!(fmt_reset(7260), "2h 1m");
}

#[test]
fn fmt_long_covers_all_four_buckets() {
    assert_eq!(fmt_long(-1), "--");
    assert_eq!(fmt_long(1800), "30m");
    assert_eq!(fmt_long(3600), "1h 0m");
    assert_eq!(fmt_long(90_000), "1d 1h"); // 25h
}

#[test]
fn wrap_lines_greedily_wraps_on_word_boundaries() {
    let lines = wrap_lines("the quick brown fox jumps", 10, 3);
    assert!(lines.len() <= 3);
    for l in &lines {
        assert!(l.len() <= 10, "line {l:?} exceeds cols");
    }
    // Rejoining (with the spaces the wrapper consumed) should reproduce the
    // original words in order.
    assert_eq!(lines.join(" "), "the quick brown fox jumps");
}

#[test]
fn wrap_lines_caps_at_max_lines() {
    let long = "one two three four five six seven eight nine ten";
    let lines = wrap_lines(long, 6, 2);
    assert_eq!(lines.len(), 2);
}

#[test]
fn wrap_lines_empty_input_yields_no_lines() {
    let lines = wrap_lines("", 10, 3);
    assert!(lines.is_empty());
}
