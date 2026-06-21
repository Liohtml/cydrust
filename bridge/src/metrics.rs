// =============================================================================
// Metrics + titles — Rust port of vibemonitor's costs.py / metrics.py / summarizer.py.
//
// Two public entry points:
//   summarize_metrics(now, pricing) -> Metrics
//       Today's per-provider token/cost/model rollup + a global totals line.
//   build_titles(now) -> HashMap<session_id, title>
//       First substantive user prompt (or DB title) per session, noise-filtered.
//
// Data sources (all opened defensively — one bad file/row never sinks a function):
//   claude   : ~/.claude/projects/<encoded-dir>/*.jsonl  (assistant msg.usage{}, msg.model)
//   codex    : ~/.codex/sessions/Y/M/D/*.jsonl           (token_count.total_token_usage)
//   opencode : ~/.local/share/opencode/opencode.db       (message.data JSON; session.title)
//   hermes   : %LOCALAPPDATA%/hermes/state.db            (sessions row; *_tokens, *_cost_usd)
//
// "today" = since local midnight (chrono local). Claude/Codex only count files whose
// mtime is at or after local midnight; OpenCode/Hermes filter on their time columns.
// =============================================================================

use crate::model::{Metrics, ModelMetric, ProviderMetric, Totals};
use chrono::{Datelike, Local, TimeZone, Timelike};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

// ─── paths ───────────────────────────────────────────────────────────────────

fn home() -> PathBuf {
    dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn claude_projects_root() -> PathBuf {
    home().join(".claude").join("projects")
}

fn codex_sessions_root() -> PathBuf {
    home().join(".codex").join("sessions")
}

/// OpenCode data root: $XDG_DATA_HOME/opencode else ~/.local/share/opencode.
fn opencode_db() -> PathBuf {
    let root = std::env::var_os("XDG_DATA_HOME")
        .map(|x| PathBuf::from(x).join("opencode"))
        .unwrap_or_else(|| home().join(".local").join("share").join("opencode"));
    root.join("opencode.db")
}

/// Hermes data root: %LOCALAPPDATA%/hermes (Windows) else ~/.local/share/hermes.
fn hermes_db() -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .map(|x| PathBuf::from(x).join("hermes"))
        .or_else(|| std::env::var_os("XDG_DATA_HOME").map(|x| PathBuf::from(x).join("hermes")))
        .unwrap_or_else(|| home().join(".local").join("share").join("hermes"));
    root.join("state.db")
}

// ─── time helpers ────────────────────────────────────────────────────────────

/// Epoch seconds of local midnight for the day containing `now` (mirrors
/// costs._local_midnight: subtract the local h/m/s of the instant).
fn local_midnight(now: f64) -> f64 {
    match Local.timestamp_opt(now as i64, 0).single() {
        Some(lt) => {
            now - (lt.hour() as f64 * 3600.0 + lt.minute() as f64 * 60.0 + lt.second() as f64)
        }
        None => now,
    }
}

fn mtime_secs(path: &Path) -> Option<f64> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    Some(mtime.duration_since(UNIX_EPOCH).ok()?.as_secs_f64())
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

// ─── pricing ─────────────────────────────────────────────────────────────────

/// Look up a pricing entry by exact key, else by substring (first key contained
/// in `model`). Mirrors costs._price_model's lookup.
fn pricing_entry<'a>(
    model: &str,
    pricing: &'a HashMap<String, (f64, f64)>,
) -> Option<&'a (f64, f64)> {
    if let Some(e) = pricing.get(model) {
        return Some(e);
    }
    pricing.iter().find(|(k, _)| model.contains(k.as_str())).map(|(_, v)| v)
}

/// USD using separate input/output rates plus a cache-read discount (cache at 0.1x
/// the input rate). rates are $/1M tokens. None when no pricing entry for the model.
/// Port of costs._price_model.
fn price_model(
    model: &str,
    pricing: &HashMap<String, (f64, f64)>,
    tokens_in: i64,
    tokens_out: i64,
    tokens_cache: i64,
) -> Option<f64> {
    if model.is_empty() {
        return None;
    }
    let (ri, ro) = *pricing_entry(model, pricing)?;
    let cost = tokens_in as f64 / 1_000_000.0 * ri
        + tokens_out as f64 / 1_000_000.0 * ro
        + tokens_cache as f64 / 1_000_000.0 * ri * 0.1;
    Some(round4(cost))
}

