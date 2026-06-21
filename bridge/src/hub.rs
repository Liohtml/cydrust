use crate::{
    model::{SessionRow, StateResponse, Status, UsageBlock, UsageInfo},
    state::Store,
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
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

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
        .map(|t| t == token)
        .unwrap_or(false)
}

#[derive(Clone)]
struct AppState {
    store: Arc<Store>,
    token: String,
}

pub fn create_router(store: Arc<Store>, token: String) -> Router {
    let state = AppState { store, token };
    Router::new()
        .route("/state", get(state_handler))
        .route("/ack", post(ack_handler))
        .route("/hook", post(hook_handler))
        .with_state(state)
}

async fn state_handler(
    State(app): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !check_token(&headers, &app.token) {
        return (StatusCode::UNAUTHORIZED, axum::Json(serde_json::json!({"error":"unauthorized"}))).into_response();
    }

    let now = now_secs();
    let sessions = app.store.snapshot();
    let last_scan = app.store.last_scan();

    const WORKING_SEC: f64 = 60.0;
    const GONE_TTL: f64 = 14400.0;

    let mut rows: Vec<SessionRow> = sessions
        .into_iter()
        .filter_map(|s| {
            let age = now - s.last_activity;
            if age > GONE_TTL { return None; }
            let status = if s.waiting {
                Status::Waiting
            } else if age < WORKING_SEC {
                Status::Working
            } else {
                Status::Idle
            };
            let waiting_sec = if s.waiting {
                s.waiting_since.map(|ws| (now - ws) as u64)
            } else {
                None
            };
            Some(SessionRow {
                id: s.id,
                tool: s.tool,
                project: s.project,
                status,
                age_sec: age as u64,
                waiting: s.waiting,
                waiting_sec,
            })
        })
        .collect();

    rows.sort_by_key(|r| match r.status {
        Status::Waiting => 0,
        Status::Working => 1,
        Status::Idle => 2,
    });

    let stale_sec = if last_scan > 0.0 { (now - last_scan) as i64 } else { -1 };

    let resp = StateResponse {
        ts: now as i64,
        sessions: rows,
        usage: UsageBlock {
            claude: UsageInfo { ok: false, pct: None, reset_sec: None },
            codex:  UsageInfo { ok: false, pct: None, reset_sec: None },
        },
        stale_sec,
    };

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
    if matches!(event.as_str(), "Notification" | "Stop") && !id.is_empty() {
        app.store.mark_waiting(&id, now_secs());
    }
    StatusCode::OK
}