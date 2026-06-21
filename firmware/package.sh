#!/usr/bin/env bash
#
# Package CYDRUST firmware into flashable merged .bin artifacts (Linux/macOS).
#
# Produces a single merged, flashable firmware image per variant using
# `cargo espflash save-image --merge`. A merged image contains the bootloader
# (0x1000), partition table (0x8000), and app (0x10000) and is flashed to the
# device at offset 0x0 — so end users need only esptool / espflash, NOT the
# full esp Rust toolchain.
#
# For each requested variant it emits, into the output directory:
#   * vibe-firmware-<variant>-<chip>.bin   (the merged image)
#   * flash.ps1 / flash.sh / flash.txt     (ready-to-run flash commands)
#
# Usage:
#   ./package.sh [variant] [chip] [outdir]
#     variant : default | wifi | all   (default: default)
#     chip    : esp32 (default)
#     outdir  : output dir              (default: ./dist)
#
# NOTE on 'wifi': the wifi build hard-requires VIBE_SSID / VIBE_PASS /
# VIBE_HOST / VIBE_PORT / VIBE_TOKEN at COMPILE time (env!()), so a wifi image
# baked in CI contains placeholder/secret values and is generally NOT
# redistributable. See RELEASES.md.

set -euo pipefail

VARIANT="${1:-default}"
CHIP="${2:-esp32}"
OUTDIR="${3:-./dist}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Honor an existing CARGO_TARGET_DIR; otherwise let .cargo/config.toml decide.
# (On Windows the project uses C:\t to dodge MAX_PATH; on Linux/CI a tmp dir
#  is typically exported by the caller, e.g. CARGO_TARGET_DIR=/tmp/fw-target.)

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.+)".*/\1/')"

if ! cargo espflash --version >/dev/null 2>&1; then
  echo "ERROR: cargo-espflash not found. Install it with: cargo install espflash" >&2
  exit 1
fi
echo "Using $(cargo espflash --version)"

mkdir -p "$OUTDIR"
OUTDIR_ABS="$(cd "$OUTDIR" && pwd)"

build_variant() {
  local name="$1"
  local -a feature_args=()

  if [ "$name" = "default" ]; then
    feature_args=(--no-default-features)
  elif [ "$name" = "wifi" ]; then
    feature_args=(--features wifi)
    # Provide harmless placeholders so the compile-time env!() does not fail
    # if the caller hasn't exported real values. Such an image will NOT
    # connect to anything useful — real releases must export real secrets.
    : "${VIBE_SSID:=CHANGEME}"
    : "${VIBE_PASS:=CHANGEME}"
    : "${VIBE_HOST:=192.168.1.100}"
    : "${VIBE_PORT:=5151}"
    : "${VIBE_TOKEN:=CHANGEME}"
    export VIBE_SSID VIBE_PASS VIBE_HOST VIBE_PORT VIBE_TOKEN
    [ "$VIBE_SSID" = "CHANGEME" ] && echo "WARNING: VIBE_SSID not set; using placeholder." >&2
  fi

  local bin_name="vibe-firmware-${name}-${CHIP}.bin"
  local bin_path="${OUTDIR_ABS}/${bin_name}"

  echo
  echo "=== Packaging variant '${name}' -> ${bin_name} ==="

  cargo espflash save-image \
    --release \
    --chip "$CHIP" \
    --merge \
    --skip-update-check \
    "${feature_args[@]}" \
    "$bin_path"

  if [ ! -f "$bin_path" ]; then
    echo "ERROR: expected output $bin_path was not produced." >&2
    exit 1
  fi
  local size
  size="$(stat -c%s "$bin_path" 2>/dev/null || stat -f%z "$bin_path")"
  echo "Produced ${bin_name} (${size} bytes)"
}