/// USD using a single blended rate over a token total (Codex path). Port of
/// costs._price_for, used for the "codex" pricing key. None when no entry.
fn price_blended(
    model: &str,
    pricing: &HashMap<String, (f64, f64)>,
    tokens: i64,
) -> Option<f64> {
    if model.is_empty() {
        return None;
    }
    let (ri, ro) = *pricing_entry(model, pricing)?;
    let blended = (ri + ro) / 2.0;
    Some(round4(tokens as f64 / 1_000_000.0 * blended))
}

// ─── model label shortening (metrics._short_model) ───────────────────────────

/// Trim a verbose model id to a device-friendly label:
/// 'claude-sonnet-4-5-20250929' -> 'sonnet-4-5'; 'openai/gpt-5-codex' -> 'gpt-5-codex'.
fn short_model(model: &str) -> String {
    let mut m = model.rsplit('/').next().unwrap_or(model).to_string();
    // strip a trailing -YYYYMMDD date stamp
    let parts: Vec<&str> = m.split('-').collect();
    if let Some(last) = parts.last() {
        if last.len() == 8 && last.chars().all(|c| c.is_ascii_digit()) {
            m = parts[..parts.len() - 1].join("-");
        }
    }
    for vendor in ["claude-", "anthropic-"] {
        if let Some(rest) = m.strip_prefix(vendor) {
            m = rest.to_string();
            break;
        }
    }
    m.chars().take(24).collect()
}

// ─── per-model token bucket ──────────────────────────────────────────────────

#[derive(Default, Clone)]
struct Bucket {
    tin: i64,
    tout: i64,
    cache: i64,
    total: i64,
    usd: Option<f64>,
}

/// A provider metric under construction: keeps a raw-model-id keyed breakdown that
/// `finalize` collapses into the public `models` list (sorted tokens desc),
/// the scalar max-token `model`, and tokensIn/tokensOut.
#[derive(Default)]
struct Builder {
    tokens_in: i64,
    tokens_out: i64,
    tokens: i64,
    usd: Option<f64>,
    sessions: i64,
    by_model: BTreeMap<String, Bucket>,
}

impl Builder {
    /// Accumulate one model bucket's contribution (in/out where out already folds
    /// cache+reasoning per the Python `_accum(... out + extra ...)` convention).
    fn accum(&mut self, raw_model: &str, tin: i64, tout: i64, total: i64, usd: Option<f64>) {
        self.tokens_in += tin;
        self.tokens_out += tout;
        self.tokens += total;
        if let Some(u) = usd {
            self.usd = Some(round4(self.usd.unwrap_or(0.0) + u));
        }
        let key = if raw_model.is_empty() { "unknown" } else { raw_model };
        let b = self.by_model.entry(key.to_string()).or_default();
        b.tin += tin;
        b.tout += tout;
        b.total += total;
        if let Some(u) = usd {
            b.usd = Some(round4(b.usd.unwrap_or(0.0) + u));
        }
    }

