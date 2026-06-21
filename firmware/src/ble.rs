// ── BLE transport (NimBLE GATT server) ───────────────────────────────────────
//
// Receives newline-delimited mini-JSON frames (the SAME wire format the USB
// stdin transport consumes — see `parse_state` in main.rs) over a single
// writable GATT characteristic, and pushes each decoded `DisplayState` into the
// shared state the render loop reads. No WiFi, no USB data.
//
// This whole module is `#[cfg(feature = "ble")]`-gated; the default USB build
// never references it.
//
// Wire model
// ----------
// A BLE characteristic write is MTU-limited (~20 bytes at the default 23-byte
// ATT MTU, up to ~512 with a negotiated MTU), and a single mini-JSON frame from
// the bridge routinely exceeds that. So the host streams the bytes as a series
// of WRITE / WRITE_NO_RSP requests and we reassemble here, splitting on '\n' —
// exactly the buffer-until-newline logic the USB reader thread uses. The host
// MUST terminate each complete JSON frame with a '\n'.
//
// GATT layout
// -----------
//   Device name : "VibeMonitor"
//   Service     : 6e6d0001-b5a3-f393-e0a9-e50e24dcca9e
//   Char (WRITE | WRITE_NO_RSP) : 6e6d0002-b5a3-f393-e0a9-e50e24dcca9e
//
// (Random 128-bit UUIDs — no registered 16-bit assignment is claimed.)

use std::sync::{Arc, Mutex};
use std::time::Instant;

use esp32_nimble::{uuid128, BLEAdvertisementData, BLEDevice, NimbleProperties};

use crate::{parse_state, DisplayState};

/// Shared state handle: `(latest DisplayState, last-receive Instant)`.
/// Same type the render loop locks on.
pub type Shared = Arc<Mutex<(DisplayState, Instant)>>;

/// Cap on the reassembly buffer. A mini-JSON frame for 8 sessions + metrics is
/// comfortably under 4 KiB; if a peer floods us without ever sending '\n' we
/// drop the buffer rather than grow unbounded.
const MAX_FRAME: usize = 8192;

/// Initialise the NimBLE stack, register a writable characteristic whose writes
/// are reassembled into newline-delimited JSON frames, and start advertising.
///
/// Returns after advertising has begun; the NimBLE host task keeps running in
/// the background and re-advertises after a disconnect, so the device stays
/// reconnectable. The render loop in `main` owns `shared` and simply reads it.
pub fn start(shared: Shared) {
    let device = BLEDevice::take();
    let advertising = device.get_advertising();
    let server = device.get_server();

    // Re-advertise on disconnect so the host can reconnect without a reboot.
    {
        let adv = advertising;
        server.on_disconnect(move |_desc, _reason| {
            ::log::info!("BLE: client disconnected, re-advertising");
            let _ = adv.lock().start();
        });
    }
    server.on_connect(|_server, desc| {
        ::log::info!("BLE: client connected: {:?}", desc);
    });

    let service = server.create_service(uuid128!("6e6d0001-b5a3-f393-e0a9-e50e24dcca9e"));

    let characteristic = service.lock().create_characteristic(
        uuid128!("6e6d0002-b5a3-f393-e0a9-e50e24dcca9e"),
        NimbleProperties::WRITE | NimbleProperties::WRITE_NO_RSP,
    );

    // Per-connection reassembly buffer, owned by the write callback. The
    // callback runs on the NimBLE host task, so we guard the buffer with a
    // Mutex (the closure must be Send + Sync + 'static).
    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::with_capacity(512)));
    let shared_cb = shared.clone();

    characteristic.lock().on_write(move |args| {
        let chunk = args.recv_data();
        let mut buf = buf.lock().unwrap();
        buf.extend_from_slice(chunk);

        // Split on '\n', parse each complete line, retain the trailing partial.
        loop {
            let Some(nl) = buf.iter().position(|&b| b == b'\n') else { break };
            // Take the line (without the newline) out of the buffer.
            let line: Vec<u8> = buf.drain(..=nl).take(nl).collect();
            if let Ok(s) = std::str::from_utf8(&line) {
                let t = s.trim();
                if t.starts_with('{') {
                    if let Some(ds) = parse_state(t) {
                        let mut g = shared_cb.lock().unwrap();
                        g.0 = ds;
                        g.1 = Instant::now();
                    }
                }
            }
        }

        // Guard against a peer that never sends a newline.
        if buf.len() > MAX_FRAME {
            ::log::warn!("BLE: frame buffer overflow ({} B), dropping", buf.len());
            buf.clear();
        }
    });

    let _ = advertising.lock().set_data(
        BLEAdvertisementData::new()
            .name("VibeMonitor")
            .add_service_uuid(uuid128!("6e6d0001-b5a3-f393-e0a9-e50e24dcca9e")),
    );
    if let Err(e) = advertising.lock().start() {
        ::log::error!("BLE: advertising start failed: {:?}", e);
    } else {
        ::log::info!("BLE: advertising as \"VibeMonitor\"");
    }
}
