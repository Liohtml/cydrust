# Troubleshooting

Cross-cutting triage for the bridge, firmware, and transport layer. For deep
hardware-wiring debugging (SPI pinout, solid-white/black boot screens, touch
calibration), see [docs/hardware.md](hardware.md#troubleshooting-hardware) instead.
For WSL2/usbipd-specific issues, see [docs/wsl2.md](wsl2.md#troubleshooting).

---

## Display shows nothing / white screen

| Check | Resolution |
|-------|-----------|
| Backlight | `GPIO21` must be driven HIGH — verify it is not floating |
| SPI pins | Double-check SCL=14, SDA=13, CS=15, DC=2 against your module's pinout |
| Power | Some 3.2" ST7789 modules need 3.3 V; never connect VCC to 5 V |
| `display init failed` in serial monitor | Try swapping `ColorInversion::Inverted` ↔ `Normal` and `ColorOrder::Bgr` ↔ `Rgb` in `firmware/src/main.rs` |
| Build for wrong target | Confirm `firmware/.cargo/config.toml` has `target = "xtensa-esp32-espidf"` |

## WiFi not connecting

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

## Serial bridge / USB timeout

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

## Sessions not appearing

| Check | Resolution |
|-------|-----------|
| Claude projects dir | Bridge scans `~/.claude/projects/`; run `ls ~/.claude/projects` to confirm JSONL files exist |
| Active Claude Code session | Open a project in Claude Code; JSONL files are created on first tool use |
| Bridge not running | `curl http://localhost:5151/state -H "X-VibeMonitor-Token: <tok>"` should return JSON |
| Age threshold | A session appears as *working* only if its JSONL mtime is < 60 s ago |
| Hooks not installed | `waiting`/idle transitions still work without hooks, but the *Waiting* amber state only fires if `install_hooks` has been run (see [docs/api.md](api.md#post-hook)) |

## `hub offline` banner stuck

The banner appears when:
- **WiFi mode:** 3 or more consecutive failed HTTP requests to `/state`
- **USB mode:** no newline received on UART within the last 6 seconds

Check: is `vibe-bridge` running? Is `serial_bridge` running (USB mode)? Check firewall
rules.

---

For hook-specific behaviour (which events flip *Waiting*/clear it, and how
`install_hooks` registers them), see [docs/api.md](api.md#post-hook) and
[docs/architecture.md](architecture.md#session-lifecycle).
