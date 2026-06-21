/// USB serial bridge for vibe-firmware (USB transport mode).
///
/// Runs alongside the vibe-bridge HTTP server.  Every POLL_SECS it:
///   1. GETs /state from the bridge
///   2. Writes the JSON as a single newline-terminated line to the CYD serial port
///
/// ACK lines sent back by the CYD (`{"ack":"<id>"}`) are forwarded via POST /ack
/// to the bridge so sessions clear their "waiting" state.
///
/// Usage:
///   cargo run --bin serial_bridge -- --port COM7
///   cargo run --bin serial_bridge -- --port COM7 --url http://localhost:5151 --token <tok>

use std::{
    io::{Read, Write},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serialport::SerialPort;
use serde_json::Value;

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

/// Open COM port with the CYD-safe DTR/RTS handling (lowered so opening the
/// port doesn't reset the ESP32). Returns None if the device isn't present.
fn open_port(port_name: &str) -> Option<Box<dyn SerialPort>> {
    match serialport::new(port_name, BAUD)
        .timeout(Duration::from_millis(100))
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .open()
    {
        Ok(mut p) => {
            let _ = p.write_data_terminal_ready(false);
            let _ = p.write_request_to_send(false);
            Some(p)
        }
        Err(_) => None,
    }
}

fn extract_ack_id(s: &str) -> Option<&str> {
    // minimal: find "ack":"<id>"
    let key = "\"ack\":\"";
    let start = s.find(key)? + key.len();
    let end = s[start..].find('"')? + start;
    Some(&s[start..end])
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut port_name = "COM7".to_string();
    let mut bridge_url = "http://localhost:5151".to_string();
    let mut token: Option<String> = None;
    let mut config_path = "config.toml".to_string();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port"   => { i += 1; port_name   = args[i].clone(); }
            "--url"    => { i += 1; bridge_url  = args[i].clone(); }
            "--token"  => { i += 1; token = Some(args[i].clone()); }
            "--config" => { i += 1; config_path = args[i].clone(); }
            _ => {}
        }
        i += 1;
    }

    // resolve token: --token flag > config.toml
    let token = match token {
        Some(t) => t,
        None => {
            let content = std::fs::read_to_string(&config_path)
                .with_context(|| format!("reading {config_path}"))?;
            let parsed: toml::Value = content.parse().context("parse config.toml")?;
            parsed["token"].as_str()
                .with_context(|| "token not found in config.toml")?
                .to_string()
        }
    };

    println!("[serial_bridge] {port_name} @ {BAUD}, bridge {bridge_url}");

    // Reconnect loop: survives the device being unplugged/replugged. On any
    // port I/O error we drop the handle and re-open COM port until it returns.
    let mut rx: Vec<u8> = Vec::new();
    'reconnect: loop {
        let mut port = match open_port(&port_name) {
            Some(p) => { println!("[serial_bridge] connected to {port_name}"); p }
            None => {
                eprintln!("[serial_bridge] {port_name} unavailable; retrying in 2s...");
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
                            continue 'reconnect;        // device gone → re-open
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
                            if l.is_empty() { continue; }
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
                    if rx.len() > 8192 { rx.clear(); }   // guard against runaway
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}   // normal idle
                Err(e) => {
                    eprintln!("[serial_bridge] read error ({e}); reconnecting...");
                    continue 'reconnect;                 // device gone → re-open
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
            let status  = s["status"].as_str()?;
            let tool    = s["tool"].as_str().unwrap_or("claude");
            let mut o = serde_json::Map::new();
            o.insert("project".into(), project.into());
            o.insert("status".into(),  status.into());
            o.insert("tool".into(),    tool.into());
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
        .take(6)            // device shows at most 6 cards; bounds the UART payload
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
                    if tk <= 0.0 { continue; }
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
        if let Some(u) = usd { o.insert("u".into(), ((u * 100.0).round() / 100.0).into()); }
        arr.push(Value::Object(o));
    }
    out.insert("m".into(), Value::Array(arr));
    if let Some(t) = m["totals"]["tokens"].as_f64() { out.insert("tt".into(), t.into()); }
    if let Some(u) = m["totals"]["usd"].as_f64() {
        out.insert("tu".into(), ((u * 100.0).round() / 100.0).into());
    }
    if let Some(s) = m["totals"]["sessions"].as_i64() { out.insert("ts".into(), s.into()); }
    if m["usdComplete"].as_bool().unwrap_or(false) { out.insert("uc".into(), true.into()); }
    Value::Object(out)
}

/// Round to 3 decimals so fractions stay short on the wire (0.423 not 0.42318).
fn round3(x: f64) -> f64 { (x * 1000.0).round() / 1000.0 }

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
    if !ok { return Value::Object(m); }

    m.insert("p".into(), round3(pv["pct"].as_f64().unwrap_or(0.0)).into());
    m.insert("r".into(), pv["resetSec"].as_i64().unwrap_or(0).into());

    let wp = pv["weekPct"].as_f64().unwrap_or(-1.0);
    if wp >= 0.0 { m.insert("wp".into(), round3(wp).into()); }
    let wr = pv["weekResetSec"].as_i64().unwrap_or(-1);
    if wr >= 0 { m.insert("wr".into(), wr.into()); }
    if pv["willExhaustBeforeReset"].as_bool().unwrap_or(false) {
        m.insert("we".into(), true.into());
    }
    let b = pv["burnPerHr"].as_f64().unwrap_or(0.0);
    if b > 0.0 { m.insert("b".into(), round3(b).into()); }
    let lo = pv["leftoverPct"].as_f64().unwrap_or(-1.0);
    if lo >= 0.0 { m.insert("lo".into(), round3(lo).into()); }
    if let Some(e) = pv["etaClock"].as_str() {
        if !e.is_empty() { m.insert("e".into(), e.into()); }
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
        if escape { out.push(c); escape = false; continue; }
        if c == '\\' && in_string { out.push(c); escape = true; continue; }
        if c == '"' { in_string = !in_string; }
        if !in_string && c.is_ascii_whitespace() { continue; }
        out.push(c);
    }
    out
}
