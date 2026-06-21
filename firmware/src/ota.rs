//! OTA (Over-The-Air) firmware updates over WiFi.
//!
//! WiFi-only — the whole module is compiled out of the default USB build.
//!
//! Flow (HTTPS streamed straight into the inactive OTA slot):
//!   1. `EspHttpConnection` opens an HTTPS GET to `url` using the ESP-IDF
//!      mbedTLS certificate bundle (`esp_crt_bundle_attach`) for server-cert
//!      validation. (Plain `http://` is also accepted for a LAN test server.)
//!   2. The body is read in chunks. The first chunk is peeked with
//!      `EspFirmwareInfoLoad::fetch` to pull the new image's `esp_app_desc_t`
//!      version; if it equals the running slot's version we abort early and
//!      return `Ok(false)` (already current — no flash wear, no reboot).
//!   3. Otherwise each chunk is streamed to the next OTA partition via
//!      `EspOta::initiate_update()` -> `EspOtaUpdate::write()`.
//!   4. `EspOtaUpdate::complete()` calls `esp_ota_end` (validates the image:
//!      magic byte + SHA256) and `esp_ota_set_boot_partition` (atomically points
//!      otadata at the new slot). On ANY error before this point the update is
//!      dropped → `esp_ota_abort` runs → the current slot stays bootable. The
//!      device is therefore un-brickable by a failed/partial download.
//!   5. Returns `Ok(true)`; the CALLER is responsible for rebooting.
//!
//! After a successful boot of the new image the app SHOULD call
//! [`mark_valid`] once it is confident it is healthy. This is only strictly
//! required if app-rollback (`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`) is turned
//! on; with the default config it is a harmless no-op-ish bookkeeping call.
//!
//! Underlying API used (esp-idf-svc 0.50 / esp-idf-sys 0.36):
//!   - `esp_idf_svc::ota::{EspOta, EspOtaUpdate, EspFirmwareInfoLoad}`
//!     wrapping `esp_ota_begin` / `esp_ota_write` / `esp_ota_end` /
//!     `esp_ota_set_boot_partition` / `esp_ota_get_running_partition`.
//!   - `esp_idf_svc::http::client::{EspHttpConnection, Configuration}` (the
//!     ESP-IDF `esp_http_client` with mbedTLS) for the HTTPS transport.
//!   - `esp_idf_svc::sys::esp_crt_bundle_attach` for TLS root-cert validation.

use anyhow::{anyhow, Context, Result};
use embedded_svc::{http::client::Client as HttpClient, http::Method};
use embedded_svc::io::Read as EmbeddedRead;
use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};
use esp_idf_svc::ota::{EspFirmwareInfoLoad, EspOta, FirmwareInfo};
use log::{info, warn};

/// Streamed-read chunk size. 4 KiB keeps RAM use tiny while still being a
/// comfortable flash-write granularity.
const CHUNK: usize = 4096;

