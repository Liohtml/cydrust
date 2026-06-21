# Hardware Setup Guide

This guide covers assembling the CYDRUST hardware: an ESP32 development board wired to
an ST7789 SPI display. Total build time is roughly 20–30 minutes.

---

## Bill of Materials

| Component | Recommendation | Notes |
|---|---|---|
| ESP32 development board | ESP32-2432S028R ("Cheap Yellow Display") | Preferred — all pins pre-wired to the onboard ST7789 + XPT2046 touch IC. No loose wires needed. |
| ESP32 DevKit (fallback) | ESP32 DevKit v1 / WROOM-32 | Use if you don't have a CYD. Requires manual wiring per the table below. |
| ST7789 display module | 2.0" or 2.4" 240×320 SPI module | Choose ILI9341 only if you patch the `Builder::new(ST7789, …)` call in `firmware/src/main.rs`. |
| Jumper wires | Female-female Dupont, 10 cm | Required for breadboard DevKit builds only. |
| USB cable | USB-A to Micro-USB (CYD) or USB-C (DevKit v4+) | Used for flashing, serial monitor, and USB transport mode. |
| Host PC serial adapter | Built-in CH340 on CYD / CP2102 on DevKit | Both are auto-detected by `espflash`. |
| Optional: 3D-printed case | Community STL files for CYD enclosures | Search Printables / Thingiverse for "ESP32-2432S028R case". |

> **Recommended path:** The ESP32-2432S028R (CYD) is the cheapest fully integrated
> option — display, touchscreen, and ESP32 on one PCB for under $10. All firmware pin
> assignments in `src/main.rs` default to the CYD layout.

---

## Wiring (ESP32 DevKit → ST7789)

This table applies to a standalone ESP32 DevKit v1 wired to a bare ST7789 breakout board.
**Skip this section entirely if you have a CYD** — the display is soldered on.

```
ESP32 Pin   │  ST7789 Pin     │  Color (suggested)  │  Function
────────────┼─────────────────┼─────────────────────┼────────────────────
3.3V        │  VCC            │  Red                │  Power (3.3 V only!)
GND         │  GND            │  Black              │  Ground
GPIO 14     │  SCL (SCLK)     │  Yellow             │  SPI Clock
GPIO 13     │  SDA (MOSI)     │  Blue               │  SPI Data Out
GPIO 15     │  CS             │  Orange             │  Chip Select
GPIO 2      │  DC             │  Purple             │  Data / Command select
GPIO 4      │  RES (RESET)    │  White              │  Hardware reset
GPIO 21     │  BLK (LED/BL)   │  Green              │  Backlight (HIGH = on)
```

> **Warning:** ST7789 modules are 3.3 V devices. Do **not** connect VCC to the 5 V pin;
> it will damage the display controller.

### CYD (ESP32-2432S028R) pre-wired assignments

The CYD routes the display differently from a generic DevKit. The firmware already uses
the correct CYD pin numbers:

| Signal  | GPIO | Notes |
|---------|------|-------|
| SPI CLK | 14   | SPI2 clock |
| SPI MOSI| 13   | SPI2 data  |
| CS      | 15   | SPI2 chip select |
| DC      | 2    | Data/Command |
| BL      | 21   | Backlight, driven HIGH in firmware |

Touch controller (XPT2046) — bit-banged, CYD fixed pins:

| Signal  | GPIO | Direction |
|---------|------|-----------|
| T_CS    | 33   | Output    |
| T_CLK   | 25   | Output    |
| T_DIN   | 32   | Output    |
| T_DO    | 39   | Input-only (no pull-up) |
| T_IRQ   | 36   | Input (active-low when touched) |

---

## Pin Configuration in Firmware

All pin assignments are in `firmware/src/main.rs` inside `fn run()`:

```rust
// Backlight
let mut bl = PinDriver::output(peripherals.pins.gpio21)?;
bl.set_high()?;

// SPI for display
let spi = SpiDriver::new(
    peripherals.spi2,
    peripherals.pins.gpio14,   // SCK
    peripherals.pins.gpio13,   // MOSI
    None::<AnyInputPin>,       // MISO — not needed for write-only display
    &DriverConfig::new(),
)?;
let spi_device = SpiDeviceDriver::new(
    spi,
    Some(peripherals.pins.gpio15), // CS
    &SpiConfig::new().baudrate(55.MHz().into()),
)?;
let dc = PinDriver::output(peripherals.pins.gpio2)?;   // DC

// Touch (XPT2046 bit-bang)
let mut t_cs   = PinDriver::output(peripherals.pins.gpio33)?;
let mut t_clk  = PinDriver::output(peripherals.pins.gpio25)?;
let mut t_mosi = PinDriver::output(peripherals.pins.gpio32)?;
let t_miso     = PinDriver::input(peripherals.pins.gpio39)?;
let t_irq      = PinDriver::input(peripherals.pins.gpio36)?;
```

To adapt to a different board, change the `gpio*` numbers to match your hardware and
recompile.

---

## Display Orientation and Colour

The display is configured in landscape mode (320 wide × 240 tall):

```rust
Builder::new(ST7789, di)
    .display_size(240, 320)                     // native portrait size
    .invert_colors(ColorInversion::Inverted)    // required for this panel type
    .color_order(ColorOrder::Bgr)
    .orientation(Orientation::new().rotate(Rotation::Deg90))  // landscape
    .init(&mut FreeRtos)
```

The `ColorInversion::Inverted` flag is required for the CYD's ST7789 variant. Without
it, all colours appear as their complements. The RGB565 colour constants in the firmware
are pre-inverted to display correctly on screen.

**Layout zones (y-axis, landscape):**

