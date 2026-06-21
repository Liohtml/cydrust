use crate::model::{Metrics, Session, UsageInfo};
use std::{
    collections::HashMap,
    sync::RwLock,
    time::{SystemTime, UNIX_EPOCH},
};

/// Background-computed data (usage gauges, metrics, titles) that the slow loops
/// refresh and the /state handler reads. Kept separate from the live session
/// Store so the fast session scan and the slow API/cost loops don't block each
/// other. (Capacity is derived per-request from these + live session counts.)
#[derive(Default)]
pub struct Shared {
    pub claude_usage: UsageInfo,
    pub codex_usage: UsageInfo,
    pub metrics: Metrics,
    pub titles: HashMap<String, String>,
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<String, Session>,
    last_scan: f64,
}

pub struct Store {
    inner: RwLock<Inner>,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
        }
    }

    pub fn upsert(&self, session: Session) {
        let mut g = self.inner.write().unwrap();
        let existing = g
            .sessions
            .entry(session.id.clone())
            .or_insert_with(|| session.clone());
        existing.last_activity = existing.last_activity.max(session.last_activity);
        existing.project = session.project;
        existing.tool = session.tool;
        existing.active_turn = session.active_turn; // collectors recompute each scan
        g.last_scan = now_secs();
    }

    pub fn mark_waiting(&self, id: &str, ts: f64) {
        let mut g = self.inner.write().unwrap();
        if let Some(s) = g.sessions.get_mut(id) {
            s.waiting = true;
            s.waiting_since = Some(ts);
        }
    }

    pub fn ack(&self, id: &str) {
        let mut g = self.inner.write().unwrap();
        if let Some(s) = g.sessions.get_mut(id) {
            s.waiting = false;
            s.waiting_since = None;
        }
    }

    pub fn snapshot(&self) -> Vec<Session> {
        self.inner
            .read()
            .unwrap()
            .sessions
            .values()
            .cloned()
            .collect()
    }

    pub fn last_scan(&self) -> f64 {
        self.inner.read().unwrap().last_scan
    }

    /// Reaper: drop sessions whose last activity is older than `gone_ttl`.
    pub fn remove_gone(&self, now: f64, gone_ttl: f64) {
        let mut g = self.inner.write().unwrap();
        g.sessions
            .retain(|_, s| (now - s.last_activity) <= gone_ttl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Session;

    fn session(id: &str, last_activity: f64) -> Session {
        Session {
            id: id.into(),
            tool: "claude".into(),
            project: "test-project".into(),
            last_activity,
            waiting: false,
            waiting_since: None,
            active_turn: false,
        }
    }

    // ── Store::new ────────────────────────────────────────────────────────────

    #[test]
    fn new_store_is_empty() {
        let store = Store::new();
        assert!(store.snapshot().is_empty());
    }

    #[test]
    fn new_store_last_scan_is_zero() {
        let store = Store::new();
        assert_eq!(store.last_scan(), 0.0);
    }

    // ── upsert ────────────────────────────────────────────────────────────────

    #[test]
    fn upsert_inserts_new_session() {
        let store = Store::new();
        store.upsert(session("s1", 1.0));
        let snap = store.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].id, "s1");
    }

    #[test]
    fn upsert_updates_last_scan_timestamp() {
        let store = Store::new();
        let before = now_secs();
        store.upsert(session("s1", 1.0));
        let after = now_secs();
        let ls = store.last_scan();
        assert!(ls >= before, "last_scan {ls} should be >= {before}");
        assert!(ls <= after, "last_scan {ls} should be <= {after}");
    }

    #[test]
    fn upsert_keeps_max_last_activity() {
        let store = Store::new();
        store.upsert(session("s1", 100.0));
        store.upsert(session("s1", 50.0)); // older — must NOT win
        let snap = store.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap[0].last_activity, 100.0,
            "should keep the higher timestamp"
        );
    }

    #[test]
    fn upsert_advances_last_activity_when_newer() {
        let store = Store::new();
        store.upsert(session("s1", 100.0));
        store.upsert(session("s1", 200.0)); // newer — must win
        let snap = store.snapshot();
        assert_eq!(snap[0].last_activity, 200.0);
    }

    #[test]
    fn upsert_updates_project_and_tool() {
        let store = Store::new();
        store.upsert(session("s1", 1.0));
        let updated = Session {
            id: "s1".into(),
            tool: "codex".into(),
            project: "new-project".into(),
            last_activity: 2.0,
            waiting: false,
            waiting_since: None,
            active_turn: false,
        };
        store.upsert(updated);
        let snap = store.snapshot();
        assert_eq!(snap[0].tool, "codex");
        assert_eq!(snap[0].project, "new-project");
    }

    #[test]
    fn upsert_multiple_sessions_are_all_stored() {
        let store = Store::new();
        for i in 0..5 {
            store.upsert(session(&format!("s{i}"), i as f64));
        }
        assert_eq!(store.snapshot().len(), 5);
    }

    // ── mark_waiting ──────────────────────────────────────────────────────────

    #[test]
    fn mark_waiting_sets_flag_and_timestamp() {
        let store = Store::new();
        store.upsert(session("s1", 1.0));
        let ts = 9999.0_f64;
        store.mark_waiting("s1", ts);
        let snap = store.snapshot();
        assert!(snap[0].waiting);
        assert_eq!(snap[0].waiting_since, Some(ts));
    }

    #[test]
    fn mark_waiting_on_unknown_id_is_noop() {
        let store = Store::new();
        // must not panic
        store.mark_waiting("does-not-exist", 1.0);
        assert!(store.snapshot().is_empty());
    }

    // ── ack ───────────────────────────────────────────────────────────────────

    #[test]
    fn ack_clears_waiting_flag() {
        let store = Store::new();
        store.upsert(session("s1", 1.0));
        store.mark_waiting("s1", 9999.0);
        store.ack("s1");
        let snap = store.snapshot();
        assert!(!snap[0].waiting);
        assert_eq!(snap[0].waiting_since, None);
    }

    #[test]
    fn ack_on_non_waiting_session_is_noop() {
        let store = Store::new();
        store.upsert(session("s1", 1.0));
        // calling ack when not waiting should not panic
        store.ack("s1");
        let snap = store.snapshot();
        assert!(!snap[0].waiting);
    }

    #[test]
    fn ack_on_unknown_id_is_noop() {
        let store = Store::new();
        // must not panic
        store.ack("ghost");
    }

    // ── snapshot ──────────────────────────────────────────────────────────────

    #[test]
    fn snapshot_returns_independent_clone() {
        let store = Store::new();
        store.upsert(session("s1", 1.0));
        let snap = store.snapshot();
        // mutate the original store after snapshot — clone must not change
        store.upsert(session("s2", 2.0));
        assert_eq!(snap.len(), 1, "snapshot is a point-in-time copy");
    }

    #[test]
    fn snapshot_after_ack_reflects_change() {
        let store = Store::new();
        store.upsert(session("s1", 1.0));
        store.mark_waiting("s1", 42.0);
        store.ack("s1");
        let snap = store.snapshot();
        assert!(!snap[0].waiting);
        assert!(snap[0].waiting_since.is_none());
    }
}
