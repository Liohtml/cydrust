<#
.SYNOPSIS
    Package CYDRUST firmware into flashable merged .bin artifacts (Windows).

.DESCRIPTION
    Produces a single merged, flashable firmware image per variant using
    `cargo espflash save-image --merge`. A merged image contains the
    bootloader (0x1000), partition table (0x8000), and app (0x10000) and is
    flashed to the device at offset 0x0 — so end users need only esptool /
    espflash, NOT the full esp Rust toolchain.

    For each requested variant it emits, into the output directory:
      * vibe-firmware-<variant>-<chip>.bin   (the merged image)
      * flash.ps1 / flash.sh / flash.txt     (ready-to-run flash commands)

.PARAMETER Variant
    Which build(s) to package: 'default', 'wifi', or 'all' (default 'default').
    NOTE on 'wifi': the wifi build hard-requires VIBE_SSID / VIBE_PASS /
    VIBE_HOST / VIBE_PORT / VIBE_TOKEN at COMPILE time (env!()), so a wifi
    image baked in CI contains placeholder/secret values and is generally
    NOT redistributable. See RELEASES.md.

.PARAMETER Chip
    Target chip (default 'esp32').

.PARAMETER OutDir
    Output directory (default '.\dist').

.EXAMPLE
    .\package.ps1                       # package the default (USB) build
    .\package.ps1 -Variant all         # package default + wifi
#>
[CmdletBinding()]
param(
    [ValidateSet('default', 'wifi', 'all')]
    [string]$Variant = 'default',
    [string]$Chip = 'esp32',
    [string]$OutDir = '.\dist'
)

$ErrorActionPreference = 'Stop'
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
Push-Location $ScriptDir

# Work around Windows MAX_PATH the same way the rest of the project does.
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = 'C:\t' }

# Read the crate version from Cargo.toml for naming / flash notes.
$Version = (Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value

# espflash 4.x is invoked via the cargo subcommand so it builds with the esp
# toolchain pinned by rust-toolchain.toml. Verify it is present.
$cargoEspflash = (& cargo espflash --version 2>$null)
if ($LASTEXITCODE -ne 0) {
    Write-Error "cargo-espflash not found. Install it with: cargo install espflash"
}
Write-Host "Using $cargoEspflash" -ForegroundColor Cyan

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDirAbs = (Resolve-Path $OutDir).Path

# Merged image flashes at offset 0x0.
$MergedOffset = '0x0'

function Build-Variant {
    param([string]$Name)

    $featureArgs = @()
    if ($Name -eq 'default') {
        $featureArgs = @('--no-default-features')
    }
    elseif ($Name -eq 'wifi') {
        $featureArgs = @('--features', 'wifi')
        # The wifi build needs these at compile time. Provide harmless
        # placeholders if the caller hasn't exported real values so the build
        # does not fail; the resulting image will NOT connect to anything
        # useful. Real releases must set these in the environment first.
        if (-not $env:VIBE_SSID)  { $env:VIBE_SSID  = 'CHANGEME'; Write-Warning 'VIBE_SSID not set; using placeholder.' }
        if (-not $env:VIBE_PASS)  { $env:VIBE_PASS  = 'CHANGEME' }
        if (-not $env:VIBE_HOST)  { $env:VIBE_HOST  = '192.168.1.100' }
        if (-not $env:VIBE_PORT)  { $env:VIBE_PORT  = '5151' }
        if (-not $env:VIBE_TOKEN) { $env:VIBE_TOKEN = 'CHANGEME' }
    }

    $binName = "vibe-firmware-$Name-$Chip.bin"
    $binPath = Join-Path $OutDirAbs $binName

    Write-Host "`n=== Packaging variant '$Name' -> $binName ===" -ForegroundColor Green

    # save-image --merge builds (incremental, fast) and writes a single image.
    # Stream cargo's output straight to the host so it is NOT captured into the
    # function's return pipeline.
    & cargo espflash save-image `
        --release `
        --chip $Chip `
        --merge `
        --skip-update-check `
        @featureArgs `
        $binPath | Out-Host
    if ($LASTEXITCODE -ne 0) { Write-Error "save-image failed for variant '$Name'." }

    if (-not (Test-Path $binPath)) { Write-Error "Expected output $binPath was not produced." }
    $sizeBytes = (Get-Item $binPath).Length
    $sizeKB = [math]::Round($sizeBytes / 1KB, 1)
    Write-Host ("Produced {0} ({1} KB / {2} bytes)" -f $binName, $sizeKB, $sizeBytes) -ForegroundColor Green

    # Record the produced file name for the flash-helper generation below.
    $script:ProducedBins += $binName
}

$variantsToBuild = @()
switch ($Variant) {
    'all'     { $variantsToBuild = @('default', 'wifi') }
    default   { $variantsToBuild = @($Variant) }
}

$script:ProducedBins = @()
foreach ($v in $variantsToBuild) { Build-Variant -Name $v }

# ── Emit flash helpers ────────────────────────────────────────────────────────
# A merged image is written to flash at 0x0. Two equivalent tools are documented:
#   espflash write-bin 0x0 <bin>           (from the espflash project)
#   esptool.py --chip esp32 write_flash 0x0 <bin>   (no Rust toolchain needed)

$binList = $script:ProducedBins
$variantLines = ($binList | ForEach-Object { "  - $_" }) -join "`n"

$flashTxt = @"
CYDRUST firmware $Version - flashing instructions
==================================================

These are MERGED full-flash images. They include the bootloader, partition
table, and application, so you flash them at offset 0x0. No Rust toolchain
is required - only esptool or espflash.

Set PORT to your device's serial port first (e.g. COM7 on Windows,
/dev/ttyUSB0 on Linux, /dev/cu.usbserial-* on macOS).

Variants in this directory:
$variantLines

--- Option A: esptool (Python, no Rust toolchain) -------------------------------
  pip install esptool
  esptool.py --chip esp32 --port <PORT> --baud 460800 write_flash 0x0 <BIN>

--- Option B: espflash (standalone) ---------------------------------------------
  cargo install espflash      # or download a prebuilt espflash binary
  espflash write-bin --port <PORT> 0x0 <BIN>

After flashing, reset the board. The display should light up.
"@

Set-Content -Path (Join-Path $OutDirAbs 'flash.txt') -Value $flashTxt -Encoding ascii

# flash.ps1 — interactive convenience wrapper for Windows users.
$flashPs1 = @'
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
'@
Set-Content -Path (Join-Path $OutDirAbs 'flash.ps1') -Value $flashPs1 -Encoding ascii

# flash.sh — same convenience for Linux / macOS users.
$flashSh = @'
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
'@
Set-Content -Path (Join-Path $OutDirAbs 'flash.sh') -Value $flashSh -Encoding ascii

Write-Host "`nArtifacts written to: $OutDirAbs" -ForegroundColor Cyan
Get-ChildItem $OutDirAbs | Select-Object Name, Length | Format-Table -AutoSize

Pop-Location