| Range   | Height | Content |
|---------|--------|---------|
| y 0–25  | 26 px  | Tab bar (SESSIONS / USAGE / SETTINGS) |
| y 26–44 | 19 px  | Usage header line (Claude NN% / Codex NN%) |
| y 45    | 1 px   | Separator |
| y 46–207| 162 px | Session cards — 27 px stride, max 6 cards |
| y 214–239| 26 px | Footer / offline banner |

---

## Flash Instructions

### Prerequisites

```sh
# Install the esp toolchain (Xtensa + RISC-V targets)
espup install

# Reload environment variables printed by espup
. $HOME/export-esp.sh      # Linux/macOS
# or in PowerShell:
$env:PATH = ...            # follow espup output

# Install the espflash CLI
cargo install espflash
```

### Build and flash (USB transport — default)

```sh
cd firmware

# Build
cargo +esp build --release

# Flash and open serial monitor in one step
cargo +esp espflash flash --release --monitor
# or equivalently:
espflash flash target/xtensa-esp32-espidf/release/vibe-firmware --monitor
```

### Build and flash (WiFi transport)

Set the required environment variables before building:

```sh
export VIBE_SSID="YourNetworkName"
export VIBE_PASS="YourPassword"
export VIBE_HOST="192.168.1.100"   # IP of the machine running vibe-bridge
export VIBE_PORT="5151"
export VIBE_TOKEN="your-secret-token"

cargo +esp build --release --features wifi
espflash flash target/xtensa-esp32-espidf/release/vibe-firmware --monitor
```

On Windows (PowerShell):

```powershell
$env:VIBE_SSID  = "YourNetworkName"
$env:VIBE_PASS  = "YourPassword"
$env:VIBE_HOST  = "192.168.1.100"
$env:VIBE_PORT  = "5151"
$env:VIBE_TOKEN = "your-secret-token"

cargo +esp build --release --features wifi
```

> **Note:** Credentials are baked into the firmware binary at compile time via
> `env!("VIBE_SSID")`. Changing network details requires a recompile and reflash.

---

## Serial Monitor

After flashing, open the serial monitor to see firmware log output:

```sh
# Using espflash (recommended — handles CH340 reset quirk)
espflash monitor

# Or using cargo-espflash
cargo +esp espflash monitor

# Raw serial (Linux)
screen /dev/ttyUSB0 115200

# Raw serial (Windows PowerShell)
# Use PuTTY or the Windows Terminal with a COM port profile
```

Expected startup output:

```
I (312) main_task: Started main task
I (380) vibe-firmware: USB mode
```

Or in WiFi mode:

```
I (312) main_task: Started main task
I (380) vibe-firmware: WiFi mode
I (2100) wifi: connected to "YourNetworkName"
```

---

## Touchscreen Calibration

The XPT2046 raw ADC readings are mapped to screen pixels using constants in
`firmware/src/main.rs`:

```rust
const X_MIN: u16 =  300;
const X_MAX: u16 = 3800;
const Y_MIN: u16 =  300;
const Y_MAX: u16 = 3800;
```

If tap targets feel offset, touch four corners while logging the raw values from
`xpt_send_recv()` and update these constants to match your panel's calibration range.

---

## Troubleshooting Hardware

### Display shows solid white after boot

The display received power but no SPI commands. Likely causes:
- Wrong MOSI/SCK pins — double-check GPIO 13 and 14.
- DC pin (GPIO 2) not connected — the controller cannot distinguish commands from data.
- Missing `RES` (reset) signal — on DevKit builds, GPIO 4 must pulse the reset line.
  The firmware does not manually drive RES; `mipidsi::Builder` handles it via the
  hardware reset pin passed to `.init()`. If you wired no reset pin, pass
  `NoPin` and see if the display initialises without it.

### Display shows solid black (backlight off)

GPIO 21 (BL) is not being driven HIGH, or the backlight wire is disconnected.
Confirm with a multimeter: GPIO 21 should read ~3.3 V after boot.

### No session data appears on display (USB mode)

Check that `serial_bridge` is running and targeting the correct COM port:

```sh
cargo run --bin serial_bridge -- --port COM7
```

Watch the bridge console for `[serial_bridge] /state error:` messages. Confirm the
bridge is running and the token in `config.toml` matches.

### ESP32 resets when serial port is opened

The CH340 on many CYD boards connects DTR→GPIO0 and RTS→EN through capacitors, causing
an unintended reset sequence when a program opens the COM port. `serial_bridge` mitigates
this by explicitly lowering both control lines:

```rust
let _ = port.write_data_terminal_ready(false);
let _ = port.write_request_to_send(false);
```

If resets persist, add a 10 µF capacitor in series with the RTS→EN trace to increase the
RC time constant, or disable the DTR/RTS connections on the board.

### WiFi connects but display stays offline

1. Verify `VIBE_HOST` is the LAN IP of the machine running `vibe-bridge`, not
   `127.0.0.1` (loopback is unreachable from the ESP32).
2. Confirm `vibe-bridge` is binding to `0.0.0.0` (the default `host` in
   `config.example.toml`), not `127.0.0.1`.
3. Check that no firewall blocks port 5151 on the host.
4. After 3 consecutive fetch failures, the firmware sets `ds.offline = true` and shows
   the red "hub offline" banner. Reflash with debug logging and watch for HTTP error
   codes in the monitor output.

### SPI clock speed issues

The firmware sets SPI to 55 MHz (`55.MHz().into()`). Some cheap ST7789 breakout boards
cannot sustain this rate reliably. If you see display corruption (partial rows, colour
glitches), reduce the clock:

```rust
// In firmware/src/main.rs — change 55 to 40 or 27
&SpiConfig::new().baudrate(40.MHz().into()),
```