    /// Collapse `by_model` (raw ids) into a short-label-keyed map, build the sorted
    /// `models` list, set scalar `model`, and repair tokensIn/Out if they came as 0.
    fn finalize(self) -> ProviderMetric {
        // merge raw ids sharing a short label (dated variants of one model)
        let mut merged: BTreeMap<String, Bucket> = BTreeMap::new();
        for (raw, b) in &self.by_model {
            let label = if raw == "unknown" {
                "unknown".to_string()
            } else {
                let s = short_model(raw);
                if s.is_empty() { raw.clone() } else { s }
            };
            let m = merged.entry(label).or_default();
            m.tin += b.tin;
            m.tout += b.tout;
            m.total += b.total;
            if let Some(u) = b.usd {
                m.usd = Some(round4(m.usd.unwrap_or(0.0) + u));
            }
        }
        let mut models: Vec<ModelMetric> = merged
            .iter()
            .filter(|(_, m)| m.total > 0)
            .map(|(label, m)| ModelMetric { model: label.clone(), tokens: m.total, usd: m.usd })
            .collect();
        // sort by tokens DESC; ties keep stable order
        models.sort_by(|a, b| b.tokens.cmp(&a.tokens));

        let model = models.first().map(|m| m.model.clone());

        // repair in/out that arrived as 0 even though a real breakdown exists
        let (mut tokens_in, mut tokens_out) = (self.tokens_in, self.tokens_out);
        if self.tokens > 0 && tokens_in == 0 && tokens_out == 0 {
            tokens_in = merged.values().map(|m| m.tin).sum();
            tokens_out = merged.values().map(|m| m.tout).sum();
        }

        ProviderMetric {
            model,
            tokens_in,
            tokens_out,
            tokens: self.tokens,
            usd: self.usd,
            sessions: self.sessions,
            models,
        }
    }
}

// ─── JSON line helpers ───────────────────────────────────────────────────────

fn read_text(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn as_i64(v: &serde_json::Value) -> i64 {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).unwrap_or(0)
}

// ─── claude: per-model token breakdown (costs._claude_tokens_by_model) ───────

fn claude_tokens_by_model(path: &Path) -> BTreeMap<String, Bucket> {
    let mut by_model: BTreeMap<String, Bucket> = BTreeMap::new();
    let text = match read_text(path) {
        Some(t) => t,
        None => return by_model,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let o: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let msg = match o.get("message") {
            Some(m) if m.is_object() => m,
            _ => continue,
        };
        let usage = match msg.get("usage") {
            Some(u) if u.is_object() => u,
            _ => continue,
        };
        let model = msg.get("model").and_then(|m| m.as_str()).unwrap_or("unknown");
        let ti = usage.get("input_tokens").map(as_i64).unwrap_or(0);
        let to = usage.get("output_tokens").map(as_i64).unwrap_or(0);
        let cache = usage.get("cache_creation_input_tokens").map(as_i64).unwrap_or(0)
            + usage.get("cache_read_input_tokens").map(as_i64).unwrap_or(0);
        let b = by_model.entry(model.to_string()).or_default();
        b.tin += ti;
        b.tout += to;
        b.cache += cache;
        b.total += ti + to + cache;
    }
    by_model
}

fn claude_metrics(midnight: f64, pricing: &HashMap<String, (f64, f64)>) -> ProviderMetric {
    let mut builder = Builder::default();
    let root = claude_projects_root();
    if !root.exists() {
        return builder.finalize();
    }
    for entry in WalkDir::new(&root).max_depth(2).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if mtime_secs(path).map(|m| m < midnight).unwrap_or(true) {
            continue;
        }
        let by_model = claude_tokens_by_model(path);
        if by_model.is_empty() {
            continue;
        }
        let mut counted = false;
        for (raw, b) in &by_model {
            if b.total <= 0 {
                continue;
            }
            let usd = price_model(raw, pricing, b.tin, b.tout, b.cache);
            // out folds cache (mirrors _accum tokens_out=to+cache)
            builder.accum(raw, b.tin, b.tout + b.cache, b.total, usd);
            if !counted {
                builder.sessions += 1; // one session per file
                counted = true;
            }
        }
    }
    builder.finalize()
}

// ─── codex: per-model token breakdown (costs._codex_tokens_by_model) ─────────

