use crate::{model::Session, state::Store};
use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags};

// =============================================================================
// Hermes CLI session collector — Rust port.
//
// Verified against a real install at %LOCALAPPDATA%\hermes\state.db (SQLite).
// Real schema (columns this collector reads):
//
//   sessions( id text PK, source text, user_id text, model text, ...,
//             started_at REAL NOT NULL,   -- epoch SECONDS (float)
//             ended_at REAL,              -- epoch SECONDS (float), NULL if open
//             end_reason text, message_count int, ...,
//             cwd text,                   -- working dir
//             title text,
//             archived integer NOT NULL DEFAULT 0 )  -- non-zero => skip
//
//   messages( id int PK, session_id text, role text, content text, ...,
//             timestamp REAL NOT NULL, active int )
//
// last_activity = max(ended_at, started_at, latest message timestamp).
// active_turn / waiting inference (from the latest message role):
//   * active_turn = the user spoke last (model owes a reply) AND the session has
//     not been closed out (ended_at IS NULL).
//   * waiting = the last message role is "assistant" => waiting on the user.
//   The `sessions` table may be EMPTY on this machine — handled gracefully (the
//   scan simply does nothing). Opened strictly read-only + immutable. Never panics.
// =============================================================================

const TOOL: &str = "hermes";

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

// --- path discovery ----------------------------------------------------------

/// Resolve Hermes' data dir.
///
/// Resolution order:
///   1. $HERMES_DATA_DIR / $HERMES_HOME (first existing)
///   2. platform default:
///        Windows: %LOCALAPPDATA%\hermes  (confirmed on this host; APPDATA fallback)
///        else:    $XDG_DATA_HOME/hermes if set, else ~/.local/share/hermes
fn hermes_data_root() -> PathBuf {
    for var in ["HERMES_DATA_DIR", "HERMES_HOME"] {
        if let Ok(v) = env::var(var) {
            if !v.is_empty() {
                let p = PathBuf::from(v);
                if p.exists() {
                    return p;
                }
            }
        }
    }
    if cfg!(windows) {
        let base = env::var("LOCALAPPDATA")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| env::var("APPDATA").ok().filter(|s| !s.is_empty()));
        if let Some(base) = base {
            return PathBuf::from(base).join("hermes");
        }
    }
    if let Ok(xdg) = env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            let p = PathBuf::from(xdg).join("hermes");
            if p.exists() {
                return p;
            }
        }
    }
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("share")
        .join("hermes")
}

fn state_db(root: &Path) -> PathBuf {
    root.join("state.db")
}

// --- helpers -----------------------------------------------------------------

fn project_from_cwd(cwd: Option<&str>) -> String {
    let cwd = match cwd {
        Some(c) => c,
        None => return "?".into(),
    };
    let d = cwd.replace('\\', "/");
    let d = d.trim_end_matches('/');
    match d.rsplit('/').find(|s| !s.is_empty()) {
        Some(seg) => seg.to_string(),
        None => "?".into(),
    }
}

/// Hermes timestamps are already epoch SECONDS (REAL).
fn sec(v: Option<f64>) -> Option<f64> {
    match v {
        Some(n) if n > 0.0 => Some(n),
        _ => None,
    }
}

/// Open the db strictly read-only + immutable via a URI so we never lock/corrupt a
/// live DB. Returns None on any error (never panics).
fn open_ro(db: &Path) -> Option<Connection> {
    let uri = format!(
        "file:{}?immutable=1",
        db.to_string_lossy().replace('\\', "/")
    );
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()
}

/// Role of the most-recent message, lowercased, or None on error/empty.
fn last_role(con: &Connection, session_id: &str) -> Option<String> {
    let role: Option<String> = con
        .query_row(
            "SELECT role FROM messages WHERE session_id = ?1 \
             ORDER BY timestamp DESC, id DESC LIMIT 1",
            [session_id],
            |row| row.get(0),
        )
        .ok()?;
    Some(role.unwrap_or_default().to_lowercase())
}

/// Newest message timestamp for the session, if any.
fn max_message_ts(con: &Connection, session_id: &str) -> Option<f64> {
    let t: Option<f64> = con
        .query_row(
            "SELECT MAX(timestamp) FROM messages WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .ok()?;
    sec(t)
}

// --- public entry point (mirrors scan_claude / scan_codex) -------------------

/// Upsert one `Session` per Hermes session from state.db (read-only).
///
/// Safe on a missing root/db / empty table. Opened immutable so it never contends
/// with a live hermes process. Skips archived sessions. Never panics.
pub fn scan_hermes(store: &Arc<Store>) {
    let root = hermes_data_root();
    if !root.exists() {
        return;
    }
    let db = state_db(&root);
    if !db.exists() {
        return;
    }

    let con = match open_ro(&db) {
        Some(c) => c,
        None => return,
    };

    let now = now_secs();

    let mut stmt = match con.prepare("SELECT id, cwd, started_at, ended_at, archived FROM sessions")
    {
        Ok(s) => s,
        Err(_) => return,
    };

    // (id, cwd, started_at, ended_at, archived)
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, Option<f64>>(3)?,
            row.get::<_, Option<i64>>(4)?,
        ))
    });
    let rows = match rows {
        Ok(r) => r,
        Err(_) => return,
    };

    for row in rows.flatten() {
        let (id, cwd, started_at, ended_at, archived) = row;
        let sid = match id {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        if archived.unwrap_or(0) != 0 {
            continue; // archived
        }

        let mut last = match sec(ended_at).or_else(|| sec(started_at)) {
            Some(v) => v,
            None => continue,
        };
        // the latest message can be newer than ended_at; prefer it when present
        if let Some(mt) = max_message_ts(&con, &sid) {
            last = last.max(mt);
        }
        last = last.min(now); // clamp clock skew / future rows

        let role = last_role(&con, &sid);
        // turn in progress when the user spoke last (model owes a reply) and the
        // session has not been closed out (ended_at set).
        let active = role.as_deref() == Some("user") && sec(ended_at).is_none();

        store.upsert(Session {
            id: sid.clone(),
            tool: TOOL.into(),
            project: project_from_cwd(cwd.as_deref()),
            last_activity: last,
            waiting: false,
            waiting_since: None,
            active_turn: active,
        });

        if role.as_deref() == Some("assistant") {
            store.mark_waiting(&sid, last);
        }
    }
}
