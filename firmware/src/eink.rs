//! e-ink variant — low-power monochrome SPI e-paper status view.
//!
//! This is the `--features eink` DISPLAY path. It replaces the colour ST7789
//! LCD + tabbed `embedded-graphics` Rgb565 UI with a single, static, glanceable
//! black-on-white status screen drawn for a Waveshare B/W e-paper panel.
//!
//! Why a separate view (and not the colour UI):
//!   e-paper panels are 1-bit (`BinaryColor` / the driver's `Color`); the rich
//!   Rgb565 cards/bars/badges do not map to them, and a full refresh takes
//!   ~2-4 s and must NOT run continuously (panel longevity + power). So the
//!   e-ink build shows a compact summary and only re-renders + full-refreshes
//!   when `DisplayState` actually changes, throttled to ~once per `MIN_REFRESH`.
//!   Between refreshes the loop idles (low power).
//!
//! ── Assumed wiring (DOCUMENTED ASSUMPTION) ───────────────────────────────────
//! The CYD has no e-paper connector, so this variant targets a user who rewires
//! a CYD or uses a bare ESP32 + a Waveshare e-paper HAT. We drive the e-paper on
//! a DEDICATED SPI bus (SPI3 / VSPI) using GPIOs that are free on a bare ESP32
//! and not on the colour-LCD SPI bus, so the two display paths never contend:
//!
//!     e-paper signal   ESP32 GPIO   note
//!     ──────────────   ──────────   ───────────────────────────────────────────
//!     CLK  (SCK)       GPIO18       VSPI default SCLK
//!     DIN  (MOSI)      GPIO23       VSPI default MOSI
//!     CS               GPIO5        VSPI default CS
//!     DC               GPIO17       data/command
//!     RST              GPIO16       hardware reset
//!     BUSY             GPIO4        busy / ready (input)
//!     VCC / GND        3V3 / GND
//!
//! These are the *assumed* pins — change the constants below to match your
//! wiring. (On an unmodified CYD several of these collide with the on-board
//! peripherals; this build is intended for a rewired/bare board.)
//!
//! ── Panel ────────────────────────────────────────────────────────────────────
//! Defaults to the Waveshare 2.9" B/W (296×128, `epd2in9`). For the 2.13" swap
//! the `epd2in9` / `Display2in9` / `Epd2in9` imports for `epd2in13_v2` etc.
//!
//! Status: CODE-COMPLETE-BUT-UNTESTED — no e-paper hardware is available here.
//! See the module-level build notes / task report for how to flash + verify.

use crate::{DisplayState, SessionStatus};

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_6X10, FONT_9X15_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{Line, PrimitiveStyle},
    text::{Alignment, Text},
};

use esp_idf_hal::{
    delay::{Delay, FreeRtos},
    gpio::PinDriver,
    peripherals::Peripherals,
    prelude::*,
    spi::{config::Config as SpiConfig, SpiDeviceDriver, SpiDriver},
};

use epd_waveshare::{
    color::Color,
    epd2in9::{Display2in9, Epd2in9},
    prelude::*,
};

use log::info;
use std::time::Instant;

// ── Assumed pinout (see module docs) ─────────────────────────────────────────
// Centralised so a rewirer changes them in one place.
//   SPI bus: SPI3 (VSPI)  SCLK=18  MOSI=23  CS=5
//   control: DC=17  RST=16  BUSY=4

/// Minimum wall-clock gap between full panel refreshes. e-paper full refresh is
/// slow (~2-4 s) and frequent refreshes shorten panel life, so even when the
/// state changes we never refresh more often than this.
const MIN_REFRESH: std::time::Duration = std::time::Duration::from_secs(45);

/// Idle granularity of the loop. We only *poll* the shared state this often;
/// the heavy refresh is still gated by `MIN_REFRESH` + an actual data change.
const POLL_MS: u32 = 1000;

// ── Render ───────────────────────────────────────────────────────────────────

/// Draw the glanceable monochrome status into a 1-bit `DrawTarget` (the
/// e-paper frame buffer). Black-on-white: title, the session list (project +
/// status letter W/!/z) and a compact Claude/Codex usage line.
///
/// Generic over the draw target so it can be unit-rendered against any
/// `DrawTarget<Color = Color>` (the panel buffer is `Display2in9`).
///
/// Layout is tuned for the 2.9" panel (296×128 landscape). It degrades fine on
/// smaller panels (extra rows are simply clipped by the buffer).
pub fn render_status<D>(display: &mut D, ds: &DisplayState)
where
    D: DrawTarget<Color = Color>,
{
    // White background (clear). Ignoring the clear error keeps the signature
    // panic-free; the buffer clear can't really fail for an in-RAM target.
    let _ = display.clear(Color::White);

    let black = Color::Black;
    let title = MonoTextStyle::new(&FONT_9X15_BOLD, black);
    let body = MonoTextStyle::new(&FONT_6X10, black);

    // Title + a thin rule under it.
    let _ = Text::with_alignment("VibeMonitor", Point::new(4, 13), title, Alignment::Left)
        .draw(display);
    let _ = Line::new(Point::new(0, 18), Point::new(295, 18))
        .into_styled(PrimitiveStyle::with_stroke(black, 1))
        .draw(display);

    if ds.offline {
        let _ = Text::with_alignment("hub offline", Point::new(4, 40), title, Alignment::Left)
            .draw(display);
    } else if ds.sessions.is_empty() {
        let _ = Text::with_alignment("no sessions", Point::new(4, 40), body, Alignment::Left)
            .draw(display);
    } else {
        // Session rows: "<status> project". One glyph status keeps it terse:
        //   W = working, ! = waiting (needs you), z = idle.
        let mut y = 32i32;
        for row in ds.sessions.iter().take(7) {
            let sym = match row.status {
                SessionStatus::Working => "W",
                SessionStatus::Waiting => "!",
                SessionStatus::Idle => "z",
            };
            let line = format!("{}  {}", sym, row.project.as_str());
            let _ = Text::with_alignment(&line, Point::new(4, y), body, Alignment::Left)
                .draw(display);
            y += 12;
        }
    }

    // Compact usage line at the bottom: Claude / Codex percentages.
    let usage = format!("{}   {}", pct(&ds.claude, "C"), pct(&ds.codex, "X"));
    let _ = Text::with_alignment(&usage, Point::new(4, 124), body, Alignment::Left).draw(display);
}

