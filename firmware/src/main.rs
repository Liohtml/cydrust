use anyhow::Result;
use embedded_graphics::{
    mono_font::{
        ascii::{FONT_7X13, FONT_7X13_BOLD, FONT_9X15, FONT_9X15_BOLD},
        MonoFont, MonoTextStyle,
    },
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use display_interface_spi::SPIInterface;
use esp_idf_hal::{
    delay::FreeRtos,
    gpio::{Input, Level, Output, PinDriver},
    ledc::{config::TimerConfig, LedcDriver, LedcTimerDriver, Resolution},
    prelude::*,
    spi::{config::Config as SpiConfig, SpiDeviceDriver, SpiDriver},
};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use heapless::String as HString;
use log::info;
use mipidsi::{
    models::ST7789,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
    Builder,
};
use std::time::Instant;
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "wifi")]
use embedded_svc::{http::client::Client as HttpClient, http::Method, io::Read as EmbeddedRead};
#[cfg(feature = "wifi")]
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    http::client::{Configuration as HttpConfig, EspHttpConnection},
    wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};
#[cfg(not(feature = "wifi"))]
use std::sync::{Arc, Mutex};

mod icons;

// ── Data model ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tab { Sessions, Usage, Settings }

// Sessions tab can show the list or a single-session detail overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
enum View { List, Detail { index: usize } }

#[derive(Debug, Clone, PartialEq)]
enum SessionStatus { Working, Idle, Waiting }

#[derive(Debug, Clone, PartialEq)]
struct SessionRow {
    project:  HString<32>,
    status:   SessionStatus,
    tool:     HString<8>,
    id:       HString<16>,   // "i" — session id (truncated)
    age_sec:  i32,           // "a" — age in seconds, -1 unknown
    wait_sec: i32,           // "ws" — waiting seconds, -1 if not waiting
    summary:  HString<80>,   // "s" — short summary (waiting sessions), "" none
}

// Per-provider usage. Mirrors the original VibeMonitor `Usage` model.
// pct/week_pct are 0..1 fractions; sentinels: week_pct/leftover_pct = -1.0,
// week_reset_sec = -1, burn_per_hr = 0.0, eta_clock = "" mean "unknown".
#[derive(Debug, Clone, PartialEq)]
struct Usage {
    ok:             bool,
    pct:            f32,
    reset_sec:      u32,
    week_pct:       f32,
    week_reset_sec: i32,
    will_exhaust:   bool,
    burn_per_hr:    f32,
    leftover_pct:   f32,
    eta_clock:      HString<12>,
}

impl Default for Usage {
    fn default() -> Self {
        Usage {
            ok: false, pct: 0.0, reset_sec: 0, week_pct: -1.0,
            week_reset_sec: -1, will_exhaust: false, burn_per_hr: 0.0,
            leftover_pct: -1.0, eta_clock: HString::new(),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
struct DisplayState {
    sessions: heapless::Vec<SessionRow, 8>,
    claude:   Usage,
    codex:    Usage,
    offline:  bool,
}

// User settings (persisted in NVS).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Settings {
    brightness: u8,    // 10..=100 (%)
    sleep_min:  u16,   // 0=never, else minutes until screen-off
    dark:       bool,  // theme: true=dark, false=light
}
impl Default for Settings {
    fn default() -> Self { Settings { brightness: 100, sleep_min: 0, dark: true } }
}

const SLEEP_VALS: [u16; 5] = [0, 1, 5, 15, 30];
const SLEEP_LBL:  [&str; 5] = ["Never", "1m", "5m", "15m", "30m"];

fn snap_sleep(m: u16) -> u16 {
    *SLEEP_VALS.iter().min_by_key(|&&v| (v as i32 - m as i32).abs()).unwrap_or(&0)
}

// ── JSON parsing ─────────────────────────────────────────────────────────────

fn parse_state(text: &str) -> Option<DisplayState> {
    let mut ds = DisplayState::default();
    if let Some(start) = text.find("\"sessions\":[") {
        let rest = &text[start + 12..];
        let mut depth = 1i32;
        let mut obj_start = None;
        for (i, c) in rest.char_indices() {
            match c {
                '{' => { if depth == 1 { obj_start = Some(i); } depth += 1; }
                '}' => {
                    depth -= 1;
                    if depth == 1 {
                        if let Some(s) = obj_start {
                            let obj = &rest[s..=i];
                            let project  = extract_str(obj, "project").unwrap_or("?");
                            let status_s = extract_str(obj, "status").unwrap_or("idle");
                            let tool_s   = extract_str(obj, "tool").unwrap_or("claude");
                            let status = match status_s {
                                "waiting" => SessionStatus::Waiting,
                                "working" => SessionStatus::Working,
                                _         => SessionStatus::Idle,
                            };
                            let mut p: HString<32> = HString::new();
                            let _ = p.push_str(&project[..project.len().min(32)]);
                            let mut t: HString<8> = HString::new();
                            let _ = t.push_str(&tool_s[..tool_s.len().min(8)]);
                            let mut idh: HString<16> = HString::new();
                            if let Some(idv) = extract_str(obj, "i") {
                                let _ = idh.push_str(&idv[..idv.len().min(16)]);
                            }
                            let mut sh: HString<80> = HString::new();
                            if let Some(sv) = extract_str(obj, "s") {
                                let _ = sh.push_str(&sv[..sv.len().min(80)]);
                            }
                            let age  = num_field(obj, "a").map(|v| v as i32).unwrap_or(-1);
                            let wsec = num_field(obj, "ws").map(|v| v as i32).unwrap_or(-1);
                            let _ = ds.sessions.push(SessionRow {
                                project: p, status, tool: t,
                                id: idh, age_sec: age, wait_sec: wsec, summary: sh,
                            });
                        }
                        obj_start = None;
                    }
                    if depth == 0 { break; }
                }
                ']' => { if depth == 1 { break; } }
                _ => {}
            }
        }
    }
    ds.claude = parse_provider(text, "\"claude\":{");
    ds.codex  = parse_provider(text, "\"codex\":{");
    Some(ds)
}

// Scan a numeric field within a provider object slice. Uses a boundary-prefixed
// needle ({"k": or ,"k":) so short keys never match as a suffix of a longer key
// (e.g. "p" inside "wp", "lo" inside ... ).
fn num_field(obj: &str, bare_key: &str) -> Option<f32> {
    for pre in ['{', ','] {
        let needle = format!("{}\"{}\":", pre, bare_key);
        if let Some(p) = obj.find(&needle) {
            let rest = &obj[p + needle.len()..];
            let end  = rest.find(|c: char| c == ',' || c == '}').unwrap_or(rest.len());
            if let Ok(v) = rest[..end].trim().parse::<f32>() { return Some(v); }
        }
    }
    None
}

fn parse_provider(text: &str, marker: &str) -> Usage {
    let mut u = Usage::default();
    let Some(pos) = text.find(marker) else { return u; };
    // object slice from just after the marker's '{' to its matching '}'
    let rest = &text[pos + marker.len()..];
    let mut depth = 1i32;
    let mut endi = rest.len();
    for (i, c) in rest.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => { depth -= 1; if depth == 0 { endi = i; break; } }
            _ => {}
        }
    }
    let obj = &rest[..endi];

