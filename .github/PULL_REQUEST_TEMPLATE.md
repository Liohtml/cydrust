## Description

<!-- What does this PR do and why? Link to related issue if applicable. Fixes #____ -->

## Type of Change

- [ ] Bug fix (non-breaking change that fixes an issue)
- [ ] New feature (non-breaking change that adds functionality)
- [ ] Breaking change (fix or feature that changes existing behavior)
- [ ] Refactoring (no behavior change)
- [ ] Documentation update
- [ ] CI/build system change

## Component

- [ ] Bridge (HTTP server, session collection)
- [ ] Serial Bridge (USB transport binary)
- [ ] Firmware (ESP32 display)
- [ ] CI / GitHub Actions
- [ ] Documentation

## Testing

- [ ] `cargo test -p bridge` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo fmt --check` passes
- [ ] Firmware builds cleanly (`cargo build -p firmware --release`)
- [ ] Manual testing performed (describe below)

**Manual testing details:**

<!-- What did you test, how, and what did you observe? -->

## Hardware Testing (Firmware PRs only)

- ESP32 board: <!-- e.g. ESP32-WROOM-32 DevKit v1 -->
- Display module: <!-- e.g. 2.0" ST7789 SPI 240x320 -->
- Transport mode tested: <!-- WiFi / USB serial / both -->
- Serial monitor output: <!-- paste relevant lines or attach screenshot -->

## Breaking Changes

<!-- If this is a breaking change, describe what breaks and what callers need to update. -->

## Screenshots / Video

<!-- For display/UI changes: attach a photo or short video of the ESP32 display. -->

## Checklist

- [ ] PR title follows conventional commit format (`feat(scope): description`)
- [ ] `CHANGELOG.md` updated under `[Unreleased]`
- [ ] Documentation updated if behavior changed
- [ ] New public functions/types have doc comments
- [ ] No secrets, credentials, or personal data in the diff
