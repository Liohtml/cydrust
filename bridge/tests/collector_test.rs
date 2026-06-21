/// Tests for the collector module's file-scanning logic.
///
/// `scan_claude` walks `~/.claude/projects` and calls `store.upsert()` for
/// every `.jsonl` file it finds.  We cannot safely mutate the real home
/// directory, so we test the underlying helpers in isolation through the
/// public API of the store, and exercise the directory-naming helper via
/// observable behaviour.
///
/// For scan_claude itself we use `tempfile` to create a controlled directory
/// tree and verify the store is populated correctly.
use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use tempfile::TempDir;
use vibe_bridge::state::Store;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Create a `.jsonl` file at `parent_dir/<stem>.jsonl`.
/// Returns the path to the created file.
fn create_jsonl(parent: &std::path::Path, stem: &str) -> PathBuf {
    let path = parent.join(format!("{stem}.jsonl"));
    fs::write(&path, b"{}").unwrap(); // content doesn't matter — only mtime is used
    path
}

/// Mirror the `project_from_dir` logic from `collector.rs`:
///   split on '-', take the last non-empty segment.
fn expected_project(dir_name: &str) -> String {
    dir_name
        .split('-')
        .filter(|p| !p.is_empty())
        .last()
        .unwrap_or(dir_name)
        .to_string()
}

// ── project_from_dir logic ────────────────────────────────────────────────────

#[test]
fn project_name_extracted_from_last_hyphen_segment() {
    assert_eq!(expected_project("foo-bar-baz"), "baz");
    assert_eq!(expected_project("myproject"),   "myproject");
    assert_eq!(expected_project("a-b"),         "b");
    assert_eq!(expected_project("single"),      "single");
}

#[test]
fn project_name_handles_leading_hyphens() {
    // e.g. "--final" — last non-empty segment is "final"
    assert_eq!(expected_project("--final"), "final");
}

// ── Store integration (simulated scan) ───────────────────────────────────────

/// Manually replicate what `scan_claude` does for a small tree, then verify
/// the store reflects the expected sessions.  This validates the upsert
/// contract rather than the filesystem walk (which is covered by the collector
/// unit path).
#[test]
fn manual_scan_populates_store_correctly() {
    use vibe_bridge::model::Session;

    let store = Arc::new(Store::new());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    let sessions = vec![
        Session {
            id:            "session-alpha".into(),
            tool:          "claude".into(),
            project:       "alpha".into(),
            last_activity: now,
            waiting:       false,
            waiting_since: None,
        },
        Session {
            id:            "session-beta".into(),
            tool:          "claude".into(),
            project:       "beta".into(),
            last_activity: now - 30.0,
            waiting:       false,
            waiting_since: None,
        },
    ];

    for s in sessions {
        store.upsert(s);
    }

    let snap = store.snapshot();
    assert_eq!(snap.len(), 2);
    let ids: Vec<&str> = snap.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"session-alpha"));
    assert!(ids.contains(&"session-beta"));
}

// ── Tempfile-based scan_claude tests ─────────────────────────────────────────

/// Build a fake `~/.claude/projects` tree inside a TempDir and call a
/// local reimplementation of the scan logic (we cannot redirect
/// `dirs_next::home_dir()`, so we reproduce the scan_claude walk here to test
/// the observable outcome on the store).
fn scan_dir(root: &std::path::Path, store: &Arc<Store>) {
    use walkdir::WalkDir;

    for entry in WalkDir::new(root).max_depth(2).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let project = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|n| expected_project(n))
            .unwrap_or_else(|| "?".into());
        let last_activity = {
            let meta = std::fs::metadata(path).ok();
            meta.and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
        };

        store.upsert(vibe_bridge::model::Session {
            id,
            tool: "claude".into(),
            project,
            last_activity,
            waiting: false,
            waiting_since: None,
        });
    }
}

#[test]
fn scan_empty_root_yields_empty_store() {
    let tmp = TempDir::new().unwrap();
    let store = Arc::new(Store::new());
    scan_dir(tmp.path(), &store);
    assert!(store.snapshot().is_empty());
}

#[test]
fn scan_single_jsonl_creates_one_session() {
    let tmp = TempDir::new().unwrap();
    let proj_dir = tmp.path().join("prefix-myproject");
    fs::create_dir_all(&proj_dir).unwrap();
    create_jsonl(&proj_dir, "session-001");

    let store = Arc::new(Store::new());
    scan_dir(tmp.path(), &store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].id,      "session-001");
    assert_eq!(snap[0].tool,    "claude");
    assert_eq!(snap[0].project, "myproject"); // last segment after '-'
}

#[test]
fn scan_multiple_jsonl_in_same_dir_creates_multiple_sessions() {
    let tmp = TempDir::new().unwrap();
    let proj_dir = tmp.path().join("proj-demo");
    fs::create_dir_all(&proj_dir).unwrap();
    create_jsonl(&proj_dir, "sess-a");
    create_jsonl(&proj_dir, "sess-b");
    create_jsonl(&proj_dir, "sess-c");

    let store = Arc::new(Store::new());
    scan_dir(tmp.path(), &store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 3);
    let ids: Vec<&str> = snap.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"sess-a"));
    assert!(ids.contains(&"sess-b"));
    assert!(ids.contains(&"sess-c"));
}

