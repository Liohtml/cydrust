//! Multi-host federation — aggregate sessions from several developer machines.
//!
//! Push model (firewall-friendly): each `vibe-bridge` has a `[federation]`
//! `role`. A `"node"` periodically POSTs its current local session rows to an
//! `"aggregator"`'s `POST /federation/ingest`; the aggregator merges them into
//! a [`RemoteStore`] and includes them in its own `/state` so the device shows
//! sessions from ALL machines. `"standalone"` (the default) does nothing — this
//! module is a complete no-op when federation is off.
//!
//! This module is intentionally self-contained: it reuses `crate::model`'s
//! [`SessionRow`]/[`Status`] for the on-device wire format but defines its own
//! [`FedSession`] for the node→aggregator wire so the federation payload is
//! decoupled from the hub's internal `/state` schema and stays robust across
//! version skew between machines.

use crate::model::{SessionRow, Status};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::RwLock, time::Duration};

/// Epsilon tolerance for floating-point TTL comparisons (1ms).
const EPSILON: f64 = 0.001;

/// Network timeout for a node's push to the aggregator.
const PUSH_TIMEOUT: Duration = Duration::from_secs(3);

/// The wire payload a node pushes to the aggregator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedPayload {
    /// The pushing node's id (defaults to its hostname). Used to namespace
    /// remote sessions so machines stay distinguishable on the device.
    pub node: String,
    pub sessions: Vec<FedSession>,
}

/// A single session as carried over the federation wire. Deliberately a flat,
/// stringly-typed mirror of the device-facing [`SessionRow`] so nodes and
/// aggregators on slightly different versions still interoperate (status is a
/// plain string, decoded leniently on ingest).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedSession {
    pub id: String,
    pub tool: String,
    pub project: String,
    /// "working" | "idle" | "waiting" (matches `Status` serde rename).
    pub status: String,
    pub age_sec: u64,
    pub waiting: bool,
    #[serde(default)]
    pub waiting_sec: Option<u64>,
    #[serde(default)]
    pub summary: Option<String>,
}

impl FedSession {
    fn from_row(row: &SessionRow) -> Self {
        FedSession {
            id: row.id.clone(),
            tool: row.tool.clone(),
            project: row.project.clone(),
            status: status_to_str(&row.status).to_string(),
            age_sec: row.age_sec,
            waiting: row.waiting,
            waiting_sec: row.waiting_sec,
            summary: row.summary.clone(),
        }
    }
}

fn status_to_str(s: &Status) -> &'static str {
    match s {
        Status::Working => "working",
        Status::Idle => "idle",
        Status::Waiting => "waiting",
    }
}

/// Lenient string → Status mapping. Unknown values fall back to `Idle` so a
/// future/garbled status from a remote node can never break the aggregator.
fn status_from_str(s: &str) -> Status {
    match s.trim().to_ascii_lowercase().as_str() {
        "working" => Status::Working,
        "waiting" => Status::Waiting,
        _ => Status::Idle,
    }
}

/// Aggregator-side store of sessions pushed by remote nodes.
///
/// Keyed by `"<node>/<session-id>"`; each value carries the remote
/// [`FedSession`] plus the local epoch-seconds receive timestamp used to expire
/// stale entries when a node goes quiet. All access is interior-mutable via an
/// `RwLock`, so a single `Arc<RemoteStore>` can be shared by the ingest handler
/// and the `/state` handler.
pub struct RemoteStore {
    inner: RwLock<HashMap<String, (FedSession, f64)>>,
}