    u.ok = obj.contains("\"ok\":true");
    if !u.ok { return u; }

    if let Some(v) = num_field(obj, "p")  { u.pct = v; }
    if let Some(v) = num_field(obj, "r")  { u.reset_sec = v.max(0.0) as u32; }
    if let Some(v) = num_field(obj, "wp") { u.week_pct = v; }
    if let Some(v) = num_field(obj, "wr") { u.week_reset_sec = v as i32; }
    if obj.contains("\"we\":true") { u.will_exhaust = true; }
    if let Some(v) = num_field(obj, "b")  { u.burn_per_hr = v; }
    if let Some(v) = num_field(obj, "lo") { u.leftover_pct = v; }
    if let Some(e) = extract_str(obj, "e") {
        let _ = u.eta_clock.push_str(&e[..e.len().min(12)]);
    }
    u
}

fn extract_str<'a>(obj: &'a str, key: &str) -> Option<&'a str> {
    let search = format!("\"{}\":\"", key);
    let start  = obj.find(&search)? + search.len();
    let end    = obj[start..].find('"')? + start;
    Some(&obj[start..end])
}

// ── WiFi fetch ───────────────────────────────────────────────────────────────

#[cfg(feature = "wifi")] const WIFI_SSID:    &str = env!("VIBE_SSID");
#[cfg(feature = "wifi")] const WIFI_PASS:    &str = env!("VIBE_PASS");
#[cfg(feature = "wifi")] const BRIDGE_HOST:  &str = env!("VIBE_HOST");
#[cfg(feature = "wifi")] const BRIDGE_PORT:  &str = env!("VIBE_PORT");
#[cfg(feature = "wifi")] const BRIDGE_TOKEN: &str = env!("VIBE_TOKEN");
#[cfg(feature = "wifi")] const POLL_MS: u64 = 2000;

#[cfg(feature = "wifi")]
fn fetch_state(token: &str, host: &str, port: &str) -> Option<DisplayState> {
    let url = format!("http://{}:{}/state", host, port);
    let cfg = HttpConfig { buffer_size: Some(8192), buffer_size_tx: Some(1024), ..Default::default() };
    let conn = EspHttpConnection::new(&cfg).ok()?;
    let mut client = HttpClient::wrap(conn);
    let headers = [("X-VibeMonitor-Token", token)];
    let req  = client.request(Method::Get, &url, &headers).ok()?;
    let mut resp = req.submit().ok()?;
    if resp.status() != 200 { return None; }
    let mut buf = [0u8; 6144]; let mut total = 0usize;
    loop {
        let n = EmbeddedRead::read(&mut resp, &mut buf[total..]).ok()?;
        if n == 0 { break; }
        total += n;
        if total >= buf.len() { break; }
    }
    parse_state(std::str::from_utf8(&buf[..total]).ok()?)
}

// ── Colour palette (runtime-switchable dark/light) ──────────────────────────────
// Values are direct RGB565 of the original VibeMonitor hex palettes (config.h
// dark + settings.cpp PAL_LIGHT). Panel uses ColorOrder::Bgr so stored values
// display as-is. Colours are read via pal() so the theme can switch live.
#[derive(Clone, Copy)]
struct Palette {
    bg: Rgb565, panel: Rgb565, fg: Rgb565, dim: Rgb565, claude: Rgb565,
    codex: Rgb565, wait: Rgb565, waitdk: Rgb565, work: Rgb565, offline: Rgb565,
}

static PAL_DARK: Palette = Palette {
    bg:     Rgb565::new(2,  5,  2),  panel:  Rgb565::new(5,  11, 5),
    fg:     Rgb565::new(29, 59, 29), dim:    Rgb565::new(17, 34, 17),
    claude: Rgb565::new(26, 29, 11), codex:  Rgb565::new(20, 34, 30),
    wait:   Rgb565::new(30, 41, 4),  waitdk: Rgb565::new(9,  12, 0),
    work:   Rgb565::new(9,  55, 16), offline:Rgb565::new(28, 18, 9),
};
static PAL_LIGHT: Palette = Palette {
    bg:     Rgb565::new(30, 60, 29), panel:  Rgb565::new(28, 55, 26),
    fg:     Rgb565::new(3,  7,  3),  dim:    Rgb565::new(13, 26, 13),
    claude: Rgb565::new(23, 22, 5),  codex:  Rgb565::new(13, 19, 26),
    wait:   Rgb565::new(23, 29, 1),  waitdk: Rgb565::new(30, 56, 23),
    work:   Rgb565::new(3,  39, 10), offline:Rgb565::new(24, 10, 5),
};

static DARK: AtomicBool = AtomicBool::new(true);
#[inline] fn pal() -> &'static Palette {
    if DARK.load(Ordering::Relaxed) { &PAL_DARK } else { &PAL_LIGHT }
}
// Shorthand accessors (keep call sites terse).
#[inline] fn c_bg() -> Rgb565 { pal().bg }
#[inline] fn c_panel() -> Rgb565 { pal().panel }
#[inline] fn c_fg() -> Rgb565 { pal().fg }
#[inline] fn c_dim() -> Rgb565 { pal().dim }
#[inline] fn c_claude() -> Rgb565 { pal().claude }
#[inline] fn c_codex() -> Rgb565 { pal().codex }
#[inline] fn c_wait() -> Rgb565 { pal().wait }
#[inline] fn c_waitdk() -> Rgb565 { pal().waitdk }
#[inline] fn c_work() -> Rgb565 { pal().work }
#[inline] fn c_offline() -> Rgb565 { pal().offline }

// ── Draw primitives ────────────────────────────────────────────────────────────

fn fill<D: DrawTarget<Color = Rgb565>>(d: &mut D, x: i32, y: i32, w: u32, h: u32, c: Rgb565) {
    let _ = Rectangle::new(Point::new(x, y), Size::new(w, h))
        .into_styled(PrimitiveStyleBuilder::new().fill_color(c).build())
        .draw(d);
}

