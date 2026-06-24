use crate::{
    federation::{FedPayload, RemoteStore},
    model::{Capacity, SessionRow, StateResponse, Status, UsageBlock},
    state::{Shared, Store},
    usage,
};
use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;

const WORKING_SEC: f64 = 60.0;
const GONE_TTL: f64 = 14400.0;

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn check_token(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get("X-VibeMonitor-Token")
        .and_then(|v| v.to_str().ok())
        // Constant-time comparison to avoid leaking the token via a timing
        // oracle. `ct_eq` returns Choice(0) for mismatched lengths.
        .map(|t| bool::from(t.as_bytes().ct_eq(token.as_bytes())))
        .unwrap_or(false)
}

fn status_rank(s: &Status) -> u8 {
    match s {
        Status::Waiting => 0,
        Status::Working => 1,
        Status::Idle => 2,
    }
}

#[derive(Clone)]
struct AppState {
    store: Arc<Store>,
    shared: Arc<RwLock<Shared>>,
    token: String,
    remote: Arc<RemoteStore>,
}

pub fn create_router(
    store: Arc<Store>,
    shared: Arc<RwLock<Shared>>,
    token: String,
    remote: Arc<RemoteStore>,
) -> Router {
    let state = AppState {
        store,
        shared,
        token,
        remote,
    };
    Router::new()
        .route("/state", get(state_handler))
        .route("/metrics", get(metrics_handler)) // Prometheus scrape (requires token)
        .route("/ack", post(ack_handler))
        .route("/hook", post(hook_handler))
        .route("/federation/ingest", post(ingest_handler))
        .with_state(state)
}

/// Expose the dedupe logic so the federation node push-loop reuses it.
pub fn derive_rows_pub(store: &Store, shared: &Shared, now: f64) -> Vec<SessionRow> {
    derive_rows(store, shared, now)
}

// A derived row before dedupe.
struct Derived {
    id: String,
    tool: String,
    project: String,
    status: Status,
    age: f64,
    waiting: bool,
    waiting_sec: Option<u64>,
}

