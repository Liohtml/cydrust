use anyhow::Result;
use embedded_graphics::{
    mono_font::{
        ascii::{FONT_10X20, FONT_7X13, FONT_7X13_BOLD, FONT_9X15, FONT_9X15_BOLD},
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
    prelude::*,
    spi::{config::Config as SpiConfig, SpiDeviceDriver, SpiDriver},
};
use heapless::String as HString;
use log::info;
use mipidsi::{
    models::ST7789,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
    Builder,
};
use std::time::Instant;

#[cfg(feature = "wifi")]
use embedded_svc::{http::client::Client as HttpClient, http::Method, io::Read as EmbeddedRead};
#[cfg(feature = "wifi")]
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    http::client::{Configuration as HttpConfig, EspHttpConnection},
    nvs::EspDefaultNvsPartition,
    wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};
#[cfg(not(feature = "wifi"))]
use std::sync::{Arc, Mutex};

// ── Data model ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tab { Sessions, Usage, Settings }

#[derive(Debug, Clone, PartialEq)]
enum SessionStatus { Working, Idle, Waiting }

#[derive(Debug, Clone, PartialEq)]
struct SessionRow {
    project: HString<32>,
    status:  SessionStatus,
    tool:    HString<8>,
}

#[derive(Debug, Default, Clone, PartialEq)]
struct DisplayState {
    sessions:   heapless::Vec<SessionRow, 8>,
    claude_pct: Option<f32>,
    codex_pct:  Option<f32>,
    offline:    bool,
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
                            let _ = ds.sessions.push(SessionRow { project: p, status, tool: t });
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
    parse_pct(text, "\"claude\":{", &mut ds.claude_pct);
    parse_pct(text, "\"codex\":{",  &mut ds.codex_pct);
    Some(ds)
}