fn rfill<D: DrawTarget<Color = Rgb565>>(d: &mut D, x: i32, y: i32, w: u32, h: u32, r: u32, c: Rgb565) {
    let _ = RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(x, y), Size::new(w, h)),
        Size::new(r, r),
    )
    .into_styled(PrimitiveStyleBuilder::new().fill_color(c).build())
    .draw(d);
}

fn txt<D: DrawTarget<Color = Rgb565>>(
    d: &mut D, font: &MonoFont<'_>, s: &str,
    x: i32, y: i32, align: Alignment, color: Rgb565,
) {
    let style = MonoTextStyle::new(font, color);
    let _ = Text::with_alignment(s, Point::new(x, y), style, align).draw(d);
}

// Brand colours for providers without a real logo bitmap (fixed, theme-independent).
const BRAND_OPENCODE: Rgb565 = Rgb565::new(2, 46, 20);   // teal
const BRAND_HERMES:   Rgb565 = Rgb565::new(7, 32, 30);   // blue

// Provider badge (18x18): real logo for Claude/Codex, a coloured monogram for
// OpenCode/Hermes (no logo bitmap available).
fn draw_badge<D: DrawTarget<Color = Rgb565>>(display: &mut D, tool: &str, x: i32, y: i32) {
    match tool {
        "codex"    => icons::draw_codex(display, x, y),
        "opencode" => mono_badge(display, x, y, BRAND_OPENCODE, "O"),
        "hermes"   => mono_badge(display, x, y, BRAND_HERMES, "H"),
        _          => icons::draw_claude(display, x, y),
    }
}

fn mono_badge<D: DrawTarget<Color = Rgb565>>(display: &mut D, x: i32, y: i32, bg: Rgb565, ch: &str) {
    rfill(display, x, y, 18, 18, 4, bg);
    txt(display, &FONT_9X15_BOLD, ch, x + 9, y + 14, Alignment::Center, Rgb565::new(31, 63, 31));
}

// Display name + accent colour for a provider tool string.
fn provider_meta(tool: &str) -> (&'static str, Rgb565) {
    match tool {
        "codex"    => ("Codex",    c_codex()),
        "opencode" => ("OpenCode", BRAND_OPENCODE),
        "hermes"   => ("Hermes",   BRAND_HERMES),
        _          => ("Claude",   c_claude()),
    }
}

// ── Tab bar ───────────────────────────────────────────────────────────────────

fn draw_tab_bar<D: DrawTarget<Color = Rgb565>>(d: &mut D, active: Tab) {
    let tabs: &[(&str, i32, u32, Tab)] = &[
        ("SESSIONS", 53,  107, Tab::Sessions),
        ("USAGE",    160, 106, Tab::Usage),
        ("SETTINGS", 267, 107, Tab::Settings),
    ];
    let mut x = 1i32;
    for (label, cx, w, tab) in tabs {
        let (bg, fg) = if *tab == active { (c_claude(), c_bg()) } else { (c_panel(), c_dim()) };
        rfill(d, x, 1, *w, 24, 5, bg);
        txt(d, &FONT_7X13_BOLD, label, *cx, 17, Alignment::Center, fg);
        x += *w as i32 + 1;
    }
}

// ── Render ────────────────────────────────────────────────────────────────────
//
// Layout 320×240 landscape:
//   y= 0-25   Tab bar        (26 px)
//   y=26-44   Usage header   (19 px)
//   y=45      Separator       (1 px)
//   y=46..    Session cards  (27 px stride × 6 max)
//   y=214-239 Footer/offline (26 px)

// `full_clear` is only true when the layout identity (tab/view) changed; on a
// plain data refresh it is false and each element repaints over its own region,
// so the screen never blanks ("background" refresh — no flicker).
fn render<D: DrawTarget<Color = Rgb565>>(
    display: &mut D, ds: &DisplayState, active: Tab, view: View, set: &Settings, full_clear: bool,
) {
    if full_clear { fill(display, 0, 0, 320, 240, c_bg()); }
    draw_tab_bar(display, active);

    match active {
        Tab::Sessions => match view {
            View::List             => render_sessions(display, ds),
            View::Detail { index } => match ds.sessions.get(index) {
                Some(row) => render_detail(display, row),
                None      => render_sessions(display, ds),
            },
        },
        Tab::Usage    => render_usage(display, ds),
        Tab::Settings => render_settings(display, set),
    }
}

fn pct_str(u: &Usage, name: &str) -> String {
    if u.ok { format!("{} {:.0}%", name, u.pct * 100.0) }
    else    { format!("{} --", name) }
}

// Humanize seconds since last activity → "now / 12s ago / 5m ago / 3h ago".
fn humanize_age(sec: i32) -> String {
    if sec < 0 { return String::new(); }
    let s = sec as u32;
    if s < 5 { "now".to_string() }
    else if s < 60 { format!("{}s ago", s) }
    else if s < 3600 { format!("{}m ago", s / 60) }
    else if s < 86400 { format!("{}h ago", s / 3600) }
    else { format!("{}d ago", s / 86400) }
}

// Bare duration → "45s / 5m / 3h".
fn humanize_dur(sec: i32) -> String {
    if sec < 0 { return String::new(); }
    let s = sec as u32;
    if s < 60 { format!("{}s", s) }
    else if s < 3600 { format!("{}m", s / 60) }
    else { format!("{}h", s / 3600) }
}

// Greedy word-wrap into up to `max` slices of <= `cols` chars.
fn wrap_lines(s: &str, cols: usize, max: usize) -> heapless::Vec<&str, 4> {
    let mut out: heapless::Vec<&str, 4> = heapless::Vec::new();
    let bytes = s.as_bytes();
    let mut start = 0usize;
    while start < s.len() && out.len() < max {
        let mut end = (start + cols).min(s.len());
        if end < s.len() {
            if let Some(sp) = s[start..end].rfind(' ') { if sp > 0 { end = start + sp; } }
        }
        let _ = out.push(s[start..end].trim_end());
        start = end;
        while bytes.get(start) == Some(&b' ') { start += 1; }
    }
    out
}

