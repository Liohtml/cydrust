use crate::{model::Session, state::Store};
use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use walkdir::WalkDir;

// Sibling Codex collector. Declared here (rather than in lib.rs/main.rs) via an
// explicit #[path] so the new file is reachable as `collector::scan_codex`
// without touching the crate root.
#[path = "collector_codex.rs"]
mod codex;
pub use codex::scan_codex;

/// How many bytes from the end of a transcript to scan for the last turn marker.
/// Plenty for the final few JSONL records without reading multi-MB files each poll.
const TAIL_BYTES: u64 = 65_536;

/// A file untouched for longer than this can't be mid-turn, so we skip the tail read.
const ACTIVE_WINDOW_SECS: f64 = 300.0;

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn claude_projects_root() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("projects")
}

/// Claude encodes the cwd as a path-with-dashes (e.g. `-Users-foo-bar`); the
/// project name is the last non-empty dash-separated segment (`bar`). Mirrors the
/// Python `project_from_encoded_dir`.
fn project_from_encoded_dir(name: &str) -> String {
    name.split('-')
        .rfind(|p| !p.is_empty())
        .unwrap_or(name)
        .to_string()
}

fn mtime_secs(path: &Path) -> Option<f64> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    Some(mtime.duration_since(UNIX_EPOCH).ok()?.as_secs_f64())
}

/// Read the last ~`TAIL_BYTES` of a file and return its complete trailing lines.
/// Cheap enough to run every poll; tolerant of a partial first line / bad bytes.
/// Returns an empty vec on any I/O trouble (never panics).
fn tail_lines(path: &Path) -> Vec<String> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let size = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return Vec::new(),
    };
    let truncated = size > TAIL_BYTES;
    if truncated {
        // Seek to the tail window. On failure, bail rather than risk reading the
        // whole (possibly huge) file.
        if file.seek(SeekFrom::Start(size - TAIL_BYTES)).is_err() {
            return Vec::new();
        }
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    // Lossy decode: bad UTF-8 bytes become replacement chars, never an error.
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    if truncated && !lines.is_empty() {
        lines.remove(0); // drop the possibly-truncated first line
    }
    lines
}

/// True if the assistant currently owes a response (a turn is in progress).
///
/// Heuristic from the transcript tail: find the last entry whose type is "user" or
/// "assistant". If it is a "user" entry (a real prompt OR a tool_result being fed
/// back to the model), the assistant has not yet replied -> working. If it is an
/// "assistant" entry, the model finished its turn -> not in-progress. Returns false
/// on any read/parse trouble so a bad file can never wedge a session into permanent
/// "working".
fn detect_active_turn(path: &Path) -> bool {
    let lines = tail_lines(path);
    for line in lines.iter().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines, never crash
        };
        match obj.get("type").and_then(|t| t.as_str()) {
            Some("user") => return true,
            Some("assistant") => return false,
            _ => {}
        }
    }
    false
}

/// Upsert one `Session` per `*.jsonl` found under `~/.claude/projects/<dir>/`.
///
/// Fixes over the original scan:
///   - project name is decoded from the encoded directory (last path segment),
///   - `last_activity` uses the file mtime, clamped to "now" for clock skew,
///   - `active_turn` is set by a cheap transcript tail-read (only for files
///     modified within the last 5 minutes, for perf).
///
/// Safe to call on a missing root; a malformed line/file is skipped, never panics.
pub fn scan_claude(store: &Arc<Store>) {
    let root = claude_projects_root();
    if !root.exists() {
        return;
    }
    let now = now_secs();

    for entry in WalkDir::new(&root).max_depth(2).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }

        let id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let project = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(project_from_encoded_dir)
            .unwrap_or_else(|| "?".into());

        // Clamp clock skew / future-dated files to "now".
        let mtime = mtime_secs(path).unwrap_or(0.0).min(now);

        // Only inspect the transcript tail when the file is recent enough to
        // plausibly be mid-turn. A file untouched for >5min can't be actively
        // generating, so skip the read (avoids tailing many stale transcripts).
        let active = (now - mtime) <= ACTIVE_WINDOW_SECS && detect_active_turn(path);

        store.upsert(Session {
            id,
            tool: "claude".into(),
            project,
            last_activity: mtime,
            waiting: false,
            waiting_since: None,
            active_turn: active,
        });
    }
}
