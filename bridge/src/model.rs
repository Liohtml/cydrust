use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Working,
    Idle,
    Waiting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub tool: String,
    pub project: String,
    pub last_activity: f64,  // epoch seconds
    pub waiting: bool,
    pub waiting_since: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub id: String,
    pub tool: String,
    pub project: String,
    pub status: Status,
    #[serde(rename = "ageSec")]
    pub age_sec: u64,
    pub waiting: bool,
    #[serde(rename = "waitingSec")]
    pub waiting_sec: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageInfo {
    pub ok: bool,
    pub pct: Option<f64>,
    #[serde(rename = "resetSec")]
    pub reset_sec: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateResponse {
    pub ts: i64,
    pub sessions: Vec<SessionRow>,
    pub usage: UsageBlock,
    #[serde(rename = "staleSec")]
    pub stale_sec: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageBlock {
    pub claude: UsageInfo,
    pub codex: UsageInfo,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Status ────────────────────────────────────────────────────────────────

    #[test]
    fn status_serialises_to_lowercase() {
        assert_eq!(serde_json::to_string(&Status::Working).unwrap(), r#""working""#);
        assert_eq!(serde_json::to_string(&Status::Idle).unwrap(),    r#""idle""#);
        assert_eq!(serde_json::to_string(&Status::Waiting).unwrap(), r#""waiting""#);
    }

    #[test]
    fn status_deserialises_from_lowercase() {
        let w: Status = serde_json::from_str(r#""working""#).unwrap();
        let i: Status = serde_json::from_str(r#""idle""#).unwrap();
        let wt: Status = serde_json::from_str(r#""waiting""#).unwrap();
        assert_eq!(w,  Status::Working);
        assert_eq!(i,  Status::Idle);
        assert_eq!(wt, Status::Waiting);
    }

    #[test]
    fn status_roundtrip_equality() {
        for s in [Status::Working, Status::Idle, Status::Waiting] {
            let json = serde_json::to_string(&s).unwrap();
            let back: Status = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    // ── Session ───────────────────────────────────────────────────────────────

    fn make_session() -> Session {
        Session {
            id:            "abc-123".into(),
            tool:          "claude".into(),
            project:       "myproject".into(),
            last_activity: 1_700_000_000.0,
            waiting:       false,
            waiting_since: None,
        }
    }

    #[test]
    fn session_serialise_contains_expected_fields() {
        let s = make_session();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["id"],            "abc-123");
        assert_eq!(v["tool"],          "claude");
        assert_eq!(v["project"],       "myproject");
        assert_eq!(v["waiting"],       false);
        assert!(v["waiting_since"].is_null());
    }

    #[test]
    fn session_roundtrip_no_waiting() {
        let original = make_session();
        let json = serde_json::to_string(&original).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id,            original.id);
        assert_eq!(back.tool,          original.tool);
        assert_eq!(back.project,       original.project);
        assert_eq!(back.last_activity, original.last_activity);
        assert_eq!(back.waiting,       original.waiting);
        assert_eq!(back.waiting_since, original.waiting_since);
    }

    #[test]
    fn session_roundtrip_with_waiting() {
        let mut s = make_session();
        s.waiting       = true;
        s.waiting_since = Some(1_700_000_100.5);
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert!(back.waiting);
        assert!((back.waiting_since.unwrap() - 1_700_000_100.5).abs() < 1e-6);
    }

    // ── SessionRow ────────────────────────────────────────────────────────────

    #[test]
    fn session_row_age_sec_renamed_in_json() {
        let row = SessionRow {
            id:          "r1".into(),
            tool:        "claude".into(),
            project:     "proj".into(),
            status:      Status::Working,
            age_sec:     42,
            waiting:     false,
            waiting_sec: None,
        };
        let v = serde_json::to_value(&row).unwrap();
        // field renamed via #[serde(rename = "ageSec")]
        assert_eq!(v["ageSec"], 42);
        assert!(!v.as_object().unwrap().contains_key("age_sec"),
            "raw snake_case key must not appear");
    }

    #[test]
    fn session_row_waiting_sec_renamed_in_json() {
        let row = SessionRow {
            id:          "r2".into(),
            tool:        "claude".into(),
            project:     "proj".into(),
            status:      Status::Waiting,
            age_sec:     10,
            waiting:     true,
            waiting_sec: Some(30),
        };
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["waitingSec"], 30);
        assert!(!v.as_object().unwrap().contains_key("waiting_sec"),
            "raw snake_case key must not appear");
    }

    // ── UsageInfo / UsageBlock ────────────────────────────────────────────────

    #[test]
    fn usage_info_ok_false_pct_none() {
        let u = UsageInfo { ok: false, pct: None, reset_sec: None };
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v["ok"], false);
        assert!(v["pct"].is_null());
        assert!(v["resetSec"].is_null());
    }

    #[test]
    fn usage_info_reset_sec_renamed() {
        let u = UsageInfo { ok: true, pct: Some(0.75), reset_sec: Some(120) };
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v["resetSec"], 120);
        assert!(!v.as_object().unwrap().contains_key("reset_sec"));
    }

    // ── StateResponse ─────────────────────────────────────────────────────────

    #[test]
    fn state_response_stale_sec_renamed() {
        let resp = StateResponse {
            ts:        1_700_000_000,
            sessions:  vec![],
            usage:     UsageBlock {
                claude: UsageInfo { ok: false, pct: None, reset_sec: None },
                codex:  UsageInfo { ok: false, pct: None, reset_sec: None },
            },
            stale_sec: 5,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["staleSec"], 5);
        assert!(!v.as_object().unwrap().contains_key("stale_sec"));
    }
}