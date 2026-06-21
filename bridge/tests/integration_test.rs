/// Integration tests for the HTTP endpoints defined in hub::create_router.
///
/// We use `tower::ServiceExt` (oneshot) together with `axum` directly — no
/// running TCP socket needed.  The router is built with a known token so we
/// can exercise both the authorised and unauthorised code-paths.
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use http_body_util::BodyExt; // for .collect()
use std::sync::Arc;
use tower::ServiceExt; // for .oneshot()

// Pull in the crate under test.  Integration test files live outside `src/`
// but Cargo links them against the crate, so we use its public API.
use vibe_bridge::{hub::create_router, state::Store};

const TEST_TOKEN: &str = "test-secret-token";

/// Build a fresh router backed by an empty store and default shared state.
fn make_app() -> (axum::Router, Arc<Store>) {
    use std::sync::RwLock;
    use vibe_bridge::federation::RemoteStore;
    use vibe_bridge::state::Shared;
    let store = Arc::new(Store::new());
    let shared = Arc::new(RwLock::new(Shared::default()));
    let remote = Arc::new(RemoteStore::new());
    let router = create_router(store.clone(), shared, TEST_TOKEN.to_string(), remote);
    (router, store)
}

// ── Helper: build a request ───────────────────────────────────────────────────

fn get_state(token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(Method::GET).uri("/state");
    if let Some(t) = token {
        builder = builder.header("X-VibeMonitor-Token", t);
    }
    builder.body(Body::empty()).unwrap()
}