/// Returns (model, in, out, cache, total). total<=0 means "no usage".
fn codex_tokens_by_model(path: &Path) -> (Option<String>, i64, i64, i64, i64) {
    let mut model: Option<String> = None;
    let (mut total, mut ti, mut to, mut cache) = (0i64, 0i64, 0i64, 0i64);
    let text = match read_text(path) {
        Some(t) => t,
        None => return (None, 0, 0, 0, 0),
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let o: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let payload = o.get("payload").filter(|p| p.is_object()).unwrap_or(&o);
        if model.is_none() {
            let m = payload
                .get("model")
                .and_then(|v| v.as_str())
                .or_else(|| payload.get("info").and_then(|i| i.get("model")).and_then(|v| v.as_str()));
            if let Some(m) = m {
                if !m.is_empty() {
                    model = Some(m.to_string());
                }
            }
        }
        if payload.get("type").and_then(|t| t.as_str()) == Some("token_count") {
            let empty = serde_json::Value::Null;
            let info = payload.get("info").filter(|i| i.is_object()).unwrap_or(payload);
            let tu = info
                .get("total_token_usage")
                .or_else(|| info.get("last_token_usage"))
                .unwrap_or(&empty);
            if tu.is_object() {
                if let Some(t) = tu.get("total_tokens").filter(|v| v.is_number()) {
                    // token_count is cumulative; take the latest snapshot
                    total = as_i64(t);
                    ti = tu.get("input_tokens").map(as_i64).unwrap_or(0);
                    to = tu.get("output_tokens").map(as_i64).unwrap_or(0);
                    cache = tu.get("cached_input_tokens").map(as_i64).unwrap_or(0);
                } else {
                    ti = tu.get("input_tokens").map(as_i64).unwrap_or(0);
                    to = tu.get("output_tokens").map(as_i64).unwrap_or(0);
                    cache = tu.get("cached_input_tokens").map(as_i64).unwrap_or(0)
                        + tu.get("reasoning_output_tokens").map(as_i64).unwrap_or(0);
                    let s = ti + to + cache;
                    if s != 0 {
                        total = s;
                    }
                }
            } else if let Some(t) = payload.get("total_tokens").filter(|v| v.is_number()) {
                total = as_i64(t);
            }
        }
    }
    if total <= 0 {
        return (project_none(), 0, 0, 0, 0);
    }
    (model, ti, to, cache, total)
}

#[inline]
fn project_none() -> Option<String> {
    None
}

fn codex_metrics(midnight: f64, pricing: &HashMap<String, (f64, f64)>) -> ProviderMetric {
    let mut builder = Builder::default();
    let root = codex_sessions_root();
    if !root.exists() {
        return builder.finalize();
    }
    for entry in WalkDir::new(&root).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if mtime_secs(path).map(|m| m < midnight).unwrap_or(true) {
            continue;
        }
        let (model, ti, to, cache, total) = codex_tokens_by_model(path);
        if total <= 0 {
            continue;
        }
        let raw = model.unwrap_or_else(|| "codex".to_string());
        // codex pricing is keyed on "codex"; price the file total under that key
        let usd = price_blended("codex", pricing, total);
        builder.accum(&raw, ti, to + cache, total, usd);
        builder.sessions += 1;
    }
    builder.finalize()
}

// ─── opencode metrics (metrics._opencode_metrics) ────────────────────────────

fn open_ro(db: &Path) -> Option<rusqlite::Connection> {
    if !db.exists() {
        return None;
    }
    let uri = format!("file:{}?mode=ro&immutable=1", db.to_string_lossy().replace('\\', "/"));
    rusqlite::Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()
}