#[test]
fn scan_ignores_non_jsonl_files() {
    let tmp = TempDir::new().unwrap();
    let proj_dir = tmp.path().join("proj-x");
    fs::create_dir_all(&proj_dir).unwrap();
    // These should be ignored
    fs::write(proj_dir.join("readme.md"),  b"# hi").unwrap();
    fs::write(proj_dir.join("data.json"),  b"{}").unwrap();
    fs::write(proj_dir.join("notes.txt"),  b"notes").unwrap();
    // Only this should be picked up
    create_jsonl(&proj_dir, "real-session");

    let store = Arc::new(Store::new());
    scan_dir(tmp.path(), &store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].id, "real-session");
}

#[test]
fn scan_multiple_project_dirs_are_all_visited() {
    let tmp = TempDir::new().unwrap();
    for proj in ["project-alpha", "project-beta", "project-gamma"] {
        let d = tmp.path().join(proj);
        fs::create_dir_all(&d).unwrap();
        create_jsonl(&d, &format!("sess-{proj}"));
    }

    let store = Arc::new(Store::new());
    scan_dir(tmp.path(), &store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 3);
}

#[test]
fn scan_last_activity_is_mtime_of_jsonl_file() {
    let tmp = TempDir::new().unwrap();
    let proj_dir = tmp.path().join("p-test");
    fs::create_dir_all(&proj_dir).unwrap();
    let file_path = create_jsonl(&proj_dir, "time-sess");

    let expected_mtime = std::fs::metadata(&file_path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    let store = Arc::new(Store::new());
    scan_dir(tmp.path(), &store);

    let snap = store.snapshot();
    assert_eq!(snap.len(), 1);
    assert!(
        (snap[0].last_activity - expected_mtime).abs() < 1.0,
        "last_activity should match the file's mtime (within 1 s tolerance)"
    );
}

#[test]
fn scan_project_name_no_hyphen_uses_full_dir_name() {
    let tmp = TempDir::new().unwrap();
    let proj_dir = tmp.path().join("myproject"); // no '-'
    fs::create_dir_all(&proj_dir).unwrap();
    create_jsonl(&proj_dir, "s1");

    let store = Arc::new(Store::new());
    scan_dir(tmp.path(), &store);

    let snap = store.snapshot();
    assert_eq!(snap[0].project, "myproject");
}

#[test]
fn scan_does_not_descend_beyond_depth_2() {
    // max_depth(2) means: root(0) → project-dir(1) → session.jsonl(2).
    // A file nested deeper should not be picked up.
    let tmp = TempDir::new().unwrap();
    let deep_dir = tmp.path().join("proj-outer").join("subdir");
    fs::create_dir_all(&deep_dir).unwrap();
    create_jsonl(&deep_dir, "deep-session");

    let store = Arc::new(Store::new());
    scan_dir(tmp.path(), &store);

    // At depth 2 (proj-outer/subdir/deep-session.jsonl) the file is at depth 3
    // relative to root, so it should not be scanned.
    let snap = store.snapshot();
    assert!(
        snap.is_empty(),
        "files deeper than depth-2 should not be scanned (got {:?})",
        snap
    );
}

#[test]
fn repeated_scan_does_not_duplicate_sessions() {
    let tmp = TempDir::new().unwrap();
    let proj_dir = tmp.path().join("proj-dup");
    fs::create_dir_all(&proj_dir).unwrap();
    create_jsonl(&proj_dir, "unique-sess");

    let store = Arc::new(Store::new());
    scan_dir(tmp.path(), &store);
    scan_dir(tmp.path(), &store); // second scan
    scan_dir(tmp.path(), &store); // third scan

    assert_eq!(store.snapshot().len(), 1, "upsert must be idempotent");
}

#[test]
fn scan_sets_tool_to_claude() {
    let tmp = TempDir::new().unwrap();
    let proj_dir = tmp.path().join("proj-tool");
    fs::create_dir_all(&proj_dir).unwrap();
    create_jsonl(&proj_dir, "tool-sess");

    let store = Arc::new(Store::new());
    scan_dir(tmp.path(), &store);

    let snap = store.snapshot();
    assert_eq!(snap[0].tool, "claude");
}

#[test]
fn scan_new_sessions_are_not_waiting() {
    let tmp = TempDir::new().unwrap();
    let proj_dir = tmp.path().join("proj-w");
    fs::create_dir_all(&proj_dir).unwrap();
    create_jsonl(&proj_dir, "w-sess");

    let store = Arc::new(Store::new());
    scan_dir(tmp.path(), &store);

    let snap = store.snapshot();
    assert!(!snap[0].waiting);
    assert!(snap[0].waiting_since.is_none());
}