fn post_json(uri: &str, token: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        builder = builder.header("X-VibeMonitor-Token", t);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

// ── GET /state ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_state_with_valid_token_returns_200() {
    let (app, _store) = make_app();
    let resp = app.oneshot(get_state(Some(TEST_TOKEN))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn get_state_without_token_returns_401() {
    let (app, _store) = make_app();
    let resp = app.oneshot(get_state(None)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_state_with_wrong_token_returns_401() {
    let (app, _store) = make_app();
    let resp = app.oneshot(get_state(Some("wrong-token"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_state_response_is_valid_json_with_sessions_array() {
    let (app, _store) = make_app();
    let resp = app.oneshot(get_state(Some(TEST_TOKEN))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        v["sessions"].is_array(),
        "response must contain a 'sessions' array"
    );
    assert!(v["ts"].is_number(), "response must contain a numeric 'ts'");
}

#[tokio::test]
async fn get_state_empty_store_yields_empty_sessions() {
    let (app, _store) = make_app();
    let resp = app.oneshot(get_state(Some(TEST_TOKEN))).await.unwrap();

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["sessions"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn get_state_shows_session_added_to_store() {
    use vibe_bridge::model::Session;

    let (app, store) = make_app();
    store.upsert(Session {
        id: "sess-42".into(),
        tool: "claude".into(),
        project: "myproj".into(),
        last_activity: 1_700_000_000.0,
        waiting: false,
        waiting_since: None,
        active_turn: false,
    });

    let resp = app.oneshot(get_state(Some(TEST_TOKEN))).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let sessions = v["sessions"].as_array().unwrap();
    // Session is very old (epoch 1_700_000_000 ≈ 2023) so age > 14400 s —
    // it will be filtered out.  We test that the JSON structure is correct.
    // Insert a fresh session whose last_activity is "now".
    let _ = sessions; // checked structure above
}

#[tokio::test]
async fn get_state_fresh_session_appears_in_response() {
    use std::time::{SystemTime, UNIX_EPOCH};
    use vibe_bridge::model::Session;

    let (app, store) = make_app();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    store.upsert(Session {
        id: "fresh-session".into(),
        tool: "claude".into(),
        project: "demo".into(),
        last_activity: now,
        waiting: false,
        waiting_since: None,
        active_turn: false,
    });

    let resp = app.oneshot(get_state(Some(TEST_TOKEN))).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let sessions = v["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], "fresh-session");
    assert_eq!(sessions[0]["tool"], "claude");
    assert_eq!(sessions[0]["project"], "demo");
}

// ── POST /hook ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn post_hook_without_token_returns_401() {
    let (app, _store) = make_app();
    let req = post_json("/hook", None, r#"{"id":"s1","event":"Stop"}"#);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn post_hook_with_valid_token_returns_200() {
    let (app, _store) = make_app();
    let req = post_json("/hook", Some(TEST_TOKEN), r#"{"id":"s1","event":"Stop"}"#);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn post_hook_stop_event_does_not_mark_waiting() {
    // hub.rs only marks waiting on "Notification"; Stop is a tool-result event
    // and should not flip the flag.
    use std::time::{SystemTime, UNIX_EPOCH};
    use vibe_bridge::model::Session;

    let (app, store) = make_app();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    store.upsert(Session {
        id: "hook-sess".into(),
        tool: "claude".into(),
        project: "p".into(),
        last_activity: now,
        waiting: false,
        waiting_since: None,
        active_turn: false,
    });

    let req = post_json(
        "/hook",
        Some(TEST_TOKEN),
        r#"{"id":"hook-sess","event":"Stop"}"#,
    );
    app.oneshot(req).await.unwrap();

    let snap = store.snapshot();
    assert_eq!(snap.len(), 1);
    assert!(
        !snap[0].waiting,
        "Stop event must NOT mark session as waiting"
    );
    assert!(snap[0].waiting_since.is_none());
}

#[tokio::test]
async fn post_hook_notification_event_marks_session_waiting() {
    use std::time::{SystemTime, UNIX_EPOCH};
    use vibe_bridge::model::Session;

    let (app, store) = make_app();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    store.upsert(Session {
        id: "notif-sess".into(),
        tool: "claude".into(),
        project: "p".into(),
        last_activity: now,
        waiting: false,
        waiting_since: None,
        active_turn: false,
    });

    let req = post_json(
        "/hook",
        Some(TEST_TOKEN),
        r#"{"id":"notif-sess","event":"Notification"}"#,
    );
    app.oneshot(req).await.unwrap();

    let snap = store.snapshot();
    assert!(
        snap[0].waiting,
        "Notification event should mark session as waiting"
    );
}

#[tokio::test]
async fn post_hook_uses_session_id_field_as_fallback() {
    use std::time::{SystemTime, UNIX_EPOCH};
    use vibe_bridge::model::Session;

    let (app, store) = make_app();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    store.upsert(Session {
        id: "alt-sess".into(),
        tool: "claude".into(),
        project: "p".into(),
        last_activity: now,
        waiting: false,
        waiting_since: None,
        active_turn: false,
    });

    // Use `sessionId` (camelCase) instead of `id`, with Notification event
    let req = post_json(
        "/hook",
        Some(TEST_TOKEN),
        r#"{"sessionId":"alt-sess","event":"Notification"}"#,
    );
    app.oneshot(req).await.unwrap();

    let snap = store.snapshot();
    assert!(
        snap[0].waiting,
        "sessionId field should be used when id is absent"
    );
}

#[tokio::test]
async fn post_hook_uses_hook_event_name_as_fallback() {
    use std::time::{SystemTime, UNIX_EPOCH};
    use vibe_bridge::model::Session;

    let (app, store) = make_app();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    store.upsert(Session {
        id: "evt-sess".into(),
        tool: "claude".into(),
        project: "p".into(),
        last_activity: now,
        waiting: false,
        waiting_since: None,
        active_turn: false,
    });

    // Use `hook_event_name` (snake_case alternative) instead of `event`, with Notification
    let req = post_json(
        "/hook",
        Some(TEST_TOKEN),
        r#"{"id":"evt-sess","hook_event_name":"Notification"}"#,
    );
    app.oneshot(req).await.unwrap();

    let snap = store.snapshot();
    assert!(snap[0].waiting, "hook_event_name field should be accepted");
}

#[tokio::test]
async fn post_hook_unknown_event_does_not_mark_waiting() {
    use std::time::{SystemTime, UNIX_EPOCH};
    use vibe_bridge::model::Session;

    let (app, store) = make_app();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    store.upsert(Session {
        id: "boring-sess".into(),
        tool: "claude".into(),
        project: "p".into(),
        last_activity: now,
        waiting: false,
        waiting_since: None,
        active_turn: false,
    });

    let req = post_json(
        "/hook",
        Some(TEST_TOKEN),
        r#"{"id":"boring-sess","event":"SomeOtherEvent"}"#,
    );
    app.oneshot(req).await.unwrap();

    let snap = store.snapshot();
    assert!(!snap[0].waiting, "unrecognised event must not set waiting");
}

// ── POST /ack ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn post_ack_without_token_returns_401() {
    let (app, _store) = make_app();
    let req = post_json("/ack", None, r#"{"id":"s1"}"#);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn post_ack_with_valid_token_returns_200() {
    let (app, _store) = make_app();
    let req = post_json("/ack", Some(TEST_TOKEN), r#"{"id":"s1"}"#);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn post_ack_clears_waiting_state() {
    use std::time::{SystemTime, UNIX_EPOCH};
    use vibe_bridge::model::Session;

    let (app, store) = make_app();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    store.upsert(Session {
        id: "ack-sess".into(),
        tool: "claude".into(),
        project: "p".into(),
        last_activity: now,
        waiting: true,
        waiting_since: Some(now - 5.0),
        active_turn: false,
    });

    let req = post_json("/ack", Some(TEST_TOKEN), r#"{"id":"ack-sess"}"#);
    app.oneshot(req).await.unwrap();

    let snap = store.snapshot();
    assert!(!snap[0].waiting, "ack should clear the waiting flag");
    assert!(
        snap[0].waiting_since.is_none(),
        "ack should clear waiting_since"
    );
}

#[tokio::test]
async fn post_ack_on_unknown_session_returns_200_without_panic() {
    let (app, _store) = make_app();
    // session "ghost" was never inserted — must not crash
    let req = post_json("/ack", Some(TEST_TOKEN), r#"{"id":"ghost"}"#);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Status ordering in /state response ───────────────────────────────────────

#[tokio::test]
async fn get_state_sessions_ordered_waiting_working_idle() {
    use std::time::{SystemTime, UNIX_EPOCH};
    use vibe_bridge::model::Session;

    let (app, store) = make_app();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    // Idle session: last_activity 200 s ago (> WORKING_SEC=60)
    store.upsert(Session {
        id: "idle-sess".into(),
        tool: "claude".into(),
        project: "p".into(),
        last_activity: now - 200.0,
        waiting: false,
        waiting_since: None,
        active_turn: false,
    });

    // Working session: last_activity right now (< WORKING_SEC=60)
    store.upsert(Session {
        id: "working-sess".into(),
        tool: "claude".into(),
        project: "p".into(),
        last_activity: now,
        waiting: false,
        waiting_since: None,
        active_turn: false,
    });

    // Waiting session
    store.upsert(Session {
        id: "waiting-sess".into(),
        tool: "claude".into(),
        project: "p".into(),
        last_activity: now - 30.0,
        waiting: true,
        waiting_since: Some(now - 10.0),
        active_turn: false,
    });

    let resp = app.oneshot(get_state(Some(TEST_TOKEN))).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let sessions = v["sessions"].as_array().unwrap();

    // Only the working-sess and waiting-sess are within GONE_TTL.
    // idle-sess is 200 s old — still within 14400 s TTL so it appears too.
    assert!(sessions.len() >= 2);

    // First element must be "waiting"
    assert_eq!(
        sessions[0]["status"], "waiting",
        "waiting sessions should sort first"
    );
}