fn opencode_metrics(midnight: f64) -> ProviderMetric {
    let mut builder = Builder::default();
    let con = match open_ro(&opencode_db()) {
        Some(c) => c,
        None => return builder.finalize(),
    };
    let ms = (midnight * 1000.0) as i64;
    let mut stmt = match con.prepare("SELECT data FROM message WHERE time_created >= ?1") {
        Ok(s) => s,
        Err(_) => return builder.finalize(),
    };
    let rows = match stmt.query_map([ms], |r| r.get::<_, String>(0)) {
        Ok(r) => r,
        Err(_) => return builder.finalize(),
    };
    let mut seen: HashSet<String> = HashSet::new();
    for data in rows.flatten() {
        let d: serde_json::Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if d.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let tk = d.get("tokens");
        let empty = serde_json::Value::Null;
        let tk = tk.unwrap_or(&empty);
        let cache = tk.get("cache").unwrap_or(&empty);
        let ti = tk.get("input").map(as_i64).unwrap_or(0);
        let to = tk.get("output").map(as_i64).unwrap_or(0);
        let extra = tk.get("reasoning").map(as_i64).unwrap_or(0)
            + cache.get("read").map(as_i64).unwrap_or(0)
            + cache.get("write").map(as_i64).unwrap_or(0);
        let tot = ti + to + extra;
        if tot <= 0 {
            continue;
        }
        let usd = d.get("cost").and_then(|c| c.as_f64());
        let model = d.get("modelID").and_then(|m| m.as_str()).unwrap_or("unknown");
        builder.accum(model, ti, to + extra, tot, usd);
        // one session per distinct contributing session id
        let sid = d
            .get("sessionID")
            .or_else(|| d.get("parentID"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        match sid {
            Some(s) => {
                if seen.insert(s) {
                    builder.sessions += 1;
                }
            }
            None => builder.sessions += 1,
        }
    }
    builder.finalize()
}

// ─── hermes metrics (metrics._hermes_metrics) ────────────────────────────────

fn hermes_metrics(today_start: f64) -> ProviderMetric {
    let mut builder = Builder::default();
    let con = match open_ro(&hermes_db()) {
        Some(c) => c,
        None => return builder.finalize(),
    };
    let sql = "SELECT model, input_tokens, output_tokens, cache_read_tokens, \
               cache_write_tokens, reasoning_tokens, estimated_cost_usd, actual_cost_usd \
               FROM sessions WHERE archived=0 AND started_at >= ?1";
    let mut stmt = match con.prepare(sql) {
        Ok(s) => s,
        Err(_) => return builder.finalize(),
    };
    let rows = stmt.query_map([today_start], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<f64>>(6)?,
            r.get::<_, Option<f64>>(7)?,
        ))
    });
    let rows = match rows {
        Ok(r) => r,
        Err(_) => return builder.finalize(),
    };
    for row in rows.flatten() {
        let (model, ti, to, cr, cw, rt, est, act) = row;
        let ti = ti.unwrap_or(0);
        let to = to.unwrap_or(0);
        let extra = cr.unwrap_or(0) + cw.unwrap_or(0) + rt.unwrap_or(0);
        let tot = ti + to + extra;
        if tot <= 0 {
            continue;
        }
        let usd = act.or(est);
        let raw = model.unwrap_or_else(|| "unknown".to_string());
        builder.accum(&raw, ti, to + extra, tot, usd);
        builder.sessions += 1;
    }
    builder.finalize()
}

// ─── public: summarize_metrics ───────────────────────────────────────────────

/// Today's per-provider token/cost/model rollup + global totals.
/// `pricing` maps a model substring -> (input $/1M, output $/1M).
/// usd is None when no pricing entry; usd_complete is true only when every
/// token-bearing provider has a usd figure. Never panics.
pub fn summarize_metrics(now: f64, pricing: &HashMap<String, (f64, f64)>) -> Metrics {
    let midnight = local_midnight(now);

    let mut providers: BTreeMap<String, ProviderMetric> = BTreeMap::new();
    providers.insert("claude".into(), claude_metrics(midnight, pricing));
    providers.insert("codex".into(), codex_metrics(midnight, pricing));
    providers.insert("opencode".into(), opencode_metrics(midnight));
    providers.insert("hermes".into(), hermes_metrics(midnight));

    let total_tokens: i64 = providers.values().map(|p| p.tokens).sum();
    let total_sessions: i64 = providers.values().map(|p| p.sessions).sum();
    let usd_vals: Vec<Option<f64>> =
        providers.values().filter(|p| p.tokens > 0).map(|p| p.usd).collect();
    let active_count = usd_vals.len();
    let any_usd = usd_vals.iter().any(|v| v.is_some());
    let total_usd = if any_usd {
        Some(round4(usd_vals.iter().map(|v| v.unwrap_or(0.0)).sum()))
    } else {
        None
    };
    let usd_complete = active_count > 0 && usd_vals.iter().all(|v| v.is_some());

    Metrics {
        providers,
        totals: Totals {
            tokens: total_tokens,
            usd: total_usd,
            sessions: total_sessions,
            providers_active: active_count as i64,
        },
        usd_complete,
    }
}

// =============================================================================
// Titles — first substantive user prompt (or DB title) per session.
// =============================================================================

