# Changelog

All notable changes to CYDRUST will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
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

[Unreleased]: https://github.com/liomachire/cydrust/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/liomachire/cydrust/releases/tag/v0.1.0
