//! Fixture-based tests for the OpenCode SQLite collector (`scan_opencode`).
//!
//! The collector resolves its data root via `$OPENCODE_DATA_DIR` (first existing
//! path wins), so each test builds a real SQLite fixture (`opencode.db`) inside a
//! `TempDir` with rusqlite and points the env var at it. Because environment
//! variables are process-global, every test serialises through `ENV_LOCK`.
//!
//! Schema mirrored from the comment block in `bridge/src/collector_opencode.rs`:
//!   session(id, directory, time_created(ms), time_updated(ms), time_archived)
//!   message(id, session_id, time_created(ms), data JSON)

use std::{
    env,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;
use tempfile::TempDir;
use vibe_bridge::{collector_opencode::scan_opencode, model::Session, state::Store};

// ── env plumbing ──────────────────────────────────────────────────────────────

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Sets `OPENCODE_DATA_DIR` for the duration of a test and restores the previous
/// value on drop. Holds the global lock so parallel tests cannot race on the
/// process environment.
struct DataDirGuard {
    old: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

impl DataDirGuard {
    fn point_at(dir: &Path) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let old = env::var("OPENCODE_DATA_DIR").ok();
        env::set_var("OPENCODE_DATA_DIR", dir);
        Self { old, _lock: lock }
    }
}

impl Drop for DataDirGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(v) => env::set_var("OPENCODE_DATA_DIR", v),
            None => env::remove_var("OPENCODE_DATA_DIR"),
        }
    }
}

// ── fixture helpers ───────────────────────────────────────────────────────────

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

/// Create `opencode.db` in `root` with the real (relevant) schema.
fn create_db(root: &Path) -> Connection {
    let con = Connection::open(root.join("opencode.db")).unwrap();
    con.execute_batch(
        "CREATE TABLE session (
            id TEXT PRIMARY KEY,
            project_id TEXT,
            parent_id TEXT,
            slug TEXT,
            directory TEXT,
            title TEXT,
            version TEXT,
            time_created INTEGER,
            time_updated INTEGER,
            time_compacting INTEGER,
            time_archived INTEGER
         );
         CREATE TABLE message (
            id TEXT PRIMARY KEY,
            session_id TEXT,
            time_created INTEGER,
            time_updated INTEGER,
            data TEXT
         );",
    )
    .unwrap();
    con
}

fn insert_session(
    con: &Connection,
    id: &str,
    directory: &str,
    created_ms: i64,
    updated_ms: i64,
    archived_ms: Option<i64>,
) {
    con.execute(
        "INSERT INTO session (id, directory, time_created, time_updated, time_archived) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, directory, created_ms, updated_ms, archived_ms],
    )
    .unwrap();
}

fn insert_message(con: &Connection, id: &str, session_id: &str, created_ms: i64, data: &str) {
    con.execute(
        "INSERT INTO message (id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![id, session_id, created_ms, data],
    )
    .unwrap();
}

fn find<'a>(snap: &'a [Session], id: &str) -> &'a Session {
    snap.iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("session {id} not found in {snap:?}"))
}

fn ms(secs: f64) -> i64 {
    (secs * 1000.0) as i64
}

// ── happy path ────────────────────────────────────────────────────────────────