const INJECTED_PREFIXES: &[&str] = &[
    "<command-", "<local-command", "<system-reminder", "Caveat:", "<task-", "<task ",
    "<tool", "<function", "<budget", "<user-prompt-submit", "<post-tool", "<pre-tool",
    "<bash-input", "<bash-stdout", "<bash-stderr", "<ide_", "Base directory for this skill",
];

const NOISE_REPLIES: &[&str] = &[
    "yes", "ok", "okay", "y", "go", "go on", "continue", "sure", "do it", "next", "yep",
    "yeah", "no", "n", "stop", "thanks", "thank you", "k", "please continue",
];

/// True if `text` opens with an angle-bracket wrapper tag like `<foo-bar>` or
/// `<foo_bar ...>` (port of summarizer._WRAPPER_TAG_RE: `^<[A-Za-z][\w-]*[\s>/]`).
fn matches_wrapper_tag(text: &str) -> bool {
    let b = text.as_bytes();
    if b.len() < 2 || b[0] != b'<' {
        return false;
    }
    if !b[1].is_ascii_alphabetic() {
        return false;
    }
    let mut i = 2;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            i += 1;
            continue;
        }
        // first non-[\w-] char must be whitespace, '>' or '/'
        return c.is_whitespace() || c == '>' || c == '/';
    }
    false
}

fn is_injected(text: &str) -> bool {
    if INJECTED_PREFIXES.iter().any(|p| text.starts_with(p)) {
        return true;
    }
    matches_wrapper_tag(text)
}

/// Port of summarizer._is_noise_prompt: injected wrapper, trivial reply, or a
/// "[Request interrupted..." system notice -> noise (not a usable title).
fn is_noise_prompt(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || is_injected(t) {
        return true;
    }
    let lower = t.to_lowercase();
    if NOISE_REPLIES.contains(&lower.as_str()) {
        return true;
    }
    if t.starts_with('[') && lower.starts_with("[request interrupted") {
        return true;
    }
    false
}

/// First line, whitespace-collapsed, cut at a word boundary, <= `limit` chars.
/// Port of metrics._first_line.
fn first_line(text: &str, limit: usize) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let raw_line = trimmed.lines().next().unwrap_or("").trim();
    let line: String = raw_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= limit {
        return Some(line);
    }
    let cut: String = chars[..limit].iter().collect();
    // cut at the last space if it's not too early
    if let Some(sp) = cut.rfind(' ') {
        if sp >= limit / 2 {
            let s = cut[..sp].trim_end().to_string();
            return if s.is_empty() { None } else { Some(s) };
        }
    }
    let s = cut.trim_end().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// Pull human prompt text out of a Claude message value (string content verbatim,
/// or join of `text` blocks; None for pure tool_result). Port of
/// summarizer._extract_message_text.
fn extract_message_text(message: &serde_json::Value) -> Option<String> {
    let content = message.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        let mut texts = String::new();
        let mut found = false;
        for b in arr {
            if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    texts.push_str(t);
                    found = true;
                }
            }
        }
        if found {
            return Some(texts);
        }
    }
    None
}

/// First substantive human prompt from a Claude session jsonl. Port of
/// summarizer.extract_first_prompt (skips wrappers/trivial replies). Never panics.
fn claude_first_prompt(path: &Path) -> Option<String> {
    let text = read_text(path)?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if obj.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        let msg = match obj.get("message") {
            Some(m) => m,
            None => continue,
        };
        let raw = match extract_message_text(msg) {
            Some(t) => t,
            None => continue,
        };
        let t = raw.trim();
        if is_noise_prompt(t) {
            continue;
        }
        return Some(t.chars().take(200).collect());
    }
    None
}

fn claude_titles(out: &mut HashMap<String, String>) {
    let root = claude_projects_root();
    if !root.exists() {
        return;
    }
    // mirror collector scan: <root>/<projdir>/*.jsonl, session id = file stem
    for entry in WalkDir::new(&root).max_depth(2).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let sid = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if let Some(text) = claude_first_prompt(path) {
            if let Some(t) = first_line(&text, 48) {
                out.insert(sid, t);
            }
        }
    }
}

