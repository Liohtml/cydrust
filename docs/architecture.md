# System Architecture

## Overview

CYDRUST is a real-time AI session monitor that brings Claude Code activity onto a physical
ESP32 display. The host-side **bridge** continuously watches Claude Code's project JSONL
files, aggregates session state in memory, and exposes it over a small HTTP API. A
companion binary — `serial_bridge` — relays that state over USB serial to the ESP32.
The ESP32 firmware renders the sessions, tool usage gauges, and status indicators on a
320×240 ST7789 LCD in a tabbed UI.

The system is designed to run entirely on a local network (or USB-only) with no cloud
dependency. Authentication is handled by a single shared secret token sent in the
`X-VibeMonitor-Token` header. Session data is never persisted to disk; the bridge holds
everything in a `RwLock<HashMap>` in process memory and discards sessions older than
four hours.

The firmware supports two transport modes compiled as Cargo features. The default
`usb` mode reads newline-delimited JSON from stdin via the host-side `serial_bridge`
binary. The optional `wifi` feature connects the ESP32 directly to the bridge HTTP API
over the local network, removing the need for the serial bridge entirely.

---

## Component Diagram

```
┌──────────────────────────────────────────────────────────────────────┐
│                          Host Machine                                │
│                                                                      │
│  Claude Code                                                         │
│  ──────────                                                          │
│  ~/.claude/projects/                                                 │
│    └── {project-dir}/                                                │
│          └── {session-id}.jsonl  ──►  collector::scan_claude()      │
│                                         (every 2 s, mtime probe)    │
│                                                │                     │
│                                                ▼                     │
│                                         state::Store                │
│                                    RwLock<HashMap<id, Session>>     │
│                                                │                     │
│                              ┌─────────────────┼──────────────┐     │
│                              │                 │              │     │
│                              ▼                 ▼              ▼     │
│                        GET /state         POST /ack      POST /hook │
│                        hub.rs             hub.rs         hub.rs     │
│                              │                                       │
│                    ┌─────────┴──────────┐                           │
│                    │                    │                            │
│             serial_bridge           (WiFi mode only)                │
│             (USB transport)         firmware polls /state directly  │
│                    │                                                 │
└────────────────────┼─────────────────────────────────────────────── ┘
                     │ USB Serial  115 200 baud
                     │ newline-JSON  (~80 bytes per frame)
                     ▼
             ┌───────────────┐
             │   ESP32       │
             │  Firmware     │
             │               │
             │  ST7789 LCD   │
             │  320 × 240    │
             │  XPT2046 touch│
             └───────────────┘
```

---

## Component Summary

| Component | Crate / binary | Role |
|-----------|---------------|------|
| `bridge/src/collector.rs` | `walkdir`, `dirs-next` | Scans `~/.claude/projects` every 2 s |
| `bridge/src/collector_codex.rs` | `serde_json` | Reads Codex session DB; extracts usage + sessions |
| `bridge/src/collector_opencode.rs` | `rusqlite` | Reads OpenCode SQLite DB (bundled driver) |
| `bridge/src/collector_hermes.rs` | `rusqlite` | Reads Hermes SQLite DB |
| `bridge/src/state.rs` | stdlib `RwLock` | In-memory session store (upsert / ack / snapshot / reap) |
| `bridge/src/hub.rs` | `axum 0.8` | REST endpoints `/state`, `/ack`, `/hook`, `/metrics`, `/federation/ingest` |
| `bridge/src/federation.rs` | `ureq` | `RemoteStore` TTL cache; push (node→aggregator) and ingest (aggregator endpoint) |
| `bridge/src/metrics.rs` | stdlib | Prometheus text exposition for sessions, usage, and per-model cost |
| `bridge/src/bin/install_hooks.rs` | `serde_json` | Idempotent Claude Code hook installer (merges into `settings.json`) |
| `bridge/src/bin/vibe_hook.rs` | `ureq` | Lightweight per-event hook process spawned by Claude Code; always exits 0 |
| `bridge/src/bin/serial_bridge.rs` | `serialport`, `ureq` | USB transport proxy, emits compact short-key JSON |
| `firmware/src/main.rs` | `esp-idf-hal`, `mipidsi`, `embedded-graphics` | Display driver, JSON parser, settings (NVS), render loop |
| `firmware/src/icons.rs` | `embedded-graphics` | All four provider 18×18 logos with alpha compositing |
| `firmware/src/ota.rs` | `esp-idf-svc` | OTA update via `esp_https_ota`; aborts to old slot on failure |
| `firmware/src/eink.rs` | `epd-waveshare` | E-paper display driver for Waveshare 2.9" B/W (SSD1680/IL3820) |
| `firmware/src/ble.rs` | `esp32-nimble` | BLE GATT server — code-complete, toolchain-blocked |

