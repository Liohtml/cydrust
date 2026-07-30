# Development Guide

This document covers everything needed to build, run, test, and modify CYDRUST locally.

---

## Repository Layout

```
cydrust/
├── Cargo.toml                      Workspace root (members: ["bridge"])
├── bridge/                         # Host-side Rust/Axum server
│   ├── Cargo.toml                  # vibe-bridge crate (axum, tokio, walkdir, ureq, rusqlite…)
│   ├── config.toml                 # Runtime config: token, host, port, [federation]
│   ├── config.example.toml         # Template — tracked in git
│   ├── deny.toml                   # cargo-deny license + advisory policy
│   └── src/
│       ├── main.rs                 # Entry point — config, background threads, HTTP server
│       ├── collector.rs            # Walks ~/.claude/projects/**/*.jsonl every 2 s
│       ├── collector_codex.rs      # Reads Codex session DB for usage + sessions
│       ├── collector_opencode.rs   # Reads OpenCode SQLite DB via bundled rusqlite
│       ├── collector_hermes.rs     # Reads Hermes SQLite DB
│       ├── state.rs                # RwLock<HashMap> session store (upsert / ack / snapshot / reap)
│       ├── hub.rs                  # Axum router: /state, /ack, /hook, /metrics, /federation/ingest
│       ├── federation.rs           # RemoteStore TTL cache + push loop (node→aggregator)
│       ├── metrics.rs              # Prometheus text exposition (sessions + usage + cost)
│       ├── model.rs                # Shared types: Session, SessionRow, StateResponse, Metrics…
│       ├── usage.rs                # Usage polling (Anthropic API + Codex)
│       └── bin/
│           ├── serial_bridge.rs    # USB transport binary: polls /state → COM port
│           ├── install_hooks.rs    # Idempotent Claude Code hook installer (settings.json)
│           └── vibe_hook.rs        # Per-event hook process spawned by Claude Code
│
├── firmware/                       # ESP32 embedded Rust
│   ├── Cargo.toml                  # vibe-firmware crate; features: wifi, ota, eink, ble
│   ├── build.rs                    # embuild sysenv output (ESP-IDF integration)
│   ├── rust-toolchain.toml         # channel = "esp"
│   ├── .cargo/config.toml          # target = xtensa-esp32-espidf, ESP_IDF_VERSION = v5.3.2
│   ├── partitions_ota.csv          # Dual OTA partition table (2×1664 KiB, 4 MB flash)
│   ├── sdkconfig.defaults          # Base ESP-IDF sdkconfig
│   ├── sdkconfig.defaults.ota      # OTA-specific sdkconfig overrides
│   ├── sdkconfig.defaults.ble      # BLE-specific sdkconfig overrides
│   ├── package.sh / package.ps1    # Merges bootloader + table + app into flashable .bin
│   ├── RELEASES.md                 # Pre-built release instructions
│   ├── deny.toml                   # cargo-deny policy for firmware workspace
│   └── src/
│       ├── main.rs                 # SPI init, render(), parse_state(), settings (NVS), transport loops
│       ├── icons.rs                # All four provider 18×18 logos (r5,g6,b5,alpha pixel arrays)
│       ├── ota.rs                  # OTA update via esp_https_ota (wifi,ota feature)
│       ├── eink.rs                 # E-paper display driver — Waveshare 2.9" B/W (eink feature)
│       └── ble.rs                  # BLE GATT server — NimBLE, newline-JSON frames (ble feature)
│
└── docs/
    ├── api.md / architecture.md / development.md / hardware.md
    ├── troubleshooting.md          # Cross-cutting troubleshooting guide
    ├── wsl2.md                     # WSL2 + usbipd setup for Windows hosts
    └── assets/
        ├── banner.png              # Header banner
        └── banner.html             # Banner source
```

> **Workspace note:** `firmware/` is excluded from the Cargo workspace because it
> targets the Xtensa toolchain (`esp`), which is incompatible with the host Rust
> stable/nightly toolchain used by `bridge/`. Always `cd firmware` before running
> cargo commands for the firmware.

---

## Prerequisites

### Bridge (host machine)

