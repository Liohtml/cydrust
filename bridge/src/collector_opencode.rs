use crate::{model::Session, state::Store};
use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags};

// =============================================================================
// OpenCode (sst/opencode, opencode.ai) session collector — Rust port.
//
// Verified against a real install at ~/.local/share/opencode/opencode.db
// (Drizzle/SQLite). Real schema (columns this collector reads):
//
//   session( id text PK, project_id text, parent_id text, slug text,
//            directory text NOT NULL,        -- working dir / cwd
//            title text, version text, ...,
//            time_created integer NOT NULL,   -- epoch MILLISECONDS
//            time_updated integer NOT NULL,   -- epoch MILLISECONDS
//            time_compacting integer,
//            time_archived integer )          -- non-NULL => archived (skip)
//
//   message( id text PK, session_id text, time_created int, time_updated int,
//            data text NOT NULL )             -- JSON: {"role":"user"|"assistant",
//                            "time":{"created":<ms>,"completed":<ms>?}, ...}
//
// active_turn / waiting inference (from the latest message row's JSON `data`):
//   * active_turn = the model owes a reply: the last message role is "user", OR an
//     assistant message is still generating (time.created present, time.completed
//     absent).
//   * waiting = the assistant had the last turn and finished (role == "assistant"
//     and NOT still generating) => waiting on the user.
//   Opened strictly read-only + immutable so it never contends with a live
//   opencode process. Never panics: any missing file / failed query returns quietly.
// =============================================================================

const TOOL: &str = "opencode";

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

// --- path discovery ----------------------------------------------------------

/// Resolve OpenCode's data/storage dir.
///
/// Resolution order:
///   1. $OPENCODE_DATA_DIR (os-pathsep- or comma-separated -> first existing)
///   2. $XDG_DATA_HOME/opencode (if it exists)
///   3. platform default: ~/.local/share/opencode  (confirmed on this Windows host
///      too -- OpenCode uses the XDG-style path under the user home on Windows).
fn opencode_data_root() -> PathBuf {
    if let Ok(env_val) = env::var("OPENCODE_DATA_DIR") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        for part in env_val.replace(',', &sep.to_string()).split(sep) {
            let part = part.trim();
            if !part.is_empty() {
                let p = PathBuf::from(part);
                if p.exists() {
                    return p;
                }
            }
        }
    }
    if let Ok(xdg) = env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            let p = PathBuf::from(xdg).join("opencode");
            if p.exists() {
                return p;
            }
        }
    }
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("share")
        .join("opencode")
}

fn sqlite_db(root: &Path) -> PathBuf {
    root.join("opencode.db")
}

// --- helpers -----------------------------------------------------------------

/// Last path segment of the session working dir == project name. Handles both
/// `\` and `/` separators.
fn project_from_directory(directory: Option<&str>) -> String {
    let directory = match directory {
        Some(d) => d,
        None => return "?".into(),
    };
    let d = directory.replace('\\', "/");
    let d = d.trim_end_matches('/');
    match d.rsplit('/').find(|s| !s.is_empty()) {
        Some(seg) => seg.to_string(),
        None => "?".into(),
    }
}

/// OpenCode timestamps are epoch MILLISECONDS. Return epoch seconds. Tolerate a
/// value that is already in seconds (< 1e12).
fn ms_to_epoch(v: Option<i64>) -> Option<f64> {
    match v {
        Some(n) if n > 0 => {
            let f = n as f64;
            Some(if f > 1e12 { f / 1000.0 } else { f })
        }
        _ => None,
    }
}

/// Open the db strictly read-only + immutable via a URI so we never lock/corrupt a
/// live DB. Returns None on any error (never panics).
fn open_ro(db: &Path) -> Option<Connection> {
    // file:<path>?immutable=1  — requires SQLITE_OPEN_URI. Forward-slashed path.
    let uri = format!("file:{}?immutable=1", db.to_string_lossy().replace('\\', "/"));
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()
}

/// Decode the JSON `data` of the most-recent message row for a session.
fn last_message(con: &Connection, session_id: &str) -> Option<serde_json::Value> {
    let data: String = con
        .query_row(
            "SELECT data FROM message WHERE session_id = ?1 \
             ORDER BY time_created DESC, id DESC LIMIT 1",
            [session_id],
            |row| row.get(0),
        )
        .ok()?;
    serde_json::from_str(&data).ok()
}

/// The assistant had the last turn and finished => waiting on the user.
/// An assistant message still generating (time.created present, no time.completed)
/// is treated as NOT waiting (working). Anything ending on a user message is not
/// waiting.
fn infer_waiting(last_msg: Option<&serde_json::Value>) -> bool {
    let msg = match last_msg {
        Some(m) => m,
        None => return false,
    };
    if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return false;
    }
    if let Some(t) = msg.get("time") {
        let created = t.get("created").map(|v| !v.is_null()).unwrap_or(false);
        let completed = t.get("completed").map(|v| !v.is_null()).unwrap_or(false);
        if created && !completed {
            return false; // still generating
        }
    }
    true
}

/// True when an assistant message is still generating (created, not completed) OR
/// the last message is from the user (the model owes a reply).
fn infer_active_turn(last_msg: Option<&serde_json::Value>) -> bool {
    let msg = match last_msg {
        Some(m) => m,
        None => return false,
    };
    match msg.get("role").and_then(|r| r.as_str()) {
        Some("user") => true, // model owes a response
        Some("assistant") => {
            if let Some(t) = msg.get("time") {
                let created = t.get("created").map(|v| !v.is_null()).unwrap_or(false);
                let completed = t.get("completed").map(|v| !v.is_null()).unwrap_or(false);
                return created && !completed; // still generating
            }
            false
        }
        _ => false,
    }
}

// --- public entry point (mirrors scan_claude / scan_codex) -------------------

/// Upsert one `Session` per OpenCode session from opencode.db (read-only).
///
/// Safe on a missing root/db. Opened immutable so it never contends with a live
/// opencode process. Skips archived sessions. Never panics.
pub fn scan_opencode(store: &Arc<Store>) {
    let root = opencode_data_root();
    if !root.exists() {
        return;
    }
    let db = sqlite_db(&root);
    if !db.exists() {
        return;
    }

    let con = match open_ro(&db) {
        Some(c) => c,
        None => return,
    };

    let now = now_secs();

    let mut stmt = match con.prepare(
        "SELECT id, directory, time_created, time_updated, time_archived FROM session",
    ) {
        Ok(s) => s,
        Err(_) => return,
    };

    // (id, directory, time_created, time_updated, time_archived)
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<i64>>(4)?,
        ))
    });
    let rows = match rows {
        Ok(r) => r,
        Err(_) => return,
    };

    for row in rows.flatten() {
        let (id, directory, time_created, time_updated, time_archived) = row;
        let sid = match id {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        if time_archived.is_some() {
            continue; // archived
        }
        let last = match ms_to_epoch(time_updated).or_else(|| ms_to_epoch(time_created)) {
            Some(v) => v.min(now), // clamp clock skew / future rows
            None => continue,
        };

        let last_msg = last_message(&con, &sid);

        store.upsert(Session {
            id: sid.clone(),
            tool: TOOL.into(),
            project: project_from_directory(directory.as_deref()),
            last_activity: last,
            waiting: false,
            waiting_since: None,
            active_turn: infer_active_turn(last_msg.as_ref()),
        });

        if infer_waiting(last_msg.as_ref()) {
            store.mark_waiting(&sid, last);
        }
    }
}
