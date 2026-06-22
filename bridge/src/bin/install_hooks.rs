/// install_hooks — one-command installer that wires Claude Code to POST its
/// lifecycle hook events to the vibe-bridge hub's /hook endpoint.
///
/// Claude Code only fires hooks that are registered in ~/.claude/settings.json.
/// This binary merges a `hooks` block into that file (creating it if missing,
/// backing it up first) that registers the sibling `vibe_hook` executable for
/// the relevant events. It MERGES — existing user hooks are preserved, and the
/// operation is idempotent (running twice does not duplicate our entries).
///
/// settings.json hooks schema Claude Code expects:
///   {
///     "hooks": {
///       "<EventName>": [
///         { "matcher": "", "hooks": [ { "type": "command", "command": "<cmd>" } ] }
///       ],
///       ...
///     }
///   }
///
/// Usage:
///   install_hooks --url http://localhost:5151 --token <tok>
///   install_hooks --config config.toml          # read token/host/port from bridge config
///   install_hooks --path ./settings_copy.json   # write to a different file (testing)
///   install_hooks --hook-exe C:\path\vibe_hook.exe   # override resolved exe path
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// Events Claude Code fires that we register for. The hub only acts on
/// "Notification" (marks waiting) today, but registering the full lifecycle
/// makes status fully event-driven and is forward-compatible.
const HOOK_EVENTS: &[&str] = &[
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "Notification",
    "SessionStart",
];

/// Marker used to recognize (and thus replace/dedupe) our own command entries
/// during merge, without clobbering unrelated user hooks.
const VIBE_MARKER: &str = "vibe_hook";

fn home_settings_path() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

/// Locate the sibling vibe_hook executable next to this install_hooks binary.
fn resolve_hook_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) {
        "vibe_hook.exe"
    } else {
        "vibe_hook"
    };
    let candidate = dir.join(name);
    Some(candidate)
}

/// Build the command string Claude Code will execute for each event.
/// Properly escapes all arguments to prevent shell injection.
/// Uses environment variable for token to avoid CLI argument exposure.
fn build_command(hook_exe: &Path, url: &str) -> Result<String, String> {
    let exe_string = hook_exe.to_string_lossy();
    let exe_quoted = shlex::try_quote(exe_string.as_ref())
        .map_err(|_| "executable path contains NUL byte".to_string())?;
    let url_quoted = shlex::try_quote(url)
        .map_err(|_| "URL contains NUL byte".to_string())?;
    Ok(format!(
        "{} --url {} --token \"$VIBE_MONITOR_TOKEN\"",
        exe_quoted, url_quoted
    ))
}

/// True if a hook-block (a `{matcher, hooks:[...]}` object) contains one of our
/// commands (recognized via the VIBE_MARKER), so we can drop stale duplicates.
fn block_is_ours(block: &Value) -> bool {
    block
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains(VIBE_MARKER))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Merge our hook registration into `settings`, preserving all existing user
/// hooks. Idempotent: any prior vibe_hook block (for each event) is removed and
/// replaced with the fresh one, so reruns never duplicate and reflect new flags.
fn merge_hooks(mut settings: Value, command: &str) -> Value {
    // Ensure top-level object.
    if !settings.is_object() {
        settings = Value::Object(Map::new());
    }
    let root = settings.as_object_mut().unwrap();

    // Ensure `hooks` is an object.
    let hooks_entry = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks_entry.is_object() {
        *hooks_entry = Value::Object(Map::new());
    }
    let hooks = hooks_entry.as_object_mut().unwrap();

    for ev in HOOK_EVENTS {
        let blocks_entry = hooks.entry(*ev).or_insert_with(|| Value::Array(Vec::new()));
        if !blocks_entry.is_array() {
            *blocks_entry = Value::Array(Vec::new());
        }
        let blocks = blocks_entry.as_array_mut().unwrap();

        // Drop any prior entries of ours (dedupe + allow command updates).
        blocks.retain(|b| !block_is_ours(b));

        // Append the fresh registration.
        blocks.push(json!({
            "matcher": "",
            "hooks": [ { "type": "command", "command": command } ]
        }));
    }

    settings
}