| Tool | Install command | Version requirement |
|---|---|---|
| Rust stable | `rustup install stable` | Rust 1.75 or later |
| cargo (bundled with Rust) | — | — |

No additional system libraries are required on Windows. On a fresh Linux or WSL2
install, add the build essentials plus `libudev-dev` for the `serialport` crate:

```sh
# Debian / Ubuntu
sudo apt install -y build-essential pkg-config libudev-dev
```

Additionally, add your user to the `dialout` group so `serial_bridge` can access USB
serial ports without `sudo`:

```sh
sudo usermod -aG dialout $USER
# Log out and back in (or: wsl --shutdown; restart WSL)
```

If you skip this step, `serial_bridge` will fail with `Permission denied` when opening
`/dev/ttyUSB0`.

### Firmware (ESP32)

| Tool | Install command | Notes |
|---|---|---|
| espup | `cargo install espup && espup install` | Installs the Xtensa Rust fork + GCC toolchain |
| ldproxy | `cargo install ldproxy` | Linker proxy required by the `esp` toolchain |
| espflash | `cargo install espflash` | Flashing and serial monitor |
| ESP-IDF v5.3.2 | Installed automatically by `embuild` on first build | Requires Python 3.8+ and pip |

After `espup install`, load the environment:

```sh
# Linux / macOS (add to ~/.bashrc or ~/.zshrc for persistence)
. $HOME/export-esp.sh

# Windows PowerShell — follow the output of espup install
# It prints the exact $env:PATH update to apply
```

Verify the toolchain is available:

```sh
rustup toolchain list | grep esp
# Expected: esp (or similar)
```

---

## Bridge Development

### Initial setup

```sh
# Clone and enter the workspace
cd D:\CODEENV\CYDRUST

# Copy the example config and fill in your token
copy bridge\config.example.toml bridge\config.toml
# Edit bridge\config.toml — set token to a random value
```

Generate a token:

```sh
# Linux / macOS
openssl rand -hex 32

# Windows (PowerShell)
[System.Web.Security.Membership]::GeneratePassword(32, 8)
# or:
-join ((48..57) + (97..102) | Get-Random -Count 32 | ForEach-Object { [char]$_ })
```

### Run the bridge

```sh
cd bridge

# Development (default config.toml in current directory)
cargo run

# Specify a different config file
cargo run -- path/to/config.toml

# With debug logging
$env:RUST_LOG = "debug"
cargo run
```

The bridge binds on `http://0.0.0.0:5151` by default (configurable). You should see:

```
2026-06-21T10:00:00Z  INFO vibe_bridge: vibe-bridge listening on http://0.0.0.0:5151
```

### Run the serial bridge (USB transport)

```sh
cd bridge

# Basic usage — reads token from config.toml
cargo run --bin serial_bridge -- --port COM7

# Override all defaults
cargo run --bin serial_bridge -- \
  --port COM7 \
  --url http://localhost:5151 \
  --token your-secret-here \
  --config config.toml
```

Replace `COM7` with the actual port of your ESP32:
- **Windows:** Check Device Manager → Ports (COM & LPT)
- **Linux:** Usually `/dev/ttyUSB0` or `/dev/ttyACM0`
- **macOS:** Usually `/dev/cu.usbserial-*`

The serial bridge runs in a foreground loop. You will see:

```
[serial_bridge] COM7 @ 115200, bridge http://localhost:5151
[serial_bridge] ack forwarded: abc123def456   ← when firmware sends an ack
[cyd] I (12345) vibe-firmware: ...            ← firmware log lines
```

### Run tests

```sh
cd bridge
cargo test
```

The test suite covers `model.rs` (serialisation / deserialisation, camelCase field
renames) and `state.rs` (upsert semantics, mark_waiting, ack, snapshot isolation).

Run a specific test:

```sh
cargo test status_serialises_to_lowercase
cargo test upsert_keeps_max_last_activity
```

Run tests with output (useful for debugging):

```sh
cargo test -- --nocapture
```

### Linting

```sh
cd bridge
cargo clippy -- -D warnings
```

All clippy lints must pass with zero warnings before committing. The `-D warnings` flag
promotes warnings to errors.

### Formatting