fn render_sessions<D: DrawTarget<Color = Rgb565>>(display: &mut D, ds: &DisplayState) {
    // Usage header (self-clearing bboxes)
    let claude_h = pct_str(&ds.claude, "Claude");
    let codex_h  = pct_str(&ds.codex,  "Codex");
    fill(display, 4, 28, 150, 16, c_bg());
    fill(display, 166, 28, 150, 16, c_bg());
    txt(display, &FONT_7X13, &claude_h, 4,   40, Alignment::Left,  c_claude());
    txt(display, &FONT_7X13, &codex_h,  316, 40, Alignment::Right, c_codex());
    fill(display, 0, 45, 320, 1, c_panel());

    // Session cards — 6 fixed slots; draw a card or clear the empty slot.
    let n = if ds.offline { 0 } else { ds.sessions.len().min(6) };
    if !ds.offline && ds.sessions.is_empty() {
        fill(display, 0, 46, 320, 168, c_bg());
        txt(display, &FONT_9X15, "no sessions", 160, 130, Alignment::Center, c_dim());
    } else {
        for i in 0..6 {
            let y = 47 + (i as i32) * 27;
            if i < n {
                let row = &ds.sessions[i];
                let card_bg = if row.status == SessionStatus::Waiting { c_waitdk() } else { c_panel() };
                rfill(display, 2, y, 316, 25, 4, card_bg);     // repaint clears old content
                draw_badge(display, row.tool.as_str(), 5, y + 4);
                txt(display, &FONT_7X13_BOLD, row.project.as_str(), 28, y + 17, Alignment::Left, c_fg());
                let (sym, sc) = match row.status {
                    SessionStatus::Working => (">>",  c_work()),
                    SessionStatus::Waiting => ("!",   c_wait()),
                    SessionStatus::Idle    => ("Zzz", c_dim()),
                };
                txt(display, &FONT_9X15_BOLD, sym, 312, y + 17, Alignment::Right, sc);
            } else {
                fill(display, 2, y, 316, 25, c_bg());            // erase removed card
            }
        }
    }

    // Footer / offline banner
    fill(display, 0, 224, 320, 16, c_bg());
    if ds.offline {
        rfill(display, 2, 215, 316, 23, 5, c_offline());
        txt(display, &FONT_9X15_BOLD, "hub offline", 160, 231, Alignment::Center, c_fg());
    } else {
        let working = ds.sessions.iter().filter(|s| s.status == SessionStatus::Working).count();
        let waiting = ds.sessions.iter().filter(|s| s.status == SessionStatus::Waiting).count();
        let more    = if ds.sessions.len() > 6 { "   +more" } else { "" };
        let footer  = format!("{} working   {} waiting{}", working, waiting, more);
        txt(display, &FONT_7X13, &footer, 4, 234, Alignment::Left, c_dim());
    }
}

// Single-session detail overlay (tap a card to open, tap anywhere to go back).
fn render_detail<D: DrawTarget<Color = Rgb565>>(display: &mut D, row: &SessionRow) {
    let is_codex = row.tool.as_str() == "codex";
    let (pname, accent) = if is_codex { ("Codex", c_codex()) } else { ("Claude", c_claude()) };

    // back hint + id
    fill(display, 0, 28, 320, 16, c_bg());
    txt(display, &FONT_7X13, "< back", 6, 40, Alignment::Left, c_dim());
    if !row.id.is_empty() {
        txt(display, &FONT_7X13, row.id.as_str(), 314, 40, Alignment::Right, c_dim());
    }

    // provider icon + project + provider name
    if is_codex { icons::draw_codex(display, 8, 54); } else { icons::draw_claude(display, 8, 54); }
    fill(display, 32, 58, 288, 38, c_bg());
    txt(display, &FONT_9X15_BOLD, row.project.as_str(), 32, 74, Alignment::Left, c_fg());
    txt(display, &FONT_7X13, pname, 32, 92, Alignment::Left, accent);

    // status + age
    let (sw, sc) = match row.status {
        SessionStatus::Working => ("working", c_work()),
        SessionStatus::Waiting => ("waiting", c_wait()),
        SessionStatus::Idle    => ("idle",    c_dim()),
    };
    fill(display, 0, 106, 320, 18, c_bg());
    txt(display, &FONT_9X15, sw, 8, 120, Alignment::Left, sc);
    let age = humanize_age(row.age_sec);
    if !age.is_empty() { txt(display, &FONT_7X13, &age, 314, 120, Alignment::Right, c_dim()); }

    // waiting line
    fill(display, 0, 128, 320, 15, c_bg());
    if row.status == SessionStatus::Waiting && row.wait_sec >= 0 {
        let w = format!("waiting {}", humanize_dur(row.wait_sec));
        txt(display, &FONT_7X13, &w, 8, 140, Alignment::Left, c_wait());
    }

    fill(display, 8, 150, 304, 1, c_panel());   // separator

    // summary (wrapped, up to 3 lines)
    fill(display, 0, 156, 320, 52, c_bg());
    if !row.summary.is_empty() {
        for (i, line) in wrap_lines(row.summary.as_str(), 42, 3).iter().enumerate() {
            txt(display, &FONT_7X13, line, 8, 168 + (i as i32) * 16, Alignment::Left, c_fg());
        }
    } else {
        txt(display, &FONT_7X13, "(no summary)", 8, 168, Alignment::Left, c_dim());
    }

    // ack button (waiting only) or hint
    fill(display, 0, 206, 320, 32, c_bg());
    if row.status == SessionStatus::Waiting {
        rfill(display, 8, 210, 150, 26, 5, c_wait());
        txt(display, &FONT_7X13, "Clear waiting", 83, 227, Alignment::Center, c_bg());
        txt(display, &FONT_7X13, "tap = back", 314, 227, Alignment::Right, c_dim());
    } else {
        txt(display, &FONT_7X13, "tap anywhere to go back", 160, 227, Alignment::Center, c_dim());
    }
}

// resetSec -> "Xh Ym" (>=60min) or "Ym". Mirrors ui.cpp::fmt_reset.
fn fmt_reset(reset_sec: u32) -> String {
    let mins = reset_sec / 60;
    if mins >= 60 { format!("{}h {}m", mins / 60, mins % 60) } else { format!("{}m", mins) }
}

// sec -> "Xd Yh" (>=24h) / "Xh Ym" (>=1h) / "Ym"; negative -> "--".
// Mirrors ui.cpp::fmt_long.
fn fmt_long(sec: i32) -> String {
    if sec < 0 { return "--".to_string(); }
    let mins = (sec as u32) / 60;
    let hrs  = mins / 60;
    if hrs >= 24 { format!("{}d {}h", hrs / 24, hrs % 24) }
    else if hrs >= 1 { format!("{}h {}m", hrs, mins % 60) }
    else { format!("{}m", mins) }
}

