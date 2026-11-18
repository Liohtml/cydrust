/// USB serial bridge for vibe-firmware (USB transport mode).
///
/// Runs alongside the vibe-bridge HTTP server.  Every POLL_SECS it:
///   1. GETs /state from the bridge
///   2. Writes the JSON as a single newline-terminated line to the CYD serial port
///
/// ACK lines sent back by the CYD (`{"ack":"<id>"}`) are forwarded via POST /ack
/// to the bridge so sessions clear their "waiting" state.
///
/// The port is found automatically by USB id (CH340 / CP2102) and re-detected
/// on every reconnect, so a replug onto a different port number is picked up.
/// Token and hub address come from the shared config lookup.
///
/// Usage:
///   serial_bridge                       # auto-detect the CYD
///   serial_bridge --port /dev/ttyUSB1   # force a specific port
///   serial_bridge --help
use std::{
    io::{Read, Write},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde_json::Value;
use serialport::SerialPort;
use vibe_bridge::{
    config,
    serial_port::{self, Detection},
};

const POLL_SECS: u64 = 2;
const BAUD: u32 = 115_200;

fn get_state(url: &str, token: &str) -> Result<String> {
    let body = ureq::get(&format!("{url}/state"))
        .set("X-VibeMonitor-Token", token)
        .timeout(Duration::from_secs(3))
        .call()
        .context("/state request failed")?
        .into_string()
        .context("read /state body")?;
    Ok(body)
}

fn post_ack(url: &str, token: &str, id: &str) -> Result<()> {
    let body = format!("{{\"id\":\"{id}\"}}");
    ureq::post(&format!("{url}/ack"))
        .set("X-VibeMonitor-Token", token)
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(3))
        .send_string(&body)
        .context("/ack request failed")?;
    Ok(())
}

/// Open the port with the CYD-safe DTR/RTS handling (lowered so opening the
/// port doesn't reset the ESP32).
fn open_port(port_name: &str) -> serialport::Result<Box<dyn SerialPort>> {
    let mut p = serialport::new(port_name, BAUD)
        .timeout(Duration::from_millis(100))
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .open()?;
    let _ = p.write_data_terminal_ready(false);
    let _ = p.write_request_to_send(false);
    Ok(p)
}

/// Turn a failed open into something the user can act on. `os` is
/// `std::env::consts::OS`, passed in so every branch is testable anywhere.
fn describe_open_error(port: &str, kind: &serialport::ErrorKind, os: &str) -> String {
    use serialport::ErrorKind;
    match kind {
        ErrorKind::Io(std::io::ErrorKind::PermissionDenied) if os == "linux" => format!(
            "{port}: permission denied. Add yourself to the dialout group, then log out \
             and back in (under WSL: `wsl --shutdown`):\n    sudo usermod -aG dialout $USER"
        ),
        ErrorKind::Io(std::io::ErrorKind::PermissionDenied) => format!(
            "{port}: access denied — another program has the port open \
             (a serial monitor or a second serial_bridge?)"
        ),
        ErrorKind::NoDevice | ErrorKind::Io(std::io::ErrorKind::NotFound) => {
            format!("{port}: not present (unplugged?)")
        }
        other => format!("{port}: cannot open ({other:?})"),
    }
}

const USAGE: &str = "\
Usage: serial_bridge [--port PORT] [--url URL] [--config FILE] [--token TOKEN]