impl Default for RemoteStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteStore {
    pub fn new() -> Self {
        RemoteStore {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Upsert all sessions from one node's push, stamping each with `now`.
    ///
    /// Replaces the full set of sessions previously seen for `payload.node`:
    /// any session the node no longer reports is dropped immediately, so a
    /// closed session disappears on the next push rather than lingering until
    /// the TTL expires.
    pub fn merge(&self, payload: FedPayload, now: f64) {
        let node = payload.node;
        let prefix = format!("{node}/");
        let mut g = match self.inner.write() {
            Ok(g) => g,
            Err(p) => p.into_inner(), // never panic on a poisoned lock
        };
        // Drop this node's previous entries, then re-insert the fresh set.
        g.retain(|k, _| !k.starts_with(&prefix));
        for sess in payload.sessions {
            let key = format!("{node}/{}", sess.id);
            g.insert(key, (sess, now));
        }
    }

    /// Live remote sessions as [`SessionRow`]s for inclusion in `/state`.
    ///
    /// Entries whose receive timestamp is older than `ttl` seconds are dropped
    /// (and lazily pruned from the store) so a disconnected node's sessions
    /// vanish. Each row's `project` is prefixed with `"<node>/"` (e.g.
    /// `"host1/myproj"`) so machines stay distinguishable on the device.
    /// Uses epsilon tolerance for floating-point comparisons.
    pub fn rows(&self, now: f64, ttl: f64) -> Vec<SessionRow> {
        let mut g = match self.inner.write() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        // Lazy prune of expired entries with epsilon tolerance.
        g.retain(|_, (_, recv_ts)| (now - *recv_ts) <= (ttl + EPSILON));

        let mut rows: Vec<SessionRow> = Vec::with_capacity(g.len());
        for (key, (sess, _recv_ts)) in g.iter() {
            // Recover the node id from the key prefix ("<node>/<id>"); fall back
            // to the whole key if (defensively) no slash is present.
            let node = key.split('/').next().unwrap_or(key.as_str());
            rows.push(SessionRow {
                id: sess.id.clone(),
                tool: sess.tool.clone(),
                project: format!("{node}/{}", sess.project),
                status: status_from_str(&sess.status),
                age_sec: sess.age_sec,
                waiting: sess.waiting,
                waiting_sec: sess.waiting_sec,
                summary: sess.summary.clone(),
            });
        }
        rows
    }
}

/// Build a node's push payload from its local derived [`SessionRow`]s.
pub fn from_session_rows(rows: &[SessionRow], node: &str) -> FedPayload {
    FedPayload {
        node: node.to_string(),
        sessions: rows.iter().map(FedSession::from_row).collect(),
    }
}

/// POST `payload` to `<upstream>/federation/ingest` with the shared token.
///
/// Uses `ureq` with a 3s timeout and the `X-VibeMonitor-Token` header. Returns
/// `Err` on any network/transport/HTTP-status failure so the caller can log and
/// continue; never panics.
pub fn push(payload: &FedPayload, upstream: &str, token: &str) -> anyhow::Result<()> {
    let endpoint = format!("{}/federation/ingest", upstream.trim_end_matches('/'));
    let body = serde_json::to_string(payload)?;
    ureq::post(&endpoint)
        .timeout(PUSH_TIMEOUT)
        .set("Content-Type", "application/json")
        .set("X-VibeMonitor-Token", token)
        .send_string(&body)
        .map_err(|e| anyhow::anyhow!("federation push to {endpoint} failed: {e}"))?;
    Ok(())
}

/// Best-effort machine hostname for the default `node_id`.
///
/// Reads `COMPUTERNAME` (Windows) or `HOSTNAME` (unix), falling back to the
/// `hostname` command, then to `"unknown-host"`. Never panics.
pub fn hostname() -> String {
    if let Ok(h) = std::env::var("COMPUTERNAME") {
        if !h.trim().is_empty() {
            return h.trim().to_string();
        }
    }
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.trim().is_empty() {
            return h.trim().to_string();
        }
    }
    if let Ok(out) = std::process::Command::new("hostname").output() {
        if out.status.success() {
            let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !h.is_empty() {
                return h;
            }
        }
    }
    "unknown-host".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, project: &str, status: Status) -> SessionRow {
        let waiting = matches!(status, Status::Waiting);
        SessionRow {
            id: id.into(),
            tool: "claude".into(),
            project: project.into(),
            status,
            age_sec: 5,
            waiting,
            waiting_sec: None,
            summary: None,
        }
    }

