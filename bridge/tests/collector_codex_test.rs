//! Fixture-based tests for the Codex rollout collector (`scan_codex`).
//!
//! Unlike the SQLite-backed OpenCode/Hermes collectors, Codex sessions are
//! JSONL rollout files under `~/.codex/sessions/YYYY/MM/DD/*.jsonl`. The root
//! is resolved by `codex_sessions_root()`, which checks `$CODEX_HOME` before
//! falling back to `dirs::home_dir()/.codex` — the same env-var-first pattern
//! `collector_hermes.rs` uses, and portable (unlike hijacking `$HOME`, which
//! `dirs::home_dir()` ignores on Windows). Each test points `$CODEX_HOME` at a
//! `TempDir` fixture; because environment variables are process-global, every
//! test serialises through `ENV_LOCK`.
//!
//! Rollout schema mirrored from the comment block in
//! `bridge/src/collector_codex.rs`:
//!   {"type":"session_meta","payload":{"id":<uuid>,"cwd":<path>}}
//!   {"type":"event_msg","payload":{"type":"task_started"|"task_complete"|
//!                                    "turn_aborted"|"agent_message"|"user_message"}}

use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::UNIX_EPOCH,
};

use chrono::{Duration, Local, NaiveDate};
use tempfile::TempDir;
use vibe_bridge::{collector::scan_codex, model::Session, state::Store};

// ── env plumbing ──────────────────────────────────────────────────────────────

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Sets `CODEX_HOME` for the duration of a test and restores the previous
/// value on drop. Holds the global lock so parallel tests cannot race on the
/// process environment.
struct CodexHomeGuard {
    old: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

impl CodexHomeGuard {
    fn point_at(dir: &Path) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let old = env::var("CODEX_HOME").ok();
        env::set_var("CODEX_HOME", dir);
        Self { old, _lock: lock }
    }
}

impl Drop for CodexHomeGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(v) => env::set_var("CODEX_HOME", v),
            None => env::remove_var("CODEX_HOME"),
        }
    }
}

// ── fixture helpers ───────────────────────────────────────────────────────────

/// `home` here is the fixture dir pointed at by `$CODEX_HOME` — i.e. already
/// the `.codex`-equivalent root, so unlike the real `~/.codex` layout this
/// does NOT join an extra `.codex` segment.
fn day_dir(home: &Path, d: NaiveDate) -> PathBuf {
    use chrono::Datelike;
    home.join("sessions")
        .join(format!("{:04}", d.year()))
        .join(format!("{:02}", d.month()))
        .join(format!("{:02}", d.day()))
}

/// Write a rollout JSONL file for date `d` with the given session id / cwd /
/// ordered list of event_msg marker types (e.g. `["task_started"]` or
/// `["task_started", "task_complete"]`). Returns the created file path.
fn write_rollout(
    home: &Path,
    d: NaiveDate,
    filename: &str,
    id: &str,
    cwd: &str,
    markers: &[&str],
) -> PathBuf {
    let dir = day_dir(home, d);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(filename);

    let mut lines = Vec::new();
    lines.push(
        serde_json::json!({"type": "session_meta", "payload": {"id": id, "cwd": cwd}}).to_string(),
    );
    for m in markers {
        lines.push(serde_json::json!({"type": "event_msg", "payload": {"type": m}}).to_string());
    }
    fs::write(&path, lines.join("\n")).unwrap();
    path
}

fn find<'a>(snap: &'a [Session], id: &str) -> &'a Session {
    snap.iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("session {id} not found in {snap:?}"))
}

fn mtime_secs(path: &Path) -> f64 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

// ── happy path ────────────────────────────────────────────────────────────────

