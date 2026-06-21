# Pre-built Firmware Releases

CYDRUST publishes pre-built, **flashable** ESP32 firmware images so you can run
the CYD display monitor **without installing the esp Rust toolchain**. You only
need a flashing tool (`esptool` or `espflash`) and a USB cable.

Releases are attached to GitHub Releases and named:

```
vibe-firmware-<variant>-<chip>.bin
```

e.g. `vibe-firmware-default-esp32.bin`.

---

## TL;DR — flash a release

1. Download `vibe-firmware-default-esp32.bin` from the
   [Releases page](../../releases) (it is attached to each `v*` tag).
2. Plug your CYD board in over USB and find its serial port:
   - Windows: `COM3`, `COM7`, ... (Device Manager → Ports)
   - Linux: `/dev/ttyUSB0` (CP210x) or `/dev/ttyACM0`
   - macOS: `/dev/cu.usbserial-*` or `/dev/cu.SLAB_USBtoUART`
3. Flash it at offset **`0x0`** (these are *merged* full-flash images):

   **esptool (Python — no Rust toolchain needed):**
   ```bash
   pip install esptool
   esptool.py --chip esp32 --port <PORT> --baud 460800 write_flash 0x0 vibe-firmware-default-esp32.bin
   ```

   **espflash (standalone):**
   ```bash
   cargo install espflash        # or grab a prebuilt espflash binary
   espflash write-bin --port <PORT> 0x0 vibe-firmware-default-esp32.bin
   ```

4. Press the board's reset (EN) button. The display should light up.

Each release also ships `flash.sh`, `flash.ps1`, and `flash.txt` helpers that
wrap the commands above — just pass your port:

```bash
./flash.sh /dev/ttyUSB0          # Linux/macOS
```
```powershell
.\flash.ps1 -Port COM7           # Windows
```

---

## Why offset `0x0`?

The released `.bin` is a **merged image**: `espflash save-image --merge` packs
the bootloader, partition table, and application into one file laid out exactly
as flash memory expects:

| Region          | Offset    | Source                  |
|-----------------|-----------|-------------------------|
| Bootloader      | `0x1000`  | ESP-IDF bootloader      |
| Partition table | `0x8000`  | `partition-table.bin`   |
| Application     | `0x10000` | `vibe-firmware` ELF     |
| (padding)       | ...       | `0xFF` to flash size    |

Because the image already contains those regions at their correct internal
offsets, you write the **whole file to `0x0`**. You do **not** pass separate
`0x1000 bootloader.bin 0x8000 partitions.bin 0x10000 app.bin` arguments — that
form is only for the individual (non-merged) binaries.

