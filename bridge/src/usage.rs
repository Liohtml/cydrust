//! Usage / rate-limit telemetry, ported from VibeMonitor's Python `usage.py`
//! (`bridge/vibemonitor/usage.py`) and `usageanalytics.py`.
//!
//! Public API (called on a slow background poll by the integrator):
//!   * [`claude_usage`] — probe the Anthropic API with the Claude Code OAuth token
//!     and parse the unified 5h / 7d rate-limit headers.
//!   * [`codex_usage`]  — read the freshest Codex `token_count` rate-limit snapshot
//!     from `~/.codex/sessions`.
//!   * [`capacity`]     — go / pace / throttle verdict from current usage + counts.
//!
//! Nothing here panics: every fallible path collapses to
//! `UsageInfo { ok: false, ..Default::default() }`.
//!
//! ## Faithfulness notes / simplifications vs. Python
//! * The Anthropic request URL, auth scheme, `anthropic-version`,
//!   `anthropic-beta`, probe model, body, and the four rate-limit header names
//!   are ported verbatim (see the consts below). This is the load-bearing part.
//! * `spark`, multi-day `daily` history, and the least-squares `burnPerHr` in
//!   Python come from a persistent `UsageHistory` DB of samples across calls.
//!   This module is stateless (single API call per invocation), so:
//!     - `spark` is left empty (`vec![]`) for v1.
//!     - `burn_per_hr` / `leftover_pct` / `will_exhaust` / `eta_clock` are
//!       derived from the *single* reading using the same `project()` math as
//!       Python, but with `burn` estimated from elapsed-fraction-of-window
//!       (pct over time-elapsed-in-window) rather than a regression over
//!       historical samples. If burn can't be estimated, those fields are left
//!       `None` (matching Python's "not enough data" behaviour).

use crate::model::{Capacity, UsageInfo};

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone, Timelike};

// ── Anthropic probe constants (verbatim from usage.py) ───────────────────────
const MESSAGES_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const PROBE_MODEL: &str = "claude-haiku-4-5-20251001";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const USER_AGENT: &str = "vibemonitor/0.1";

// Unified rate-limit header names (verbatim from usage.py).
const H5U: &str = "anthropic-ratelimit-unified-5h-utilization";
const H5R: &str = "anthropic-ratelimit-unified-5h-reset";
const D7U: &str = "anthropic-ratelimit-unified-7d-utilization";
const D7R: &str = "anthropic-ratelimit-unified-7d-reset";

// The full duration of the unified 5h window, used to estimate elapsed fraction
// for a single-call burn estimate (see module note).
const WINDOW_5H_SECS: f64 = 5.0 * 3600.0;

/// Current unix time in whole seconds (never panics; clamps to 0 pre-epoch).
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────────────────────
// OAuth token
// ─────────────────────────────────────────────────────────────────────────────