#[test]
fn happy_path_sessions_with_status_project_and_age() {
    let tmp = TempDir::new().unwrap();
    let today = Local::now().date_naive();

    let waiting_path = write_rollout(
        tmp.path(),
        today,
        "rollout-2026-01-01T00-00-00-11111111-1111-1111-1111-111111111111.jsonl",
        "s-wait",
        "/home/dev/projects/alpha",
        &["task_started", "task_complete"],
    );
    let active_path = write_rollout(
        tmp.path(),
        today,
        "rollout-2026-01-01T00-00-00-22222222-2222-2222-2222-222222222222.jsonl",
        "s-active",
        "C:\\Users\\dev\\repos\\beta\\",
        &["task_started"],
    );

    let _guard = CodexHomeGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_codex(&store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 2);

    let wait = find(&snap, "s-wait");
    assert_eq!(wait.tool, "codex");
    assert_eq!(wait.project, "alpha");
    assert!(wait.waiting, "task_complete => waiting on the user");
    assert!(wait.waiting_since.is_some());
    assert!(!wait.active_turn);
    assert!(
        (wait.last_activity - mtime_secs(&waiting_path)).abs() < 1.0,
        "last_activity should be the rollout file's mtime"
    );

    let active = find(&snap, "s-active");
    // Windows-style directory with trailing backslash still yields the last segment.
    assert_eq!(active.project, "beta");
    assert!(
        active.active_turn,
        "task_started with no later marker => turn active"
    );
    assert!(!active.waiting);
    assert!((active.last_activity - mtime_secs(&active_path)).abs() < 1.0);
}

#[test]
fn turn_aborted_is_neither_waiting_nor_active() {
    let tmp = TempDir::new().unwrap();
    let today = Local::now().date_naive();
    write_rollout(
        tmp.path(),
        today,
        "rollout-aborted.jsonl",
        "s-aborted",
        "/w/gamma",
        &["task_started", "turn_aborted"],
    );

    let _guard = CodexHomeGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_codex(&store);

    let snap = store.snapshot();
    let s = find(&snap, "s-aborted");
    assert!(!s.waiting);
    assert!(!s.active_turn);
}

#[test]
fn version_agnostic_fallback_uses_last_message_kind() {
    // No task_started/task_complete/turn_aborted markers present at all — the
    // collector falls back to who spoke last (agent_message vs user_message).
    let tmp = TempDir::new().unwrap();
    let today = Local::now().date_naive();
    write_rollout(
        tmp.path(),
        today,
        "rollout-fallback-agent.jsonl",
        "s-fallback-agent",
        "/w/fallback-agent",
        &["user_message", "agent_message"],
    );
    write_rollout(
        tmp.path(),
        today,
        "rollout-fallback-user.jsonl",
        "s-fallback-user",
        "/w/fallback-user",
        &["agent_message", "user_message"],
    );

    let _guard = CodexHomeGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_codex(&store);

    let snap = store.snapshot();
    let agent_last = find(&snap, "s-fallback-agent");
    assert!(agent_last.waiting, "agent spoke last => waiting");
    assert!(!agent_last.active_turn);

    let user_last = find(&snap, "s-fallback-user");
    assert!(!user_last.waiting);
    assert!(user_last.active_turn, "user spoke last => active turn");
}

#[test]
fn yesterdays_rollouts_are_also_scanned() {
    let tmp = TempDir::new().unwrap();
    let yesterday = Local::now().date_naive() - Duration::days(1);
    write_rollout(
        tmp.path(),
        yesterday,
        "rollout-yesterday.jsonl",
        "s-yesterday",
        "/w/yesterday",
        &["task_complete"],
    );

    let _guard = CodexHomeGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_codex(&store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].id, "s-yesterday");
}

#[test]
fn rollouts_older_than_yesterday_are_ignored() {
    let tmp = TempDir::new().unwrap();
    let old_day = Local::now().date_naive() - Duration::days(3);
    write_rollout(
        tmp.path(),
        old_day,
        "rollout-old.jsonl",
        "s-old",
        "/w/old",
        &["task_complete"],
    );

    let _guard = CodexHomeGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_codex(&store);

    assert!(
        store.snapshot().is_empty(),
        "a rollout more than one day old must not be scanned"
    );
}