    #[test]
    fn from_session_rows_carries_node_and_status() {
        let rows = vec![row("a", "proj", Status::Working)];
        let p = from_session_rows(&rows, "host1");
        assert_eq!(p.node, "host1");
        assert_eq!(p.sessions.len(), 1);
        assert_eq!(p.sessions[0].status, "working");
        assert_eq!(p.sessions[0].project, "proj");
    }

    #[test]
    fn merge_then_rows_prefixes_project_with_node() {
        let store = RemoteStore::new();
        let p = from_session_rows(&[row("a", "proj", Status::Working)], "host1");
        store.merge(p, 100.0);
        let rows = store.rows(101.0, 30.0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].project, "host1/proj");
        assert_eq!(rows[0].status, Status::Working);
    }

    #[test]
    fn rows_drops_entries_older_than_ttl() {
        let store = RemoteStore::new();
        store.merge(
            from_session_rows(&[row("a", "p", Status::Idle)], "h"),
            100.0,
        );
        // 40s later with a 30s TTL → gone.
        assert!(store.rows(140.0, 30.0).is_empty());
    }

    #[test]
    fn merge_replaces_a_nodes_previous_sessions() {
        let store = RemoteStore::new();
        store.merge(
            from_session_rows(
                &[row("a", "p", Status::Idle), row("b", "p", Status::Idle)],
                "h",
            ),
            100.0,
        );
        // Second push for the same node reports only "a" now.
        store.merge(
            from_session_rows(&[row("a", "p", Status::Working)], "h"),
            101.0,
        );
        let rows = store.rows(101.0, 30.0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "a");
        assert_eq!(rows[0].status, Status::Working);
    }

    #[test]
    fn merge_keeps_distinct_nodes_separate() {
        let store = RemoteStore::new();
        store.merge(
            from_session_rows(&[row("a", "p", Status::Idle)], "h1"),
            100.0,
        );
        store.merge(
            from_session_rows(&[row("a", "p", Status::Idle)], "h2"),
            100.0,
        );
        let mut projects: Vec<String> = store
            .rows(100.0, 30.0)
            .into_iter()
            .map(|r| r.project)
            .collect();
        projects.sort();
        assert_eq!(projects, vec!["h1/p".to_string(), "h2/p".to_string()]);
    }

    #[test]
    fn status_from_str_is_lenient() {
        assert_eq!(status_from_str("WORKING"), Status::Working);
        assert_eq!(status_from_str("waiting"), Status::Waiting);
        assert_eq!(status_from_str("garbage"), Status::Idle);
    }

    #[test]
    fn hostname_is_non_empty() {
        assert!(!hostname().is_empty());
    }

    #[test]
    fn rows_handles_ttl_boundary_with_epsilon() {
        let store = RemoteStore::new();
        store.merge(
            from_session_rows(&[row("a", "p", Status::Idle)], "h"),
            100.0,
        );
        // Exactly 30s later with a 30s TTL: (130 - 100) = 30, which should be <= (30 + 0.001)
        let rows = store.rows(130.0, 30.0);
        assert_eq!(
            rows.len(),
            1,
            "entry at exactly TTL boundary should be kept due to epsilon tolerance"
        );
    }

    #[test]
    fn rows_drops_just_beyond_ttl_boundary() {
        let store = RemoteStore::new();
        store.merge(
            from_session_rows(&[row("a", "p", Status::Idle)], "h"),
            100.0,
        );
        // 30.01s later with a 30s TTL: (130.01 - 100) = 30.01 > (30 + 0.001)
        let rows = store.rows(130.01, 30.0);
        assert!(
            rows.is_empty(),
            "entry beyond TTL boundary should be dropped"
        );
    }
}