The default image targets a 4 MB flash (the CYD's size) and is therefore ~4 MB
on disk; the actual application is ~570 KB and the rest is `0xFF` padding.

---

## Supported board variants — honest status

The "CYD" is the AITRIP / generic **ESP32-2432S028R** 2.8" board with an
**ST7789** display. The firmware currently **hardcodes one display/touch
pinset** in `src/main.rs`, so the release matrix is by **feature**, not by
board revision:

| Artifact                          | What it is                              | Works on                                              | Status |
|-----------------------------------|-----------------------------------------|-------------------------------------------------------|--------|
| `vibe-firmware-default-esp32.bin` | USB/serial transport (no WiFi)          | ESP32-2432S028R (single-USB) and micro-USB/USB-C revs that share the ST7789 pinout; generic `esp32dev` for bring-up | **Real, redistributable** |
| `vibe-firmware-wifi-esp32.bin`    | WiFi transport                          | same boards, but see caveat below                     | **Conditional / not public by default** |

### Honesty notes

- **Display pins are hardcoded.** All CYD revisions that ship the ST7789 panel
  on the standard CYD pinout will work with the `default` image. CYD variants
  that use a different panel (e.g. some **ILI9341** "2432S028" units) or
  remapped touch pins are **not** covered by a separate `.bin` today — they
  would need pin-config changes in `src/main.rs` and a new build. Those are
  **aspirational**, not shipped.
- **`esp32dev` / generic ESP32** boards will boot and run the app logic, but
  without the CYD display wired to the expected pins you won't see output. It's
  useful for smoke-testing, not as an end-user target.
- **The `wifi` image bakes in secrets at compile time.** `src/main.rs` reads
  `VIBE_SSID`, `VIBE_PASS`, `VIBE_HOST`, `VIBE_PORT`, and `VIBE_TOKEN` via
  `env!()` — i.e. they are **compiled into the binary**. A WiFi `.bin` is
  therefore specific to one network + bridge and **should not be published
  publicly**. The release workflow only builds it when `VIBE_*` repository
  secrets are present, and even then you'd normally keep it as a private
  artifact. Most users should flash the `default` image and use the USB serial
  bridge.

---

## Building the images yourself

You need the esp Rust toolchain (`espup install`) and `espflash`
(`cargo install espflash`). From `firmware/`:

**Windows:**
```powershell
.\package.ps1                  # default (USB) image -> .\dist\
.\package.ps1 -Variant all     # default + wifi (wifi needs VIBE_* env vars)
```

**Linux/macOS:**
```bash
./package.sh default esp32 ./dist
./package.sh all esp32 ./dist  # wifi needs VIBE_* env vars exported
```

Under the hood each variant runs:

```bash
cargo espflash save-image --release --chip esp32 --merge --skip-update-check <OUT.bin>
```

(The default variant adds `--no-default-features`; the wifi variant adds
`--features wifi` and requires the `VIBE_*` env vars.)

The scripts also emit `flash.txt`, `flash.sh`, and `flash.ps1` next to the
`.bin`.

> The repo uses `CARGO_TARGET_DIR=C:\t` on Windows to avoid the `MAX_PATH`
> limit; `package.ps1` sets this automatically if unset. In CI we override it to
> `/tmp/fw-target`.

---

## Cutting a release (maintainers)

Releases are produced by
[`.github/workflows/firmware-release.yml`](../.github/workflows/firmware-release.yml),
triggered on any `v*` tag push:

```bash
# 1. Bump versions (optional but recommended)
just bump-version 0.2.0           # updates bridge/ + firmware/ Cargo.toml
#    ...update CHANGELOG.md...

# 2. Commit, then tag with annotated release notes
git commit -am "Release v0.2.0"
git tag -a v0.2.0 -m "Highlights of this release..."

# 3. Push the tag — this fires the workflow
git push origin master
git push origin v0.2.0
```

The workflow then:

1. Installs the esp Rust toolchain via `esp-rs/xtensa-toolchain@v1.5`.
2. Installs `espflash` (`cargo install espflash`).
3. Runs `firmware/package.sh default esp32 ./dist` to build + merge the USB image.
4. (Optional) builds the `wifi` image **only if** a `VIBE_SSID` repo secret is set.
5. Verifies the produced `.bin` exists and is a sane size.
6. Attaches the `.bin`(s) + `flash.*` helpers to the GitHub Release via
   `softprops/action-gh-release@v2.0.8`.

You can also run it manually from the Actions tab (**workflow_dispatch**); a
manual run builds and uploads CI artifacts but only attaches to a Release when
triggered by a tag.

### Optional WiFi release secrets

To have the workflow also produce a `wifi` image, define these repository
secrets (Settings → Secrets and variables → Actions). **Remember the values
are compiled into the binary** — treat the resulting `.bin` as sensitive:

- `VIBE_SSID`, `VIBE_PASS` — WiFi credentials
- `VIBE_HOST`, `VIBE_PORT` — bridge host/port
- `VIBE_TOKEN` — bridge auth token

If `VIBE_SSID` is absent, the wifi step is skipped and only the `default` image
is released — which is the correct behavior for public releases.