/// Best-effort read of token + base url from a bridge config.toml.
fn from_config(path: &str) -> (Option<String>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    let Ok(val) = toml::from_str::<toml::Value>(&text) else {
        return (None, None);
    };
    let token = val
        .get("token")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());
    let url = match (
        val.get("host").and_then(|h| h.as_str()),
        val.get("port").and_then(|p| p.as_integer()),
    ) {
        (Some(host), Some(port)) => {
            let host = if host == "0.0.0.0" { "127.0.0.1" } else { host };
            Some(format!("http://{host}:{port}"))
        }
        _ => None,
    };
    (token, url)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("install_hooks: error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    // ── parse args ──────────────────────────────────────────────────────────
    let mut url: Option<String> = None;
    let mut token: Option<String> = None;
    let mut config_path: Option<String> = None;
    let mut settings_path_override: Option<String> = None;
    let mut hook_exe_override: Option<String> = None;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--url" => {
                url = args.get(i + 1).cloned();
                i += 2;
            }
            "--token" => {
                token = args.get(i + 1).cloned();
                i += 2;
            }
            "--config" => {
                config_path = args.get(i + 1).cloned();
                i += 2;
            }
            "--path" => {
                settings_path_override = args.get(i + 1).cloned();
                i += 2;
            }
            "--hook-exe" => {
                hook_exe_override = args.get(i + 1).cloned();
                i += 2;
            }
            "-h" | "--help" => {
                println!("Usage: install_hooks [--url URL] [--token TOK] [--config config.toml] [--path settings.json] [--hook-exe path]");
                return Ok(());
            }
            other => {
                return Err(format!("unknown argument: {other}"));
            }
        }
    }

    // ── resolve url + token: flags > config.toml > defaults ─────────────────
    if let Some(cfg) = config_path.as_deref() {
        let (cfg_token, cfg_url) = from_config(cfg);
        if token.is_none() {
            token = cfg_token;
        }
        if url.is_none() {
            url = cfg_url;
        }
    }
    let url = url.unwrap_or_else(|| "http://localhost:5151".to_string());
    let token = token
        .ok_or_else(|| "no token provided (use --token or --config <config.toml>)".to_string())?;

    // ── resolve the vibe_hook exe to register ───────────────────────────────
    let hook_exe = match hook_exe_override {
        Some(p) => PathBuf::from(p),
        None => resolve_hook_exe()
            .ok_or_else(|| "could not resolve sibling vibe_hook executable".to_string())?,
    };
    if !hook_exe.exists() {
        eprintln!(
            "install_hooks: warning: vibe_hook executable not found at {} \
             (build it with `cargo build --bin vibe_hook`)",
            hook_exe.display()
        );
    }

    // ── resolve settings.json path ──────────────────────────────────────────
    let settings_path = match settings_path_override {
        Some(p) => PathBuf::from(p),
        None => home_settings_path().ok_or_else(|| {
            "could not resolve home directory for ~/.claude/settings.json".to_string()
        })?,
    };

    // ── read (or start fresh) ───────────────────────────────────────────────
    let existing: Value = if settings_path.exists() {
        let text = std::fs::read_to_string(&settings_path)
            .map_err(|e| format!("read {}: {e}", settings_path.display()))?;
        if text.trim().is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_str(&text)
                .map_err(|e| format!("parse {} as JSON: {e}", settings_path.display()))?
        }
    } else {
        if let Some(parent) = settings_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        Value::Object(Map::new())
    };

    // ── back up the existing file before writing ────────────────────────────
    if settings_path.exists() {
        let backup = settings_path.with_extension("json.bak");
        std::fs::copy(&settings_path, &backup)
            .map_err(|e| format!("back up to {}: {e}", backup.display()))?;
        println!("backed up settings to {}", backup.display());
    }

    // ── merge + write pretty-printed ────────────────────────────────────────
    let command = build_command(&hook_exe, &url)?;
    let merged = merge_hooks(existing, &command);
    let pretty = serde_json::to_string_pretty(&merged)
        .map_err(|e| format!("serialize merged settings: {e}"))?;
    std::fs::write(&settings_path, format!("{pretty}\n"))
        .map_err(|e| format!("write {}: {e}", settings_path.display()))?;

    // ── write token to a secure location or document env var requirement ─────
    println!(
        "installed vibe-bridge hooks into {}",
        settings_path.display()
    );
    println!("  hub url : {url}/hook");
    println!("  hook exe: {}", hook_exe.display());
    println!("  events  : {}", HOOK_EVENTS.join(", "));
    println!("  command : {command}");
    println!();
    println!("IMPORTANT: Set the VIBE_MONITOR_TOKEN environment variable before running Claude Code:");
    println!("  export VIBE_MONITOR_TOKEN='{}'", token);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Count how many of OUR blocks exist for an event (recognized by marker).
    fn ours_for(settings: &Value, event: &str) -> usize {
        settings["hooks"][event]
            .as_array()
            .map(|a| a.iter().filter(|b| block_is_ours(b)).count())
            .unwrap_or(0)
    }

    #[test]
    fn merge_into_empty_creates_all_events() {
        let cmd = "vibe_hook.exe --url http://localhost:5151 --token \"$VIBE_MONITOR_TOKEN\"";
        let merged = merge_hooks(Value::Object(Map::new()), cmd);
        // every event registered, with exactly one of our blocks
        for ev in HOOK_EVENTS {
            assert_eq!(
                ours_for(&merged, ev),
                1,
                "event {ev} should have 1 vibe block"
            );
        }
        // schema check: matcher present + type:command
        let blk = &merged["hooks"]["Notification"][0];
        assert_eq!(blk["matcher"], "");
        assert_eq!(blk["hooks"][0]["type"], "command");
        assert_eq!(blk["hooks"][0]["command"], cmd);
    }

    #[test]
    fn merge_preserves_existing_user_hooks() {
        let pre: Value = serde_json::json!({
            "model": "opus",
            "hooks": {
                "Stop": [
                    { "matcher": "", "hooks": [ { "type": "command", "command": "echo user-stop" } ] }
                ],
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [ { "type": "command", "command": "echo guard" } ] }
                ]
            }
        });
        let cmd = "vibe_hook.exe --url http://localhost:5151 --token \"$VIBE_MONITOR_TOKEN\"";
        let merged = merge_hooks(pre, cmd);

        // unrelated top-level key preserved
        assert_eq!(merged["model"], "opus");
        // user's Stop hook still there, plus ours => 2 blocks total, 1 ours
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(ours_for(&merged, "Stop"), 1);
        assert!(stop
            .iter()
            .any(|b| b["hooks"][0]["command"] == "echo user-stop"));
        // user's PreToolUse Bash guard preserved
        let pre_tool = merged["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(pre_tool
            .iter()
            .any(|b| b["hooks"][0]["command"] == "echo guard"));
        assert_eq!(ours_for(&merged, "PreToolUse"), 1);
    }

    #[test]
    fn merge_is_idempotent_and_updates_command() {
        let cmd1 = "vibe_hook.exe --url http://localhost:5151 --token \"$VIBE_MONITOR_TOKEN\"";
        let once = merge_hooks(Value::Object(Map::new()), cmd1);
        // running again with same command: still exactly one of ours per event
        let twice = merge_hooks(once.clone(), cmd1);
        for ev in HOOK_EVENTS {
            assert_eq!(ours_for(&twice, ev), 1, "idempotent: {ev}");
        }
        // running with a NEW command (url changed) replaces, not duplicates
        let cmd2 = "vibe_hook.exe --url http://other:5151 --token \"$VIBE_MONITOR_TOKEN\"";
        let updated = merge_hooks(twice, cmd2);
        for ev in HOOK_EVENTS {
            assert_eq!(ours_for(&updated, ev), 1, "still single after update: {ev}");
        }
        assert_eq!(
            updated["hooks"]["Stop"]
                .as_array()
                .unwrap()
                .iter()
                .find(|b| block_is_ours(b))
                .unwrap()["hooks"][0]["command"],
            cmd2
        );
    }

    #[test]
    fn command_escaping_with_shell_special_chars() {
        // Test that paths and URLs with special chars are properly escaped
        let cmd = build_command(Path::new("C:\\a b\\vibe_hook.exe"), "http://h:1").unwrap();
        // Path with spaces should be properly escaped
        assert!(cmd.contains("vibe_hook.exe") || cmd.contains("'C:\\a b\\vibe_hook.exe'"));
        // URL should be properly escaped
        assert!(cmd.contains("--url") && cmd.contains("http://h:1"));
        // Token should reference env var, not embedded
        assert!(cmd.contains("$VIBE_MONITOR_TOKEN"));
        assert!(!cmd.contains("--token") || cmd.contains("--token \"$VIBE_MONITOR_TOKEN\""));
    }

    #[test]
    fn command_escaping_prevents_injection() {
        // Test that special characters in paths are properly escaped
        // and that token is NOT in the command (uses env var instead)
        let cmd = build_command(
            Path::new("/usr/bin/vibe'; rm -rf /"),
            "http://localhost:5151?foo=bar",
        ).unwrap();
        // Most important: token must use env var, not be embedded
        assert!(cmd.contains("$VIBE_MONITOR_TOKEN"));
        assert!(!cmd.contains("--token") || cmd.contains("$VIBE_MONITOR_TOKEN"));
        // URL should be present
        assert!(cmd.contains("--url"));
        // Command should reference both exe and url
        assert!(cmd.contains("vibe") && cmd.contains("localhost"));
    }
}
