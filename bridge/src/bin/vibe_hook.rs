/// vibe_hook — the per-event hook process that Claude Code invokes.
///
/// Claude Code fires lifecycle hooks (UserPromptSubmit, PreToolUse, PostToolUse,
/// Stop, Notification, SessionStart) by spawning the configured command and
/// piping a JSON object on STDIN. That JSON contains fields like
/// `hook_event_name`, `session_id`, `cwd`, and `transcript_path`.
///
/// This process reads that payload, builds the body the hub's POST /hook handler
/// expects ({id, project, tool, event, ts}), and POSTs it to <hub>/hook with the
/// `X-VibeMonitor-Token` header. The hub uses it to mark a session "waiting" on
/// Notification, making session status event-driven instead of mtime-derived.
///
/// HARD RULE: a monitoring hook must NEVER break the user's Claude Code session.
/// We use a short timeout, swallow every error, and always exit 0 quickly.
///
/// Configuration (precedence: CLI flags > env > config.toml > defaults):
///   --url   / VIBE_HUB_URL   base hub url   (default http://localhost:5151)
///   --token / VIBE_TOKEN     auth token     (default empty)
///   --config <path>          a bridge config.toml to read token/host/port from
///
/// The registered command (written by install_hooks) passes
/// `--url <base> --token <tok>`; env + config.toml are fallbacks for manual use.
use std::{
    io::Read,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const DEFAULT_URL: &str = "http://localhost:5151";
const HTTP_TIMEOUT: Duration = Duration::from_millis(1500);

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Last path component of cwd, used as the project label (mirrors the Python hook).
fn project_from_cwd(cwd: &str) -> String {
    let cwd = cwd.replace('\\', "/");
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        return "?".to_string();
    }
    trimmed.rsplit('/').next().unwrap_or("?").to_string()
}

/// Minimal config.toml reader: pulls token + (host, port) to build a base url.
/// Best-effort; any failure just yields None so other config sources win.
fn from_config(path: &str) -> Option<(Option<String>, Option<String>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let val: toml::Value = toml::from_str(&text).ok()?;
    let token = val
        .get("token")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());
    let url = match (
        val.get("host").and_then(|h| h.as_str()),
        val.get("port").and_then(|p| p.as_integer()),
    ) {
        (Some(host), Some(port)) => {
            // 0.0.0.0 means "bind all" on the server; from the client side talk to localhost.
            let host = if host == "0.0.0.0" { "127.0.0.1" } else { host };
            Some(format!("http://{host}:{port}"))
        }
        _ => None,
    };
    Some((token, url))
}

fn main() {
    // Never let a panic surface as a non-zero exit that disrupts Claude Code.
    let _ = std::panic::catch_unwind(run);
    std::process::exit(0);
}

fn run() {
    // ── parse args ──────────────────────────────────────────────────────────
    let mut url: Option<String> = None;
    let mut token: Option<String> = None;
    let mut config_path: Option<String> = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--url" => {
                url = args.get(i + 1).cloned();
                i += 2;
            }
            "--token" => {
                token = args.get(i + 1).cloned();
                i += 2;
            }
            "--config" => {
                config_path = args.get(i + 1).cloned();
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    // ── resolve config: flags > env > config.toml > default ─────────────────
    if let Some(cfg) = config_path.as_deref().and_then(from_config) {
        let (cfg_token, cfg_url) = cfg;
        if token.is_none() {
            token = cfg_token;
        }
        if url.is_none() {
            url = cfg_url;
        }
    }
    let url = url
        .or_else(|| std::env::var("VIBE_HUB_URL").ok())
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    let token = token
        .or_else(|| std::env::var("VIBE_TOKEN").ok())
        .unwrap_or_default();

    // ── read the hook payload Claude Code pipes on stdin ────────────────────
    let mut stdin_buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin_buf);
    let payload: serde_json::Value =
        serde_json::from_str(stdin_buf.trim()).unwrap_or(serde_json::Value::Null);

    let get_str = |keys: &[&str]| -> Option<String> {
        for k in keys {
            if let Some(s) = payload.get(*k).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
        None
    };

    let event =
        get_str(&["hook_event_name", "hookEventName"]).unwrap_or_else(|| "Stop".to_string());
    let id = get_str(&["session_id", "sessionId"]).unwrap_or_else(|| "unknown".to_string());
    let cwd = get_str(&["cwd"]).unwrap_or_default();
    let project = project_from_cwd(&cwd);

    // Body shape the hub's HookBody understands ({id, event} are what it reads;
    // project/tool/ts are sent for parity with the Python hook + forward-compat).
    let body = serde_json::json!({
        "id": id,
        "project": project,
        "tool": "claude",
        "event": event,
        "ts": now_secs(),
    });

    // ── POST to <url>/hook, swallowing every error ──────────────────────────
    let endpoint = format!("{}/hook", url.trim_end_matches('/'));
    let _ = ureq::post(&endpoint)
        .set("Content-Type", "application/json")
        .set("X-VibeMonitor-Token", &token)
        .timeout(HTTP_TIMEOUT)
        .send_string(&body.to_string());
    // Intentionally ignore the result: monitoring must never disrupt Claude Code.
}