// ── empty / broken data ────────────────────────────────────────────────────────

#[test]
fn missing_codex_root_yields_no_sessions_and_no_panic() {
    // Documenting current behaviour, analogous to a "missing table" for the
    // SQLite collectors: no `sessions` directory under $CODEX_HOME at all —
    // the collector returns immediately without creating anything or panicking.
    let tmp = TempDir::new().unwrap(); // no sessions dir created at all

    let _guard = CodexHomeGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_codex(&store); // must not panic
    assert!(store.snapshot().is_empty());
}

#[test]
fn empty_day_dir_yields_no_sessions_and_no_panic() {
    let tmp = TempDir::new().unwrap();
    let today = Local::now().date_naive();
    fs::create_dir_all(day_dir(tmp.path(), today)).unwrap(); // dir exists, no files

    let _guard = CodexHomeGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_codex(&store);
    assert!(store.snapshot().is_empty());
}

#[test]
fn non_jsonl_files_are_ignored() {
    let tmp = TempDir::new().unwrap();
    let today = Local::now().date_naive();
    let dir = day_dir(tmp.path(), today);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("notes.txt"), b"not a rollout").unwrap();
    fs::write(dir.join("data.json"), b"{}").unwrap();

    let _guard = CodexHomeGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_codex(&store);
    assert!(store.snapshot().is_empty());
}

#[test]
fn malformed_jsonl_content_still_yields_a_session_with_fallback_defaults() {
    // Documenting a real behavioural difference from the SQLite collectors:
    // Codex's file-based scan NEVER swallows the whole result on bad content —
    // it always upserts one Session per *.jsonl file found, falling back to a
    // filename-derived id, project "?", and waiting=false/active=false when the
    // content can't be parsed as rollout JSON. A schema drift here degrades
    // gracefully per-file rather than hiding all sessions like OpenCode/Hermes.
    let tmp = TempDir::new().unwrap();
    let today = Local::now().date_naive();
    let dir = day_dir(tmp.path(), today);
    fs::create_dir_all(&dir).unwrap();
    let uuid = "33333333-3333-3333-3333-333333333333";
    let path = dir.join(format!("rollout-2026-01-01T00-00-00-{uuid}.jsonl"));
    fs::write(&path, b"not json at all\n{{{ broken\n").unwrap();

    let _guard = CodexHomeGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_codex(&store); // must not panic

    let snap = store.snapshot();
    assert_eq!(snap.len(), 1);
    assert_eq!(
        snap[0].id, uuid,
        "id falls back to the uuid extracted from the filename stem"
    );
    assert_eq!(
        snap[0].project, "?",
        "project falls back to \"?\" with no session_meta"
    );
    assert!(!snap[0].waiting);
    assert!(!snap[0].active_turn);
}

#[test]
fn corrupt_binary_content_is_tolerated_via_lossy_decode() {
    let tmp = TempDir::new().unwrap();
    let today = Local::now().date_naive();
    let dir = day_dir(tmp.path(), today);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-binary-garbage.jsonl");
    fs::write(&path, [0xFF, 0xFE, 0x00, 0x01, 0x02, b'\n', 0xC0, 0xC1]).unwrap();

    let _guard = CodexHomeGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_codex(&store); // must not panic

    let snap = store.snapshot();
    assert_eq!(
        snap.len(),
        1,
        "even binary garbage still yields one fallback session"
    );
}

#[test]
fn repeated_scan_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let today = Local::now().date_naive();
    write_rollout(
        tmp.path(),
        today,
        "rollout-dup.jsonl",
        "s-dup",
        "/w/dup",
        &["task_complete"],
    );

    let _guard = CodexHomeGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_codex(&store);
    scan_codex(&store);
    scan_codex(&store);

    assert_eq!(store.snapshot().len(), 1);
}
