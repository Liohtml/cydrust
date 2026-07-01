# Changelog

All notable changes to CYDRUST will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-07-01

### Added
- **Bridge** — Multi-provider session collectors: Codex (`~/.codex/sessions/`), OpenCode (`opencode.db`), Hermes (`state.db`) — all opened read-only/immutable so they never contend with live processes
- **Bridge** — `active_turn` field on `Session`: keeps a session "Working" even when the transcript mtime briefly goes stale mid-turn (tail-read heuristic for Claude, event markers for Codex, message role for OpenCode/Hermes)
- **Bridge** — `usage.rs`: `claude_usage()` probes Anthropic API with OAuth token; `codex_usage()` reads freshest `token_count` rate-limit snapshot; `capacity()` returns go/pace/throttle verdict
- **Bridge** — `metrics.rs`: `summarize_metrics()` — today's per-provider token/cost/model rollup for all 4 providers; `build_titles()` — first substantive user prompt per session (noise-filtered) as summary title
- **Bridge** — `/state` response extended: `capacity`, `metrics`, session `summary` field; `dedup by (tool, project)` collapses multiple sessions for the same project to one card
- **Bridge** — 4 background threads: session scan (2s), usage gauges (60s), metrics (120s), titles (120s); `Shared` struct separates slow computed data from the fast session store
- **Bridge** — `serial_bridge`: reconnect loop (survives device unplug/replug); non-blocking ACK/log read; DTR+RTS lowered on open to prevent ESP32 reset
- **Bridge** — Optional `[pricing]` table in `config.toml` for per-model USD cost calculation
- **Firmware** — METRICS tab (4th tab): per-model token/cost rows with provider badge, share bar, and % label; totals line; `parse_metrics()` / `render_metrics()`
- **Firmware** — `trunc_bytes()`: UTF-8-safe string truncation (prevents panic on em-dashes, accented chars, CJK in model names / session summaries)
- **Firmware** — Session detail overlay (`View::Detail`): shows truncated id, human-readable age, wait duration, word-wrapped summary
- **Firmware** — Provider icons: Claude, Codex, OpenCode, and Hermes 18×18 px logos alpha-composited onto display (`icons.rs`)
- **Firmware** — `provider_meta()` helper: centralised name + accent colour lookup for all four providers
- **Firmware** — `draw_badge()`: unified icon dispatch — real pixel logos for all providers (no more monogram fallbacks)
- **Firmware** — Full token usage model (`Usage` struct): `pct`, `reset_sec`, `week_pct`, `week_reset_sec`, `will_exhaust`, `burn_per_hr`, `leftover_pct`, `eta_clock`
- **Firmware** — NVS-persisted `Settings` struct: brightness (LEDC PWM, 10–100 %), sleep timer (off/1/5/15/30 min), dark/light theme
- **Firmware** — Functional SETTINGS tab with interactive brightness and sleep controls
- **Firmware** — Flicker-free rendering: `full_clear` flag separates layout changes from data refreshes
- **Firmware** — Dark/light theme via dynamic colour functions (`c_bg()`, `c_claude()`, etc.)
- **Firmware** — Helper functions: `humanize_age()`, `humanize_dur()`, `wrap_lines()`, `pct_str()`
- **Bridge** — `serial_bridge`: extended session wire format with short keys (`i`, `a`, `ws`, `s`)
- **Bridge** — `serial_bridge`: `provider_mini()` emits compact usage objects (`p`, `r`, `wp`, `wr`, `we`, `b`, `lo`, `e`); fields omitted at sentinel values
- **Bridge** — `serial_bridge`: `round3()` keeps float values to 3 decimal places on the wire
- GitHub Actions CI/CD pipelines (bridge build/test/lint, firmware build, release automation)
- Comprehensive test suite: 59 tests covering state management, HTTP endpoints, and session collection
- Project documentation: architecture overview, hardware wiring guide, API reference, development guide
- Community files: CONTRIBUTING, SECURITY policy, issue templates, PR template
- Justfile for ergonomic development commands
- cargo-deny and cargo-audit security scanning
- Dependabot configuration for automated dependency updates
- `.gitignore` covering Rust targets, ESP-IDF artifacts, and secrets
- `config.example.toml` and `.env.example` templates

## [0.1.0] - 2026-06-21

### Added
- **Bridge** — Axum 0.8 HTTP server scanning `~/.claude/projects/**/*.jsonl` every 2 seconds
- **Bridge** — REST API: `GET /state`, `POST /ack`, `POST /hook`
- **Bridge** — Token-based authentication via `X-VibeMonitor-Token` header
- **Bridge** — In-memory session store with `RwLock<HashMap>` for thread-safe concurrent access
- **Bridge** — Session lifecycle: Working (< 60s activity), Idle (≥ 60s), Waiting (hook event)
- **Bridge** — Automatic session expiry after 4 hours (14400s)
- **Bridge** — `serial_bridge` binary: polls `/state`, writes compact JSON to serial port, forwards ACKs
- **Bridge** — TOML-based configuration (`token`, `host`, `port`)
- **Firmware** — ESP32 ST7789 320×240 SPI display with mipidsi driver
- **Firmware** — Session cards displaying project name and status with color coding
- **Firmware** — Usage percentage tab showing Claude vs Codex attribution
- **Firmware** — WiFi transport mode (`--features wifi`): polls bridge every 2 seconds
- **Firmware** — USB serial transport mode (default): receives newline-delimited JSON from serial_bridge
- **Firmware** — Hand-written JSON parser to avoid heap allocation on constrained MCU
- **Firmware** — Offline detection: 3 consecutive WiFi failures or 6-second serial timeout
- **Firmware** — Color palette: dark bg (#141414), Claude orange (#D97757), Codex purple (#A78BFA)
- **Firmware** — Display layout: tab bar, header with usage %, session cards (up to 6), footer

[Unreleased]: https://github.com/Liohtml/cydrust/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Liohtml/cydrust/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Liohtml/cydrust/releases/tag/v0.1.0