#[test]
fn happy_path_sessions_with_status_project_and_age() {
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        // s-user: user spoke last => model owes a reply (active_turn), not waiting.
        insert_session(
            &con,
            "s-user",
            "/home/dev/projects/alpha",
            ms(now - 7200.0),
            ms(now - 3600.0),
            None,
        );
        insert_message(
            &con,
            "m1",
            "s-user",
            ms(now - 3700.0),
            &serde_json::json!({"role": "assistant",
                "time": {"created": ms(now - 3700.0), "completed": ms(now - 3650.0)}})
            .to_string(),
        );
        insert_message(
            &con,
            "m2",
            "s-user",
            ms(now - 3600.0),
            &serde_json::json!({"role": "user", "time": {"created": ms(now - 3600.0)}}).to_string(),
        );
        // s-wait: assistant finished last => waiting on the user.
        insert_session(
            &con,
            "s-wait",
            "C:\\Users\\dev\\repos\\beta\\",
            ms(now - 900.0),
            ms(now - 600.0),
            None,
        );
        insert_message(
            &con,
            "m3",
            "s-wait",
            ms(now - 600.0),
            &serde_json::json!({"role": "assistant",
                "time": {"created": ms(now - 610.0), "completed": ms(now - 600.0)}})
            .to_string(),
        );
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 2);

    let user = find(&snap, "s-user");
    assert_eq!(user.tool, "opencode");
    assert_eq!(user.project, "alpha");
    assert!(user.active_turn, "user spoke last => turn in progress");
    assert!(!user.waiting);
    assert!(
        (user.last_activity - (now - 3600.0)).abs() < 1.0,
        "last_activity should come from time_updated (ms -> s)"
    );

    let wait = find(&snap, "s-wait");
    // Windows-style directory with trailing backslash still yields the last segment.
    assert_eq!(wait.project, "beta");
    assert!(wait.waiting, "assistant finished last => waiting on user");
    assert!(wait.waiting_since.is_some());
    assert!(!wait.active_turn);
    assert!((wait.last_activity - (now - 600.0)).abs() < 1.0);
}

#[test]
fn assistant_still_generating_is_active_not_waiting() {
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        insert_session(
            &con,
            "s-gen",
            "/w/gamma",
            ms(now - 100.0),
            ms(now - 10.0),
            None,
        );
        // time.created present, time.completed absent => still generating.
        insert_message(
            &con,
            "m1",
            "s-gen",
            ms(now - 10.0),
            &serde_json::json!({"role": "assistant", "time": {"created": ms(now - 10.0)}})
                .to_string(),
        );
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store);

    let snap = store.snapshot();
    let s = find(&snap, "s-gen");
    assert!(s.active_turn, "in-flight assistant message => active turn");
    assert!(!s.waiting);
}

#[test]
fn session_without_messages_is_neither_active_nor_waiting() {
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        insert_session(
            &con,
            "s-empty",
            "/w/delta",
            ms(now - 50.0),
            ms(now - 50.0),
            None,
        );
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store);

    let snap = store.snapshot();
    let s = find(&snap, "s-empty");
    assert!(!s.active_turn);
    assert!(!s.waiting);
}

#[test]
fn archived_sessions_are_skipped() {
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        insert_session(
            &con,
            "s-live",
            "/w/live",
            ms(now - 60.0),
            ms(now - 60.0),
            None,
        );
        insert_session(
            &con,
            "s-archived",
            "/w/old",
            ms(now - 60.0),
            ms(now - 60.0),
            Some(ms(now - 30.0)),
        );
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].id, "s-live");
}

// ── timestamp handling ────────────────────────────────────────────────────────

#[test]
fn seconds_scale_timestamps_are_tolerated() {
    // ms_to_epoch treats values < 1e12 as already-in-seconds.
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        insert_session(
            &con,
            "s-sec",
            "/w/sec",
            (now - 500.0) as i64,
            (now - 100.0) as i64,
            None,
        );
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store);

    let snap = store.snapshot();
    let s = find(&snap, "s-sec");
    assert!((s.last_activity - (now - 100.0)).abs() < 2.0);
}

#[test]
fn future_timestamps_are_clamped_to_now() {
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        // one hour in the future — must be clamped, not reported as negative age.
        insert_session(
            &con,
            "s-future",
            "/w/fut",
            ms(now + 3600.0),
            ms(now + 3600.0),
            None,
        );
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store);

    let snap = store.snapshot();
    let s = find(&snap, "s-future");
    assert!(
        s.last_activity <= now_secs() + 0.001,
        "future time_updated must be clamped to now (got {})",
        s.last_activity
    );
}

