#!/usr/bin/env bash
# wsl-cyd-up.sh — Bring the CYDRUST hub up under WSL2.
#
# Under WSL2 a Windows COM port is NOT visible to Linux. The bridge must run
# inside WSL (it scans ~/.claude/projects), so the CYD's USB-serial device has
# to be forwarded into WSL via usbipd-win. This script does that *only when the
# CYD is plugged in*, then starts the bridge + serial_bridge so the firmware's
# "hub offline" banner clears.
#
# Target device: USB-SERIAL CH340 on the CYD board.
set -euo pipefail

# --- Device identity (the "only when this device is connected" guard) ---
HWID="1a86:7523"          # CH340 on the CYD (VID 1a86 / PID 7523)
SERIAL_DEV="/dev/ttyUSB0"
BRIDGE_PORT="5151"

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_DIR"

log() { printf '\033[36m[cyd]\033[0m %s\n' "$*"; }
err() { printf '\033[31m[cyd]\033[0m %s\n' "$*" >&2; }

# --- 0. usbipd-win present on the Windows side? ---
if ! command -v usbipd.exe >/dev/null 2>&1; then
  err "usbipd-win not found. Install once from an admin PowerShell:"
  err "    winget install usbipd"
  exit 1
fi

# --- 1. Is the CYD connected right now? ---
# NB: 'usbipd list' does NOT accept --hardware-id; filter its output ourselves.
LIST="$(usbipd.exe list 2>/dev/null || true)"
if ! grep -qi "$HWID" <<<"$LIST"; then
  log "CYD (CH340 $HWID) is not connected — nothing to do."
  exit 0
fi
log "CYD found: $HWID"

# --- 2. Bind (persistent share) if needed — requires admin once ---
if grep -i "$HWID" <<<"$LIST" | grep -qi "Not shared"; then
  log "Device not shared yet — attempting bind ..."
  if ! usbipd.exe bind --hardware-id "$HWID" 2>/dev/null; then
    err "bind failed (needs admin). Run once in an admin PowerShell:"
    err "    usbipd bind --hardware-id $HWID"
    exit 1
  fi
fi

# --- 3. Attach into WSL (idempotent; harmless if already attached) ---
if [[ ! -e "$SERIAL_DEV" ]]; then
  log "Attaching USB device to WSL ..."
  usbipd.exe attach --wsl --hardware-id "$HWID" 2>/dev/null || true
  for _ in $(seq 1 20); do
    [[ -e "$SERIAL_DEV" ]] && break
    sleep 0.5
  done
fi
if [[ ! -e "$SERIAL_DEV" ]]; then
  err "$SERIAL_DEV did not appear. Check:  usbipd.exe list   and   dmesg | tail"
  exit 1
fi
log "Serial device available: $SERIAL_DEV"

# --- 3b. Read/write permission on the device? (root:dialout by default) ---
if [[ ! -r "$SERIAL_DEV" || ! -w "$SERIAL_DEV" ]]; then
  err "No access to $SERIAL_DEV (owned by $(stat -c '%U:%G' "$SERIAL_DEV"))."
  if ! id -nG | grep -qw dialout; then
    err "You are not in the 'dialout' group. Fix permanently, then restart WSL:"
    err "    sudo usermod -aG dialout \$USER   # then: wsl --shutdown"
  fi
  err "For this session only:  sudo chmod 666 $SERIAL_DEV"
  exit 1
fi

# --- 4. Bridge server listening on :5151? else start it ---
if ! ss -tlnp 2>/dev/null | grep -q ":$BRIDGE_PORT "; then
  log "Starting bridge on :$BRIDGE_PORT ..."
  ( cd bridge && cargo run --release --bin vibe-bridge -- config.toml ) >/tmp/cyd-bridge.log 2>&1 &
  for _ in $(seq 1 60); do
    ss -tlnp 2>/dev/null | grep -q ":$BRIDGE_PORT " && break
    sleep 0.5
  done
fi
if ! ss -tlnp 2>/dev/null | grep -q ":$BRIDGE_PORT "; then
  err "bridge did not come up on :$BRIDGE_PORT. Log: tail -30 /tmp/cyd-bridge.log"
  exit 1
fi
log "bridge is up (:$BRIDGE_PORT)"

# --- 5. serial_bridge: poll bridge -> push compact JSON to the CYD ---
log "Starting serial_bridge on $SERIAL_DEV (Ctrl-C to stop) ..."
exec cargo run --release --bin serial_bridge -- --port "$SERIAL_DEV" --config bridge/config.toml
