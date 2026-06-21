# Development Guide

This document covers everything needed to build, run, test, and modify CYDRUST locally.

---

## Repository Layout

```
CYDRUST/
├── Cargo.toml               Workspace root (members: ["bridge"])
├── bridge/
│   ├── Cargo.toml           vibe-bridge crate (axum server + serial_bridge binary)
│   ├── config.toml          Local config — git-ignored (token, host, port)
│   ├── config.example.toml  Template — tracked in git
│   └── src/
│       ├── main.rs          Entry point — reads config, spawns collector, starts axum
│       ├── collector.rs     Walks ~/.claude/projects/**/*.jsonl, upserts sessions
│       ├── hub.rs           Axum route handlers (GET /state, POST /ack, POST /hook)
│       ├── model.rs         Data types — Session, SessionRow, Status, StateResponse
│       ├── state.rs         RwLock<HashMap> store with upsert / ack / mark_waiting
│       └── bin/
│           └── serial_bridge.rs  Polls /state, writes mini-JSON to serial port
└── firmware/
    ├── Cargo.toml           vibe-firmware crate (ESP32 / Xtensa)
    ├── rust-toolchain.toml  channel = "esp"
    ├── .cargo/config.toml   target = xtensa-esp32-espidf, ESP_IDF_VERSION = v5.3.2
    ├── build.rs             embuild integration (ESP-IDF cmake)
    └── src/
        └── main.rs          Display driver, JSON parser, WiFi/USB transport logic
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

No additional system libraries are required on Windows. On Linux, install
`libudev-dev` for the `serialport` crate:

```sh
# Debian / Ubuntu
sudo apt install libudev-dev
```

### Firmware (ESP32)

| Tool | Install command | Notes |
|---|---|---|
| espup | `cargo install espup && espup install` | Installs the Xtensa Rust fork + GCC toolchain |
| espflash | `cargo install espflash` | Flashing and serial monitor |
| ESP-IDF v5.3.2 | Installed automatically by `embuild` on first build | Requires Python 3.8+ |

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

| Feature | Cargo flag | Description |
|---|---|---|
| `usb` (default) | `cargo +esp build --release` | USB CDC transport via `serial_bridge` |
| `wifi` | `cargo +esp build --release --features wifi` | Direct HTTP to bridge over WiFi |

The features are mutually exclusive via `#[cfg(feature = "wifi")]` / `#[cfg(not(feature = "wifi"))]`
guards throughout `src/main.rs`.

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

## Adding a New Tool (e.g. Codex)

The collector currently hard-codes `tool: "claude".into()` for every session it finds.
To track a second tool:

1. Add a second `scan_*()` function in `bridge/src/collector.rs` that walks the new
   tool's project directory and calls `Store::upsert()` with `tool: "codex".into()`.
2. Spawn that function in the collector loop in `bridge/src/main.rs`.
3. In `firmware/src/main.rs`, the `tool` field already flows through `make_mini()` and
   `parse_state()`. The card render checks `row.tool.as_str() == "codex"` to pick the
   purple `C_CODEX` accent. No firmware changes needed for display.

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
