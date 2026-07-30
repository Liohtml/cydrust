//! Pure protocol + formatting logic — NO esp-idf / embedded dependencies.
//!
//! This module holds everything that can run on a plain std toolchain: the
//! hand-rolled scanner for the bridge's compact `/state` payload (see
//! `make_mini` in bridge/src/bin/serial_bridge.rs and docs/api.md) plus the
//! small display-formatting helpers. Keeping it std-only lets the host test
//! suite compile it directly (bridge/tests/firmware_proto_test.rs includes
//! this file via `#[path]`), so the parser is exercised by `cargo test`
//! without an ESP32 toolchain.
//!
//! The parser is deliberately not a full JSON implementation: it scans for
//! known keys with brace-depth tracking, tolerates missing fields, and never
//! panics on malformed input (it just yields defaults). String values are
//! byte-capped at parse time (`trunc_bytes`), mirroring the fixed buffers the
//! device build used historically, so a hostile/oversized payload cannot blow
//! up RAM.

// ── Data model ───────────────────────────────────────────────────────────────

// Bounds mirroring the original heapless capacities: the device shows at most
// 6 session cards / 6 model rows, so anything beyond these is dropped.
pub const MAX_SESSIONS: usize = 8;
pub const MAX_MODELS: usize = 6;

#[derive(Debug, Clone, PartialEq)]
pub enum SessionStatus {
    Working,
    Idle,
    Waiting,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub project: String, // capped at 32 bytes
    pub status: SessionStatus,
    pub tool: String,    // capped at 8 bytes
    pub id: String,      // "i" — session id (truncated, 16 bytes)
    pub age_sec: i32,    // "a" — age in seconds, -1 unknown
    pub wait_sec: i32,   // "ws" — waiting seconds, -1 if not waiting
    pub summary: String, // "s" — short summary (waiting sessions), "" none (80 bytes)
}

// Per-provider usage. Mirrors the original VibeMonitor `Usage` model.
// pct/week_pct are 0..1 fractions; sentinels: week_pct/leftover_pct = -1.0,
// week_reset_sec = -1, burn_per_hr = 0.0, eta_clock = "" mean "unknown".
#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    pub ok: bool,
    pub pct: f32,
    pub reset_sec: u32,
    pub week_pct: f32,
    pub week_reset_sec: i32,
    pub will_exhaust: bool,
    pub burn_per_hr: f32,
    pub leftover_pct: f32,
    pub eta_clock: String, // capped at 12 bytes
}

impl Default for Usage {
    fn default() -> Self {
        Usage {
            ok: false,
            pct: 0.0,
            reset_sec: 0,
            week_pct: -1.0,
            week_reset_sec: -1,
            will_exhaust: false,
            burn_per_hr: 0.0,
            leftover_pct: -1.0,
            eta_clock: String::new(),
        }
    }
}

