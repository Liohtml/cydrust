use crate::{model::Session, state::Store};
use std::{
    path::PathBuf,
    sync::Arc,
    time::UNIX_EPOCH,
};
use walkdir::WalkDir;

fn claude_projects_root() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("projects")
}

fn project_from_dir(name: &str) -> String {
    name.split('-')
        .filter(|p| !p.is_empty())
        .last()
        .unwrap_or(name)
        .to_string()
}

fn mtime_secs(path: &std::path::Path) -> Option<f64> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    Some(mtime.duration_since(UNIX_EPOCH).ok()?.as_secs_f64())
}

pub fn scan_claude(store: &Arc<Store>) {
    let root = claude_projects_root();
    if !root.exists() {
        return;
    }
    for entry in WalkDir::new(&root).max_depth(2).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let id = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let project = path.parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(project_from_dir)
            .unwrap_or_else(|| "?".into());
        let last_activity = mtime_secs(path).unwrap_or(0.0);

        store.upsert(Session {
            id,
            tool: "claude".into(),
            project,
            last_activity,
            waiting: false,
            waiting_since: None,
        });
    }
}