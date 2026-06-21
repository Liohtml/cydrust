---
name: Bug Report
about: Something is not working as expected
title: "[Bug] "
labels: bug
assignees: liomachire
---

## Describe the Bug

<!-- A clear and concise description of what the bug is. -->

## Component

- [ ] Bridge (HTTP server / session collection)
- [ ] Serial Bridge (USB transport binary)
- [ ] Firmware (ESP32 display)
- [ ] CI / Build system
- [ ] Documentation

## Environment

**For Bridge / Serial Bridge bugs:**
- OS: <!-- Windows 11 / Ubuntu 22.04 / macOS 14 -->
- Rust version: <!-- output of `rustc --version` -->
- Bridge version/commit: <!-- git rev-parse --short HEAD -->

**For Firmware bugs:**
- ESP32 board: <!-- e.g. ESP32-WROOM-32 DevKit v1, ESP32-S3-DevKitC-1 -->
- Display module: <!-- e.g. 2.0" ST7789 SPI 240x320 -->
- Transport mode: <!-- WiFi / USB serial -->
- ESP-IDF version: <!-- from firmware/rust-toolchain.toml or `idf.py --version` -->

## Steps to Reproduce

1. 
2. 
3. 

## Expected Behavior

<!-- What did you expect to happen? -->

## Actual Behavior

<!-- What actually happened? -->

## Logs

<details>
<summary>Bridge output (<code>RUST_LOG=debug cargo run</code>)</summary>

```
paste log here
```
</details>

<details>
<summary>Serial monitor output (<code>cargo espflash monitor</code>)</summary>

```
paste serial output here
```
</details>

## Additional Context

<!-- Screenshots, wiring photos, config (without secrets), anything else relevant. -->