// Projection line text + color. Mirrors ui.cpp::projection_text decision tree.
fn projection(u: &Usage) -> (String, Rgb565) {
    if !u.ok { return (String::new(), c_dim()); }
    if u.will_exhaust && !u.eta_clock.is_empty() {
        return (format!("out ~{}", u.eta_clock.as_str()), c_wait());
    }
    if u.burn_per_hr > 0.0 && u.leftover_pct >= 0.0 {
        let spare = (u.leftover_pct * 100.0 + 0.5).max(0.0) as i32;
        return (format!("{}% to spare", spare), c_work());
    }
    ("steady".to_string(), c_dim())
}

// One provider block: icon, label, progress bar, projection line.
fn usage_provider<D: DrawTarget<Color = Rgb565>>(
    display: &mut D, u: &Usage, name: &str, accent: Rgb565,
    icon_y: i32, label_base: i32, bar_y: i32, proj_base: i32, is_codex: bool,
) {
    if is_codex { icons::draw_codex(display, 8, icon_y); }
    else        { icons::draw_claude(display, 8, icon_y); }

    let label = if u.ok {
        format!("{} {:.0}%   resets {}", name, u.pct * 100.0, fmt_reset(u.reset_sec))
    } else {
        format!("{} --", name)
    };
    fill(display, 32, label_base - 13, 288, 17, c_bg());          // clear old label
    txt(display, &FONT_9X15, &label, 32, label_base, Alignment::Left, accent);

    // progress bar — trough repaint erases the old fill length
    let bw: u32 = 264;
    rfill(display, 8, bar_y, bw, 16, 4, c_panel());
    if u.ok {
        let fw = (u.pct.clamp(0.0, 1.0) * bw as f32) as u32;
        if fw > 0 { rfill(display, 8, bar_y, fw, 16, 4, accent); }
    }

    // projection line (always clear so a removed line disappears)
    fill(display, 32, proj_base - 11, 260, 15, c_bg());
    let (ptxt, pcol) = projection(u);
    if !ptxt.is_empty() {
        txt(display, &FONT_7X13, &ptxt, 32, proj_base, Alignment::Left, pcol);
    }
}

fn render_usage<D: DrawTarget<Color = Rgb565>>(display: &mut D, ds: &DisplayState) {
    // Claude block
    usage_provider(display, &ds.claude, "Claude", c_claude(),
                   34, 48, 58, 86, false);
    // Codex block
    usage_provider(display, &ds.codex, "Codex", c_codex(),
                   96, 110, 120, 148, true);

    // Weekly reset countdown — prefer whichever provider reports it (claude first)
    let wsec = if ds.claude.ok && ds.claude.week_reset_sec >= 0 { ds.claude.week_reset_sec }
               else if ds.codex.ok && ds.codex.week_reset_sec >= 0 { ds.codex.week_reset_sec }
               else { -1 };
    fill(display, 8, 162, 240, 14, c_bg());
    if wsec >= 0 {
        let wl = format!("week resets in {}", fmt_long(wsec));
        txt(display, &FONT_7X13, &wl, 8, 174, Alignment::Left, c_dim());
    }

    // Weekly utilization bars (Claude then Codex)
    week_bar(display, &ds.claude, c_claude(), 184, 190);
    week_bar(display, &ds.codex,  c_codex(),  199, 205);

    if ds.offline {
        rfill(display, 2, 215, 316, 23, 5, c_offline());
        txt(display, &FONT_9X15_BOLD, "hub offline", 160, 231, Alignment::Center, c_fg());
    }
}

fn week_bar<D: DrawTarget<Color = Rgb565>>(
    display: &mut D, u: &Usage, accent: Rgb565, bar_y: i32, val_base: i32,
) {
    let bw: u32 = 190;
    rfill(display, 8, bar_y, bw, 8, 3, c_panel());
    fill(display, 202, val_base - 11, 80, 14, c_bg());
    if u.ok && u.week_pct >= 0.0 {
        let wp = (u.week_pct * 100.0 + 0.5).clamp(0.0, 100.0) as i32;
        let fw = (u.week_pct.clamp(0.0, 1.0) * bw as f32) as u32;
        if fw > 0 { rfill(display, 8, bar_y, fw, 8, 3, accent); }
        let s = format!("wk {}%", wp);
        txt(display, &FONT_7X13, &s, 202, val_base, Alignment::Left, accent);
    }
}

// Settings tab: brightness slider + sleep-timeout chips, drawn and hit-tested
// manually (no widget toolkit). Layout in absolute screen coords below the
// 26px tab bar.
//   Brightness:  label y=44, track (8,54,304,14), knob follows fill
//   Sleep:       label y=104, chips row (4,118,..,34) 5 segments
fn render_settings<D: DrawTarget<Color = Rgb565>>(display: &mut D, set: &Settings) {
    // ── Brightness ──
    txt(display, &FONT_7X13, "Brightness", 8, 48, Alignment::Left, c_fg());
    let bv = format!("{}%", set.brightness);
    fill(display, 250, 38, 66, 14, c_bg());
    txt(display, &FONT_7X13, &bv, 312, 48, Alignment::Right, c_dim());
    let tx = 8i32; let tw: u32 = 304;
    fill(display, 4, 50, 312, 24, c_bg());              // clear slider band (old knob)
    fill(display, tx, 56, tw, 12, c_panel());
    let fillw = ((set.brightness.saturating_sub(10)) as u32 * tw) / 90;
    if fillw > 0 { fill(display, tx, 56, fillw, 12, c_claude()); }
    let knob_x = tx + fillw as i32;
    rfill(display, knob_x - 4, 52, 8, 20, 2, c_claude());

    // ── Sleep after ──
    txt(display, &FONT_7X13, "Sleep after", 8, 104, Alignment::Left, c_fg());
    let sel = SLEEP_VALS.iter().position(|&v| v == set.sleep_min).unwrap_or(0);
    for i in 0..5 {
        let cx = 4 + (i as i32) * 62;
        let on = i == sel;
        rfill(display, cx, 114, 60, 34, 5, if on { c_claude() } else { c_panel() });
        txt(display, &FONT_7X13, SLEEP_LBL[i], cx + 30, 135, Alignment::Center,
            if on { c_bg() } else { c_dim() });
    }

    // ── Theme switch ──
    let tlabel = if set.dark { "Theme: Dark" } else { "Theme: Light" };
    fill(display, 8, 164, 232, 16, c_bg());
    txt(display, &FONT_7X13, tlabel, 8, 176, Alignment::Left, c_fg());
    fill(display, 246, 162, 70, 28, c_bg());                 // clear switch area
    rfill(display, 250, 166, 62, 22, 11, c_panel());          // track
    let knob_x = if set.dark { 292 } else { 252 };            // right=dark, left=light
    rfill(display, knob_x, 168, 18, 18, 9, if set.dark { c_claude() } else { c_dim() });
}