Relays session state from the hub to a CYD over USB serial.

  --port PORT     serial port to use (default: auto-detect the CYD by USB id)
  --url URL       hub base url (default: from the config, else http://localhost:5151)
  --config FILE   hub config to read token and address from
                  (default: ./config.toml, then the per-user config)
  --token TOKEN   hub token, if there is no config file on this machine
  -h, --help      show this help

The VIBE_MONITOR_TOKEN environment variable overrides the token from the config.";

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    port: Option<String>,
    url: Option<String>,
    token: Option<String>,
    config: Option<String>,
    help: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || match it.next() {
            Some(v) if !v.starts_with("--") => Ok(v.clone()),
            _ => Err(format!("{flag} needs a value")),
        };
        match flag.as_str() {
            "--port" => out.port = Some(value()?),
            "--url" => out.url = Some(value()?),
            "--token" => out.token = Some(value()?),
            "--config" => out.config = Some(value()?),
            "-h" | "--help" => out.help = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(out)
}

/// Print a status line only when it differs from the last one, so waiting for
/// a device does not scroll the same message every two seconds.
fn report(last: &mut String, msg: String) {
    if *last != msg {
        eprintln!("[serial_bridge] {msg}");
        *last = msg;
    }
}

/// Longest ack id we accept from the wire. Session ids are UUIDs (36 chars);
/// anything past this is a malformed or malicious frame, not an id.
const MAX_ACK_ID_LEN: usize = 128;

fn extract_ack_id(s: &str) -> Option<&str> {
    // minimal: find "ack":"<id>"
    let key = "\"ack\":\"";
    let start = s.find(key)? + key.len();
    let end = s[start..].find('"')? + start;
    if end - start > MAX_ACK_ID_LEN {
        return None;
    }
    Some(&s[start..end])
}

fn main() -> Result<()> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&raw) {
        Ok(a) if a.help => {
            println!("{USAGE}");
            return Ok(());
        }
        Ok(a) => a,
        Err(e) => {
            eprintln!("serial_bridge: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    // The config file is --config if given, otherwise the shared lookup
    // (./config.toml, then the per-user config vibe-bridge creates on first run).
    let client = match config::find(args.config.as_deref()) {
        Ok(found) => found.as_deref().and_then(config::read_client),
        // An explicit --config that does not exist is only fatal when there
        // is no other token source to fall back on.
        Err(_) if args.token.is_some() || std::env::var("VIBE_MONITOR_TOKEN").is_ok() => None,
        Err(e) => return Err(e),
    };

    // token: VIBE_MONITOR_TOKEN > config file > --token
    let token = std::env::var("VIBE_MONITOR_TOKEN")
        .ok()
        .or_else(|| client.as_ref().and_then(|c| c.token.clone()))
        .or(args.token.clone())
        .context(
            "no hub token found: start vibe-bridge once (it creates the config), \
             or pass --config <path>, --token, or set VIBE_MONITOR_TOKEN",
        )?;
    // url: --url > config file > default
    let bridge_url = args
        .url
        .clone()
        .or_else(|| client.as_ref().map(|c| c.url.clone()))
        .unwrap_or_else(|| "http://localhost:5151".to_string());

    match &args.port {
        Some(p) => println!("[serial_bridge] {p} @ {BAUD}, bridge {bridge_url}"),
        None => println!("[serial_bridge] auto-detecting the CYD @ {BAUD}, bridge {bridge_url}"),
    }

    // Reconnect loop: survives the device being unplugged/replugged. On any
    // port I/O error we drop the handle and look for the device again — by USB
    // id unless --port pinned a name, so a renumbered port is found too.
    let mut rx: Vec<u8> = Vec::new();
    let mut status = String::new();
    'reconnect: loop {
        let port_name = match &args.port {
            Some(p) => p.clone(),
            None => match serial_port::detect() {
                Detection::Found {
                    port,
                    chip,
                    also_matching,
                } => {
                    if !also_matching.is_empty() {
                        report(
                            &mut status,
                            format!(
                                "several candidate devices; using {port} ({chip}). \
                                 Others: {}. Use --port to choose.",
                                also_matching.join(", ")
                            ),
                        );
                    }
                    port
                }
                Detection::NotFound { seen } => {
                    let seen = if seen.is_empty() {
                        "none".to_string()
                    } else {
                        seen.join(", ")
                    };
                    report(
                        &mut status,
                        format!(
                            "no CYD found (looking for a CH340 or CP2102 USB-serial device). \
                             Serial ports present: {seen}. Is it plugged in? \
                             Use --port to force a port. Waiting..."
                        ),
                    );
                    thread::sleep(Duration::from_secs(2));
                    continue 'reconnect;
                }
            },
        };
        let mut port = match open_port(&port_name) {
            Ok(p) => {
                println!("[serial_bridge] connected to {port_name}");
                status.clear();
                p
            }
            Err(e) => {
                report(
                    &mut status,
                    format!(
                        "{} Retrying...",
                        describe_open_error(&port_name, &e.kind, std::env::consts::OS)
                    ),
                );
                thread::sleep(Duration::from_secs(2));
                continue 'reconnect;
            }
        };
        rx.clear();
        let mut last_push = Instant::now()
            .checked_sub(Duration::from_secs(POLL_SECS))
            .unwrap_or_else(Instant::now);

        loop {
            // 1) push state to the device every POLL_SECS
            if last_push.elapsed() >= Duration::from_secs(POLL_SECS) {
                last_push = Instant::now();
                match get_state(&bridge_url, &token) {
                    Err(e) => eprintln!("[serial_bridge] /state error: {e}"),
                    Ok(json) => {
                        let line = make_mini(&json) + "\n";
                        if let Err(e) = port.write_all(line.as_bytes()) {
                            eprintln!("[serial_bridge] write error ({e}); reconnecting...");
                            continue 'reconnect; // device gone → re-open
                        }
                        let _ = port.flush();
                    }
                }
            }

            // 2) read any ACK / log lines coming back from the device
            let mut tmp = [0u8; 256];
            match port.read(&mut tmp) {
                Ok(0) => {}
                Ok(n) => {
                    rx.extend_from_slice(&tmp[..n]);
                    while let Some(pos) = rx.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = rx.drain(..=pos).collect();
                        if let Ok(s) = std::str::from_utf8(&line) {
                            let l = s.trim();
                            if l.is_empty() {
                                continue;
                            }
                            if let Some(id) = extract_ack_id(l) {
                                match post_ack(&bridge_url, &token, id) {
                                    Ok(_) => println!("[serial_bridge] ack forwarded: {id}"),
                                    Err(e) => eprintln!("[serial_bridge] ack failed: {e}"),
                                }
                            } else {
                                println!("[cyd] {l}");
                            }
                        }
                    }
                    if rx.len() > 8192 {
                        rx.clear();
                    } // guard against runaway
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {} // normal idle
                Err(e) => {
                    eprintln!("[serial_bridge] read error ({e}); reconnecting...");
                    continue 'reconnect; // device gone → re-open
                }
            }
            thread::sleep(Duration::from_millis(40));
        }
    }
}

/// Reduce the full /state payload to only the fields the CYD firmware parses.
/// Full payload ~700 bytes, mini ~80 bytes — fits inside the ESP32 UART FIFO.
fn make_mini(body: &str) -> String {
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return compact_json(body),
    };

    let empty = vec![];
    let sessions: Vec<Value> = v["sessions"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|s| {
            let project = s["project"].as_str()?;
            let status = s["status"].as_str()?;
            let tool = s["tool"].as_str().unwrap_or("claude");
            let mut o = serde_json::Map::new();
            o.insert("project".into(), project.into());
            o.insert("status".into(), status.into());
            o.insert("tool".into(), tool.into());
            // Detail-view fields (short keys). id always; ageSec when present;
            // waitingSec + summary only for waiting sessions (keeps payload small).
            if let Some(id) = s["id"].as_str() {
                o.insert("i".into(), id.chars().take(12).collect::<String>().into());
            }
            if let Some(a) = s["ageSec"].as_i64() {
                o.insert("a".into(), a.into());
            }
            // summary/title for ALL sessions (<= 48 chars), not just waiting ones
            if let Some(sum) = s["summary"].as_str() {
                if !sum.is_empty() {
                    o.insert("s".into(), sum.chars().take(48).collect::<String>().into());
                }
            }
            if status == "waiting" {
                if let Some(w) = s["waitingSec"].as_i64() {
                    o.insert("ws".into(), w.into());
                }
            }
            Some(Value::Object(o))
        })
        .take(6) // device shows at most 6 cards; bounds the UART payload
        .collect();

    serde_json::json!({
        "sessions": sessions,
        "claude": provider_mini(&v["usage"]["claude"]),
        "codex":  provider_mini(&v["usage"]["codex"]),
        "metrics": metrics_mini(&v["metrics"]),
    })
    .to_string()
}

