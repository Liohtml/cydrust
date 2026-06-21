<div align="center">

  <img src="docs/assets/banner.png" alt="CYDRUST Banner" width="100%">

  <h1>CYDRUST</h1>

  <p><strong>Cyberpunk Development Monitor — Real-time AI coding session tracker for Claude Code on ESP32</strong></p>

  <!-- Build & quality -->
  [![CI Bridge](https://img.shields.io/github/actions/workflow/status/Liohtml/cydrust/bridge.yml?branch=main&label=CI%20Bridge&logo=github&style=flat-square)](https://github.com/Liohtml/cydrust/actions)
  [![Firmware Build](https://img.shields.io/github/actions/workflow/status/Liohtml/cydrust/firmware.yml?branch=main&label=Firmware%20Build&logo=espressif&logoColor=white&style=flat-square)](https://github.com/Liohtml/cydrust/actions)
  [![Rust](https://img.shields.io/badge/Rust-stable%20%2B%20esp-orange?logo=rust&logoColor=white&style=flat-square)](https://www.rust-lang.org/)
  [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg?style=flat-square)](LICENSE)

  <!-- Info -->
  [![ESP-IDF](https://img.shields.io/badge/ESP--IDF-v5.3.2-blue?logo=espressif&logoColor=white&style=flat-square)](https://docs.espressif.com/projects/esp-idf/)
  [![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux-lightgrey?logo=linux&style=flat-square)]()
  [![Display](https://img.shields.io/badge/Display-320%C3%97240%20ST7789-green?style=flat-square)]()

</div>

---

## Demo / Preview

<div align="center">
  <img src="docs/assets/demo.gif" alt="CYDRUST live demo — ESP32 LCD showing Claude Code sessions" width="640">
</div>

> **What you see:** The ESP32's 320×240 LCD updates every 2 seconds. The top row shows the active tab (`SESSIONS`), while the header line displays Claude and Codex token-usage percentages in their signature brand colours. Each card in the session list names the project and carries a status glyph: `>>` in green for an actively typing session, `!` in amber when Claude is waiting for user input, and `z` in grey for idle. The red `hub offline` banner snaps in if the bridge stops responding for more than 6 seconds — and disappears the moment it comes back.

---

## Features

- **Real-time session cards** — up to 8 concurrent Claude Code sessions rendered on a 320×240 ST7789 LCD at up to 5 fps
- **Three status states** with distinct colours: Working (green `#4ADE80`), Waiting (amber `#F5A623`), Idle (grey)
- **Dual transport** — connect over WiFi for a truly wireless monitor, or use USB serial for zero-config corporate networks
- **Offline detection** — a red banner fires after 3 missed polls (WiFi) or 6 seconds of silence (USB), then self-heals
- **Token-usage header** — Claude (`#D97757` orange) and Codex (`#A78BFA` purple) usage percentages shown at a glance
- **Hook integration** — Claude Code `Notification`/`Stop` hook events flip a session to *Waiting*; `POST /ack` clears it
- **Session sorting** — Waiting → Working → Idle, so what needs attention is always on top
- **4-hour TTL pruning** — stale sessions aged out automatically; no manual cleanup needed
- **Cyberpunk colour palette** — dark `#141414` background, neon accents; readable in a dim hackspace at 1 metre
- **Zero heap allocation on device** — firmware uses `heapless::Vec` and `heapless::String`; no `alloc` OOM surprises
- **Compact serial protocol** — `serial_bridge` compresses the full `~700 byte` JSON payload to `~80 bytes` before writing to the UART FIFO
- **Auth token** — every bridge request requires `X-VibeMonitor-Token`; the device never exposes unauthenticated data

---

## Hardware Requirements

| Component | Specification | Notes |
|-----------|--------------|-------|
| **ESP32 board** | ESP32 DevKit v1 (ESP-WROOM-32) | Any board with SPI2, GPIO 2/13/14/15/21 broken out |
| **LCD display** | ST7789, 320×240, IPS | 2.8" or 2.4" modules work; ensure 3.3 V logic |
| **USB cable** | USB-A to Micro-B | CH340 driver required on Windows for COM port |
| **WiFi network** | 2.4 GHz 802.11 b/g/n | Only needed for WiFi transport mode |
| **Power** | 5 V via USB or LiPo 3.7 V | 80–120 mA draw with backlight on |
| **Host machine** | Windows 10/11 or Linux | Runs the `vibe-bridge` Axum server |

> **Recommended module:** The "Cheap Yellow Display" (CYD) — an all-in-one ESP32 + 320×240 ST7789 + USB-CH340 board — works out of the box with the default pin assignments below.

---

## Wiring Diagram

```
  ESP32 DevKit              ST7789 Module
  ─────────────            ────────────────
  3V3  ──────────────────► VCC   (3.3 V power)
  GND  ──────────────────► GND
  IO14 ──────────────────► SCL   (SPI clock)
  IO13 ──────────────────► SDA   (SPI MOSI)
  IO15 ──────────────────► CS    (chip select)
  IO2  ──────────────────── DC    (data/command)
  IO4  ──────────────────► RES   (reset — tie high if unused)
  IO21 ──────────────────► BLK   (backlight — driven HIGH in firmware)

  SPI bus: SPI2 (HSPI)  |  Baud: 55 MHz
```

> **CYD users:** The Cheap Yellow Display hard-wires these pins on the PCB — no jumpers needed. The backlight is on `GPIO21`. Reset is connected internally.

---

## Architecture

```
  ┌────────────────────────────────────────────────────────────────────┐
  │  Host Machine (Windows / Linux)                                    │
  │                                                                    │
  │   Claude Code         collector (every 2 s)                        │
  │   sessions ──────────► scans ~/.claude/projects/**/*.jsonl         │
  │   (.jsonl files)       extracts session id, project, mtime         │
  │                              │                                     │
  │                              ▼                                     │
  │                       state::Store  (RwLock<HashMap>)              │
  │                              │                                     │
  │                    ┌─────────┴──────────┐                          │
  │                    │   Axum HTTP server │  :5151                   │
  │                    │  GET  /state       │◄──── X-VibeMonitor-Token │
  │                    │  POST /ack         │                          │
  │                    │  POST /hook        │◄──── Claude Code hooks   │
  │                    └─────────┬──────────┘                          │
  │                              │                                     │
  │          ┌───────────────────┴───────────────────┐                 │
  │          │ WiFi path                             │ USB path        │
  │          │                                       │                 │
  │          │  (firmware polls                      │  serial_bridge  │
  │          │   /state directly)                    │  binary polls   │
  │          │                                       │  /state, writes │
  │          │                                       │  compact JSON   │
  │          │                                       │  to COM port    │
  └──────────┼───────────────────────────────────────┼─────────────────┘
             │                                       │
             │ HTTP (2 s)                            │ UART 115200 baud
             │                                       │ (~80 byte lines)
             ▼                                       ▼
     ┌──────────────────────────────────────────────────┐
     │              ESP32  (xtensa-esp32-espidf)         │
     │                                                   │
     │  parse_state()  →  DisplayState                   │
     │       │                                           │
     │       ▼                                           │
     │  render()  →  embedded-graphics  →  ST7789 LCD    │
     │                   320 × 240 px                    │
     └──────────────────────────────────────────────────┘
```

### Component summary

| Component | Crate / binary | Role |
|-----------|---------------|------|
| `bridge/src/collector.rs` | `walkdir`, `dirs-next` | Scans `~/.claude/projects` every 2 s |
| `bridge/src/state.rs` | stdlib `RwLock` | In-memory session store |
| `bridge/src/hub.rs` | `axum 0.8` | REST endpoints `/state`, `/ack`, `/hook` |
| `bridge/src/bin/serial_bridge.rs` | `serialport`, `ureq` | USB transport proxy, compacts JSON |
| `firmware/src/main.rs` | `esp-idf-hal`, `mipidsi`, `embedded-graphics` | Display driver + JSON parser |

---

## Quick Start

### Prerequisites

| Tool | Version | Install |
|------|---------|---------|
| Rust toolchain | stable | `rustup toolchain install stable` |
| Espressif Rust toolchain | `esp` channel | `rustup toolchain install esp` (via `espup`) |
| `espup` | latest | `cargo install espup && espup install` |
| `ldproxy` | latest | `cargo install ldproxy` |
| `espflash` | latest | `cargo install espflash` |
| Python 3 + pip | 3.10+ | Required by `embuild` for ESP-IDF download |

### 1 — Clone

```bash
git clone https://github.com/Liohtml/cydrust.git
cd cydrust
```

### 2 — Configure the bridge

Edit `bridge/config.toml` (already present, just change the token):

```toml
token = "your-secret-token-here"   # shared with the firmware
host  = "0.0.0.0"                  # bind address (127.0.0.1 for localhost-only)
port  = 5151
```

> Keep the token out of version control — add `bridge/config.toml` to `.gitignore` if you commit secrets.

### 3 — Build and run the bridge

```bash
cd bridge
cargo build --release
cargo run --release -- config.toml
# => INFO vibe-bridge listening on http://0.0.0.0:5151
```

The bridge starts scanning `~/.claude/projects` immediately.

### 4 — Wire up Claude Code hooks (optional but recommended)

Add this to your Claude Code `settings.json` so *Waiting* state fires in real time:

```json
{
  "hooks": {
    "Notification": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST http://localhost:5151/hook -H 'Content-Type: application/json' -H 'X-VibeMonitor-Token: your-secret-token-here' -d '{\"sessionId\":\"$SESSION_ID\",\"hook_event_name\":\"Notification\"}'"
          }
        ]
      }
    ],
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST http://localhost:5151/hook -H 'Content-Type: application/json' -H 'X-VibeMonitor-Token: your-secret-token-here' -d '{\"sessionId\":\"$SESSION_ID\",\"hook_event_name\":\"Stop\"}'"
          }
        ]
      }
    ]
  }
}
```

### 5a — Build and flash (WiFi transport)

Set your WiFi credentials and bridge address as environment variables before building:

```bash
cd firmware

# Linux / macOS
export VIBE_SSID="MyWiFi"
export VIBE_PASS="hunter2"
export VIBE_HOST="192.168.1.42"   # IP of the machine running vibe-bridge
export VIBE_PORT="5151"
export VIBE_TOKEN="your-secret-token-here"

# Windows (PowerShell)
$env:VIBE_SSID  = "MyWiFi"
$env:VIBE_PASS  = "hunter2"
$env:VIBE_HOST  = "192.168.1.42"
$env:VIBE_PORT  = "5151"
$env:VIBE_TOKEN = "your-secret-token-here"

cargo build --release --features wifi
espflash flash --monitor target/xtensa-esp32-espidf/release/vibe-firmware
```

### 5b — Build and flash (USB / serial transport)

The default feature set builds for USB — no env vars needed:

```bash
cd firmware
cargo build --release          # no --features flag → USB mode
espflash flash --monitor target/xtensa-esp32-espidf/release/vibe-firmware
```

Then start the serial bridge on the host (in a separate terminal):

```bash
cd bridge
# Linux
cargo run --bin serial_bridge -- --port /dev/ttyUSB0

# Windows
cargo run --bin serial_bridge -- --port COM7

# Override bridge URL or token if needed
cargo run --bin serial_bridge -- --port COM7 --url http://localhost:5151 --token your-secret-token-here
```

> The serial bridge polls `/state` every 2 s, strips the payload to `~80 bytes`, and writes it to the ESP32's UART. ACK lines from the device are forwarded back to `POST /ack` automatically.

---

## Configuration

### Bridge — `bridge/config.toml`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `token` | `String` | *(required)* | Shared secret. Sent as `X-VibeMonitor-Token` header |
| `host` | `String` | `"0.0.0.0"` | TCP bind address for the Axum server |
| `port` | `u16` | `5151` | TCP port |

### Firmware — environment variables (WiFi mode only)

| Variable | Example | Description |
|----------|---------|-------------|
| `VIBE_SSID` | `"HomeNet"` | 2.4 GHz WiFi SSID |
| `VIBE_PASS` | `"s3cret"` | WiFi WPA2 passphrase |
| `VIBE_HOST` | `"192.168.1.10"` | IP of the host running `vibe-bridge` |
| `VIBE_PORT` | `"5151"` | Port of `vibe-bridge` |
| `VIBE_TOKEN` | `"HGWc..."` | Must match bridge `config.toml` `token` |

These are baked into the binary at compile time via `env!()` macros; no runtime config file on the device.

### Firmware — build features

| Feature flag | Default | Effect |
|-------------|---------|--------|
| *(none)* | yes | USB serial transport via UART0 |
| `wifi` | no | WiFi transport; requires `VIBE_*` env vars |

```bash
# USB (default)
cargo build --release

# WiFi
cargo build --release --features wifi
```

### Tunable constants (source-level)

| Constant | Location | Default | Description |
|----------|----------|---------|-------------|
| `WORKING_SEC` | `bridge/src/hub.rs:60` | `60.0` s | Age threshold below which a session is *Working* |
| `GONE_TTL` | `bridge/src/hub.rs:61` | `14400.0` s | Sessions older than 4 hours are pruned from `/state` |
| `POLL_MS` | `firmware/src/main.rs:137` | `2000` ms | WiFi poll interval |
| `POLL_SECS` | `bridge/src/bin/serial_bridge.rs:25` | `2` s | Serial bridge push interval |
| `BAUD` | `bridge/src/bin/serial_bridge.rs:26` | `115200` | UART baud rate |

---

## API Reference

All endpoints require the `X-VibeMonitor-Token` header matching `config.toml:token`. Requests without it return `401 Unauthorized`.

### `GET /state`

Returns current session list and usage snapshot.

**Request**

```http
GET /state HTTP/1.1
X-VibeMonitor-Token: HGWcjjofIUFUTLxo
```

**Response** `200 OK`

```jsonc
{
  "ts": 1750000000,          // Unix timestamp (seconds)
  "staleSec": 1,             // seconds since last collector scan (-1 = never scanned)
  "sessions": [
    {
      "id": "abc123",        // session UUID (JSONL file stem)
      "tool": "claude",      // always "claude" in current collector
      "project": "cydrust",  // derived from parent directory name
      "status": "working",   // "working" | "waiting" | "idle"
      "ageSec": 12,          // seconds since last activity
      "waiting": false,
      "waitingSec": null     // seconds in waiting state (null if not waiting)
    },
    {
      "id": "def456",
      "tool": "claude",
      "project": "myapp",
      "status": "waiting",
      "ageSec": 95,
      "waiting": true,
      "waitingSec": 42
    }
  ],
  "usage": {
    "claude": { "ok": false, "pct": null, "resetSec": null },
    "codex":  { "ok": false, "pct": null, "resetSec": null }
  }
}
```

**Session status logic**

```
waiting == true          → "waiting"
age_sec < 60             → "working"
60 ≤ age_sec < 14400     → "idle"
age_sec ≥ 14400          → pruned (not returned)
```

Sessions are sorted: `waiting` first, then `working`, then `idle`.

---

### `POST /ack`

Clears the *waiting* flag on a session (e.g., after the user has responded to Claude's prompt).

**Request**

```http
POST /ack HTTP/1.1
Content-Type: application/json
X-VibeMonitor-Token: HGWcjjofIUFUTLxo

{"id": "abc123"}
```

**Response** `200 OK` (empty body)

---

### `POST /hook`

Receives Claude Code hook events. Setting a session to *waiting* can be triggered via `Notification` or `Stop` events.

**Request**

```http
POST /hook HTTP/1.1
Content-Type: application/json
X-VibeMonitor-Token: HGWcjjofIUFUTLxo

{
  "sessionId": "abc123",
  "hook_event_name": "Notification"
}
```

*Alternatively* (legacy field names are accepted):

```jsonc
{
  "id": "abc123",
  "event": "Stop"
}
```

**Response** `200 OK` (empty body)

**Events that trigger *waiting*:** `"Notification"`, `"Stop"`
All other event names are silently accepted and ignored.

---

## Project Structure

```
cydrust/
├── bridge/                         # Host-side Rust/Axum server
│   ├── Cargo.toml                  # vibe-bridge crate (axum, tokio, walkdir…)
│   ├── config.toml                 # Runtime config: token, host, port
│   └── src/
│       ├── main.rs                 # Entry point — loads config, spawns collector, starts HTTP
│       ├── collector.rs            # Walks ~/.claude/projects/**/*.jsonl every 2 s
│       ├── state.rs                # RwLock<HashMap> session store (upsert / ack / snapshot)
│       ├── hub.rs                  # Axum router: GET /state, POST /ack, POST /hook
│       ├── model.rs                # Shared types: Session, SessionRow, StateResponse…
│       └── bin/
│           └── serial_bridge.rs   # USB transport binary: polls /state → COM port
│
├── firmware/                       # ESP32 embedded Rust
│   ├── Cargo.toml                  # vibe-firmware crate; feature = "wifi" for WiFi mode
│   ├── build.rs                    # embuild sysenv output (ESP-IDF integration)
│   ├── rust-toolchain.toml         # channel = "esp"
│   └── .cargo/
│       └── config.toml             # target = xtensa-esp32-espidf, ESP_IDF_VERSION = v5.3.2
│   └── src/
│       └── main.rs                 # Everything: SPI init, render(), parse_state(), WiFi/USB loops
│
└── docs/
    └── assets/
        ├── banner.png              # Header banner (place your photo/render here)
        └── demo.gif                # Screen recording of the live display
```

---

## Development

### Running the bridge locally (no hardware)

```bash
cd bridge
cargo run -- config.toml
```

Verify with curl:

```bash
curl -s http://localhost:5151/state \
  -H "X-VibeMonitor-Token: HGWcjjofIUFUTLxo" | jq .
```

Trigger a waiting state manually:

```bash
curl -s -X POST http://localhost:5151/hook \
  -H "Content-Type: application/json" \
  -H "X-VibeMonitor-Token: HGWcjjofIUFUTLxo" \
  -d '{"sessionId":"test-id","hook_event_name":"Notification"}'
```

Clear it:

```bash
curl -s -X POST http://localhost:5151/ack \
  -H "Content-Type: application/json" \
  -H "X-VibeMonitor-Token: HGWcjjofIUFUTLxo" \
  -d '{"id":"test-id"}'
```

### Running bridge tests

```bash
cd bridge
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

### Firmware — iterating without hardware

The render logic is pure `embedded-graphics` code and can be unit-tested against a mock display target:

```bash
cd firmware
cargo check                      # type-check without flashing
cargo check --features wifi      # also checks WiFi code paths
```

### Firmware — monitoring serial output

```bash
espflash monitor --port /dev/ttyUSB0    # Linux
espflash monitor --port COM7            # Windows
```

### Changing the colour palette

All colours are defined as `Rgb565` constants at the top of `firmware/src/main.rs`:

```rust
const C_BG:      Rgb565 = Rgb565::new(2,  5,  2);   // #141414 dark background
const C_CLAUDE:  Rgb565 = Rgb565::new(26, 29, 11);   // #D97757 Claude orange
const C_CODEX:   Rgb565 = Rgb565::new(20, 34, 30);   // #A78BFA Codex purple
const C_WORK:    Rgb565 = Rgb565::new(9,  55, 16);   // #4ADE80 working green
const C_WAIT:    Rgb565 = Rgb565::new(30, 41,  4);   // #F5A623 waiting amber
const C_OFFLINE: Rgb565 = Rgb565::new(28, 18,  9);   // #E5484D offline red
```

`Rgb565::new(r, g, b)` takes 5-bit R, 6-bit G, 5-bit B values. Use an online RGB565 converter to map hex colours.

### Adding support for other AI tools

The `tool` field in `Session` is currently always `"claude"` (set by `collector.rs`). To track Codex/Cursor/etc.:

1. Add a second collector in `bridge/src/collector.rs` that scans the relevant session directory
2. Set `tool` to `"codex"` (or another string) on the `Session` struct
3. The firmware already handles `tool == "codex"` — it renders a purple dot instead of orange

---

## Troubleshooting

### Display shows nothing / white screen

| Check | Resolution |
|-------|-----------|
| Backlight | `GPIO21` must be driven HIGH — verify it is not floating |
| SPI pins | Double-check SCL=14, SDA=13, CS=15, DC=2 against your module's pinout |
| Power | Some 3.2" ST7789 modules need 3.3 V; never connect VCC to 5 V |
| `display init failed` in serial monitor | Try swapping `ColorInversion::Inverted` ↔ `Normal` and `ColorOrder::Bgr` ↔ `Rgb` in `firmware/src/main.rs` |
| Build for wrong target | Confirm `firmware/.cargo/config.toml` has `target = "xtensa-esp32-espidf"` |

### WiFi not connecting

```
[ERROR] WifiError: EspError(...)
```

| Check | Resolution |
|-------|-----------|
| Env vars baked in | Rebuild after exporting `VIBE_SSID` / `VIBE_PASS` — values are compile-time constants |
| 5 GHz network | ESP32 only supports 2.4 GHz; check your router band |
| SSID length | Max 32 chars; use `try_into()` error logs to detect truncation |
| Bridge unreachable | Ping `VIBE_HOST` from another device on the same subnet; check firewall on port `5151` |
| `401 Unauthorized` | `VIBE_TOKEN` must exactly match `config.toml:token` |

### Serial bridge / USB timeout

```
[serial_bridge] write error: ...
```

| Check | Resolution |
|-------|-----------|
| COM port | Run `mode` (Windows) or `ls /dev/ttyUSB*` (Linux) to confirm the port name |
| CH340 driver | Download from [wch-ic.com](https://www.wch-ic.com/downloads/CH341SER_EXE.html) for Windows |
| DTR/RTS reset loop | The serial bridge explicitly lowers DTR and RTS to prevent the CH340 from resetting the ESP32 on connect |
| Firmware in USB mode | Ensure you built **without** `--features wifi`; WiFi builds do not read from UART0 |
| Baud rate mismatch | Both sides are hardcoded to `115200`; do not change one without the other |

### Sessions not appearing

| Check | Resolution |
|-------|-----------|
| Claude projects dir | Bridge scans `~/.claude/projects/`; run `ls ~/.claude/projects` to confirm JSONL files exist |
| Active Claude Code session | Open a project in Claude Code; JSONL files are created on first tool use |
| Bridge not running | `curl http://localhost:5151/state -H "X-VibeMonitor-Token: <tok>"` should return JSON |
| Age threshold | A session appears as *working* only if its JSONL mtime is < 60 s ago |

### `hub offline` banner stuck

The banner appears when:
- **WiFi mode:** 3 or more consecutive failed HTTP requests to `/state`
- **USB mode:** no newline received on UART within the last 6 seconds

Check: is `vibe-bridge` running? Is `serial_bridge` running (USB mode)? Check firewall rules.

---

## Roadmap

- [x] Core bridge: collector + REST API + token auth
- [x] ST7789 display driver with `embedded-graphics`
- [x] WiFi transport (polling)
- [x] USB / serial transport + `serial_bridge` binary
- [x] Hook integration (`Notification` / `Stop` → waiting state)
- [x] Offline detection and recovery banner
- [ ] USAGE tab — render token consumption bar chart on the second tab
- [ ] SETTINGS tab — interactive WiFi/token config via display + button input
- [ ] Claude Code extension — inject hook config automatically via `claude settings`
- [ ] OTA firmware updates over WiFi (ESP-IDF `esp_https_ota`)
- [ ] Multi-host federation — aggregate sessions from several developer machines
- [ ] BLE transport — send display data over Bluetooth LE without WiFi or USB
- [ ] `e-ink` variant — low-power SPI e-paper display for status on a shelf
- [ ] Prometheus metrics endpoint on the bridge
- [ ] Pre-built firmware releases (`.bin`) for common ESP32 boards

---

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.

**Quick checklist:**

- Run `cargo fmt` and `cargo clippy -- -D warnings` in both `bridge/` and `firmware/`
- Keep `firmware/src/main.rs` `no_std`-friendly: no heap-allocated `String` or `Vec` — use `heapless` equivalents
- For large features, open an issue first to discuss the design
- The colour palette and pin assignments are intentional — ask before changing defaults

---

## License

This project is licensed under the **MIT License** — see the [LICENSE](LICENSE) file for details.

---

<div align="center">
  <sub>Built with Rust, embedded-graphics, Axum, and too much coffee. Inspired by the neon glow of a terminal at 2 AM.</sub>
</div>