// Map a touch on the Settings tab to a settings change. Returns true if changed.
fn settings_touch(sx: i32, sy: i32, set: &mut Settings) -> bool {
    // Brightness band
    if (50..=74).contains(&sy) && (4..=316).contains(&sx) {
        let p = 10 + ((sx - 8).clamp(0, 304) as u32 * 90 / 304) as u8;
        let p = p.clamp(10, 100);
        if p != set.brightness { set.brightness = p; return true; }
        return false;
    }
    // Sleep chips
    if (114..=148).contains(&sy) && (4..=314).contains(&sx) {
        let i = (((sx - 4) / 62).clamp(0, 4)) as usize;
        let v = SLEEP_VALS[i];
        if v != set.sleep_min { set.sleep_min = v; return true; }
    }
    // Theme switch
    if (162..=190).contains(&sy) && (246..=316).contains(&sx) {
        set.dark = !set.dark;
        return true;
    }
    false
}

// ── XPT2046 touch (bit-bang) ──────────────────────────────────────────────────
//
// CYD pinout:  T_CS=GPIO33  T_CLK=GPIO25  T_DIN=GPIO32  T_DO=GPIO39  T_IRQ=GPIO36

fn xpt_send_recv<CS, CLK, MOSI, MISO>(
    cs:   &mut PinDriver<'_, CS, Output>,
    clk:  &mut PinDriver<'_, CLK, Output>,
    mosi: &mut PinDriver<'_, MOSI, Output>,
    miso: &PinDriver<'_, MISO, Input>,
    cmd:  u8,
) -> u16
where
    CS:   esp_idf_hal::gpio::OutputPin,
    CLK:  esp_idf_hal::gpio::OutputPin,
    MOSI: esp_idf_hal::gpio::OutputPin,
    MISO: esp_idf_hal::gpio::InputPin,
{
    let delay = || { for _ in 0..240u32 { core::hint::spin_loop(); } };
    cs.set_low().ok();
    for i in (0..8).rev() {
        mosi.set_level(if (cmd >> i) & 1 != 0 { Level::High } else { Level::Low }).ok();
        delay(); clk.set_high().ok(); delay(); clk.set_low().ok();
    }
    // One BUSY clock after the control byte, then 12 data bits MSB-first.
    clk.set_high().ok(); delay(); clk.set_low().ok(); delay();
    let mut result: u16 = 0;
    for _ in 0..12 {
        clk.set_high().ok(); delay();
        result = (result << 1) | (miso.get_level() == Level::High) as u16;
        clk.set_low().ok(); delay();
    }
    cs.set_high().ok();
    result
}

// XPT2046 touch-pressure read (Z1/Z2), independent of the PENIRQ pin.
// Returns a combined pressure value: ~0 when untouched, rising when pressed.
// z = z1 + (4095 - z2), per the XPT2046 datasheet / PaulStoffregen library.
fn xpt_pressure<CS, CLK, MOSI, MISO>(
    cs:   &mut PinDriver<'_, CS, Output>,
    clk:  &mut PinDriver<'_, CLK, Output>,
    mosi: &mut PinDriver<'_, MOSI, Output>,
    miso: &PinDriver<'_, MISO, Input>,
) -> u16
where
    CS:   esp_idf_hal::gpio::OutputPin,
    CLK:  esp_idf_hal::gpio::OutputPin,
    MOSI: esp_idf_hal::gpio::OutputPin,
    MISO: esp_idf_hal::gpio::InputPin,
{
    let z1 = xpt_send_recv(cs, clk, mosi, miso, 0xB1); // Z1
    let z2 = xpt_send_recv(cs, clk, mosi, miso, 0xC1); // Z2
    z1.saturating_add(4095u16.saturating_sub(z2))
}

// Pressure above this counts as a touch. Tunable from heartbeat logs.
const Z_TOUCH: u16 = 400;

// Map raw XPT2046 readings to screen coords. Calibration + axis assignment
// match the original VibeMonitor firmware (display.cpp::touch_pressed with
// XPT2046_Touchscreen::setRotation(1)): cmd 0x91 -> X, cmd 0xD1 -> Y.
fn raw_to_screen(raw_x: u16, raw_y: u16) -> (i32, i32) {
    const X_MIN: u16 = 200; const X_MAX: u16 = 3900;
    const Y_MIN: u16 = 240; const Y_MAX: u16 = 3800;
    let sx = ((raw_x.saturating_sub(X_MIN) as u32 * 320) / (X_MAX - X_MIN) as u32).min(319) as i32;
    let sy = ((raw_y.saturating_sub(Y_MIN) as u32 * 240) / (Y_MAX - Y_MIN) as u32).min(239) as i32;
    (sx, sy)
}

// ── Backlight (LEDC PWM) + settings persistence ────────────────────────────────

fn set_brightness(bl: &mut LedcDriver<'_>, pct: u8) {
    let pct = pct.clamp(10, 100) as u32;
    let max = bl.get_max_duty();
    let _ = bl.set_duty(pct * max / 100);
}

fn settings_load(nvs: &EspNvs<NvsDefault>) -> Settings {
    let brightness = nvs.get_u8("bright").ok().flatten().unwrap_or(100).clamp(10, 100);
    let sleep_min  = snap_sleep(nvs.get_u16("sleep").ok().flatten().unwrap_or(0));
    let dark       = nvs.get_u8("dark").ok().flatten().unwrap_or(1) != 0;
    DARK.store(dark, Ordering::Relaxed);
    Settings { brightness, sleep_min, dark }
}

fn settings_save(nvs: &mut EspNvs<NvsDefault>, s: &Settings) {
    let _ = nvs.set_u8("bright", s.brightness);
    let _ = nvs.set_u16("sleep", s.sleep_min);
    let _ = nvs.set_u8("dark", s.dark as u8);
}

// Acknowledge a waiting session. USB: emit a clean `{"ack":"<id>"}` line that the
// serial bridge forwards to POST /ack. (WiFi build: TODO direct POST.)
#[cfg(not(feature = "wifi"))]
fn send_ack(id: &str) {
    if !id.is_empty() { println!("{{\"ack\":\"{}\"}}", id); }
}
#[cfg(feature = "wifi")]
fn send_ack(_id: &str) { /* TODO: HTTP POST /ack in the WiFi build */ }

