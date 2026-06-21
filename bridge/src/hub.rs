use crate::{
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

const WORKING_SEC: f64 = 60.0;
const GONE_TTL: f64 = 14400.0;

fn now_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

fn check_token(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get("X-VibeMonitor-Token")
        .and_then(|v| v.to_str().ok())
        .map(|t| t == token)
        .unwrap_or(false)
}

fn status_rank(s: &Status) -> u8 {
    match s { Status::Waiting => 0, Status::Working => 1, Status::Idle => 2 }
}

#[derive(Clone)]
struct AppState {
    store: Arc<Store>,
    shared: Arc<RwLock<Shared>>,
    token: String,
}

pub fn create_router(store: Arc<Store>, shared: Arc<RwLock<Shared>>, token: String) -> Router {
    let state = AppState { store, shared, token };
    Router::new()
        .route("/state", get(state_handler))
        .route("/ack", post(ack_handler))
        .route("/hook", post(hook_handler))
        .with_state(state)
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

async fn state_handler(State(app): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !check_token(&headers, &app.token) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error":"unauthorized"})),
        ).into_response();
    }

    let now = now_secs();
    let sessions = app.store.snapshot();
    let last_scan = app.store.last_scan();

    // 1) derive status per live session
    let mut derived: Vec<Derived> = Vec::new();
    for s in sessions {
        let age = now - s.last_activity;
        if age > GONE_TTL { continue; }
        let status = if s.waiting {
            Status::Waiting
        } else if s.active_turn || age < WORKING_SEC {
            Status::Working          // active_turn keeps it working across mtime staleness
        } else {
            Status::Idle
        };
        let waiting_sec = if s.waiting {
            s.waiting_since.map(|ws| (now - ws).max(0.0) as u64)
        } else {
            None
        };
        derived.push(Derived {
            id: s.id, tool: s.tool, project: s.project, status,
            age, waiting: s.waiting, waiting_sec,
        });
    }

    // 2) dedupe by (tool, project)
    let mut groups: HashMap<(String, String), Vec<Derived>> = HashMap::new();
    for d in derived {
        groups.entry((d.tool.clone(), d.project.clone())).or_default().push(d);
    }

    let shared = app.shared.read().unwrap();
    let mut rows: Vec<SessionRow> = Vec::new();
    for ((tool, project), grp) in groups {
        // best (most important) status; smallest age; rep id prefers a waiting one
        let best_rank = grp.iter().map(|d| status_rank(&d.status)).min().unwrap_or(2);
        let status = match best_rank { 0 => Status::Waiting, 1 => Status::Working, _ => Status::Idle };
        let age = grp.iter().map(|d| d.age).fold(f64::INFINITY, f64::min);
        let waiting = grp.iter().any(|d| d.waiting);
        let rep = grp.iter()
            .filter(|d| d.waiting)
            .min_by(|a, b| a.age.partial_cmp(&b.age).unwrap_or(std::cmp::Ordering::Equal))
            .or_else(|| grp.iter().min_by(|a, b| a.age.partial_cmp(&b.age).unwrap_or(std::cmp::Ordering::Equal)))
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
        status_rank(&a.status).cmp(&status_rank(&b.status))
            .then(a.age_sec.cmp(&b.age_sec))
            .then(a.tool.cmp(&b.tool))
            .then(a.project.cmp(&b.project))
    });

    // 3) counts for capacity
    let (mut working, mut waiting, mut idle) = (0usize, 0usize, 0usize);
    for r in &rows {
        match r.status { Status::Working => working += 1, Status::Waiting => waiting += 1, Status::Idle => idle += 1 }
    }
    let cap: Capacity = usage::capacity(&shared.claude_usage, &shared.codex_usage, working, waiting, idle);

    let stale_sec = if last_scan > 0.0 { (now - last_scan) as i64 } else { -1 };

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

#[derive(Deserialize)]
struct AckBody { id: String }

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
