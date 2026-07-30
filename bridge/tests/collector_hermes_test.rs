//! Fixture-based tests for the Hermes SQLite collector (`scan_hermes`).
//!
//! The collector resolves its data root via `$HERMES_DATA_DIR` (first existing
//! path wins on non-Windows hosts), so each test builds a real SQLite fixture
//! (`state.db`) inside a `TempDir` with rusqlite and points the env var at it.
//! Because environment variables are process-global, every test serialises
//! through `ENV_LOCK`.
//!
//! Schema mirrored from the comment block in `bridge/src/collector_hermes.rs`:
//!   sessions(id, cwd, started_at(REAL sec), ended_at(REAL sec, nullable), archived)
//!   messages(id, session_id, role, timestamp(REAL sec))

use std::{
    env,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;
use tempfile::TempDir;
use vibe_bridge::{collector_hermes::scan_hermes, model::Session, state::Store};

// ── env plumbing ──────────────────────────────────────────────────────────────

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Sets `HERMES_DATA_DIR` for the duration of a test and restores the previous
/// value on drop. Holds the global lock so parallel tests cannot race on the
/// process environment.
struct DataDirGuard {
    old: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

impl DataDirGuard {
    fn point_at(dir: &Path) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let old = env::var("HERMES_DATA_DIR").ok();
        env::set_var("HERMES_DATA_DIR", dir);
        Self { old, _lock: lock }
    }
}

impl Drop for DataDirGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(v) => env::set_var("HERMES_DATA_DIR", v),
            None => env::remove_var("HERMES_DATA_DIR"),
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

/// Create `state.db` in `root` with the real (relevant) schema.
fn create_db(root: &Path) -> Connection {
    let con = Connection::open(root.join("state.db")).unwrap();
    con.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            source TEXT,
            user_id TEXT,
            model TEXT,
            started_at REAL,
            ended_at REAL,
            end_reason TEXT,
            message_count INTEGER,
            cwd TEXT,
            title TEXT,
            archived INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE messages (
            id INTEGER PRIMARY KEY,
            session_id TEXT,
            role TEXT,
            content TEXT,
            timestamp REAL,
            active INTEGER
         );",
    )
    .unwrap();
    con
}

#[allow(clippy::too_many_arguments)]
fn insert_session(
    con: &Connection,
    id: &str,
    cwd: &str,
    started_at: f64,
    ended_at: Option<f64>,
    archived: i64,
) {
    con.execute(
        "INSERT INTO sessions (id, cwd, started_at, ended_at, archived) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, cwd, started_at, ended_at, archived],
    )
    .unwrap();
}

fn insert_message(con: &Connection, session_id: &str, role: &str, timestamp: f64) {
    con.execute(
        "INSERT INTO messages (session_id, role, timestamp) VALUES (?1, ?2, ?3)",
        rusqlite::params![session_id, role, timestamp],
    )
    .unwrap();
}

fn find<'a>(snap: &'a [Session], id: &str) -> &'a Session {
    snap.iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("session {id} not found in {snap:?}"))
}

// ── happy path ────────────────────────────────────────────────────────────────

#[test]
fn happy_path_sessions_with_status_project_and_age() {
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        // s-user: open session (ended_at NULL), user spoke last => model owes a
        // reply (active_turn), not waiting.
        insert_session(
            &con,
            "s-user",
            "/home/dev/projects/alpha",
            now - 7200.0,
            None,
            0,
        );
        insert_message(&con, "s-user", "assistant", now - 3700.0);
        insert_message(&con, "s-user", "user", now - 3600.0);
        // s-wait: assistant spoke last => waiting on the user.
        insert_session(
            &con,
            "s-wait",
            "C:\\Users\\dev\\repos\\beta\\",
            now - 900.0,
            Some(now - 600.0),
            0,
        );
        insert_message(&con, "s-wait", "user", now - 650.0);
        insert_message(&con, "s-wait", "assistant", now - 620.0);
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_hermes(&store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 2);

    let user = find(&snap, "s-user");
    assert_eq!(user.tool, "hermes");
    assert_eq!(user.project, "alpha");
    assert!(
        user.active_turn,
        "user spoke last on an open session => active"
    );
    assert!(!user.waiting);
    assert!(
        (user.last_activity - (now - 3600.0)).abs() < 1.0,
        "last_activity should come from the latest message timestamp"
    );

    let wait = find(&snap, "s-wait");
    // Windows-style directory with trailing backslash still yields the last segment.
    assert_eq!(wait.project, "beta");
    assert!(wait.waiting, "assistant spoke last => waiting on user");
    assert!(wait.waiting_since.is_some());
    assert!(!wait.active_turn);
}

#[test]
fn user_last_message_on_closed_session_is_not_active() {
    // A session that has been closed out (ended_at set) is not "active" even if
    // the last message role happens to be "user" — the turn is over.
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        insert_session(
            &con,
            "s-closed",
            "/w/closed",
            now - 500.0,
            Some(now - 100.0),
            0,
        );
        insert_message(&con, "s-closed", "user", now - 110.0);
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_hermes(&store);

    let snap = store.snapshot();
    let s = find(&snap, "s-closed");
    assert!(!s.active_turn, "ended_at set => turn is not in progress");
}