---

**Key structural note:** `firmware/` is intentionally excluded from the Cargo workspace
(`Cargo.toml` workspace `members = ["bridge"]`). The firmware targets
`xtensa-esp32-espidf` via the `esp` toolchain channel and must be built independently
with `cargo +esp build` inside the `firmware/` directory.

---

## Data Flow

### USB Transport (default)

1. **Collector loop** — `collector::scan_claude()` runs every 2 seconds inside a
   `tokio::spawn` background task. It walks `~/.claude/projects/**/*.jsonl` (max depth 2)
   using `walkdir` and reads each file's filesystem `mtime` as the session's
   `last_activity` timestamp.

2. **State upsert** — For each `.jsonl` found, `Store::upsert()` inserts or updates the
   session in the `RwLock<HashMap>`. Upserts preserve the *maximum* `last_activity`
   seen so activity never appears to go backwards.

3. **Bridge API** — `axum` serves three routes. `GET /state` snapshots the store,
   filters sessions older than 14 400 s (4 h), classifies each by status, sorts
   Waiting → Working → Idle, and returns a `StateResponse` JSON object.

4. **serial_bridge polling** — The `serial_bridge` binary polls `GET /state` every 2 s,
   strips the response down to a `make_mini()` payload (~80 bytes — project names,
   statuses, and tool identity only), and writes it as a single newline-terminated JSON
   line to the configured serial port at 115 200 baud.

5. **Firmware receives** — The ESP32 firmware reads bytes from stdin (USB CDC), buffers
   until it sees a newline, then calls `parse_state()` to extract session rows and usage
   percentages using a hand-rolled zero-allocation parser (no `serde_json` on the MCU).

6. **Display render** — `render()` redraws the 320×240 framebuffer only when the
   `DisplayState` or active tab has changed (dirty-check via `PartialEq`). Cards show
   project name, provider accent colour, and a status symbol (`>>` working, `!` waiting,
   `z` idle).

7. **ACK flow** — When the firmware sends `{"ack":"<session-id>"}` back over serial,
   `serial_bridge` reads it and calls `POST /ack` on the bridge, which clears the
   session's `waiting` flag in the store.

### WiFi Transport (feature flag `wifi`)

Steps 4 and 5 are replaced: the firmware connects to WiFi at boot using credentials
baked in at compile time via `VIBE_SSID`, `VIBE_PASS`, `VIBE_HOST`, `VIBE_PORT`, and
`VIBE_TOKEN` environment variables. It polls `GET /state` directly via
`esp-idf-svc::http::client::EspHttpConnection` on the same 2 000 ms interval.
The `serial_bridge` binary is not used in this mode.

---

## Session Lifecycle

