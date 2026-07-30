//! Black-box tests for the `vibe_hook` binary.
//!
//! `vibe_hook` is a tiny process spawned by Claude Code on each lifecycle
//! event: it reads a JSON payload on stdin, builds a small JSON body, and
//! POSTs it to `<hub-url>/hook`. Its hard rule is "never disrupt the calling
//! Claude Code session" — every error is swallowed and the process always
//! exits 0.
//!
//! Running the real hub here would pull in axum/tokio + the token-comparison
//! logic tested elsewhere (`integration_test.rs`); that's unnecessary for what
//! this file wants to prove: that the *hook process itself* behaves — correct
//! payload shape, correct header, and (most importantly) that it can never
//! hang or fail the session even when nothing is listening. So instead of a
//! full hub we spin up a minimal raw `TcpListener` that just captures the raw
//! HTTP request, and drive `vibe_hook` as a subprocess (via
//! `env!("CARGO_BIN_EXE_vibe_hook")`, which Cargo provides to integration
//! tests for binaries in the same package).

use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Stdio},
    time::Duration,
};

/// A captured raw HTTP request (request line + headers + body), parsed just
/// enough to assert on.
struct CapturedRequest {
    raw: String,
    body: serde_json::Value,
}

impl CapturedRequest {
    fn header(&self, name: &str) -> Option<String> {
        let needle = format!("{name}:");
        self.raw.lines().find_map(|line| {
            if line
                .to_ascii_lowercase()
                .starts_with(&needle.to_ascii_lowercase())
            {
                Some(line.split_once(':')?.1.trim().to_string())
            } else {
                None
            }
        })
    }
}

/// Start a one-shot mini HTTP "server": accept exactly one connection, read
/// the full request (headers + Content-Length body), reply `200 OK`, and hand
/// the parsed request back over the returned join handle. Returns the bound
/// port.
fn spawn_mini_server() -> (u16, std::thread::JoinHandle<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(false).unwrap();

    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        // Read until we've seen the header terminator, then read the declared
        // Content-Length body (vibe_hook always sends Content-Length, never
        // chunked encoding).
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break None;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                break Some(pos + 4);
            }
        };

        let header_end = header_end.unwrap_or(buf.len());
        let header_text = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let content_length: usize = header_text
            .lines()
            .find_map(|l| {
                let lower = l.to_ascii_lowercase();
                lower
                    .strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse().ok())
            })
            .unwrap_or(0);

        while buf.len() - header_end < content_length {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }

        let body_bytes = &buf[header_end..(header_end + content_length).min(buf.len())];
        let body: serde_json::Value =
            serde_json::from_slice(body_bytes).unwrap_or(serde_json::Value::Null);

        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");

        CapturedRequest {
            raw: header_text,
            body,
        }
    });

    (port, handle)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn run_vibe_hook(url: &str, token: Option<&str>, stdin_payload: &str) -> std::process::ExitStatus {
    let bin = env!("CARGO_BIN_EXE_vibe_hook");
    let mut cmd = Command::new(bin);
    cmd.arg("--url")
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(t) = token {
        cmd.env("VIBE_MONITOR_TOKEN", t);
    } else {
        cmd.env_remove("VIBE_MONITOR_TOKEN");
        cmd.env_remove("VIBE_TOKEN");
    }
    let mut child = cmd.spawn().expect("failed to spawn vibe_hook binary");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin_payload.as_bytes())
        .unwrap();
    child.wait().expect("vibe_hook did not exit")
}

// ── payload shape ─────────────────────────────────────────────────────────────