/// Compact rollup for the device Metrics tab. Flattens every provider's
/// per-model breakdown into one list, sorted by tokens desc, top 6 — so the
/// device shows Opus/Sonnet/Haiku/GPT each on their own row.
///   m=[{p=provider, n=model, t=tokens, u=usd?}]
///   tt=total tokens  tu=total usd  ts=total sessions  uc=usd complete
fn metrics_mini(m: &Value) -> Value {
    let mut out = serde_json::Map::new();
    let mut rows: Vec<(String, String, f64, Option<f64>)> = vec![];
    if let Some(provs) = m["providers"].as_object() {
        for (pname, pv) in provs {
            if let Some(models) = pv["models"].as_array() {
                for md in models {
                    let tk = md["tokens"].as_f64().unwrap_or(0.0);
                    if tk <= 0.0 {
                        continue;
                    }
                    let mn = md["model"].as_str().unwrap_or("?").to_string();
                    rows.push((pname.clone(), mn, tk, md["usd"].as_f64()));
                }
            }
        }
    }
    rows.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    rows.truncate(6);
    let mut arr: Vec<Value> = vec![];
    for (p, mn, tk, usd) in rows {
        let mut o = serde_json::Map::new();
        o.insert("p".into(), p.into());
        o.insert("n".into(), mn.chars().take(16).collect::<String>().into());
        o.insert("t".into(), tk.into());
        if let Some(u) = usd {
            o.insert("u".into(), ((u * 100.0).round() / 100.0).into());
        }
        arr.push(Value::Object(o));
    }
    out.insert("m".into(), Value::Array(arr));
    if let Some(t) = m["totals"]["tokens"].as_f64() {
        out.insert("tt".into(), t.into());
    }
    if let Some(u) = m["totals"]["usd"].as_f64() {
        out.insert("tu".into(), ((u * 100.0).round() / 100.0).into());
    }
    if let Some(s) = m["totals"]["sessions"].as_i64() {
        out.insert("ts".into(), s.into());
    }
    if m["usdComplete"].as_bool().unwrap_or(false) {
        out.insert("uc".into(), true.into());
    }
    Value::Object(out)
}