/// Codex session uuid from a rollout filename stem (port of _uuid_from_stem):
/// first 8-4-4-4-12 uuid in the stem, else the last dash-group.
fn uuid_from_stem(stem: &str) -> String {
    let b = stem.as_bytes();
    let is_hex = |c: u8| c.is_ascii_hexdigit();
    let groups = [8usize, 4, 4, 4, 12];
    let mut start = 0;
    while start + 36 <= b.len() {
        let mut pos = start;
        let mut ok = true;
        for (gi, &glen) in groups.iter().enumerate() {
            for _ in 0..glen {
                if pos >= b.len() || !is_hex(b[pos]) {
                    ok = false;
                    break;
                }
                pos += 1;
            }
            if !ok {
                break;
            }
            if gi < groups.len() - 1 {
                if pos >= b.len() || b[pos] != b'-' {
                    ok = false;
                    break;
                }
                pos += 1;
            }
        }
        if ok {
            return stem[start..pos].to_string();
        }
        start += 1;
    }
    stem.rsplit('-').next().unwrap_or(stem).to_string()
}

/// First substantive user_message per recent Codex rollout (today + yesterday).
fn codex_titles(now: f64, out: &mut HashMap<String, String>) {
    let root = codex_sessions_root();
    if !root.exists() {
        return;
    }
    let today = match Local.timestamp_opt(now as i64, 0).single() {
        Some(dt) => dt.date_naive(),
        None => return,
    };
    for offset in [0i64, 1] {
        let d = match today.checked_sub_signed(chrono::Duration::days(offset)) {
            Some(d) => d,
            None => continue,
        };
        let day = root
            .join(format!("{:04}", d.year()))
            .join(format!("{:02}", d.month()))
            .join(format!("{:02}", d.day()));
        if !day.exists() {
            continue;
        }
        let entries = match std::fs::read_dir(&day) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for ent in entries.flatten() {
            let path = ent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };
            let sid = uuid_from_stem(stem);
            if out.contains_key(&sid) {
                continue;
            }
            let text = match read_text(&path) {
                Some(t) => t,
                None => continue,
            };
            for line in text.lines() {
                if !line.contains("user_message") {
                    continue;
                }
                let o: serde_json::Value = match serde_json::from_str(line.trim()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let payload = o.get("payload").filter(|p| p.is_object()).unwrap_or(&o);
                if payload.get("type").and_then(|t| t.as_str()) != Some("user_message") {
                    continue;
                }
                let msg = payload
                    .get("message")
                    .and_then(|m| m.as_str())
                    .or_else(|| payload.get("text").and_then(|m| m.as_str()));
                let msg = match msg {
                    Some(m) => m,
                    None => continue,
                };
                if is_noise_prompt(msg) {
                    continue;
                }
                if let Some(fl) = first_line(msg, 48) {
                    out.insert(sid.clone(), fl);
                    break;
                }
            }
        }
    }
}