#[test]
fn posts_expected_body_and_token_header_for_a_notification_event() {
    let (port, handle) = spawn_mini_server();
    let url = format!("http://127.0.0.1:{port}");

    let stdin_payload = serde_json::json!({
        "hook_event_name": "Notification",
        "session_id": "sess-abc123",
        "cwd": "/home/dev/projects/my-repo",
    })
    .to_string();

    let status = run_vibe_hook(&url, Some("secret-token"), &stdin_payload);
    assert!(status.success(), "vibe_hook must always exit 0");

    let req = handle.join().expect("server thread panicked");
    assert!(
        req.raw.starts_with("POST /hook"),
        "raw request: {}",
        req.raw
    );
    assert_eq!(
        req.header("x-vibemonitor-token").as_deref(),
        Some("secret-token")
    );
    assert_eq!(req.body["id"], "sess-abc123");
    assert_eq!(req.body["project"], "my-repo", "project = last cwd segment");
    assert_eq!(req.body["tool"], "claude");
    assert_eq!(req.body["event"], "Notification");
    assert!(req.body["ts"].is_number());
}

#[test]
fn accepts_camelcase_field_fallbacks() {
    let (port, handle) = spawn_mini_server();
    let url = format!("http://127.0.0.1:{port}");

    let stdin_payload = serde_json::json!({
        "hookEventName": "Stop",
        "sessionId": "sess-camel",
        "cwd": "/w/camel-project",
    })
    .to_string();

    let status = run_vibe_hook(&url, Some("t"), &stdin_payload);
    assert!(status.success());

    let req = handle.join().unwrap();
    assert_eq!(req.body["id"], "sess-camel");
    assert_eq!(req.body["event"], "Stop");
    assert_eq!(req.body["project"], "camel-project");
}

// ── robustness: malformed / missing input must never disrupt the caller ──────

#[test]
fn malformed_stdin_json_still_exits_zero_and_posts_defaults() {
    let (port, handle) = spawn_mini_server();
    let url = format!("http://127.0.0.1:{port}");

    let status = run_vibe_hook(&url, Some("t"), "not valid json {{{");
    assert!(
        status.success(),
        "malformed stdin must never fail the hook process"
    );

    let req = handle.join().unwrap();
    // event_name/session_id/cwd all fall back to their documented defaults.
    assert_eq!(req.body["id"], "unknown");
    assert_eq!(req.body["event"], "Stop");
    assert_eq!(req.body["project"], "?");
}

#[test]
fn empty_stdin_still_exits_zero() {
    let (port, handle) = spawn_mini_server();
    let url = format!("http://127.0.0.1:{port}");

    let status = run_vibe_hook(&url, None, "");
    assert!(status.success());

    let req = handle.join().unwrap();
    assert_eq!(req.body["id"], "unknown");
}

#[test]
fn missing_token_still_posts_with_empty_token_header() {
    let (port, handle) = spawn_mini_server();
    let url = format!("http://127.0.0.1:{port}");

    let stdin_payload =
        serde_json::json!({"hook_event_name": "Stop", "session_id": "s1"}).to_string();
    let status = run_vibe_hook(&url, None, &stdin_payload);
    assert!(status.success());

    let req = handle.join().unwrap();
    assert_eq!(req.header("x-vibemonitor-token"), Some(String::new()));
}

/// The hard rule: even when nothing is listening at all (connection refused),
/// vibe_hook must swallow the error and exit 0 promptly rather than hang or
/// crash the calling Claude Code session.
#[test]
fn no_server_listening_still_exits_zero_promptly() {
    // Bind to grab a free port, then drop the listener so the port is very
    // likely unused but nothing answers on it -> connection refused.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let url = format!("http://127.0.0.1:{port}");

    let stdin_payload =
        serde_json::json!({"hook_event_name": "Stop", "session_id": "s1"}).to_string();

    let start = std::time::Instant::now();
    let status = run_vibe_hook(&url, Some("t"), &stdin_payload);
    let elapsed = start.elapsed();

    assert!(status.success(), "must exit 0 even with no listener");
    assert!(
        elapsed < Duration::from_secs(5),
        "must not hang waiting on a dead endpoint (took {elapsed:?})"
    );
}