/// "C 42%" when the provider reported, else "C --". Single-letter prefix keeps
/// the bottom line short enough for narrow panels (C = Claude, X = Codex).
fn pct(u: &crate::Usage, name: &str) -> String {
    if u.ok {
        format!("{} {:.0}%", name, u.pct * 100.0)
    } else {
        format!("{} --", name)
    }
}

// ── Run loop ─────────────────────────────────────────────────────────────────

/// Initialise the e-paper panel and run the slow, low-power refresh loop.
///
/// `shared` is the SAME `(DisplayState, Instant)` the USB stdin reader feeds, so
/// the transport half is unchanged — only the display half differs from the
/// colour-LCD build. We re-render + full-refresh ONLY when the state actually
/// changed AND at least `MIN_REFRESH` has elapsed since the last refresh; the
/// panel is put to sleep between refreshes to honour the low-power goal.
///
/// Never returns under normal operation (the device loops forever).
pub fn run(
    peripherals: Peripherals,
    shared: std::sync::Arc<std::sync::Mutex<(DisplayState, Instant)>>,
) -> anyhow::Result<()> {
    info!("e-ink mode (epd-waveshare 2.9\" B/W)");

    // Dedicated SPI3 (VSPI) bus for the panel — see module-level pinout docs.
    let spi_bus = SpiDriver::new(
        peripherals.spi3,
        peripherals.pins.gpio18, // SCLK
        peripherals.pins.gpio23, // MOSI (DIN)
        None::<esp_idf_hal::gpio::AnyInputPin>, // e-paper is write-only
        &esp_idf_hal::spi::config::DriverConfig::new(),
    )?;
    // e-paper tops out around 4 MHz reliably; stay conservative.
    let mut spi = SpiDeviceDriver::new(
        spi_bus,
        Some(peripherals.pins.gpio5), // CS
        &SpiConfig::new().baudrate(4.MHz().into()),
    )?;

    // Control pins.
    let dc = PinDriver::output(peripherals.pins.gpio17)?;
    let rst = PinDriver::output(peripherals.pins.gpio16)?;
    let busy = PinDriver::input(peripherals.pins.gpio4)?;
    let mut delay = Delay::new_default();

    // Bring up the panel. epd-waveshare 0.6 takes embedded-hal 1.0 traits, so
    // the esp-idf-hal SpiDeviceDriver / PinDriver / Delay satisfy the bounds.
    let mut epd = Epd2in9::new(&mut spi, busy, dc, rst, &mut delay, None)
        .map_err(|e| anyhow::anyhow!("e-paper init failed: {e:?}"))?;

    let mut frame = Display2in9::default();

    // Force an initial render so the panel isn't blank at boot.
    let mut prev: Option<DisplayState> = None;
    let mut last_refresh = Instant::now()
        .checked_sub(MIN_REFRESH)
        .unwrap_or_else(Instant::now);

    loop {
        // Snapshot shared state; mark offline if the transport went quiet.
        let (mut state, last_rx) = {
            let g = shared.lock().unwrap();
            (g.0.clone(), g.1)
        };
        state.offline = last_rx.elapsed().as_secs() > 6;

        let changed = prev.as_ref().map(|p| *p != state).unwrap_or(true);

        if changed && last_refresh.elapsed() >= MIN_REFRESH {
            render_status(&mut frame, &state);

            // Full refresh (slow, ~2-4 s). Wake the panel, push, then sleep it
            // again to minimise idle current between updates.
            if let Err(e) = epd.wake_up(&mut spi, &mut delay) {
                log::warn!("e-paper wake failed: {e:?}");
            }
            if let Err(e) = epd.update_frame(&mut spi, frame.buffer(), &mut delay) {
                log::warn!("e-paper update_frame failed: {e:?}");
            } else if let Err(e) = epd.display_frame(&mut spi, &mut delay) {
                log::warn!("e-paper display_frame failed: {e:?}");
            }
            let _ = epd.sleep(&mut spi, &mut delay);

            prev = Some(state);
            last_refresh = Instant::now();
        }

        // Idle between polls. (A deeper esp_light_sleep could be wired here for
        // even lower power; FreeRtos delay already lets the CPU idle and keeps
        // the USB stdin reader thread serviced.)
        FreeRtos::delay_ms(POLL_MS);
    }
}