/// Best-effort read of the Claude Code OAuth access token from
/// `~/.claude/.credentials.json` (key `claudeAiOauth`, then `accessToken`
/// or `access_token`). Returns `None` on any problem. Mirrors
/// `read_claude_oauth_token` in usage.py (the Windows path is a plain file —
/// no keychain fallback needed).
fn read_claude_oauth_token() -> Option<String> {
    let path = credentials_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let data: serde_json::Value = serde_json::from_str(&text).ok()?;
    let oauth = data.get("claudeAiOauth")?;
    oauth
        .get("accessToken")
        .and_then(|v| v.as_str())
        .or_else(|| oauth.get("access_token").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

fn credentials_path() -> Option<PathBuf> {
    Some(
        dirs_next::home_dir()?
            .join(".claude")
            .join(".credentials.json"),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Claude usage (Anthropic API probe)
// ─────────────────────────────────────────────────────────────────────────────

/// Probe the Anthropic API with the OAuth token and parse the unified
/// rate-limit headers into a [`UsageInfo`]. `ok: false` on any failure
/// (no token, network error, missing headers).
pub fn claude_usage() -> UsageInfo {
    let token = match read_claude_oauth_token() {
        Some(t) if !t.is_empty() => t,
        _ => return UsageInfo::default(),
    };

    let headers = match probe_anthropic(&token) {
        Some(h) => h,
        None => return UsageInfo::default(),
    };

    let now = now_secs();
    let mut info = parse_claude_headers(&headers, now);
    if info.ok {
        attach_single_call_analytics(&mut info, now);
    }
    info
}

/// One header pair pulled off the HTTP response, lowercased name.
type Headers = Vec<(String, String)>;

fn header_get<'a>(headers: &'a Headers, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// POST the tiny probe message and return the response headers, whether the
/// response is a 2xx or a non-2xx status (the rate-limit headers ride on both,
/// and Python reads `resp.headers` regardless of status). `None` only on a
/// transport-level failure.
fn probe_anthropic(token: &str) -> Option<Headers> {
    // Serialize the probe body by hand (ureq's `send_json` needs the `json`
    // feature, which this crate doesn't enable — `send_string` + an explicit
    // content-type header is equivalent on the wire).
    let body = serde_json::json!({
        "model": PROBE_MODEL,
        "max_tokens": 1,
        "messages": [{ "role": "user", "content": "." }],
    });
    let body = serde_json::to_string(&body).ok()?;

    let req = ureq::post(MESSAGES_ENDPOINT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-version", ANTHROPIC_VERSION)
        .set("anthropic-beta", ANTHROPIC_BETA)
        .set("content-type", "application/json")
        .set("User-Agent", USER_AGENT)
        .timeout(std::time::Duration::from_secs(10));

    match req.send_string(&body) {
        Ok(resp) => Some(collect_headers(&resp)),
        // A non-2xx status still carries the rate-limit headers — use them,
        // exactly like Python which never inspects the status code.
        Err(ureq::Error::Status(_code, resp)) => Some(collect_headers(&resp)),
        // Transport / DNS / TLS / timeout: a genuine miss.
        Err(ureq::Error::Transport(_)) => None,
    }
}

fn collect_headers(resp: &ureq::Response) -> Headers {
    resp.headers_names()
        .into_iter()
        .filter_map(|name| resp.header(&name).map(|v| (name.clone(), v.to_string())))
        .collect()
}

/// Port of `parse_claude_headers`. Pulls the unified 5h utilization/reset and
/// 7d utilization/reset. `pct`/`week_pct` are fractions (0..1+) exactly as the
/// header reports them. `reset_sec` is seconds-until-reset (the 5h header is an
/// epoch seconds value); `week_reset_sec` is `epoch - now`, floored at 0.
fn parse_claude_headers(headers: &Headers, now: u64) -> UsageInfo {
    let u5 = match header_get(headers, H5U) {
        Some(v) => v,
        None => return UsageInfo::default(),
    };
    let pct = match u5.parse::<f64>() {
        Ok(p) => p,
        Err(_) => return UsageInfo::default(),
    };

    // 5h reset: epoch seconds → seconds remaining (floored at 0). Bad/absent → 0.
    let reset5_epoch: i64 = header_get(headers, H5R)
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let reset_sec = (reset5_epoch - now as i64).max(0) as u64;

    // 7d utilization (optional).
    let week_pct = header_get(headers, D7U).and_then(|s| s.parse::<f64>().ok());

    // 7d reset: present-but-unparseable → None, present-and-parseable → epoch-now.
    let week_reset_sec = match header_get(headers, D7R) {
        None => None,
        Some(s) => match s.parse::<i64>() {
            Ok(epoch) => Some((epoch - now as i64).max(0)),
            Err(_) => None,
        },
    };

    UsageInfo {
        ok: true,
        pct: Some(pct),
        reset_sec: Some(reset_sec),
        week_pct,
        week_reset_sec,
        window: Some("5h".to_string()),
        ..Default::default()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Codex usage (~/.codex/sessions/<YYYY>/<MM>/<DD>/*.jsonl)
// ─────────────────────────────────────────────────────────────────────────────

/// Real Codex usage from the most-recently-active session's `rate_limits`.
/// Scans today's + yesterday's date dirs, reads the newest file's last
/// `token_count` event. `ok: false` if Codex is inactive / no data.
/// Port of `codex_usage` + `parse_codex_token_count`.
pub fn codex_usage() -> UsageInfo {
    let root = match codex_sessions_root() {
        Some(r) => r,
        None => return UsageInfo::default(),
    };
    if !root.exists() {
        return UsageInfo::default();
    }
    let now = now_secs();

    // today and yesterday
    let today = Local::now().date_naive();
    let yesterday = today - chrono::Duration::days(1);

    let mut files: Vec<PathBuf> = Vec::new();
    for d in [today, yesterday] {
        use chrono::Datelike;
        let day_dir = root
            .join(format!("{:04}", d.year()))
            .join(format!("{:02}", d.month()))
            .join(format!("{:02}", d.day()));
        if day_dir.exists() {
            if let Ok(rd) = std::fs::read_dir(&day_dir) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        files.push(p);
                    }
                }
            }
        }
    }

    // newest first by mtime — the most-recently-active session has the freshest
    // rate-limit snapshot.
    files.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    });
    files.reverse();

    for f in files {
        let text = match std::fs::read_to_string(&f) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let res = parse_codex_token_count(&text, now);
        if res.ok {
            return res;
        }
    }
    UsageInfo::default()
}

fn codex_sessions_root() -> Option<PathBuf> {
    Some(dirs_next::home_dir()?.join(".codex").join("sessions"))
}

/// Seconds until a rate-limit window resets. Codex logs `resets_at` (epoch);
/// some versions log `resets_in_seconds`. Support both, floored at 0.
fn window_reset_sec(window: &serde_json::Value, now: u64) -> i64 {
    if let Some(s) = window.get("resets_in_seconds").and_then(json_as_i64) {
        return s.max(0);
    }
    if let Some(at) = window.get("resets_at").and_then(json_as_i64) {
        return (at - now as i64).max(0);
    }
    0
}

/// Accept either a JSON number or a numeric string as an i64.
fn json_as_i64(v: &serde_json::Value) -> Option<i64> {
    if let Some(i) = v.as_i64() {
        return Some(i);
    }
    if let Some(f) = v.as_f64() {
        return Some(f as i64);
    }
    v.as_str().and_then(|s| s.trim().parse::<i64>().ok())
}

fn json_as_f64(v: &serde_json::Value) -> Option<f64> {
    if let Some(f) = v.as_f64() {
        return Some(f);
    }
    v.as_str().and_then(|s| s.trim().parse::<f64>().ok())
}

/// Read the LAST `token_count` event's `rate_limits` from a Codex rollout jsonl.
/// `primary` = 5h window, `secondary` = weekly. Port of `parse_codex_token_count`.
/// Percentages in Codex are 0..100 and are converted to 0..1 fractions.
fn parse_codex_token_count(text: &str, now: u64) -> UsageInfo {
    let mut last: Option<serde_json::Value> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || !line.contains("token_count") {
            continue;
        }
        let o: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // payload = o["payload"] if present else o
        let payload = o.get("payload").cloned().unwrap_or(o);
        let is_token_count = payload.get("type").and_then(|t| t.as_str()) == Some("token_count");
        if is_token_count {
            if let Some(rl) = payload.get("rate_limits") {
                last = Some(rl.clone());
            }
        }
    }

    let rl = match last {
        Some(rl) if rl.get("primary").is_some() => rl,
        _ => return UsageInfo::default(),
    };

    let prim = rl
        .get("primary")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let sec = rl
        .get("secondary")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let pct_used = match prim.get("used_percent").and_then(json_as_f64) {
        Some(p) => p,
        None => return UsageInfo::default(),
    };

    let week_pct = sec
        .get("used_percent")
        .and_then(json_as_f64)
        .map(|w| w / 100.0);
    let week_reset_sec = if sec.is_object() {
        Some(window_reset_sec(&sec, now))
    } else {
        None
    };

    let mut info = UsageInfo {
        ok: true,
        pct: Some(pct_used / 100.0),
        reset_sec: Some(window_reset_sec(&prim, now).max(0) as u64),
        week_pct,
        week_reset_sec,
        window: Some("5h".to_string()),
        ..Default::default()
    };
    attach_single_call_analytics(&mut info, now);
    info
}

// ─────────────────────────────────────────────────────────────────────────────
// Single-call analytics (port of usageanalytics.py: project / build_provider)
// ─────────────────────────────────────────────────────────────────────────────

/// Estimate burn (fraction/hour) and projection scalars from a single reading.
///
/// Python computes `burn` via least-squares over a persisted sample history.
/// With no history this estimates burn from how far into the window we are:
///   elapsed = WINDOW_5H_SECS - reset_sec ; burn ≈ pct / (elapsed/3600).
/// This is only meaningful for the 5h window and only when some time has
/// elapsed. When burn can't be estimated, the projection fields are left
/// `None` (Python's "not enough data").
fn attach_single_call_analytics(info: &mut UsageInfo, now: u64) {
    let pct = match info.pct {
        Some(p) => p,
        None => return,
    };
    let reset_sec = match info.reset_sec {
        Some(r) => r as f64,
        None => return,
    };

    // Estimate average burn over the elapsed portion of the 5h window.
    let elapsed = WINDOW_5H_SECS - reset_sec;
    let burn_per_hr: Option<f64> = if elapsed > 60.0 && pct > 0.0 {
        Some(pct / (elapsed / 3600.0))
    } else {
        None
    };

    if let Some(b) = burn_per_hr {
        info.burn_per_hr = Some(round4(b));
    }

    // project() — identical math to usageanalytics.py.
    if let Some(burn) = burn_per_hr {
        let cur = pct.clamp(0.0, 1.0);
        let predicted = cur + burn * (reset_sec / 3600.0);
        if burn > 0.0 && predicted >= 1.0 {
            let remaining = (1.0 - cur).max(0.0);
            let eta_sec = ((remaining / burn) * 3600.0) as i64;
            let eta_ts = now as i64 + eta_sec;
            info.will_exhaust = Some(true);
            info.eta_clock = Some(fmt_clock(eta_ts));
        } else {
            info.will_exhaust = Some(false);
            info.leftover_pct = Some(round4((1.0 - predicted).max(0.0)));
        }
    }

    // spark / daily history require a persistent sample DB — left empty for v1.
    // info.spark stays vec![].
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// Local 12-hour clock string, e.g. "3:40 PM". Port of `fmt_clock`.
fn fmt_clock(ts: i64) -> String {
    let dt = match Local.timestamp_opt(ts, 0).single() {
        Some(d) => d,
        None => return String::new(),
    };
    let h = dt.hour();
    let ampm = if h < 12 { "AM" } else { "PM" };
    let h12 = {
        let m = h % 12;
        if m == 0 {
            12
        } else {
            m
        }
    };
    format!("{}:{:02} {}", h12, dt.minute(), ampm)
}

// ─────────────────────────────────────────────────────────────────────────────
// Capacity verdict (port of usageanalytics.py: capacity())
// ─────────────────────────────────────────────────────────────────────────────

/// Advise whether it's safe to start another agent, from current usage +
/// projection. Port of `capacity()`:
///   * `throttle` if any provider is projected to exhaust before its reset.
///   * `pace`     if utilization is moderate (max pct > ~0.7) and not exhausting.
///   * `go`       otherwise — message nudges using idle capacity.
///
/// `working` / `waiting` are accepted to match the integrator's call signature;
/// only `idle` affects the message (as in Python).
pub fn capacity(
    claude: &UsageInfo,
    codex: &UsageInfo,
    _working: usize,
    _waiting: usize,
    idle: usize,
) -> Capacity {
    const PACE_PCT: f64 = 0.7;

    let mut exhausting: Vec<&str> = Vec::new();
    let mut max_pct: f64 = 0.0;

    for (name, u) in [("claude", claude), ("codex", codex)] {
        // Only consider providers that actually reported data (ok). A missing
        // provider in Python is an empty dict → contributes nothing.
        if !u.ok {
            continue;
        }
        if u.will_exhaust == Some(true) {
            exhausting.push(name);
        }
        if let Some(pct) = u.pct {
            if pct > max_pct {
                max_pct = pct;
            }
        }
    }

    if !exhausting.is_empty() {
        exhausting.sort();
        let who = exhausting.join(" & ");
        return Capacity {
            status: "throttle".to_string(),
            message: format!("{who} will cap out before reset — hold off on new work"),
        };
    }

    if max_pct > PACE_PCT {
        return Capacity {
            status: "pace".to_string(),
            message: format!(
                "Budget tight ({}% used) — pace yourself",
                (max_pct * 100.0).round() as i64
            ),
        };
    }

    let message = if idle > 0 {
        format!("Plenty of budget · {idle} idle — safe to start another")
    } else {
        "Plenty of budget — safe to start another".to_string()
    };
    Capacity {
        status: "go".to_string(),
        message,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn hdrs(pairs: &[(&str, &str)]) -> Headers {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn missing_5h_header_is_miss() {
        let info = parse_claude_headers(&hdrs(&[]), 1000);
        assert!(!info.ok);
        assert!(info.pct.is_none());
    }

    #[test]
    fn parses_5h_and_7d() {
        let now = 1000u64;
        let info = parse_claude_headers(
            &hdrs(&[
                (H5U, "0.42"),
                (H5R, "1600"), // 600s from now
                (D7U, "0.10"),
                (D7R, "2000"), // 1000s from now
            ]),
            now,
        );
        assert!(info.ok);
        assert_eq!(info.pct, Some(0.42));
        assert_eq!(info.reset_sec, Some(600));
        assert_eq!(info.week_pct, Some(0.10));
        assert_eq!(info.week_reset_sec, Some(1000));
        assert_eq!(info.window.as_deref(), Some("5h"));
    }

    #[test]
    fn reset_floors_at_zero() {
        let info = parse_claude_headers(&hdrs(&[(H5U, "0.9"), (H5R, "500")]), 1000);
        assert_eq!(info.reset_sec, Some(0));
    }

    #[test]
    fn bad_7d_reset_is_none() {
        let info = parse_claude_headers(&hdrs(&[(H5U, "0.5"), (D7R, "notanumber")]), 0);
        assert!(info.ok);
        assert!(info.week_reset_sec.is_none());
    }

    #[test]
    fn codex_parses_last_token_count() {
        let text = concat!(
            "{\"payload\":{\"type\":\"other\"}}\n",
            "{\"payload\":{\"type\":\"token_count\",\"rate_limits\":{\"primary\":{\"used_percent\":20.0,\"resets_in_seconds\":3600},\"secondary\":{\"used_percent\":5.0,\"resets_in_seconds\":7200}}}}\n",
            "{\"payload\":{\"type\":\"token_count\",\"rate_limits\":{\"primary\":{\"used_percent\":40.0,\"resets_in_seconds\":1800},\"secondary\":{\"used_percent\":8.0,\"resets_in_seconds\":3600}}}}\n",
        );
        let info = parse_codex_token_count(text, 0);
        assert!(info.ok);
        assert_eq!(info.pct, Some(0.40)); // last event, /100
        assert_eq!(info.reset_sec, Some(1800));
        assert_eq!(info.week_pct, Some(0.08));
        assert_eq!(info.week_reset_sec, Some(3600));
    }

    #[test]
    fn codex_no_rate_limits_is_miss() {
        let info = parse_codex_token_count("{\"payload\":{\"type\":\"token_count\"}}\n", 0);
        assert!(!info.ok);
    }

    #[test]
    fn capacity_throttle_when_exhausting() {
        let claude = UsageInfo {
            ok: true,
            pct: Some(0.9),
            will_exhaust: Some(true),
            ..Default::default()
        };
        let codex = UsageInfo::default();
        let cap = capacity(&claude, &codex, 1, 0, 0);
        assert_eq!(cap.status, "throttle");
        assert!(cap.message.contains("claude"));
    }

    #[test]
    fn capacity_pace_when_tight() {
        let claude = UsageInfo {
            ok: true,
            pct: Some(0.85),
            will_exhaust: Some(false),
            ..Default::default()
        };
        let codex = UsageInfo::default();
        let cap = capacity(&claude, &codex, 0, 0, 0);
        assert_eq!(cap.status, "pace");
        assert!(cap.message.contains("85%"));
    }

    #[test]
    fn capacity_go_with_idle() {
        let claude = UsageInfo {
            ok: true,
            pct: Some(0.2),
            will_exhaust: Some(false),
            ..Default::default()
        };
        let codex = UsageInfo::default();
        let cap = capacity(&claude, &codex, 0, 0, 3);
        assert_eq!(cap.status, "go");
        assert!(cap.message.contains("3 idle"));
    }

    #[test]
    fn fmt_clock_basic() {
        // Just assert it produces an AM/PM-suffixed string and never panics.
        let s = fmt_clock(0);
        assert!(s.ends_with("AM") || s.ends_with("PM"));
    }

    #[test]
    fn project_marks_exhaust() {
        // pct high, deep into window → predicted >= 1.0 → will_exhaust true.
        let now = 0u64;
        let mut info = UsageInfo {
            ok: true,
            pct: Some(0.8),
            reset_sec: Some(3600), // 1h left → 4h elapsed
            window: Some("5h".into()),
            ..Default::default()
        };
        attach_single_call_analytics(&mut info, now);
        // burn ≈ 0.8 / 4h = 0.2/h; predicted = 0.8 + 0.2*1 = 1.0 → exhaust.
        assert_eq!(info.will_exhaust, Some(true));
        assert!(info.eta_clock.is_some());
        assert!(info.burn_per_hr.is_some());
    }

    #[test]
    fn project_leftover_when_not_exhausting() {
        let now = 0u64;
        let mut info = UsageInfo {
            ok: true,
            pct: Some(0.1),
            reset_sec: Some(3600),
            window: Some("5h".into()),
            ..Default::default()
        };
        attach_single_call_analytics(&mut info, now);
        assert_eq!(info.will_exhaust, Some(false));
        assert!(info.leftover_pct.is_some());
    }
}
