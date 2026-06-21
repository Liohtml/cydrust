//! Codex session collector.
//!
//! Ported from `vibemonitor/collector_codex.py`. Walks today's and yesterday's
//! Codex rollout date dirs (`~/.codex/sessions/YYYY/MM/DD/*.jsonl`), parses each
//! rollout for project / session-uuid / waiting / active-turn, and upserts a
//! `Session` per file. Declared as a submodule of `collector` (see the `#[path]`
//! in collector.rs) so it is reachable as `collector::scan_codex`.

use crate::{model::Session, state::Store};
use chrono::{Datelike, Duration, Local, NaiveDate};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

// event_msg payload types that mark turn boundaries.
const WORKING: &str = "task_started";
const DONE: &str = "task_complete";
const ABORTED: &str = "turn_aborted";

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn codex_sessions_root() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex")
        .join("sessions")
}

fn mtime_secs(path: &Path) -> Option<f64> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    Some(mtime.duration_since(UNIX_EPOCH).ok()?.as_secs_f64())
}

/// Extract a v4-style uuid (8-4-4-4-12 hex) from a rollout filename stem.
/// Codex names files `rollout-<ISO ts>-<uuid>.jsonl`. Falls back to the last
/// dash-group only if no uuid is present. Mirrors Python `_uuid_from_stem`.
fn uuid_from_stem(stem: &str) -> String {
    if let Some(u) = find_uuid(stem) {
        return u;
    }
    stem.rsplit('-').next().unwrap_or(stem).to_string()
}

/// Hand-rolled scan for the first 8-4-4-4-12 hex group (avoids a regex dep).
fn find_uuid(s: &str) -> Option<String> {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let bytes = s.as_bytes();
    let is_hex = |b: u8| b.is_ascii_hexdigit();

    let mut i = 0;
    while i < bytes.len() {
        // Try to match the full pattern starting at i.
        let mut pos = i;
        let mut ok = true;
        for (gi, &glen) in GROUPS.iter().enumerate() {
            if gi > 0 {
                if pos >= bytes.len() || bytes[pos] != b'-' {
                    ok = false;
                    break;
                }
                pos += 1;
            }
            let start = pos;
            while pos < bytes.len() && pos - start < glen && is_hex(bytes[pos]) {
                pos += 1;
            }
            if pos - start != glen {
                ok = false;
                break;
            }
        }
        if ok {
            // s is ASCII in the matched region, so byte slice == char slice.
            return Some(s[i..pos].to_string());
        }
        i += 1;
    }
    None
}

/// Project = last path component of the session cwd. Mirrors Python
/// `_project_from_cwd`.
fn project_from_cwd(cwd: &str) -> String {
    let normalized = cwd.replace('\\', "/");
    let trimmed = normalized.trim_end_matches('/');
    if trimmed.is_empty() {
        return "?".into();
    }
    trimmed.rsplit('/').next().unwrap_or("?").to_string()
}

struct Rollout {
    id: String,
    project: String,
    waiting: bool,
    active: bool,
}

/// Read a Codex rollout jsonl and derive {id, project, waiting, active}.
///
/// waiting (turn finished, your move) is decided by, in order:
///   1. the last task_started/task_complete/turn_aborted marker, if any
///      (task_complete -> waiting; task_started/turn_aborted -> not waiting);
///   2. a version fallback when no markers exist: last event_msg of
///      agent_message -> waiting; user_message -> not waiting.
///
/// active (a turn is in progress, the model owes a reply) is the complement:
/// task_started with no later complete/abort, or the user spoke last.
///
/// Never panics: malformed lines are skipped, and a missing/unreadable file
/// yields a best-effort result keyed on the filename stem.
fn parse_rollout(path: &Path) -> Rollout {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let mut sid = uuid_from_stem(stem);
    let mut project = "?".to_string();
    let mut last_turn_state: Option<&str> = None;
    let mut last_msg_kind: Option<&str> = None;

    // Read the whole file; tolerate bad UTF-8 via lossy decode. A read error
    // leaves us with the stem-derived id and "?" project.
    let raw = std::fs::read(path).unwrap_or_default();
    let text = String::from_utf8_lossy(&raw);

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines, never crash
        };
        let t = obj.get("type").and_then(|v| v.as_str());
        let payload = obj.get("payload");
        match t {
            Some("session_meta") => {
                if let Some(p) = payload {
                    if let Some(id) = p.get("id").and_then(|v| v.as_str()) {
                        if !id.is_empty() {
                            sid = id.to_string();
                        }
                    }
                    if let Some(cwd) = p.get("cwd").and_then(|v| v.as_str()) {
                        if !cwd.is_empty() {
                            project = project_from_cwd(cwd);
                        }
                    }
                }
            }
            Some("event_msg") => {
                let pt = payload.and_then(|p| p.get("type")).and_then(|v| v.as_str());
                match pt {
                    Some(WORKING) => last_turn_state = Some(WORKING),
                    Some(DONE) => last_turn_state = Some(DONE),
                    Some(ABORTED) => last_turn_state = Some(ABORTED),
                    Some("agent_message") => last_msg_kind = Some("agent"),
                    Some("user_message") => last_msg_kind = Some("user"),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let (waiting, active) = match last_turn_state {
        Some(state) => {
            // task_complete -> waiting; task_started with no later complete/abort
            // means a turn is actively running.
            (state == DONE, state == WORKING)
        }
        None => {
            // version-agnostic fallback: who spoke last.
            (
                last_msg_kind == Some("agent"),
                last_msg_kind == Some("user"),
            )
        }
    };

    Rollout {
        id: sid,
        project,
        waiting,
        active,
    }
}

fn day_dir(root: &Path, d: NaiveDate) -> PathBuf {
    root.join(format!("{:04}", d.year()))
        .join(format!("{:02}", d.month()))
        .join(format!("{:02}", d.day()))
}

/// Scan today's (and yesterday's) Codex date dirs only and upsert one `Session`
/// per rollout. Safe on a missing root; never panics.
///
/// Note on the Store API: the available `Store` exposes `upsert` and
/// `mark_waiting(id, ts)` only (no `clear_waiting`/`get`/`acked_at`). We upsert
/// every rollout (which carries `active_turn`), then `mark_waiting` the ones whose
/// transcript shows the assistant is awaiting user input. A session that is no
/// longer waiting simply stops being re-marked; the waiting flag then decays via
/// the state machine's waiting TTL, and `active_turn`/`last_activity` are refreshed
/// on each scan through `upsert`.
pub fn scan_codex(store: &Arc<Store>) {
    let root = codex_sessions_root();
    if !root.exists() {
        return;
    }
    let now = now_secs();

    let today = Local::now().date_naive();
    let yesterday = today - Duration::days(1);

    for d in [today, yesterday] {
        let dir = day_dir(&root, d);
        if !dir.exists() {
            continue;
        }
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            // Clamp clock skew / future-dated files to "now".
            let mtime = match mtime_secs(&path) {
                Some(m) => m.min(now),
                None => continue,
            };

            let info = parse_rollout(&path);

            store.upsert(Session {
                id: info.id.clone(),
                tool: "codex".into(),
                project: info.project,
                last_activity: mtime,
                waiting: false,
                waiting_since: None,
                active_turn: info.active,
            });

            if info.waiting {
                store.mark_waiting(&info.id, mtime);
            }
        }
    }
}
