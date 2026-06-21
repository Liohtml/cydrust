# Contributing to CYDRUST

Welcome, and thank you for considering a contribution to CYDRUST! This project bridges Claude Code session telemetry to embedded hardware — turning invisible AI usage patterns into something you can physically see on a desk. Every contribution, whether it's a bug report, a feature idea, improved documentation, or a pull request tested on real hardware, helps make the project better for everyone.

---

## Table of Contents

- [Project Mission](#project-mission)
- [Types of Contributions Welcome](#types-of-contributions-welcome)
- [Development Setup](#development-setup)
- [Code Style](#code-style)
- [Commit Message Format](#commit-message-format)
- [Branch Naming](#branch-naming)
- [Pull Request Process](#pull-request-process)
- [Testing Requirements](#testing-requirements)
- [Hardware Testing](#hardware-testing)
- [Code of Conduct](#code-of-conduct)

---

## Project Mission

CYDRUST monitors Claude Code sessions in real time and renders session state (active, waiting, idle), token usage, and AI attribution on an ESP32 with an ST7789 320×240 display. The bridge component runs on the developer's machine and exposes a REST API; the firmware polls or receives data over WiFi or USB serial. The goal is a reliable, low-latency feedback loop that is simple to build and easy to extend.

---

## Types of Contributions Welcome

| Type | Examples |
|---|---|
| **Bug reports** | Bridge crashes, display glitches, serial framing errors, wrong token counts |
| **Feature requests** | New display layouts, additional session metadata, OTA firmware update support |
| **Documentation** | Clarifying setup steps, wiring diagrams, adding examples |
| **Hardware testing** | Testing on ESP32 variants (ESP32-S3, ESP32-C3, WROOM, WROVER), alternative displays |
| **Code contributions** | Bug fixes, performance improvements, new transport modes, CI improvements |
| **Refactoring** | Improving error handling, reducing allocations in firmware, better abstractions |

If you are unsure whether your idea fits the project, open a Discussion before writing code.

---

## Development Setup

### Prerequisites

**Bridge (runs on your dev machine):**

- [Rust stable](https://rustup.rs/) — `rustup toolchain install stable`
- `cargo` (included with Rust)

**Firmware (targets ESP32):**

- Rust stable (same toolchain)
- [espup](https://github.com/esp-rs/espup) — installs the Xtensa toolchain:
  ```sh
  cargo install espup
  espup install
  ```
- [cargo-espflash](https://github.com/esp-rs/espflash):
  ```sh
  cargo install cargo-espflash
  ```
- [ldproxy](https://github.com/ivmarkov/ldproxy) (linker helper):
  ```sh
  cargo install ldproxy
  ```
- Source the Xtensa environment before building firmware:
  ```sh
  # Linux / macOS
  . $HOME/export-esp.sh
  # Windows PowerShell
  . $HOME\export-esp.ps1
  ```

### Clone and Build

```sh
git clone https://github.com/liomachire/cydrust.git
cd cydrust

# Build the bridge
cargo build -p bridge

# Build the serial bridge helper
cargo build -p serial-bridge

# Build firmware (after sourcing export-esp)
cargo build -p firmware
```

---

## Code Style

All Rust code must be formatted with `rustfmt` and must pass `clippy` with no warnings. These checks are enforced in CI and pull requests cannot be merged if they fail.

```sh
# Format
cargo fmt --all

# Lint
cargo clippy --all-targets --all-features -- -D warnings
```

Do not suppress `clippy` warnings with `#[allow(...)]` attributes unless there is a well-reasoned comment explaining why the suppression is necessary. Firmware code targeting `no_std` may require certain `clippy` configuration; see `.clippy.toml` or inline `#[allow]` with an explanation.

---

## Commit Message Format

This project uses [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/).

```
<type>(<scope>): <short description>

[optional body]

[optional footer(s)]
```

**Types:**

| Type | When to use |
|---|---|
| `feat` | A new feature |
| `fix` | A bug fix |
| `docs` | Documentation changes only |
| `chore` | Build system, CI, dependency bumps, housekeeping |
| `refactor` | Code restructuring without behavior change |
| `test` | Adding or fixing tests |
| `perf` | Performance improvements |

**Scopes (optional but encouraged):** `bridge`, `firmware`, `serial-bridge`, `ci`, `deps`

**Examples:**

```
feat(firmware): add brightness control via ambient light sensor
fix(bridge): handle missing session directory gracefully
docs: add wiring diagram for ST7789 SPI connection
chore(deps): bump axum to 0.8
```

Keep the subject line under 72 characters. Use the body to explain *why*, not *what*.

---

## Branch Naming

| Pattern | Purpose |
|---|---|
| `feature/<short-description>` | New features |
| `fix/<short-description>` | Bug fixes |
| `docs/<short-description>` | Documentation changes |
| `chore/<short-description>` | Maintenance tasks |
| `refactor/<short-description>` | Refactoring |

Examples: `feature/ota-firmware-update`, `fix/serial-framing-overflow`, `docs/esp32-s3-wiring`

---

## Pull Request Process

1. Fork the repository and create your branch from `main`.
2. Make your changes following the code style guidelines above.
3. Add or update tests where applicable (see [Testing Requirements](#testing-requirements)).
4. Ensure `cargo fmt --all` and `cargo clippy` pass with no warnings.
5. Update documentation if your change affects the public interface or setup steps.
6. Open a pull request against `main` using the pull request template.
7. At least one review approval is required before merging.
8. All CI checks must pass — fmt, clippy, and tests.
9. Squash commits before merge if the PR history is noisy (the maintainer may do this on merge).

For significant features or breaking changes, open an issue or Discussion first to align on approach before investing implementation time.

---

## Testing Requirements

**Bridge and serial-bridge:**

```sh
cargo test -p bridge
cargo test -p serial-bridge
```

All existing tests must pass. New functionality should include unit tests covering the happy path and principal error cases. Integration tests that spin up a mock HTTP server or mock serial port are welcome.

**Firmware:**

The firmware targets a bare-metal `no_std` environment; running unit tests on the host requires a `std` feature flag or host-side test harnesses. Where practical, add host-runnable unit tests for pure logic (parsing, formatting, state machines). Hardware-side testing is covered in the next section.

---

## Hardware Testing

Pull requests that modify firmware behavior **must** be tested on real ESP32 hardware before requesting review. Please document the following in your PR description:

- ESP32 board model and revision (e.g., ESP32-WROOM-32, ESP32-S3-DevKitC-1)
- Display module (e.g., 2.0" ST7789 SPI, 240×320)
- Transport mode tested (WiFi / USB serial / both)
- Observed display output and any serial monitor logs

If you do not have access to compatible hardware but have a fix you are confident in, state that clearly in the PR. The maintainer or another community member may be able to test it. Do not merge firmware PRs that have not been hardware-validated.

---

## Code of Conduct

This project follows the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md). By participating, you agree to uphold its standards. Please report unacceptable behavior to [lionel.machire@googlemail.com](mailto:lionel.machire@googlemail.com).