/// OpenCode titles: prefer the session.title column; for auto-titled "New session"
/// rows, fall back to the first user text part. Session id = session.id (DB row id).
fn opencode_titles(out: &mut HashMap<String, String>) {
    let con = match open_ro(&opencode_db()) {
        Some(c) => c,
        None => return,
    };
    let rows: Vec<(String, String)> = {
        let mut stmt = match con.prepare("SELECT id, title FROM session WHERE time_archived IS NULL")
        {
            Ok(s) => s,
            Err(_) => return,
        };
        let mapped = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default()))
        });
        match mapped {
            Ok(m) => m.flatten().collect(),
            Err(_) => return,
        }
    };
    for (sid, title) in rows {
        let title = title.trim().to_string();
        if !title.is_empty() && !title.starts_with("New session") {
            let t = first_line(&title, 48).unwrap_or_else(|| title.chars().take(48).collect());
            out.insert(sid, t);
            continue;
        }
        // fall back to first user text part
        if let Ok(mut pstmt) = con.prepare(
            "SELECT data FROM part WHERE session_id=?1 ORDER BY time_created ASC",
        ) {
            if let Ok(prows) = pstmt.query_map([&sid], |r| r.get::<_, String>(0)) {
                for data in prows.flatten() {
                    let d: serde_json::Value = match serde_json::from_str(&data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if d.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(txt) = d.get("text").and_then(|t| t.as_str()) {
                            if let Some(fl) = first_line(txt, 48) {
                                out.insert(sid.clone(), fl);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Hermes titles: the sessions.title column. Session id = sessions.id (DB row id).
fn hermes_titles(out: &mut HashMap<String, String>) {
    let con = match open_ro(&hermes_db()) {
        Some(c) => c,
        None => return,
    };
    let mut stmt = match con.prepare("SELECT id, title FROM sessions WHERE archived=0") {
        Ok(s) => s,
        Err(_) => return,
    };
    let rows = stmt.query_map([], |r| {
        Ok((row_id_to_string(r, 0), r.get::<_, Option<String>>(1)?.unwrap_or_default()))
    });
    let rows = match rows {
        Ok(r) => r,
        Err(_) => return,
    };
    for row in rows.flatten() {
        let (sid, title) = row;
        if let Some(t) = first_line(&title, 48) {
            out.insert(sid, t);
        }
    }
}

/// Hermes `id` may be stored as INTEGER or TEXT; coerce to String either way.
fn row_id_to_string(r: &rusqlite::Row, idx: usize) -> String {
    if let Ok(s) = r.get::<_, String>(idx) {
        return s;
    }
    if let Ok(i) = r.get::<_, i64>(idx) {
        return i.to_string();
    }
    String::new()
}

/// Merge per-session titles from every provider into one {session_id: title} map.
/// Each provider is guarded so a failing DB/file can't sink the others. Never panics.
pub fn build_titles(now: f64) -> HashMap<String, String> {
    let mut titles: HashMap<String, String> = HashMap::new();
    // order mirrors Python: opencode, hermes, claude, then codex
    opencode_titles(&mut titles);
    hermes_titles(&mut titles);
    claude_titles(&mut titles);
    codex_titles(now, &mut titles);
    titles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_model_strips_date_and_vendor() {
        assert_eq!(short_model("claude-sonnet-4-5-20250929"), "sonnet-4-5");
        assert_eq!(short_model("openai/gpt-5-codex"), "gpt-5-codex");
        assert_eq!(short_model("deepseek-ai/deepseek-v4-pro"), "deepseek-v4-pro");
    }

    #[test]
    fn price_model_cache_discount() {
        let mut p = HashMap::new();
        p.insert("sonnet".to_string(), (3.0, 15.0));
        // 1M in @3, 1M out @15, 1M cache @0.3 => 18.3
        let usd = price_model("claude-sonnet-4-5", &p, 1_000_000, 1_000_000, 1_000_000);
        assert_eq!(usd, Some(18.3));
        assert_eq!(price_model("gpt-5", &p, 1000, 1000, 0), None);
    }

    #[test]
    fn wrapper_tag_and_noise() {
        assert!(matches_wrapper_tag("<task-notification>hi"));
        assert!(matches_wrapper_tag("<foo_bar />"));
        assert!(!matches_wrapper_tag("hello <world>"));
        assert!(is_noise_prompt("yes"));
        assert!(is_noise_prompt("<system-reminder> x"));
        assert!(is_noise_prompt("[Request interrupted by user]"));
        assert!(!is_noise_prompt("fix the login bug"));
    }

    #[test]
    fn first_line_word_boundary() {
        let s = first_line("hello world this is a fairly long single prompt line here now", 48)
            .unwrap();
        assert!(s.len() <= 48);
        assert!(!s.ends_with(' '));
        assert_eq!(first_line("short", 48).unwrap(), "short");
        assert_eq!(first_line("  multi\nline  ", 48).unwrap(), "multi");
    }

    #[test]
    fn uuid_from_stem_extracts() {
        assert_eq!(
            uuid_from_stem("rollout-2026-06-21T10-00-00-12345678-1234-1234-1234-123456789abc"),
            "12345678-1234-1234-1234-123456789abc"
        );
        assert_eq!(uuid_from_stem("no-uuid-here"), "here");
    }

    #[test]
    fn local_midnight_is_before_now() {
        let now = 1_700_000_000.0;
        let m = local_midnight(now);
        assert!(m <= now);
        assert!(now - m < 86_400.0);
    }
}