```sh
cd bridge
cargo fmt

# Check only (no modification — use in CI)
cargo fmt -- --check
```

### Environment variables (bridge)

| Variable | Default | Description |
|---|---|---|
| `RUST_LOG` | `info` | Log level filter. Set to `debug` for verbose HTTP tracing. |

The bridge reads its runtime configuration exclusively from the TOML file (path defaulting
to `config.toml` in the working directory, or from the first CLI argument). There are no
other required environment variables for the bridge.

### Debug logging

```sh
$env:RUST_LOG = "debug"
cargo run
```

With `debug` level, `tracing-subscriber` emits per-request spans from the `axum` tower
middleware, including headers, response status, and latency.

For tower/hyper internals:

```sh
$env:RUST_LOG = "vibe_bridge=debug,tower_http=debug,hyper=debug"
cargo run
```

---

## Firmware Development

### Build (USB transport — default)

```sh
cd firmware
cargo +esp build --release
```

Build artefacts are placed in `C:\t\` (configured in `firmware/.cargo/config.toml`
as `target-dir = "C:\\t"` to avoid Windows MAX_PATH issues with the deep ESP-IDF
dependency tree).

### Build (WiFi transport)

```sh
cd firmware

# Set credentials as environment variables
$env:VIBE_SSID  = "NetworkName"
$env:VIBE_PASS  = "Password"
$env:VIBE_HOST  = "192.168.1.100"
$env:VIBE_PORT  = "5151"
$env:VIBE_TOKEN = "your-secret-here"

cargo +esp build --release --features wifi
```

### Flash and monitor

```sh
cd firmware

# Flash release build (auto-detects port)
cargo +esp espflash flash --release --monitor

# Specify port explicitly
espflash flash C:\t\xtensa-esp32-espidf\release\vibe-firmware --port COM7 --monitor
```

### Serial monitor only (without reflashing)

```sh
espflash monitor --port COM7
```

### ESP-IDF version

The firmware targets ESP-IDF `v5.3.2` (set in `firmware/.cargo/config.toml`):

```toml
[env]
ESP_IDF_VERSION = "v5.3.2"
```

On first build, `embuild` downloads and compiles ESP-IDF automatically. This takes
10–20 minutes. Subsequent builds use the cached SDK.

### Feature flags

| Feature flag | Default | Effect |
|-------------|---------|--------|
| *(none)* | yes | USB serial transport via UART0 (`serial_bridge` on the host) |
| `wifi` | no | Direct HTTP to bridge over WiFi; requires `VIBE_*` env vars |
| `wifi,ota` | no | WiFi + OTA updates via `esp_https_ota`; uses `partitions_ota.csv` |
| `eink` | no | E-paper display (Waveshare 2.9" B/W); USB-transport only — **mutually exclusive with `wifi`** |
| `ble` | no | BLE GATT server; code-complete but toolchain-blocked (see the README's BLE note) |

```sh
cargo +esp build --release                    # USB (default)
cargo +esp build --release --features wifi    # WiFi
cargo +esp build --release --features wifi,ota# WiFi + OTA
cargo +esp build --release --features eink    # E-ink (USB transport, separate SPI bus)
```

USB and WiFi transports are mutually exclusive via `#[cfg(feature = "wifi")]` /
`#[cfg(not(feature = "wifi"))]` guards throughout `src/main.rs`.

### Tunable constants (source-level)

| Constant | Location | Default | Description |
|----------|----------|---------|-------------|
| `WORKING_SEC` | `bridge/src/hub.rs` | `60.0` s | Age threshold below which a session is *Working* |
| `GONE_TTL` | `bridge/src/hub.rs` | `14400.0` s | Sessions older than 4 hours are pruned from `/state` |
| `POLL_MS` | `firmware/src/main.rs` | `2000` ms | WiFi poll interval |
| `POLL_SECS` | `bridge/src/bin/serial_bridge.rs` | `2` s | Serial bridge push interval |
| `BAUD` | `bridge/src/bin/serial_bridge.rs` | `115200` | UART baud rate |

### Environment variables (firmware — WiFi mode only)

