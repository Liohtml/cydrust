# WSL2 Setup (Windows + WSL)

Running the bridge under WSL2 lets you access the CYD firmware's data path (currently
localhost-only in the hub). Follow the steps below **once**, then use `just wsl-up` to
start the bridge on any WSL session.

---

## Prerequisites (Windows host)

1. **Install usbipd-win** — forwards USB devices into WSL:
   ```powershell
   winget install usbipd
   ```
2. **Bind the CYD to usbipd** (one-time, admin PowerShell):
   ```powershell
   usbipd bind --hardware-id 1a86:7523
   ```
   (Replace `1a86:7523` with your device's VID:PID if using a different USB-SERIAL
   adapter; check `usbipd list`.)

---

## Quick Start (WSL side)

```bash
# From the cydrust repo root:
just wsl-up
```

This one command:
1. Checks the CYD (`1a86:7523`) is actually connected — exits quietly if not.
2. Binds it to usbipd if it isn't shared yet (needs admin once; prints how).
3. Attaches the CYD to WSL (via `usbipd attach`) and waits for `/dev/ttyUSB0`.
4. Verifies read/write permission on the device (`dialout` group).
5. Starts `vibe-bridge` if it isn't already listening on `:5151`.
6. Execs `serial_bridge` to stream data from the CYD.

`just wsl-up` runs `scripts/wsl-cyd-up.sh` — see that script for the exact detection
and attach logic.

---

## What Happens on Replug?

`usbipd attach` is **one-shot** by default — it does *not* re-attach automatically when
the device is unplugged and plugged back in. After a replug:

1. Windows re-detects the CYD, but WSL's `/dev/ttyUSB0` does **not** reappear on its own.
2. Re-run `just wsl-up` (it re-attaches and `serial_bridge` reconnects), **or**
3. Start a persistent attach that survives replug:
   `usbipd.exe attach --wsl --auto-attach --hardware-id 1a86:7523` (this stays running
   in the foreground and re-attaches on each replug).

Once `/dev/ttyUSB0` is back, `serial_bridge` reconnects automatically and the firmware
shows the session list within ~5 seconds.

**Note:** usbipd bindings do not persist across a Windows reboot's usbipd service
restart in all versions — re-run `just wsl-up` after a reboot, or use `--auto-attach`
under a supervisor for unattended setups.

---

## Troubleshooting

**`Permission denied on /dev/ttyUSB0`:**
```bash
sudo usermod -aG dialout $USER
wsl --shutdown  # Restart WSL
```

**Device not appearing after replug:**
- Check Windows Device Manager — is the CYD re-detected?
- If so, re-run `just wsl-up` or manually: `usbipd.exe attach --wsl --hardware-id 1a86:7523`

**Bridge already running:**
- If the bridge didn't start in step 4, you can start it manually in another WSL tab:
  ```bash
  cd bridge && cargo run --release --bin vibe-bridge -- config.toml
  ```

For general (non-WSL) bridge and firmware issues, see
[docs/troubleshooting.md](troubleshooting.md).