// One model's token/cost usage today (Metrics tab shows the top models,
// so Opus/Sonnet/Haiku etc. each get their own row instead of collapsing
// to a single per-provider label).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ModelRow {
    pub provider: String, // "claude" / "codex" / "opencode" / "hermes" (10 bytes)
    pub model: String,    // capped at 16 bytes
    pub tokens: f32,      // f32 ok for "M/k" display
    pub usd: f32,
    pub has_usd: bool,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Metrics {
    pub models: Vec<ModelRow>, // top models by tokens (desc), <= MAX_MODELS
    pub total_tokens: f32,
    pub total_usd: f32,
    pub has_usd: bool,
    pub usd_complete: bool,
    pub total_sessions: i32,
    pub dropped_models: usize, // model rows past MAX_MODELS (not stored)
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct DisplayState {
    pub sessions: Vec<SessionRow>, // <= MAX_SESSIONS
    pub claude: Usage,
    pub codex: Usage,
    pub metrics: Metrics,
    pub offline: bool,
    pub dropped_sessions: usize, // sessions past MAX_SESSIONS (not stored)
}

// ── JSON parsing ─────────────────────────────────────────────────────────────

pub fn parse_state(text: &str) -> Option<DisplayState> {
    let mut ds = DisplayState::default();
    if let Some(start) = text.find("\"sessions\":[") {
        let rest = &text[start + 12..];
        let mut depth = 1i32;
        let mut obj_start = None;
        for (i, c) in rest.char_indices() {
            match c {
                '{' => {
                    if depth == 1 {
                        obj_start = Some(i);
                    }
                    depth += 1;
                }
                '}' => {
                    depth -= 1;
                    if depth == 1 {
                        if let Some(s) = obj_start {
                            let obj = &rest[s..=i];
                            let project = extract_str(obj, "project").unwrap_or("?");
                            let status_s = extract_str(obj, "status").unwrap_or("idle");
                            let tool_s = extract_str(obj, "tool").unwrap_or("claude");
                            let status = match status_s {
                                "waiting" => SessionStatus::Waiting,
                                "working" => SessionStatus::Working,
                                _ => SessionStatus::Idle,
                            };
                            let id = extract_str(obj, "i")
                                .map(|v| trunc_bytes(v, 16).to_string())
                                .unwrap_or_default();
                            let summary = extract_str(obj, "s")
                                .map(|v| trunc_bytes(v, 80).to_string())
                                .unwrap_or_default();
                            let age = num_field(obj, "a").map(|v| v as i32).unwrap_or(-1);
                            let wsec = num_field(obj, "ws").map(|v| v as i32).unwrap_or(-1);
                            if ds.sessions.len() < MAX_SESSIONS {
                                ds.sessions.push(SessionRow {
                                    project: trunc_bytes(project, 32).to_string(),
                                    status,
                                    tool: trunc_bytes(tool_s, 8).to_string(),
                                    id,
                                    age_sec: age,
                                    wait_sec: wsec,
                                    summary,
                                });
                            } else {
                                // over capacity — count instead of silently dropping,
                                // so the UI can show "+N more"
                                ds.dropped_sessions += 1;
                            }
                        }
                        obj_start = None;
                    }
                    if depth == 0 {
                        break;
                    }
                }
                ']' => {
                    if depth == 1 {
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    ds.claude = parse_provider(text, "\"claude\":{");
    ds.codex = parse_provider(text, "\"codex\":{");
    ds.metrics = parse_metrics(text);
    Some(ds)
}

pub fn parse_metrics(text: &str) -> Metrics {
    let mut m = Metrics::default();
    let marker = "\"metrics\":{";
    let Some(pos) = text.find(marker) else {
        return m;
    };
    let rest = &text[pos + marker.len()..]; // after the opening '{'
    let mut depth = 1i32;
    let mut ci = rest.len();
    for (i, c) in rest.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    ci = i;
                    break;
                }
            }
            _ => {}
        }
    }
    let mobj = &rest[..ci];

    // models array  "m":[ {"p","n","t","u"}, .. ]  (sorted by tokens desc by the bridge)
    if let Some(apos) = mobj.find("\"m\":[") {
        let arr = &mobj[apos + 5..];
        let mut d2 = 0i32;
        let mut start = None;
        for (i, c) in arr.char_indices() {
            match c {
                '{' => {
                    if d2 == 0 {
                        start = Some(i);
                    }
                    d2 += 1;
                }
                '}' => {
                    d2 -= 1;
                    if d2 == 0 {
                        if let Some(s) = start {
                            let o = &arr[s..=i];
                            let mut mr = ModelRow::default();
                            if let Some(p) = extract_str(o, "p") {
                                mr.provider = trunc_bytes(p, 10).to_string();
                            }
                            if let Some(n) = extract_str(o, "n") {
                                mr.model = trunc_bytes(n, 16).to_string();
                            }
                            if let Some(t) = num_field(o, "t") {
                                mr.tokens = t;
                            }
                            if let Some(u) = num_field(o, "u") {
                                mr.usd = u;
                                mr.has_usd = true;
                            }
                            if m.models.len() < MAX_MODELS {
                                m.models.push(mr);
                            } else {
                                m.dropped_models += 1;
                            } // count, don't silently drop
                        }
                        start = None;
                    }
                }
                ']' => {
                    if d2 == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(v) = num_field(mobj, "tt") {
        m.total_tokens = v;
    }
    if let Some(v) = num_field(mobj, "tu") {
        m.total_usd = v;
        m.has_usd = true;
    }
    if let Some(v) = num_field(mobj, "ts") {
        m.total_sessions = v as i32;
    }
    if mobj.contains("\"uc\":true") {
        m.usd_complete = true;
    }
    m
}

// Scan a numeric field within a provider object slice. Uses a boundary-prefixed
// needle ({"k": or ,"k":) so short keys never match as a suffix of a longer key
// (e.g. "p" inside "wp", "lo" inside ... ).
pub fn num_field(obj: &str, bare_key: &str) -> Option<f32> {
    for pre in ['{', ','] {
        let needle = format!("{}\"{}\":", pre, bare_key);
        if let Some(p) = obj.find(&needle) {
            let rest = &obj[p + needle.len()..];
            let end = rest.find([',', '}']).unwrap_or(rest.len());
            if let Ok(v) = rest[..end].trim().parse::<f32>() {
                return Some(v);
            }
        }
    }
    None
}

pub fn parse_provider(text: &str, marker: &str) -> Usage {
    let mut u = Usage::default();
    let Some(pos) = text.find(marker) else {
        return u;
    };
    // object slice from just after the marker's '{' to its matching '}'
    let rest = &text[pos + marker.len()..];
    let mut depth = 1i32;
    let mut endi = rest.len();
    for (i, c) in rest.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    endi = i;
                    break;
                }
            }
            _ => {}
        }
    }
    let obj = &rest[..endi];

    u.ok = obj.contains("\"ok\":true");
    if !u.ok {
        return u;
    }

    if let Some(v) = num_field(obj, "p") {
        u.pct = v;
    }
    if let Some(v) = num_field(obj, "r") {
        u.reset_sec = v.max(0.0) as u32;
    }
    if let Some(v) = num_field(obj, "wp") {
        u.week_pct = v;
    }
    if let Some(v) = num_field(obj, "wr") {
        u.week_reset_sec = v as i32;
    }
    if obj.contains("\"we\":true") {
        u.will_exhaust = true;
    }
    if let Some(v) = num_field(obj, "b") {
        u.burn_per_hr = v;
    }
    if let Some(v) = num_field(obj, "lo") {
        u.leftover_pct = v;
    }
    if let Some(e) = extract_str(obj, "e") {
        u.eta_clock = trunc_bytes(e, 12).to_string();
    }
    u
}

pub fn extract_str<'a>(obj: &'a str, key: &str) -> Option<&'a str> {
    let search = format!("\"{}\":\"", key);
    let start = obj.find(&search)? + search.len();
    let end = obj[start..].find('"')? + start;
    Some(&obj[start..end])
}