/// Check the firmware at `url` and, if it differs from what is currently
/// running, stream it into the next OTA slot and set it as the boot partition.
///
/// Returns:
///   - `Ok(true)`  — a new image was written and activated; the caller should
///                   reboot (e.g. `esp_idf_svc::hal::reset::restart()`).
///   - `Ok(false)` — the served image reports the same version as the running
///                   slot; nothing was written.
///   - `Err(_)`    — download/validation failed. The current slot is untouched
///                   and remains bootable (the partially-written slot is aborted).
///
/// `url` may be `https://host/path/app.bin` (recommended, cert-validated via the
/// ESP-IDF cert bundle) or `http://host:port/app.bin` for a trusted LAN server.
pub fn check_and_update(url: &str) -> Result<bool> {
    info!("OTA: checking {url}");

    // --- Identify the currently running firmware version (for the skip check). ---
    let mut ota = EspOta::new().context("EspOta::new (OTA partition table missing?)")?;
    let running_ver: Option<heapless::String<24>> = ota
        .get_running_slot()
        .ok()
        .and_then(|s| s.firmware)
        .map(|fw| fw.version);
    if let Some(v) = running_ver.as_ref() {
        info!("OTA: running firmware version = {v}");
    }

    // --- Open the HTTPS connection. ---
    // `crt_bundle_attach` enables TLS server-cert validation against the
    // bundled Mozilla roots. For plain http:// it is simply unused.
    let cfg = HttpConfig {
        buffer_size: Some(4096),
        buffer_size_tx: Some(1024),
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        timeout: Some(core::time::Duration::from_secs(30)),
        ..Default::default()
    };
    let conn = EspHttpConnection::new(&cfg).context("HTTPS connection init")?;
    let mut client = HttpClient::wrap(conn);
    let req = client
        .request(Method::Get, url, &[])
        .context("OTA request build")?;
    let mut resp = req.submit().context("OTA request submit")?;
    let status = resp.status();
    if status != 200 {
        return Err(anyhow!("OTA server returned HTTP {status}"));
    }

    // --- Stream the body into the next OTA slot. ---
    // The update is only committed on `complete()`. Any early return (or a
    // panic) drops `update`, whose `Drop` calls `esp_ota_abort`, leaving the
    // current slot bootable. Never bricks.
    let mut update = ota
        .initiate_update()
        .context("esp_ota_begin (no free OTA slot?)")?;

    let mut header: Vec<u8> = Vec::new(); // accumulates until app_desc is parseable
    let mut version_checked = false;
    let mut total: usize = 0;
    let mut buf = [0u8; CHUNK];

    loop {
        let n = match EmbeddedRead::read(&mut resp, &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                // update dropped here -> esp_ota_abort
                return Err(anyhow!("OTA read error after {total} bytes: {e:?}"));
            }
        };
        let chunk = &buf[..n];

        // Peek the version from the first ~1 KiB before committing to a flash burn.
        if !version_checked {
            header.extend_from_slice(chunk);
            let mut info = blank_info();
            match EspFirmwareInfoLoad.fetch(&header, &mut info) {
                Ok(true) => {
                    version_checked = true;
                    info!("OTA: served firmware version = {}", info.version);
                    if let Some(cur) = running_ver.as_ref() {
                        if *cur == info.version && !cur.is_empty() {
                            info!("OTA: already up to date ({cur}); aborting download");
                            // Drop `update` -> abort; nothing was committed.
                            drop(update);
                            return Ok(false);
                        }
                    }
                    // header is only kept until parsed; free it.
                    header = Vec::new();
                }
                Ok(false) => { /* need more bytes; keep accumulating */ }
                Err(e) => warn!("OTA: firmware-info parse error (continuing): {e:?}"),
            }
            // Safety valve: don't accumulate unbounded if the header never parses.
            if header.len() > 8192 {
                version_checked = true;
                header = Vec::new();
            }
        }

        if let Err(e) = update.write(chunk) {
            return Err(anyhow!("esp_ota_write failed at {total} bytes: {e:?}"));
        }
        total += n;
    }

    if total == 0 {
        return Err(anyhow!("OTA download was empty (0 bytes)"));
    }

    // `complete()` = esp_ota_end (validate magic + SHA256) + esp_ota_set_boot_partition.
    update
        .complete()
        .context("esp_ota_end/set_boot_partition (image validation failed)")?;

    info!("OTA: wrote {total} bytes; boot partition switched. Reboot to apply.");
    Ok(true)
}

/// Mark the currently running slot valid, cancelling any pending rollback.
///
/// Call once after a successful boot when app-rollback is enabled
/// (`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y`). Harmless otherwise.
pub fn mark_valid() -> Result<()> {
    let mut ota = EspOta::new().context("EspOta::new")?;
    ota.mark_running_slot_valid()
        .context("esp_ota_mark_app_valid_cancel_rollback")?;
    info!("OTA: running slot marked valid");
    Ok(())
}

fn blank_info() -> FirmwareInfo {
    FirmwareInfo {
        version: heapless::String::new(),
        released: heapless::String::new(),
        description: None,
        signature: None,
        download_id: None,
    }
}
