# CYDRUST - Development Command Runner
# Install: cargo install just

# Default: list all recipes
default:
    @just --list

# === Bridge ===

# Build bridge in release mode
build-bridge:
    cd bridge && cargo build --release

# Run bridge with config
run-bridge config="bridge/config.toml":
    cd bridge && cargo run --release -- {{config}}

# Run serial bridge (USB transport)
run-serial port="COM7" config="bridge/config.toml":
    cd bridge && cargo run --release --bin serial_bridge -- --port {{port}} --config {{config}}

# Run bridge tests
test:
    cd bridge && cargo test

# Run tests with output
test-verbose:
    cd bridge && cargo test -- --nocapture

# Lint check
lint:
    cd bridge && cargo clippy -- -D warnings

# Format check
fmt-check:
    cd bridge && cargo fmt -- --check

# Format code
fmt:
    cd bridge && cargo fmt

# Full CI check (what GitHub Actions runs)
check: fmt-check lint test

# === Firmware ===

# Build firmware (USB mode, default)
build-firmware:
    cd firmware && cargo build --release

# Build firmware with WiFi support
build-firmware-wifi ssid="" pass="" host="192.168.1.100" port="5151" token="":
    cd firmware && VIBE_SSID={{ssid}} VIBE_PASS={{pass}} VIBE_HOST={{host}} VIBE_PORT={{port}} VIBE_TOKEN={{token}} cargo build --release --features wifi

# Flash firmware to device
flash:
    cd firmware && cargo espflash flash --release

# Flash and open serial monitor
flash-monitor:
    cd firmware && cargo espflash flash --release --monitor

# Open serial monitor without flashing
monitor:
    cd firmware && cargo espflash monitor

# === Setup ===

# Initial project setup
setup:
    @echo "Installing required tools..."
    cargo install espup just espflash
    espup install
    @echo "Setup complete! Source ~/export-esp.sh before building firmware."

# Copy example config
init-config:
    cp bridge/config.example.toml bridge/config.toml
    @echo "Edit bridge/config.toml with your token"

# Generate a random token
gen-token:
    @openssl rand -hex 16 2>/dev/null || powershell -Command "[System.Web.Security.Membership]::GeneratePassword(32,0)" 2>/dev/null || echo "Install openssl to generate tokens"

# === Release ===

# Bump version (usage: just bump-version 0.2.0)
bump-version version:
    sed -i 's/^version = ".*"/version = "{{version}}"/' bridge/Cargo.toml
    sed -i 's/^version = ".*"/version = "{{version}}"/' firmware/Cargo.toml
    @echo "Version bumped to {{version}} - don't forget to update CHANGELOG.md"

# Clean all build artifacts
clean:
    cd bridge && cargo clean
    cd firmware && cargo clean