// Route a Sessions-tab body touch (sy>=34) to a view change / ack.
// Returns the (possibly new) view.
fn sessions_touch(sx: i32, sy: i32, view: View, ds: &DisplayState) -> View {
    match view {
        View::List => {
            if (46..=214).contains(&sy) && !ds.offline {
                let i = ((sy - 47) / 27) as usize;
                let within = (sy - 47) - (i as i32) * 27 <= 25;
                if within && i < ds.sessions.len().min(6) {
                    return View::Detail { index: i };
                }
            }
            View::List
        }
        View::Detail { index } => {
            // Ack button (waiting sessions) occupies x 8..158, y 210..236.
            if let Some(r) = ds.sessions.get(index) {
                if r.status == SessionStatus::Waiting
                    && (8..=158).contains(&sx) && (210..=236).contains(&sy)
                {
                    send_ack(r.id.as_str());
                }
            }
            View::List   // any tap returns to the list
        }
    }
}

// ── main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    std::thread::Builder::new()
        .stack_size(65536)
        .spawn(|| { if let Err(e) = run() { log::error!("fatal: {e:?}"); } })
        .expect("thread spawn")
        .join().ok();
    Ok(())
}

fn run() -> Result<()> {
    let peripherals = Peripherals::take()?;
    #[cfg(feature = "wifi")] let sysloop = EspSystemEventLoop::take()?;

    // NVS — shared partition for settings (and WiFi in the wifi build).
    let nvs_part = EspDefaultNvsPartition::take()?;
    let mut nvs: EspNvs<NvsDefault> = EspNvs::new(nvs_part.clone(), "vibe_set", true)?;
    let mut settings = settings_load(&nvs);

    // Backlight on GPIO21 via LEDC PWM (5 kHz, 8-bit) so brightness is adjustable.
    let bl_timer = LedcTimerDriver::new(
        peripherals.ledc.timer0,
        &TimerConfig::new().frequency(5.kHz().into()).resolution(Resolution::Bits8),
    )?;
    let mut bl = LedcDriver::new(peripherals.ledc.channel0, &bl_timer, peripherals.pins.gpio21)?;
    set_brightness(&mut bl, settings.brightness);

    let spi = SpiDriver::new(
        peripherals.spi2,
        peripherals.pins.gpio14,
        peripherals.pins.gpio13,
        None::<esp_idf_hal::gpio::AnyInputPin>,
        &esp_idf_hal::spi::config::DriverConfig::new(),
    )?;
    let spi_device = SpiDeviceDriver::new(
        spi, Some(peripherals.pins.gpio15),
        &SpiConfig::new().baudrate(55.MHz().into()),
    )?;
    let dc = PinDriver::output(peripherals.pins.gpio2)?;
    let di = SPIInterface::new(spi_device, dc);
    let mut display = Builder::new(ST7789, di)
        .display_size(240, 320)
        .invert_colors(ColorInversion::Normal)   // reference: -DTFT_INVERSION_OFF=1
        .color_order(ColorOrder::Bgr)             // reference: -DTFT_RGB_ORDER=TFT_BGR
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .init(&mut FreeRtos)
        .map_err(|_| anyhow::anyhow!("display init failed"))?;
    display.clear(Rgb565::BLACK).map_err(|_| anyhow::anyhow!("clear failed"))?;

    // Touch (XPT2046 bit-bang)
    let mut t_cs   = PinDriver::output(peripherals.pins.gpio33)?;
    let mut t_clk  = PinDriver::output(peripherals.pins.gpio25)?;
    let mut t_mosi = PinDriver::output(peripherals.pins.gpio32)?;
    let t_miso     = PinDriver::input(peripherals.pins.gpio39)?;
    let _t_irq     = PinDriver::input(peripherals.pins.gpio36)?;
    t_cs.set_high()?; t_clk.set_low()?; t_mosi.set_low()?;

    // ── WiFi transport ────────────────────────────────────────────────────────
    #[cfg(feature = "wifi")]
    {
        info!("WiFi mode");
        let mut wifi = BlockingWifi::wrap(
            EspWifi::new(peripherals.modem, sysloop.clone(), Some(nvs_part.clone()))?, sysloop,
        )?;
        wifi.set_configuration(&Configuration::Client(ClientConfiguration {
            ssid:     WIFI_SSID.try_into().map_err(|_| anyhow::anyhow!("SSID"))?,
            password: WIFI_PASS.try_into().map_err(|_| anyhow::anyhow!("PASS"))?,
            ..Default::default()
        }))?;
        wifi.start()?; wifi.connect()?; wifi.wait_netif_up()?;
        let mut ds = DisplayState::default();
        let mut last_poll   = Instant::now().checked_sub(std::time::Duration::from_secs(100))
                                .unwrap_or_else(Instant::now);
        let mut fail        = 0u8;
        let mut active_tab  = Tab::Sessions;
        let mut view        = View::List;
        let mut was_touched = false;
        let mut last_touch  = Instant::now();
        let mut sleeping    = false;
        let mut prev: Option<(DisplayState, Tab, View, Settings)> = None;
        loop {
            if last_poll.elapsed() >= std::time::Duration::from_millis(POLL_MS) {
                last_poll = Instant::now();
                match fetch_state(BRIDGE_TOKEN, BRIDGE_HOST, BRIDGE_PORT) {
                    Some(s) => { ds = s; ds.offline = false; fail = 0; }
                    None    => { fail = fail.saturating_add(1); if fail >= 3 { ds.offline = true; } }
                }
            }

            // Touch detection via XPT2046 pressure (Z) — independent of IRQ pin.
            let z = xpt_pressure(&mut t_cs, &mut t_clk, &mut t_mosi, &t_miso);
            let now_touched = z > Z_TOUCH;
            if now_touched && !was_touched {
                let raw_x = xpt_send_recv(&mut t_cs, &mut t_clk, &mut t_mosi, &t_miso, 0x91);
                let raw_y = xpt_send_recv(&mut t_cs, &mut t_clk, &mut t_mosi, &t_miso, 0xD1);
                let (sx, sy) = raw_to_screen(raw_x, raw_y);
                last_touch = Instant::now();
                if sleeping {
                    sleeping = false;
                    set_brightness(&mut bl, settings.brightness);
                    prev = None;
                } else if sy < 34 {
                    active_tab = if sx < 107 { Tab::Sessions }
                                 else if sx < 214 { Tab::Usage }
                                 else { Tab::Settings };
                    view = View::List;
                } else if active_tab == Tab::Sessions {
                    view = sessions_touch(sx, sy, view, &ds);
                } else if active_tab == Tab::Settings {
                    let prev_dark = settings.dark;
                    if settings_touch(sx, sy, &mut settings) {
                        if settings.dark != prev_dark {
                            DARK.store(settings.dark, Ordering::Relaxed);
                            prev = None;   // repaint whole screen in the new palette
                        }
                        set_brightness(&mut bl, settings.brightness);
                        settings_save(&mut nvs, &settings);
                    }
                }
            }
            was_touched = now_touched;

            if ds.sessions.iter().any(|s| s.status == SessionStatus::Waiting) {
                last_touch = Instant::now();
                if sleeping { sleeping = false; set_brightness(&mut bl, settings.brightness); prev = None; }
            }

            if !sleeping && settings.sleep_min > 0
                && last_touch.elapsed() >= std::time::Duration::from_secs(settings.sleep_min as u64 * 60)
            {
                sleeping = true;
                let _ = bl.set_duty(0);
            }

            if !sleeping {
                let layout_changed = prev.as_ref()
                    .map(|p| p.1 != active_tab || p.2 != view).unwrap_or(true);
                let content_changed = prev.as_ref().map(|p| match active_tab {
                    Tab::Settings => p.3 != settings,
                    _             => p.0 != ds,
                }).unwrap_or(true);
                if layout_changed || content_changed {
                    render(&mut display, &ds, active_tab, view, &settings, layout_changed);
                    prev = Some((ds.clone(), active_tab, view, settings));
                }
            }
            FreeRtos::delay_ms(50);
        }
    }

    // ── USB transport ─────────────────────────────────────────────────────────
    #[cfg(not(feature = "wifi"))]
    {
        info!("USB mode");
        let shared  = Arc::new(Mutex::new((DisplayState::default(), Instant::now())));
        let shared2 = shared.clone();

        std::thread::Builder::new()
            .stack_size(16384)
            .spawn(move || {
                use std::io::Read;
                let stdin   = std::io::stdin();
                let mut buf = [0u8; 1024];
                let mut len = 0usize;
                let mut tmp = [0u8; 128];
                loop {
                    let n = stdin.lock().read(&mut tmp).unwrap_or(0);
                    if n == 0 { FreeRtos::delay_ms(5); continue; }
                    let copy = n.min(buf.len() - len);
                    buf[len..len + copy].copy_from_slice(&tmp[..copy]);
                    len += copy;
                    while let Some(nl) = buf[..len].iter().position(|&b| b == b'\n') {
                        if let Ok(s) = std::str::from_utf8(&buf[..nl]) {
                            let t = s.trim();
                            if t.starts_with('{') {
                                if let Some(ds) = parse_state(t) {
                                    let mut g = shared2.lock().unwrap();
                                    g.0 = ds; g.1 = Instant::now();
                                }
                            }
                        }
                        let rest = len - (nl + 1);
                        buf.copy_within(nl + 1..len, 0);
                        len = rest;
                    }
                    if len >= buf.len() { len = 0; }
                }
            }).expect("reader thread");

        let mut active_tab  = Tab::Sessions;
        let mut view        = View::List;
        let mut was_touched = false;
        let mut hb          = 0u32;
        let mut last_touch  = Instant::now();
        let mut sleeping    = false;
        let mut prev: Option<(DisplayState, Tab, View, Settings)> = None;

        loop {
            // Current state first, so touch handling can hit-test sessions.
            let (ds, last_rx) = { let g = shared.lock().unwrap(); (g.0.clone(), g.1) };
            let mut state = ds;
            state.offline = last_rx.elapsed().as_secs() > 6;

            // Touch detection via XPT2046 pressure (Z) — independent of IRQ pin.
            let z = xpt_pressure(&mut t_cs, &mut t_clk, &mut t_mosi, &t_miso);
            let now_touched = z > Z_TOUCH;
            hb = hb.wrapping_add(1);
            if hb % 40 == 0 {
                info!("hb z={} sleeping={}", z, sleeping);
            }
            if now_touched && !was_touched {
                let raw_x = xpt_send_recv(&mut t_cs, &mut t_clk, &mut t_mosi, &t_miso, 0x91);
                let raw_y = xpt_send_recv(&mut t_cs, &mut t_clk, &mut t_mosi, &t_miso, 0xD1);
                let (sx, sy) = raw_to_screen(raw_x, raw_y);
                last_touch = Instant::now();
                if sleeping {
                    sleeping = false;
                    set_brightness(&mut bl, settings.brightness);
                    prev = None;
                } else if sy < 34 {
                    active_tab = if sx < 107 { Tab::Sessions }
                                 else if sx < 214 { Tab::Usage }
                                 else { Tab::Settings };
                    view = View::List;
                } else if active_tab == Tab::Sessions {
                    view = sessions_touch(sx, sy, view, &state);
                } else if active_tab == Tab::Settings {
                    let prev_dark = settings.dark;
                    if settings_touch(sx, sy, &mut settings) {
                        if settings.dark != prev_dark {
                            DARK.store(settings.dark, Ordering::Relaxed);
                            prev = None;   // repaint whole screen in the new palette
                        }
                        set_brightness(&mut bl, settings.brightness);
                        settings_save(&mut nvs, &settings);
                    }
                }
            }
            was_touched = now_touched;

            // Keep the screen awake while any session is waiting (alert).
            if state.sessions.iter().any(|s| s.status == SessionStatus::Waiting) {
                last_touch = Instant::now();
                if sleeping { sleeping = false; set_brightness(&mut bl, settings.brightness); prev = None; }
            }

            // Screen-off timeout
            if !sleeping && settings.sleep_min > 0
                && last_touch.elapsed() >= std::time::Duration::from_secs(settings.sleep_min as u64 * 60)
            {
                sleeping = true;
                let _ = bl.set_duty(0);
            }

            if !sleeping {
                let layout_changed = prev.as_ref()
                    .map(|p| p.1 != active_tab || p.2 != view).unwrap_or(true);
                let content_changed = prev.as_ref().map(|p| match active_tab {
                    Tab::Settings => p.3 != settings,
                    _             => p.0 != state,
                }).unwrap_or(true);
                if layout_changed || content_changed {
                    render(&mut display, &state, active_tab, view, &settings, layout_changed);
                    prev = Some((state, active_tab, view, settings));
                }
            }
            FreeRtos::delay_ms(50);
        }
    }
}