// Truncate to at most `max_bytes`, never splitting a multi-byte UTF-8 char.
// (Plain `&s[..n]` panics on a non-char-boundary — real risk with model names
// / summaries that contain em-dashes, accents, CJK, etc.)
pub fn trunc_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ── Display formatting helpers ───────────────────────────────────────────────

// "152.7M" / "84k" / "512"
pub fn fmt_tokens(t: f32) -> String {
    if t >= 1_000_000.0 {
        format!("{:.1}M", t / 1_000_000.0)
    } else if t >= 1_000.0 {
        format!("{:.0}k", t / 1_000.0)
    } else {
        format!("{:.0}", t)
    }
}
pub fn fmt_usd(u: f32) -> String {
    format!("${:.2}", u)
}

// Humanize seconds since last activity → "now / 12s ago / 5m ago / 3h ago".
pub fn humanize_age(sec: i32) -> String {
    if sec < 0 {
        return String::new();
    }
    let s = sec as u32;
    if s < 5 {
        "now".to_string()
    } else if s < 60 {
        format!("{}s ago", s)
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 86400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86400)
    }
}

// Bare duration → "45s / 5m / 3h".
pub fn humanize_dur(sec: i32) -> String {
    if sec < 0 {
        return String::new();
    }
    let s = sec as u32;
    if s < 60 {
        format!("{}s", s)
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3600)
    }
}

// resetSec -> "Xh Ym" (>=60min) or "Ym". Mirrors ui.cpp::fmt_reset.
pub fn fmt_reset(reset_sec: u32) -> String {
    let mins = reset_sec / 60;
    if mins >= 60 {
        format!("{}h {}m", mins / 60, mins % 60)
    } else {
        format!("{}m", mins)
    }
}

// sec -> "Xd Yh" (>=24h) / "Xh Ym" (>=1h) / "Ym"; negative -> "--".
// Mirrors ui.cpp::fmt_long.
pub fn fmt_long(sec: i32) -> String {
    if sec < 0 {
        return "--".to_string();
    }
    let mins = (sec as u32) / 60;
    let hrs = mins / 60;
    if hrs >= 24 {
        format!("{}d {}h", hrs / 24, hrs % 24)
    } else if hrs >= 1 {
        format!("{}h {}m", hrs, mins % 60)
    } else {
        format!("{}m", mins)
    }
}

// Greedy word-wrap into up to `max` slices of <= `cols` chars.
pub fn wrap_lines(s: &str, cols: usize, max: usize) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut start = 0usize;
    while start < s.len() && out.len() < max {
        let mut end = (start + cols).min(s.len());
        while end < s.len() && !s.is_char_boundary(end) {
            end -= 1;
        } // char-safe
        if end < s.len() {
            if let Some(sp) = s[start..end].rfind(' ') {
                if sp > 0 {
                    end = start + sp;
                }
            }
        }
        out.push(s[start..end].trim_end());
        let rest = &s[end..];
        let skip = rest.len() - rest.trim_start_matches(' ').len(); // ASCII spaces
        start = end + skip;
    }
    out
}