case "$VARIANT" in
  all)     VARIANTS=(default wifi) ;;
  default) VARIANTS=(default) ;;
  wifi)    VARIANTS=(wifi) ;;
  *) echo "ERROR: unknown variant '$VARIANT' (expected default|wifi|all)" >&2; exit 1 ;;
esac

for v in "${VARIANTS[@]}"; do
  build_variant "$v"
done

# ── Emit flash helpers ────────────────────────────────────────────────────────
# A merged image is written to flash at 0x0.

BIN_LIST="$(cd "$OUTDIR_ABS" && ls vibe-firmware-*.bin | sed 's/^/  - /')"

cat > "${OUTDIR_ABS}/flash.txt" <<EOF
CYDRUST firmware ${VERSION} — flashing instructions
==================================================

These are MERGED full-flash images. They include the bootloader, partition
table, and application, so you flash them at offset 0x0. No Rust toolchain
is required — only esptool or espflash.

Set PORT to your device's serial port first (e.g. COM7 on Windows,
/dev/ttyUSB0 on Linux, /dev/cu.usbserial-* on macOS).

Variants in this directory:
${BIN_LIST}

--- Option A: esptool (Python, no Rust toolchain) -------------------------------
  pip install esptool
  esptool.py --chip esp32 --port <PORT> --baud 460800 write_flash 0x0 <BIN>

--- Option B: espflash (standalone) ---------------------------------------------
  cargo install espflash      # or download a prebuilt espflash binary
  espflash write-bin --port <PORT> 0x0 <BIN>

After flashing, reset the board. The display should light up.
EOF

# flash.sh convenience wrapper
cat > "${OUTDIR_ABS}/flash.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
PORT="${1:-}"
BIN="${2:-}"
BAUD="${3:-460800}"
if [ -z "$PORT" ]; then
  echo "usage: ./flash.sh <PORT> [BIN] [BAUD]" >&2
  echo "  e.g. ./flash.sh /dev/ttyUSB0" >&2
  exit 1
fi
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -z "$BIN" ]; then
  BIN="$(ls "$DIR"/vibe-firmware-*.bin | head -n1)"
fi
echo "Flashing $BIN to $PORT at $BAUD baud (offset 0x0)..."
if command -v esptool.py >/dev/null 2>&1; then
  esptool.py --chip esp32 --port "$PORT" --baud "$BAUD" write_flash 0x0 "$BIN"
elif command -v espflash >/dev/null 2>&1; then
  espflash write-bin --port "$PORT" 0x0 "$BIN"
else
  echo "Neither esptool.py nor espflash found. Install one." >&2
  exit 1
fi
EOF
chmod +x "${OUTDIR_ABS}/flash.sh"

# flash.ps1 convenience wrapper for Windows users grabbing the same release
cat > "${OUTDIR_ABS}/flash.ps1" <<'EOF'
#requires -version 5
param(
    [Parameter(Mandatory = $true)][string]$Port,
    [string]$Bin,
    [int]$Baud = 460800
)
$ErrorActionPreference = 'Stop'
if (-not $Bin) {
    $Bin = (Get-ChildItem -Path $PSScriptRoot -Filter 'vibe-firmware-*.bin' | Select-Object -First 1).FullName
}
if (-not (Test-Path $Bin)) { throw "Firmware image not found: $Bin" }
Write-Host "Flashing $Bin to $Port at $Baud baud (offset 0x0)..." -ForegroundColor Cyan
if (Get-Command esptool.py -ErrorAction SilentlyContinue) {
    esptool.py --chip esp32 --port $Port --baud $Baud write_flash 0x0 $Bin
} elseif (Get-Command espflash -ErrorAction SilentlyContinue) {
    espflash write-bin --port $Port 0x0 $Bin
} else {
    throw "Neither esptool.py nor espflash found. Install one: 'pip install esptool' or 'cargo install espflash'."
}
EOF

echo
echo "Artifacts written to: ${OUTDIR_ABS}"
ls -la "$OUTDIR_ABS"