/// Round to 3 decimals so fractions stay short on the wire (0.423 not 0.42318).
fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Compact per-provider usage object with short keys. Fields are omitted when
/// at their sentinel value so a quiet provider collapses to a few bytes — the
/// firmware defaults any absent key. Keeps the whole payload under the ESP32
/// UART FIFO limit even with both providers active.
///   p=pct(0..1) r=resetSec wp=weekPct wr=weekResetSec
///   we=willExhaust b=burnPerHr lo=leftoverPct e=etaClock
fn provider_mini(pv: &Value) -> Value {
    let mut m = serde_json::Map::new();
    let ok = pv["ok"].as_bool().unwrap_or(false);
    m.insert("ok".into(), ok.into());
    if !ok {
        return Value::Object(m);
    }

    m.insert("p".into(), round3(pv["pct"].as_f64().unwrap_or(0.0)).into());
    m.insert("r".into(), pv["resetSec"].as_i64().unwrap_or(0).into());

    let wp = pv["weekPct"].as_f64().unwrap_or(-1.0);
    if wp >= 0.0 {
        m.insert("wp".into(), round3(wp).into());
    }
    let wr = pv["weekResetSec"].as_i64().unwrap_or(-1);
    if wr >= 0 {
        m.insert("wr".into(), wr.into());
    }
    if pv["willExhaustBeforeReset"].as_bool().unwrap_or(false) {
        m.insert("we".into(), true.into());
    }
    let b = pv["burnPerHr"].as_f64().unwrap_or(0.0);
    if b > 0.0 {
        m.insert("b".into(), round3(b).into());
    }
    let lo = pv["leftoverPct"].as_f64().unwrap_or(-1.0);
    if lo >= 0.0 {
        m.insert("lo".into(), round3(lo).into());
    }
    if let Some(e) = pv["etaClock"].as_str() {
        if !e.is_empty() {
            m.insert("e".into(), e.into());
        }
    }
    Value::Object(m)
}

