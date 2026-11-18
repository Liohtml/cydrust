#!/usr/bin/env bash
# Stage one self-contained host bundle for the current platform.
#
#   scripts/package-release.sh <version> [out-dir]
#
# Expects `cargo build --release` to have produced all four bridge binaries in
# target/release. Creates <out-dir>/cydrust-<version>-<host-triple>/ holding
# the binaries next to each other (install_hooks looks for vibe_hook as a
# sibling), plus the example config, licence and a short README. On Unix it
# also writes the .tar.gz; on Windows the workflow zips the staged directory.
#
# The platform part of the name comes from rustc's own host triple, so the
# label cannot drift from what was actually built.
set -euo pipefail

VERSION="${1:?usage: package-release.sh <version> [out-dir]}"
VERSION="${VERSION//\//-}" # a branch name (manual run) may contain slashes
OUT="${2:-dist}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${CARGO_TARGET_DIR:-$ROOT/target}/release"

TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
[ -n "$TRIPLE" ] || { echo "could not read host triple from rustc" >&2; exit 1; }

EXE=""
case "$TRIPLE" in *windows*) EXE=".exe" ;; esac

NAME="cydrust-${VERSION}-${TRIPLE}"
STAGE="$OUT/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE"

for bin in vibe-bridge serial_bridge install_hooks vibe_hook; do
  src="$BIN_DIR/$bin$EXE"
  [ -f "$src" ] || { echo "missing $src — run: cargo build --release --bins" >&2; exit 1; }
  cp "$src" "$STAGE/"
done
cp "$ROOT/bridge/config.example.toml" "$ROOT/LICENSE" "$STAGE/"

# Run-from-here prefix: PowerShell and Unix shells both need one.
if [ -n "$EXE" ]; then P='.\'; else P='./'; fi

cat > "$STAGE/README.txt" <<EOF
CYDRUST ${VERSION} — host tools (${TRIPLE})

Unpack anywhere and keep the files together. In this directory:

  1. Start the hub. The first run creates a config with a random token.

       ${P}vibe-bridge${EXE}

  2. Register the Claude Code hooks (once). No arguments needed.

       ${P}install_hooks${EXE}

  3. USB transport: start the relay. It finds the CYD's serial port itself.

       ${P}serial_bridge${EXE}

The device needs the firmware image from the same release
(vibe-firmware-default-esp32.bin), flashed at offset 0x0:

       esptool.py --chip esp32 write_flash 0x0 vibe-firmware-default-esp32.bin

vibe_hook${EXE} is what Claude Code calls; you never run it yourself, but it
has to stay next to install_hooks${EXE}.

Docs: https://github.com/Liohtml/cydrust
EOF

if [ -z "$EXE" ]; then
  tar -czf "$OUT/$NAME.tar.gz" -C "$OUT" "$NAME"
  echo "$OUT/$NAME.tar.gz"
else
  echo "$STAGE"
fi