fn parse_pct(text: &str, marker: &str, out: &mut Option<f32>) {
    if let Some(pos) = text.find(marker) {
        if let Some(pp) = text[pos..].find("\"pct\":") {
            let rest = &text[pos + pp + 6..];
            let end  = rest.find(|c: char| c == ',' || c == '}').unwrap_or(rest.len());
            if let Ok(v) = rest[..end].trim().parse::<f32>() {
                *out = Some(v * 100.0);
            }
        }
    }
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

// ── Colour palette ────────────────────────────────────────────────────────────
// ColorInversion::Inverted cancels the ST7789 panel's native inversion,
// so stored Rgb565 values display directly as-is.
const C_BG:      Rgb565 = Rgb565::new(2,  5,  2);   // near-black #141414
const C_PANEL:   Rgb565 = Rgb565::new(5,  11, 5);   // dark card  #2B2B2B
const C_FG:      Rgb565 = Rgb565::new(29, 59, 29);  // near-white #EDEDED
const C_DIM:     Rgb565 = Rgb565::new(17, 34, 17);  // dim        #8A8A8A
const C_CLAUDE:  Rgb565 = Rgb565::new(26, 29, 11);  // orange     #D97757
const C_CODEX:   Rgb565 = Rgb565::new(20, 34, 30);  // purple     #A78BFA
const C_WAIT:    Rgb565 = Rgb565::new(30, 41,  4);  // amber      #F5A623
const C_WAITDK:  Rgb565 = Rgb565::new(9,  12,  0);  // dark amber #4A3000
const C_WORK:    Rgb565 = Rgb565::new(9,  55, 16);  // green      #4ADE80
const C_OFFLINE: Rgb565 = Rgb565::new(28, 18,  9);  // red        #E5484D

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

// ── Tab bar ───────────────────────────────────────────────────────────────────

fn draw_tab_bar<D: DrawTarget<Color = Rgb565>>(d: &mut D, active: Tab) {
    let tabs: &[(&str, i32, u32, Tab)] = &[
        ("SESSIONS", 53,  107, Tab::Sessions),
        ("USAGE",    160, 106, Tab::Usage),
        ("SETTINGS", 267, 107, Tab::Settings),
    ];
    let mut x = 1i32;
    for (label, cx, w, tab) in tabs {
        let (bg, fg) = if *tab == active { (C_CLAUDE, C_BG) } else { (C_PANEL, C_DIM) };
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

fn render<D: DrawTarget<Color = Rgb565>>(display: &mut D, ds: &DisplayState, active: Tab) {
    fill(display, 0, 0, 320, 240, C_BG);
    draw_tab_bar(display, active);

    match active {
        Tab::Sessions => render_sessions(display, ds),
        Tab::Usage    => render_usage(display, ds),
        Tab::Settings => render_settings(display),
    }
}

fn render_sessions<D: DrawTarget<Color = Rgb565>>(display: &mut D, ds: &DisplayState) {
    // Usage header
    let claude_h = match ds.claude_pct {
        Some(p) => format!("Claude {:.0}%", p),
        None    => "Claude --".to_string(),
    };
    let codex_h = match ds.codex_pct {
        Some(p) => format!("Codex {:.0}%", p),
        None    => "Codex --".to_string(),
    };
    txt(display, &FONT_7X13, &claude_h, 4,   40, Alignment::Left,  C_CLAUDE);
    txt(display, &FONT_7X13, &codex_h,  316, 40, Alignment::Right, C_CODEX);
    fill(display, 0, 45, 320, 1, C_PANEL);

    // Session cards
    if !ds.offline {
        if ds.sessions.is_empty() {
            txt(display, &FONT_9X15, "no sessions", 160, 130, Alignment::Center, C_DIM);
        } else {
            for (i, row) in ds.sessions.iter().take(6).enumerate() {
                let y = 47 + (i as i32) * 27;
                let card_bg = if row.status == SessionStatus::Waiting { C_WAITDK } else { C_PANEL };

                // Card (rounded, 4 px radius)
                rfill(display, 2, y, 316, 25, 4, card_bg);

                // Provider accent strip (6 px wide, full card height)
                let accent = if row.tool.as_str() == "codex" { C_CODEX } else { C_CLAUDE };
                fill(display, 2, y, 6, 25, accent);

                // Project name
                txt(display, &FONT_7X13_BOLD, row.project.as_str(),
                    14, y + 17, Alignment::Left, C_FG);

                // Status symbol
                let (sym, sc) = match row.status {
                    SessionStatus::Working => (">>", C_WORK),
                    SessionStatus::Waiting => ("!",  C_WAIT),
                    SessionStatus::Idle    => ("z",  C_DIM),
                };
                txt(display, &FONT_9X15_BOLD, sym, 312, y + 17, Alignment::Right, sc);
            }
        }
    }

    // Footer / offline banner
    if ds.offline {
        rfill(display, 2, 215, 316, 23, 5, C_OFFLINE);
        txt(display, &FONT_9X15_BOLD, "hub offline", 160, 231, Alignment::Center, C_FG);
    } else {
        let working = ds.sessions.iter().filter(|s| s.status == SessionStatus::Working).count();
        let waiting = ds.sessions.iter().filter(|s| s.status == SessionStatus::Waiting).count();
        let footer  = format!("{} working   {} waiting", working, waiting);
        txt(display, &FONT_7X13, &footer, 4, 234, Alignment::Left, C_DIM);
    }
}

fn render_usage<D: DrawTarget<Color = Rgb565>>(display: &mut D, ds: &DisplayState) {
    txt(display, &FONT_10X20, "CLAUDE", 8, 62, Alignment::Left, C_CLAUDE);
    let bx = 8i32;
    let bw: u32 = 280;
    fill(display, bx, 68, bw, 20, C_PANEL);
    if let Some(pct) = ds.claude_pct {
        let fw = ((pct.min(100.0) / 100.0) * bw as f32) as u32;
        if fw > 0 { fill(display, bx, 68, fw, 20, C_CLAUDE); }
        let s = format!("{:.0}%", pct);
        txt(display, &FONT_7X13_BOLD, &s, bx + bw as i32 + 6, 83, Alignment::Left, C_CLAUDE);
    } else {
        txt(display, &FONT_7X13, "--", bx + bw as i32 + 6, 83, Alignment::Left, C_DIM);
    }

    txt(display, &FONT_10X20, "CODEX", 8, 126, Alignment::Left, C_CODEX);
    fill(display, bx, 132, bw, 20, C_PANEL);
    if let Some(pct) = ds.codex_pct {
        let fw = ((pct.min(100.0) / 100.0) * bw as f32) as u32;
        if fw > 0 { fill(display, bx, 132, fw, 20, C_CODEX); }
        let s = format!("{:.0}%", pct);
        txt(display, &FONT_7X13_BOLD, &s, bx + bw as i32 + 6, 147, Alignment::Left, C_CODEX);
    } else {
        txt(display, &FONT_7X13, "--", bx + bw as i32 + 6, 147, Alignment::Left, C_DIM);
    }

    if ds.offline {
        rfill(display, 2, 215, 316, 23, 5, C_OFFLINE);
        txt(display, &FONT_9X15_BOLD, "hub offline", 160, 231, Alignment::Center, C_FG);
    }
}

fn render_settings<D: DrawTarget<Color = Rgb565>>(display: &mut D) {
    txt(display, &FONT_9X15_BOLD, "Settings", 160, 120, Alignment::Center, C_DIM);
    txt(display, &FONT_7X13, "(coming soon)", 160, 140, Alignment::Center, C_DIM);
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
    #[cfg(feature = "wifi")] let nvs     = EspDefaultNvsPartition::take()?;

    let mut bl = PinDriver::output(peripherals.pins.gpio21)?;
    bl.set_high()?;

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
    let t_irq      = PinDriver::input(peripherals.pins.gpio36)?;
    t_cs.set_high()?; t_clk.set_low()?; t_mosi.set_low()?;

    // ── WiFi transport ────────────────────────────────────────────────────────
    #[cfg(feature = "wifi")]
    {
        info!("WiFi mode");
        let mut wifi = BlockingWifi::wrap(
            EspWifi::new(peripherals.modem, sysloop.clone(), Some(nvs))?, sysloop,
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
        let mut was_touched = false;
        let mut prev: Option<(DisplayState, Tab)> = None;
        loop {
            let now_touched = t_irq.get_level() == Level::Low;
            if now_touched && !was_touched {
                let raw_x = xpt_send_recv(&mut t_cs, &mut t_clk, &mut t_mosi, &t_miso, 0x91);
                let raw_y = xpt_send_recv(&mut t_cs, &mut t_clk, &mut t_mosi, &t_miso, 0xD1);
                let (sx, sy) = raw_to_screen(raw_x, raw_y);
                info!("touch raw=({},{}) screen=({},{})", raw_x, raw_y, sx, sy);
                if sy < 26 {
                    active_tab = if sx < 107 { Tab::Sessions }
                                 else if sx < 214 { Tab::Usage }
                                 else { Tab::Settings };
                }
            }
            was_touched = now_touched;
            if last_poll.elapsed() >= std::time::Duration::from_millis(POLL_MS) {
                last_poll = Instant::now();
                match fetch_state(BRIDGE_TOKEN, BRIDGE_HOST, BRIDGE_PORT) {
                    Some(s) => { ds = s; ds.offline = false; fail = 0; }
                    None    => { fail = fail.saturating_add(1); if fail >= 3 { ds.offline = true; } }
                }
            }
            let key = (ds.clone(), active_tab);
            if prev.as_ref().map(|p| p != &key).unwrap_or(true) {
                render(&mut display, &key.0, key.1);
                prev = Some(key);
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
                let mut buf = [0u8; 512];
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
        let mut was_touched = false;
        let mut prev: Option<(DisplayState, Tab)> = None;

        loop {
            let now_touched = t_irq.get_level() == Level::Low;
            if now_touched && !was_touched {
                let raw_x = xpt_send_recv(&mut t_cs, &mut t_clk, &mut t_mosi, &t_miso, 0x91);
                let raw_y = xpt_send_recv(&mut t_cs, &mut t_clk, &mut t_mosi, &t_miso, 0xD1);
                let (sx, sy) = raw_to_screen(raw_x, raw_y);
                info!("touch raw=({},{}) screen=({},{})", raw_x, raw_y, sx, sy);
                if sy < 26 {
                    active_tab = if sx < 107 { Tab::Sessions }
                                 else if sx < 214 { Tab::Usage }
                                 else { Tab::Settings };
                }
            }
            was_touched = now_touched;

            let (ds, last_rx) = { let g = shared.lock().unwrap(); (g.0.clone(), g.1) };
            let mut state = ds;
            state.offline = last_rx.elapsed().as_secs() > 6;

            let key = (state, active_tab);
            if prev.as_ref().map(|p| p != &key).unwrap_or(true) {
                render(&mut display, &key.0, key.1);
                prev = Some(key);
            }
            FreeRtos::delay_ms(50);
        }
    }
}