#[test]
fn session_without_messages_is_neither_active_nor_waiting() {
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        insert_session(&con, "s-empty", "/w/delta", now - 50.0, None, 0);
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_hermes(&store);

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
        insert_session(&con, "s-live", "/w/live", now - 60.0, None, 0);
        insert_session(&con, "s-archived", "/w/old", now - 60.0, None, 1);
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_hermes(&store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].id, "s-live");
}

// ── timestamp handling ────────────────────────────────────────────────────────

#[test]
fn last_activity_prefers_latest_message_timestamp_over_ended_at() {
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        insert_session(&con, "s-msg", "/w/msg", now - 500.0, Some(now - 400.0), 0);
        // a message newer than ended_at should win.
        insert_message(&con, "s-msg", "assistant", now - 50.0);
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_hermes(&store);

    let snap = store.snapshot();
    let s = find(&snap, "s-msg");
    assert!((s.last_activity - (now - 50.0)).abs() < 1.0);
}

#[test]
fn future_timestamps_are_clamped_to_now() {
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        // one hour in the future — must be clamped, not reported as negative age.
        insert_session(&con, "s-future", "/w/fut", now + 3600.0, None, 0);
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_hermes(&store);

    let snap = store.snapshot();
    let s = find(&snap, "s-future");
    assert!(
        s.last_activity <= now_secs() + 0.001,
        "future started_at must be clamped to now (got {})",
        s.last_activity
    );
}

#[test]
fn sessions_with_empty_id_or_no_timestamps_are_silently_dropped() {
    // Documenting: a row with an empty id, or with 0/negative started_at and no
    // ended_at, is skipped without any log — such sessions are simply invisible.
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        insert_session(&con, "", "/w/noid", now - 60.0, None, 0);
        insert_session(&con, "s-no-ts", "/w/nots", 0.0, None, 0);
        insert_session(&con, "s-ok", "/w/ok", now - 60.0, None, 0);
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_hermes(&store);

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
    scan_hermes(&store);

    assert!(store.snapshot().is_empty());
}

#[test]
fn missing_sessions_table_swallows_error_and_yields_nothing() {
    // Documenting current behaviour: if the `sessions` table is missing (schema
    // drift in a future Hermes release), `prepare()` fails and the collector
    // hits `Err(_) => return` — all sessions silently vanish, no panic, no log.
    let tmp = TempDir::new().unwrap();
    {
        let con = Connection::open(tmp.path().join("state.db")).unwrap();
        con.execute_batch("CREATE TABLE something_else (id TEXT);")
            .unwrap();
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_hermes(&store); // must not panic
    assert!(store.snapshot().is_empty());
}

#[test]
fn renamed_column_swallows_error_and_yields_nothing() {
    // Documenting current behaviour: a divergent schema (here: `archived` renamed)
    // makes the SELECT fail; the error is swallowed and every Hermes session
    // disappears from the dashboard without any diagnostic.
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = Connection::open(tmp.path().join("state.db")).unwrap();
        con.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                cwd TEXT,
                started_at REAL,
                ended_at REAL,
                is_archived INTEGER  -- was: archived
             );",
        )
        .unwrap();
        con.execute(
            "INSERT INTO sessions (id, cwd, started_at) VALUES ('s1', '/w/p', ?1)",
            [now - 60.0],
        )
        .unwrap();
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_hermes(&store); // must not panic
    assert!(
        store.snapshot().is_empty(),
        "schema drift silently hides all sessions (current, documented behaviour)"
    );
}

#[test]
fn missing_messages_table_keeps_sessions_but_loses_role_inference() {
    // Documenting: if only the `messages` table is missing, sessions still
    // appear (the per-session message queries fail per row and return None) but
    // waiting/active inference silently degrades to false.
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = Connection::open(tmp.path().join("state.db")).unwrap();
        con.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                cwd TEXT,
                started_at REAL,
                ended_at REAL,
                archived INTEGER NOT NULL DEFAULT 0
             );",
        )
        .unwrap();
        con.execute(
            "INSERT INTO sessions (id, cwd, started_at) VALUES ('s1', '/w/p', ?1)",
            [now - 60.0],
        )
        .unwrap();
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_hermes(&store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 1);
    assert!(!snap[0].waiting);
    assert!(!snap[0].active_turn);
}

#[test]
fn corrupt_db_file_is_ignored_without_panic() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("state.db"),
        b"this is definitely not sqlite",
    )
    .unwrap();

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_hermes(&store); // must not panic
    assert!(store.snapshot().is_empty());
}

#[test]
fn missing_db_file_is_ignored_without_panic() {
    let tmp = TempDir::new().unwrap(); // data root exists, state.db does not

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_hermes(&store); // must not panic
    assert!(store.snapshot().is_empty());
}

#[test]
fn repeated_scan_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let now = now_secs();
    {
        let con = create_db(tmp.path());
        insert_session(&con, "s-dup", "/w/dup", now - 60.0, None, 0);
    }

    let _guard = DataDirGuard::point_at(tmp.path());
    let store = Arc::new(Store::new());
    scan_hermes(&store);
    scan_hermes(&store);
    scan_hermes(&store);

    assert_eq!(store.snapshot().len(), 1);
}
