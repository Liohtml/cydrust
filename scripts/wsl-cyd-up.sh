#!/bin/bash
# WSL2 CYD bring-up helper: attach USB device, verify permissions, start bridge + serial_bridge

set -e

CYD_VENDOR_ID="1a86"   # CH340 USB-SERIAL
CYD_PRODUCT_ID="7523"
CYD_HARDWARE_ID="${CYD_VENDOR_ID}:${CYD_PRODUCT_ID}"

echo "🔌 CYD WSL2 Bring-Up Helper"
echo "   Attaching USB device ($CYD_HARDWARE_ID), checking permissions, starting bridge..."
echo

# ─── Check usbipd is installed ───
if ! command -v usbipd.exe &> /dev/null; then
    echo "❌ usbipd.exe not found. Install from:"
    echo "   https://github.com/dorssel/usbipd-win/releases"
    exit 1
fi

# ─── Check device is present ───
echo "📋 Checking if CYD ($CYD_HARDWARE_ID) is present on Windows..."
if ! usbipd.exe list --hardware-id $CYD_HARDWARE_ID &> /dev/null; then
    echo "⚠️  CYD not detected. Plug it in or check the hardware ID."
    exit 1
fi
echo "✓ CYD found"
echo

# ─── Attach the device ───
echo "🔗 Attaching USB device to WSL..."
usbipd.exe attach --wsl --hardware-id $CYD_HARDWARE_ID &> /dev/null || {
    echo "⚠️  Device may already be attached. Continuing..."
}

# ─── Wait for device node and check permissions ───
echo "⏳ Waiting for /dev/ttyUSB0..."
timeout=15
while [ $timeout -gt 0 ]; do
    if [ -e /dev/ttyUSB0 ]; then
        break
    fi
    sleep 1
    timeout=$((timeout - 1))
done

if [ ! -e /dev/ttyUSB0 ]; then
    echo "❌ Timeout waiting for /dev/ttyUSB0. Check dmesg."
    exit 1
fi
echo "✓ /dev/ttyUSB0 found"

# ─── Check read/write permission ───
if [ ! -r /dev/ttyUSB0 ] || [ ! -w /dev/ttyUSB0 ]; then
    echo "❌ Permission denied on /dev/ttyUSB0"
    echo "   Fix with: sudo usermod -aG dialout \$USER"
    echo "   Then: log out and back in (or: wsl --shutdown)"
    exit 1
fi
echo "✓ /dev/ttyUSB0 readable and writable"
echo

# ─── Start the bridge ───
echo "🌉 Starting vibe-bridge..."
if pgrep -f "cargo run.*vibe-bridge" &> /dev/null; then
    echo "   (already running)"
else
    # Try to verify the bridge is actually listening
    cd bridge || exit 1
    timeout 10 bash -c 'until curl -s http://localhost:5151/state -H "X-VibeMonitor-Token: dummy" 2>/dev/null | grep -q "ts"; do sleep 0.5; done' &> /dev/null || {
        echo "⚠️  Bridge may not have started. Starting it now..."
        cargo run --release --bin vibe-bridge -- config.toml &
        sleep 2
    }
    cd - > /dev/null
fi
echo "✓ Bridge is running (or was already running)"
echo

# ─── Start serial_bridge ───
echo "🔌 Starting serial_bridge..."
cd bridge || exit 1
exec cargo run --release --bin serial_bridge -- --port /dev/ttyUSB0 --config config.toml