#[test]
fn sessions_with_empty_id_or_no_timestamps_are_silently_dropped() {
    // Documenting: a row with an empty id, or with 0/NULL timestamps, is skipped
    // without any log — such sessions are simply invisible.
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        insert_session(&con, "", "/w/noid", ms(now - 60.0), ms(now - 60.0), None);
        insert_session(&con, "s-no-ts", "/w/nots", 0, 0, None);
        insert_session(&con, "s-ok", "/w/ok", ms(now - 60.0), ms(now - 60.0), None);
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].id, "s-ok");
}

// ── empty / broken databases ──────────────────────────────────────────────────

#[test]
fn empty_db_yields_no_sessions_and_no_panic() {
    let tmp = TempDir::new().unwrap();
    {
        let _con = create_db(tmp.path()); // schema only, zero rows
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store);

    assert!(store.snapshot().is_empty());
}

#[test]
fn missing_session_table_swallows_error_and_yields_nothing() {
    // Documenting current behaviour: if the `session` table is missing (schema
    // drift in a future OpenCode release), `prepare()` fails and the collector
    // hits `Err(_) => return` — all sessions silently vanish, no panic, no log.
    let tmp = TempDir::new().unwrap();
    {
        let con = Connection::open(tmp.path().join("opencode.db")).unwrap();
        con.execute_batch("CREATE TABLE something_else (id TEXT);")
            .unwrap();
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store); // must not panic
    assert!(store.snapshot().is_empty());
}

#[test]
fn renamed_column_swallows_error_and_yields_nothing() {
    // Documenting current behaviour: a divergent schema (here: `time_archived`
    // renamed) makes the SELECT fail; the error is swallowed and every OpenCode
    // session disappears from the dashboard without any diagnostic.
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = Connection::open(tmp.path().join("opencode.db")).unwrap();
        con.execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                directory TEXT,
                time_created INTEGER,
                time_updated INTEGER,
                archived_at INTEGER  -- was: time_archived
             );",
        )
        .unwrap();
        con.execute(
            "INSERT INTO session (id, directory, time_created, time_updated) \
             VALUES ('s1', '/w/p', ?1, ?1)",
            [ms(now - 60.0)],
        )
        .unwrap();
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store); // must not panic
    assert!(
        store.snapshot().is_empty(),
        "schema drift silently hides all sessions (current, documented behaviour)"
    );
}

#[test]
fn missing_message_table_keeps_sessions_but_loses_waiting_inference() {
    // Documenting: if only the `message` table is missing, sessions still appear
    // (the per-session message query fails per row and returns None) but
    // waiting/active inference silently degrades to false.
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = Connection::open(tmp.path().join("opencode.db")).unwrap();
        con.execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                directory TEXT,
                time_created INTEGER,
                time_updated INTEGER,
                time_archived INTEGER
             );",
        )
        .unwrap();
        con.execute(
            "INSERT INTO session (id, directory, time_created, time_updated) \
             VALUES ('s1', '/w/p', ?1, ?1)",
            [ms(now - 60.0)],
        )
        .unwrap();
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 1);
    assert!(!snap[0].waiting);
    assert!(!snap[0].active_turn);
}

#[test]
fn corrupt_db_file_is_ignored_without_panic() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("opencode.db"),
        b"this is definitely not sqlite",
    )
    .unwrap();

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store); // must not panic
    assert!(store.snapshot().is_empty());
}

#[test]
fn missing_db_file_is_ignored_without_panic() {
    let tmp = TempDir::new().unwrap(); // data root exists, opencode.db does not

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store); // must not panic
    assert!(store.snapshot().is_empty());
}

#[test]
fn repeated_scan_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        insert_session(
            &con,
            "s-dup",
            "/w/dup",
            ms(now - 60.0),
            ms(now - 60.0),
            None,
        );
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_opencode(&store);
    scan_opencode(&store);
    scan_opencode(&store);

    assert_eq!(store.snapshot().len(), 1);
}