```
                     ┌──────────────────────────────┐
                     │  .jsonl file mtime detected  │
                     │  by collector scan            │
                     └──────────────┬───────────────┘
                                    │ Store::upsert()
                                    ▼
                             ┌─────────────┐
                             │   Tracked   │  (age = 0..14 400 s)
                             └──────┬──────┘
                                    │
              ┌─────────────────────┼──────────────────────┐
              │                     │                       │
              │ age < 60 s          │ age >= 60 s           │ POST /hook
              ▼                     ▼                       │ event = Notification
         ┌─────────┐          ┌──────────┐                  ▼
         │ Working │          │   Idle   │           ┌─────────────┐
         └─────────┘          └──────────┘           │   Waiting   │
                                                     └──────┬──────┘
                                                            │ POST /ack (firmware or
                                                            │ client) or Stop hook event
                                                            ▼
                                                     ┌─────────────┐
                                                     │  (cleared)  │
                                                     └─────────────┘
              │
              │ age > 14 400 s (4 hours)
              ▼
        ┌───────────┐
        │  Expired  │  filtered from GET /state response
        └───────────┘
```

**Status thresholds (defined in `hub.rs`):**

| Constant       | Value    | Meaning                                        |
|----------------|----------|------------------------------------------------|
| `WORKING_SEC`  | 60.0 s   | `.jsonl` mtime < 60 s ago → `working`          |
| `GONE_TTL`     | 14 400 s | `.jsonl` not seen for > 4 h → filtered out     |
| Waiting        | explicit | set by `POST /hook` (Notification event); cleared by a Stop event or `POST /ack` |

---

## Transport Modes

| Aspect              | USB (default, `--no-default-features`) | WiFi (`--features wifi`)               |
|---------------------|----------------------------------------|----------------------------------------|
| Firmware feature    | `#[cfg(not(feature = "wifi"))]`        | `#[cfg(feature = "wifi")]`             |
| Host binary needed  | `serial_bridge` + `vibe-bridge`        | `vibe-bridge` only                     |
| Credentials in FW   | None baked in                          | `VIBE_SSID`, `VIBE_PASS`, `VIBE_HOST`, `VIBE_PORT`, `VIBE_TOKEN` |
| Serial port         | COM* / /dev/tty* at 115 200 baud       | Not used                               |
| Data format         | Newline-JSON mini payload ~80 bytes    | Full `/state` JSON response (8 KB buf) |
| ACK flow            | Firmware → serial → serial_bridge → `/ack` | Firmware → `POST /ack` directly over HTTP |
| Offline detection   | `last_rx.elapsed() > 6 s`              | 3 consecutive fetch failures           |
| DTR/RTS reset guard | serial_bridge lowers both signals      | Not applicable                         |
| Network dependency  | None (pure USB CDC)                    | Local WiFi / same subnet              |

**Why USB is the default:** The CH340 USB–UART chip on the CYD board wires DTR→GPIO0 and
RTS→EN through capacitors, which can reset the ESP32 when a serial port is opened.
`serial_bridge` explicitly lowers both control lines immediately after opening the port
to prevent this. WiFi mode eliminates this concern entirely at the cost of requiring
network infrastructure.

---

## Concurrency Model

The bridge runs on the Tokio async runtime (`#[tokio::main]`).

```
tokio::main
  ├── tokio::spawn ──► collector loop (every 2 s)
  │                     calls Store::upsert() under write-lock
  │
  └── axum::serve ──► HTTP request handlers (Tokio task per connection)
                        state_handler  → Store::snapshot() under read-lock
                        ack_handler    → Store::ack()       under write-lock
                        hook_handler   → Store::mark_waiting() under write-lock
```

`state::Store` wraps a `RwLock<Inner>` where `Inner` holds the `HashMap<String, Session>`
and a `last_scan: f64` timestamp. Multiple readers (`GET /state`) can proceed
simultaneously; writers (`upsert`, `ack`, `mark_waiting`) take exclusive access.
Lock contention is minimal: the collector holds the write lock only while iterating
found files; HTTP handlers hold it only long enough to update a single map entry.

The `serial_bridge` binary is a separate process and uses std threads (not Tokio):
the main thread drives the polling loop, and a background thread runs the ACK reader via
`BufReader::lines()` over a cloned serial port handle.