| Variable | Example | Description |
|---|---|---|
| `VIBE_SSID` | `"HomeNetwork"` | WiFi SSID to join |
| `VIBE_PASS` | `"password123"` | WiFi password |
| `VIBE_HOST` | `"192.168.1.100"` | LAN IP of the machine running `vibe-bridge` |
| `VIBE_PORT` | `"5151"` | Port `vibe-bridge` is listening on |
| `VIBE_TOKEN` | `"abc...def"` | Shared secret token (must match `config.toml`) |

These are consumed at compile time via `env!("VIBE_SSID")` etc., and are baked into
the firmware binary. They are not read at runtime.

---

## Code Map

| File | Responsibility |
|---|---|
| `bridge/src/main.rs` | Config loading (TOML), Tokio runtime, collector spawn, axum listener |
| `bridge/src/collector.rs` | `scan_claude()` — walk `~/.claude/projects/**/*.jsonl`, mtime → session |
| `bridge/src/hub.rs` | `create_router()`, three axum handlers, token auth, status classification |
| `bridge/src/state.rs` | `Store` — `RwLock<Inner>` with `upsert`, `mark_waiting`, `ack`, `snapshot` |
| `bridge/src/model.rs` | `Session`, `SessionRow`, `Status`, `StateResponse`, `UsageBlock`, `UsageInfo` |
| `bridge/src/bin/serial_bridge.rs` | Poll `/state`, `make_mini()`, write to serial, ACK reader thread |
| `firmware/src/main.rs` | Everything: display init, SPI, touch, JSON parser, render, transport loops |

---

## Changing the Colour Palette

Colours are returned by small inline functions in `firmware/src/main.rs` so the
dark/light theme can switch them at runtime. The dark-theme defaults are:

```rust
fn c_bg()     -> Rgb565 { Rgb565::new(2,  5,  2)  }  // #141414 dark background
fn c_claude() -> Rgb565 { Rgb565::new(26, 29, 11) }  // #D97757 Claude orange
fn c_codex()  -> Rgb565 { Rgb565::new(20, 34, 30) }  // #A78BFA Codex purple
fn c_work()   -> Rgb565 { Rgb565::new(9,  55, 16) }  // #4ADE80 working green
fn c_wait()   -> Rgb565 { Rgb565::new(30, 41,  4) }  // #F5A623 waiting amber
fn c_offline()-> Rgb565 { Rgb565::new(28, 18,  9) }  // #E5484D offline red

// Provider accent colours used in detail overlay headers
const BRAND_OPENCODE: Rgb565 = Rgb565::new(2, 46, 20);  // teal-green
const BRAND_HERMES:   Rgb565 = Rgb565::new(7, 32, 30);  // blue
```

`Rgb565::new(r, g, b)` takes 5-bit R, 6-bit G, 5-bit B values. Use an online RGB565
converter to map hex colours. To add a light theme, check `Settings.dark` inside each
function and return an alternate value.

---

## Adding Support for Other AI Tools

The firmware currently renders native icons for four providers via `draw_badge()` in
`firmware/src/main.rs`:

| `tool` string | Icon | Accent colour |
|--------------|------|--------------|
| `"claude"` (default) | Claude pixel logo | Orange `#D97757` |
| `"codex"` | Codex pixel logo | Purple `#A78BFA` |
| `"opencode"` | OpenCode terminal logo | Teal-green |
| `"hermes"` | Hermes gradient logo | Blue |

To add a fifth provider:

1. Add a collector in `bridge/src/collector.rs` that scans the relevant session
   directory and sets the `tool` field
2. Add an 18×18 px RGBA pixel array to `firmware/src/icons.rs` (use a 2-bit alpha:
   `0` = transparent, `255` = opaque)
3. Extend `draw_badge()` and `provider_meta()` in `firmware/src/main.rs` to dispatch to
   the new icon and return the correct name + accent colour

---

## CI / Quality Checklist

Before opening a PR or pushing to main:

```sh
# From workspace root
cd bridge
cargo fmt -- --check
cargo clippy -- -D warnings
cargo test
```

All three must exit 0 with no output differences (fmt) and no lint errors (clippy).
There is currently no firmware test suite (no `std` test harness on Xtensa targets);
firmware correctness is validated by flashing and observing display output.