/// Derive + dedupe live sessions into display rows (shared by /state and /metrics).
fn derive_rows(store: &Store, shared: &Shared, now: f64) -> Vec<SessionRow> {
    let mut derived: Vec<Derived> = Vec::new();
    for s in store.snapshot() {
        let age = now - s.last_activity;
        if age > GONE_TTL {
            continue;
        }
        let status = if s.waiting {
            Status::Waiting
        } else if s.active_turn || age < WORKING_SEC {
            Status::Working // active_turn keeps it working across mtime staleness
        } else {
            Status::Idle
        };
        let waiting_sec = if s.waiting {
            s.waiting_since.map(|ws| (now - ws).max(0.0) as u64)
        } else {
            None
        };
        derived.push(Derived {
            id: s.id,
            tool: s.tool,
            project: s.project,
            status,
            age,
            waiting: s.waiting,
            waiting_sec,
        });
    }

    let mut groups: HashMap<(String, String), Vec<Derived>> = HashMap::new();
    for d in derived {
        groups
            .entry((d.tool.clone(), d.project.clone()))
            .or_default()
            .push(d);
    }

    let mut rows: Vec<SessionRow> = Vec::new();
    for ((tool, project), grp) in groups {
        let best_rank = grp
            .iter()
            .map(|d| status_rank(&d.status))
            .min()
            .unwrap_or(2);
        let status = match best_rank {
            0 => Status::Waiting,
            1 => Status::Working,
            _ => Status::Idle,
        };
        let age = grp.iter().map(|d| d.age).fold(f64::INFINITY, f64::min);
        let waiting = grp.iter().any(|d| d.waiting);
        let rep = grp
            .iter()
            .filter(|d| d.waiting)
            .min_by(|a, b| {
                a.age
                    .partial_cmp(&b.age)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .or_else(|| {
                grp.iter().min_by(|a, b| {
                    a.age
                        .partial_cmp(&b.age)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            })
            .unwrap();
        let waiting_sec = grp.iter().filter_map(|d| d.waiting_sec).max();
        let summary = shared.titles.get(&rep.id).cloned();
        rows.push(SessionRow {
            id: rep.id.clone(),
            tool,
            project,
            status,
            age_sec: age.max(0.0) as u64,
            waiting,
            waiting_sec,
            summary,
        });
    }
    rows.sort_by(|a, b| {
        status_rank(&a.status)
            .cmp(&status_rank(&b.status))
            .then(a.age_sec.cmp(&b.age_sec))
            .then(a.tool.cmp(&b.tool))
            .then(a.project.cmp(&b.project))
    });
    rows
}

fn count_statuses(rows: &[SessionRow]) -> (usize, usize, usize) {
    let (mut working, mut waiting, mut idle) = (0, 0, 0);
    for r in rows {
        match r.status {
            Status::Working => working += 1,
            Status::Waiting => waiting += 1,
            Status::Idle => idle += 1,
        }
    }
    (working, waiting, idle)
}

async fn state_handler(State(app): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !check_token(&headers, &app.token) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error":"unauthorized"})),
        )
            .into_response();
    }

    let now = now_secs();
    let last_scan = app.store.last_scan();
    let shared = app.shared.read().unwrap_or_else(|p| p.into_inner());
    let mut rows = derive_rows(&app.store, &shared, now);
    // capacity reflects THIS machine's usage + local session counts only
    let (working, waiting, idle) = count_statuses(&rows);
    let cap: Capacity = usage::capacity(
        &shared.claude_usage,
        &shared.codex_usage,
        working,
        waiting,
        idle,
    );
    // append sessions federated from other machines (project prefixed with node id)
    rows.extend(app.remote.rows(now, 30.0));
    let stale_sec = if last_scan > 0.0 {
        (now - last_scan) as i64
    } else {
        -1
    };

    let resp = StateResponse {
        ts: now as i64,
        sessions: rows,
        usage: UsageBlock {
            claude: shared.claude_usage.clone(),
            codex: shared.codex_usage.clone(),
            opencode: None,
            hermes: None,
            capacity: Some(cap.clone()),
        },
        stale_sec,
        capacity: cap,
        metrics: shared.metrics.clone(),
    };
    drop(shared);

    (StatusCode::OK, axum::Json(resp)).into_response()
}

/// Prometheus text-format exposition. Requires the bearer token, same as every
/// other endpoint — configure the scrape job with `bearer_token` (or an
/// `Authorization`/`X-VibeMonitor-Token` header) so usage and cost data is not
/// exposed unauthenticated on non-localhost binds.
async fn metrics_handler(State(app): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !check_token(&headers, &app.token) {
        return (StatusCode::UNAUTHORIZED, String::new()).into_response();
    }
    let now = now_secs();
    let last_scan = app.store.last_scan();
    let shared = app.shared.read().unwrap_or_else(|p| p.into_inner());
    let rows = derive_rows(&app.store, &shared, now);
    let (working, waiting, idle) = count_statuses(&rows);

    let mut o = String::new();
    o.push_str(
        "# HELP vibemonitor_up Hub liveness.\n# TYPE vibemonitor_up gauge\nvibemonitor_up 1\n",
    );

    o.push_str(
        "# HELP vibemonitor_sessions Sessions by status.\n# TYPE vibemonitor_sessions gauge\n",
    );
    o.push_str(&format!(
        "vibemonitor_sessions{{status=\"working\"}} {working}\n"
    ));
    o.push_str(&format!(
        "vibemonitor_sessions{{status=\"waiting\"}} {waiting}\n"
    ));
    o.push_str(&format!("vibemonitor_sessions{{status=\"idle\"}} {idle}\n"));

    let stale = if last_scan > 0.0 {
        now - last_scan
    } else {
        -1.0
    };
    o.push_str("# HELP vibemonitor_stale_seconds Seconds since the last session scan.\n# TYPE vibemonitor_stale_seconds gauge\n");
    o.push_str(&format!("vibemonitor_stale_seconds {stale:.0}\n"));

    o.push_str("# HELP vibemonitor_usage_ratio Provider usage as a fraction of the window.\n# TYPE vibemonitor_usage_ratio gauge\n");
    for (name, u) in [
        ("claude", &shared.claude_usage),
        ("codex", &shared.codex_usage),
    ] {
        if let Some(p) = u.pct {
            o.push_str(&format!(
                "vibemonitor_usage_ratio{{provider=\"{name}\"}} {p}\n"
            ));
        }
    }
    o.push_str("# HELP vibemonitor_usage_reset_seconds Seconds until the usage window resets.\n# TYPE vibemonitor_usage_reset_seconds gauge\n");
    for (name, u) in [
        ("claude", &shared.claude_usage),
        ("codex", &shared.codex_usage),
    ] {
        if let Some(r) = u.reset_sec {
            o.push_str(&format!(
                "vibemonitor_usage_reset_seconds{{provider=\"{name}\"}} {r}\n"
            ));
        }
    }

    o.push_str("# HELP vibemonitor_tokens_total Tokens today per provider+model.\n# TYPE vibemonitor_tokens_total gauge\n");
    o.push_str("# HELP vibemonitor_cost_usd Estimated USD today per provider+model.\n# TYPE vibemonitor_cost_usd gauge\n");
    for (prov, pm) in &shared.metrics.providers {
        for m in &pm.models {
            o.push_str(&format!(
                "vibemonitor_tokens_total{{provider=\"{prov}\",model=\"{}\"}} {}\n",
                m.model, m.tokens
            ));
            if let Some(u) = m.usd {
                o.push_str(&format!(
                    "vibemonitor_cost_usd{{provider=\"{prov}\",model=\"{}\"}} {}\n",
                    m.model, u
                ));
            }
        }
    }
    o.push_str("# HELP vibemonitor_tokens_total_all Total tokens today (all providers).\n# TYPE vibemonitor_tokens_total_all gauge\n");
    o.push_str(&format!(
        "vibemonitor_tokens_total_all {}\n",
        shared.metrics.totals.tokens
    ));
    if let Some(u) = shared.metrics.totals.usd {
        o.push_str("# HELP vibemonitor_cost_usd_all Total estimated USD today.\n# TYPE vibemonitor_cost_usd_all gauge\n");
        o.push_str(&format!("vibemonitor_cost_usd_all {u}\n"));
    }
    drop(shared);

    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        o,
    )
        .into_response()
}

/// Federation: a node POSTs its current sessions here; we merge them so this
/// aggregator's /state shows sessions from every machine.
async fn ingest_handler(
    State(app): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<FedPayload>,
) -> impl IntoResponse {
    if !check_token(&headers, &app.token) {
        return StatusCode::UNAUTHORIZED;
    }
    app.remote.merge(payload, now_secs());
    StatusCode::OK
}

#[derive(Deserialize)]
struct AckBody {
    id: String,
}

async fn ack_handler(
    State(app): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AckBody>,
) -> impl IntoResponse {
    if !check_token(&headers, &app.token) {
        return StatusCode::UNAUTHORIZED;
    }
    app.store.ack(&body.id);
    StatusCode::OK
}

#[derive(Deserialize)]
struct HookBody {
    id: Option<String>,
    event: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "hook_event_name")]
    hook_event_name: Option<String>,
}

async fn hook_handler(
    State(app): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<HookBody>,
) -> impl IntoResponse {
    if !check_token(&headers, &app.token) {
        return StatusCode::UNAUTHORIZED;
    }
    let id = body.id.or(body.session_id).unwrap_or_default();
    let event = body.event.or(body.hook_event_name).unwrap_or_default();
    if matches!(event.as_str(), "Notification") && !id.is_empty() {
        app.store.mark_waiting(&id, now_secs());
    }
    StatusCode::OK
}