/// Strips whitespace from JSON without a full parser (the bridge already
/// produces compact JSON, but belt-and-suspenders).
fn compact_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escape = false;
    for c in s.chars() {
        if escape {
            out.push(c);
            escape = false;
            continue;
        }
        if c == '\\' && in_string {
            out.push(c);
            escape = true;
            continue;
        }
        if c == '"' {
            in_string = !in_string;
        }
        if !in_string && c.is_ascii_whitespace() {
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn f(v: &Value, k: &str) -> f64 {
        v[k].as_f64()
            .unwrap_or_else(|| panic!("key {k} missing or not a number"))
    }

    // ── parse_args ────────────────────────────────────────────────────────
    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn no_arguments_means_auto_detect_everything() {
        assert_eq!(parse_args(&[]), Ok(Args::default()));
    }

    #[test]
    fn all_flags_are_parsed() {
        let a = parse_args(&argv(&[
            "--port",
            "COM9",
            "--url",
            "http://h:1",
            "--config",
            "c.toml",
            "--token",
            "t",
        ]))
        .unwrap();
        assert_eq!(a.port.as_deref(), Some("COM9"));
        assert_eq!(a.url.as_deref(), Some("http://h:1"));
        assert_eq!(a.config.as_deref(), Some("c.toml"));
        assert_eq!(a.token.as_deref(), Some("t"));
        assert!(!a.help);
    }

    #[test]
    fn a_flag_without_its_value_is_an_error_not_a_panic() {
        // Used to index past the end of argv.
        assert_eq!(
            parse_args(&argv(&["--port"])),
            Err("--port needs a value".to_string())
        );
        // The next flag is not a value.
        assert_eq!(
            parse_args(&argv(&["--port", "--url", "http://h:1"])),
            Err("--port needs a value".to_string())
        );
    }

    #[test]
    fn an_unknown_flag_is_an_error_not_silently_ignored() {
        // A typo used to fall through to the COM7 default without a word.
        assert_eq!(
            parse_args(&argv(&["--prot", "/dev/ttyUSB0"])),
            Err("unknown argument: --prot".to_string())
        );
    }

    #[test]
    fn help_flags_are_recognised() {
        assert!(parse_args(&argv(&["--help"])).unwrap().help);
        assert!(parse_args(&argv(&["-h"])).unwrap().help);
    }

    // ── describe_open_error ───────────────────────────────────────────────
    #[test]
    fn permission_denied_on_linux_names_the_dialout_fix() {
        let kind = serialport::ErrorKind::Io(std::io::ErrorKind::PermissionDenied);
        let msg = describe_open_error("/dev/ttyUSB0", &kind, "linux");
        assert!(msg.contains("dialout"), "{msg}");
        assert!(msg.contains("usermod -aG dialout"), "{msg}");
    }

    #[test]
    fn permission_denied_elsewhere_means_the_port_is_in_use() {
        let kind = serialport::ErrorKind::Io(std::io::ErrorKind::PermissionDenied);
        let msg = describe_open_error("COM4", &kind, "windows");
        assert!(msg.contains("another program"), "{msg}");
        assert!(!msg.contains("dialout"), "{msg}");
    }

    #[test]
    fn a_missing_device_is_reported_as_unplugged() {
        let msg = describe_open_error("COM4", &serialport::ErrorKind::NoDevice, "windows");
        assert!(msg.contains("not present"), "{msg}");
    }

    // ── extract_ack_id ────────────────────────────────────────────────────
    #[test]
    fn extract_ack_id_finds_the_id() {
        assert_eq!(extract_ack_id(r#"{"ack":"abc123"}"#), Some("abc123"));
    }

    #[test]
    fn extract_ack_id_absent_is_none() {
        assert_eq!(extract_ack_id(r#"{"log":"hello"}"#), None);
    }

    #[test]
    fn extract_ack_id_empty_id_is_some_empty() {
        assert_eq!(extract_ack_id(r#"{"ack":""}"#), Some(""));
    }

    #[test]
    fn extract_ack_id_rejects_oversized_ids() {
        let huge = format!(r#"{{"ack":"{}"}}"#, "x".repeat(MAX_ACK_ID_LEN + 1));
        assert_eq!(extract_ack_id(&huge), None);
        // at the boundary it still parses
        let max = format!(r#"{{"ack":"{}"}}"#, "x".repeat(MAX_ACK_ID_LEN));
        assert!(extract_ack_id(&max).is_some());
    }

    // ── round3 ────────────────────────────────────────────────────────────
    #[test]
    fn round3_keeps_three_decimals() {
        assert!((round3(0.123456) - 0.123).abs() < 1e-9);
        assert!((round3(0.123800) - 0.124).abs() < 1e-9);
        assert!((round3(0.9999) - 1.0).abs() < 1e-9);
        assert!((round3(0.0) - 0.0).abs() < 1e-9);
    }

    // ── provider_mini ─────────────────────────────────────────────────────
    #[test]
    fn provider_mini_not_ok_collapses_to_ok_false() {
        // Even with data present, a not-ok provider emits only {"ok":false}.
        let out = provider_mini(&json!({"ok": false, "pct": 0.5}));
        assert_eq!(out, json!({"ok": false}));
    }

    #[test]
    fn provider_mini_omits_fields_at_sentinel_values() {
        // weekPct/weekResetSec/leftoverPct = -1, burn = 0, eta = "" → all omitted.
        let out = provider_mini(&json!({
            "ok": true,
            "pct": 0.42049,
            "resetSec": 3600,
            "weekPct": -1.0,
            "weekResetSec": -1,
            "willExhaustBeforeReset": false,
            "burnPerHr": 0.0,
            "leftoverPct": -1.0,
            "etaClock": ""
        }));
        assert_eq!(out["ok"], json!(true));
        assert!((f(&out, "p") - 0.420).abs() < 1e-9); // rounded to 3 dp
        assert_eq!(out["r"], json!(3600));
        for k in ["wp", "wr", "we", "b", "lo", "e"] {
            assert!(
                out.get(k).is_none(),
                "expected {k} to be omitted at sentinel"
            );
        }
    }

    #[test]
    fn provider_mini_includes_optional_fields_when_present() {
        let out = provider_mini(&json!({
            "ok": true,
            "pct": 0.1,
            "resetSec": 60,
            "weekPct": 0.5,
            "weekResetSec": 100,
            "willExhaustBeforeReset": true,
            "burnPerHr": 12.3456,
            "leftoverPct": 0.25,
            "etaClock": "14:30"
        }));
        assert!((f(&out, "wp") - 0.5).abs() < 1e-9);
        assert_eq!(out["wr"], json!(100));
        assert_eq!(out["we"], json!(true));
        assert!((f(&out, "b") - 12.346).abs() < 1e-9); // 12.3456 → 12.346
        assert!((f(&out, "lo") - 0.25).abs() < 1e-9);
        assert_eq!(out["e"], json!("14:30"));
    }

    // ── compact_json ──────────────────────────────────────────────────────
    #[test]
    fn compact_json_strips_outside_strings() {
        assert_eq!(
            compact_json("{ \"a\" : 1 , \"b\" : 2 }"),
            r#"{"a":1,"b":2}"#
        );
    }

    #[test]
    fn compact_json_preserves_whitespace_inside_strings() {
        assert_eq!(compact_json(r#"{"m":"x  y"}"#), r#"{"m":"x  y"}"#);
    }

    #[test]
    fn compact_json_preserves_escaped_quotes() {
        let input = r#"{ "m" : "he said \"hi\"" }"#;
        assert_eq!(compact_json(input), r#"{"m":"he said \"hi\""}"#);
    }
}
