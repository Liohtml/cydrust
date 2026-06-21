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
    io::{BufRead, BufReader, Write},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
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

fn ack_reader(port: Arc<Mutex<Box<dyn SerialPort>>>, bridge_url: String, token: String) {
    thread::spawn(move || {
        // Clone port reference for the reader side
        let port_clone = match port.lock().unwrap().try_clone() {
            Ok(p) => p,
            Err(e) => { eprintln!("[serial_bridge] port clone failed: {e}"); return; }
        };
        let reader = BufReader::new(port_clone);
        for line in reader.lines() {
            match line {
                Err(e) => eprintln!("[serial_bridge] read error: {e}"),
                Ok(l) => {
                    let l = l.trim().to_string();
                    if l.is_empty() { continue; }
                    // parse {"ack":"<id>"}
                    if let Some(id) = extract_ack_id(&l) {
                        match post_ack(&bridge_url, &token, id) {
                            Ok(_) => println!("[serial_bridge] ack forwarded: {id}"),
                            Err(e) => eprintln!("[serial_bridge] ack failed: {e}"),
                        }
                    } else {
                        // log line from firmware — just print it
                        println!("[cyd] {l}");
                    }
                }
            }
        }
    });
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

    let mut port: Box<dyn SerialPort> = serialport::new(&port_name, BAUD)
        .timeout(Duration::from_millis(100))
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .open()
        .with_context(|| format!("opening {port_name}"))?;

    // CH340 on CYD wires DTR→GPIO0 and RTS→EN (reset) via capacitors.
    // Explicitly lower both so opening the port doesn't reset the ESP32.
    let _ = port.write_data_terminal_ready(false);
    let _ = port.write_request_to_send(false);

    let port = Arc::new(Mutex::new(port));

    // start ACK reader thread
    ack_reader(port.clone(), bridge_url.clone(), token.clone());

    // main push loop
    loop {
        match get_state(&bridge_url, &token) {
            Err(e) => eprintln!("[serial_bridge] /state error: {e}"),
            Ok(json) => {
                // send only what the CYD firmware needs — full payload is ~700 bytes
                // which overflows the ESP32 UART FIFO (128 bytes); mini is ~80 bytes.
                let line = make_mini(&json) + "\n";
                let mut p = port.lock().unwrap();
                if let Err(e) = p.write_all(line.as_bytes()) {
                    eprintln!("[serial_bridge] write error: {e}");
                }
            }
        }
        thread::sleep(Duration::from_secs(POLL_SECS));
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
            Some(serde_json::json!({"project": project, "status": status, "tool": tool}))
        })
        .collect();

    // firmware reads pct as 0..1 fraction then × 100
    let claude_pct = v["usage"]["claude"]["pct"].as_f64().unwrap_or(0.0);
    // codex: send null when provider is inactive (ok=false)
    let codex_pct: Value = if v["usage"]["codex"]["ok"].as_bool().unwrap_or(false) {
        v["usage"]["codex"]["pct"].clone()
    } else {
        Value::Null
    };

    serde_json::json!({
        "sessions": sessions,
        "claude": { "pct": claude_pct },
        "codex":  { "pct": codex_pct  }
    })
    .to_string()
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
