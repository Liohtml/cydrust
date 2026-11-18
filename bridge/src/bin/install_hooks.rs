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
use vibe_bridge::config;

/// Events Claude Code fires that we register for. Only events the hub
/// actually evaluates: "Notification" marks a session waiting, "Stop"
/// clears it. Registering more (PreToolUse/PostToolUse/...) would fire a
/// vibe_hook POST on every tool call for no effect.
const HOOK_EVENTS: &[&str] = &["Notification", "Stop"];

/// Marker used to recognize (and thus replace/dedupe) our own command entries
/// during merge, without clobbering unrelated user hooks.
const VIBE_MARKER: &str = "vibe_hook";

fn home_settings_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("settings.json"))
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

/// Where the registered hook gets its token from.
enum TokenSource<'a> {
    /// `--config <path>`: vibe_hook reads the token from the hub's config
    /// file. Nothing to export, works from any shell, IDE or launcher.
    Config(&'a Path),
    /// `--token "$VIBE_MONITOR_TOKEN"`: the pre-config-discovery scheme, kept
    /// for setups that have a token but no config file on this machine.
    EnvVar,
}

/// Build the hook command line. Every interpolated value is shell-quoted; the
/// token itself is never written into settings.json.
fn build_command(hook_exe: &Path, url: &str, token: TokenSource) -> Result<String, String> {
    let quote = |s: &str, what: &str| {
        shlex::try_quote(s)
            .map(|q| q.into_owned())
            .map_err(|_| format!("{what} contains NUL byte"))
    };
    let exe_quoted = quote(hook_exe.to_string_lossy().as_ref(), "executable path")?;
    let url_quoted = quote(url, "URL")?;
    Ok(match token {
        TokenSource::Config(path) => {
            let cfg_quoted = quote(path.to_string_lossy().as_ref(), "config path")?;
            format!("{exe_quoted} --url {url_quoted} --config {cfg_quoted}")
        }
        TokenSource::EnvVar => {
            format!("{exe_quoted} --url {url_quoted} --token \"$VIBE_MONITOR_TOKEN\"")
        }
    })
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

    // Strip our stale registrations from events we no longer subscribe to
    // (earlier versions registered the full lifecycle). User hooks under
    // those events are untouched; empty arrays we emptied are removed.
    let stale: Vec<String> = hooks
        .iter()
        .filter(|(ev, _)| !HOOK_EVENTS.contains(&ev.as_str()))
        .map(|(ev, _)| ev.clone())
        .collect();
    for ev in stale {
        if let Some(blocks) = hooks.get_mut(&ev).and_then(|v| v.as_array_mut()) {
            blocks.retain(|b| !block_is_ours(b));
            if blocks.is_empty() {
                hooks.remove(&ev);
            }
        }
    }

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
                println!("Usage: install_hooks [--url URL] [--config config.toml] [--token TOK] [--path settings.json] [--hook-exe path]\n\nWith no arguments, uses the config vibe-bridge created on first run.");
                return Ok(());
            }
            other => {
                return Err(format!("unknown argument: {other}"));
            }
        }
    }

    // ── resolve config file, url, token source ──────────────────────────────
    // Config: --config, else the shared lookup (./config.toml, then the
    // per-user config vibe-bridge creates on first run). Registered as an
    // absolute path, because hooks run with the project as working directory.
    let config_file = config::find(config_path.as_deref())
        .map_err(|e| e.to_string())?
        .map(|p| std::path::absolute(&p).unwrap_or(p));
    let cfg = config_file.as_deref().and_then(config::read_client);
    let cfg_has_token = cfg.as_ref().is_some_and(|c| c.token.is_some());
    let url = url
        .or_else(|| cfg.as_ref().map(|c| c.url.clone()))
        .unwrap_or_else(|| "http://localhost:5151".to_string());

    // An explicit --token keeps the env-var scheme (the caller is managing the
    // token themselves); otherwise the hook reads it from the config file.
    let token_source = match (&token, &config_file) {
        (None, Some(path)) if cfg_has_token => TokenSource::Config(path),
        (Some(_), _) => TokenSource::EnvVar,
        (None, Some(path)) => {
            return Err(format!("{} has no token", path.display()));
        }
        (None, None) => {
            return Err(
                "no hub config found. Start vibe-bridge once (it creates one with a \
                 random token), or pass --config <config.toml> or --token <TOK>"
                    .to_string(),
            );
        }
    };

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
    let uses_env_var = matches!(token_source, TokenSource::EnvVar);
    let command = build_command(&hook_exe, &url, token_source)?;
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
    if uses_env_var {
        println!();
        println!("This registration reads the token from the environment. Before starting");
        println!("Claude Code, in the same shell:");
        println!(
            "  export VIBE_MONITOR_TOKEN='{}'",
            token.unwrap_or_default()
        );
    } else if let Some(path) = &config_file {
        println!("  token   : read from {}", path.display());
        println!();
        println!("Nothing else to do — restart Claude Code to pick up the hooks.");
    }
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
        // user's PreToolUse Bash guard preserved — and no vibe block added,
        // since PreToolUse is not an event the hub evaluates
        let pre_tool = merged["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(pre_tool
            .iter()
            .any(|b| b["hooks"][0]["command"] == "echo guard"));
        assert_eq!(ours_for(&merged, "PreToolUse"), 0);
    }

    #[test]
    fn merge_removes_stale_registrations_from_dropped_events() {
        // Simulate a settings.json written by an older install_hooks that
        // registered the full lifecycle, plus one user hook we must keep.
        let cmd = "vibe_hook.exe --url http://localhost:5151 --token \"$VIBE_MONITOR_TOKEN\"";
        let old: Value = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "", "hooks": [ { "type": "command", "command": cmd } ] },
                    { "matcher": "Bash", "hooks": [ { "type": "command", "command": "echo guard" } ] }
                ],
                "SessionStart": [
                    { "matcher": "", "hooks": [ { "type": "command", "command": cmd } ] }
                ]
            }
        });
        let merged = merge_hooks(old, cmd);

        // ours removed from dropped events, user hook kept
        assert_eq!(ours_for(&merged, "PreToolUse"), 0);
        let pre_tool = merged["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(pre_tool
            .iter()
            .any(|b| b["hooks"][0]["command"] == "echo guard"));
        // event arrays left empty by the cleanup disappear entirely
        assert!(merged["hooks"].get("SessionStart").is_none());
        // current events registered exactly once
        for ev in HOOK_EVENTS {
            assert_eq!(ours_for(&merged, ev), 1);
        }
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
        let cmd = build_command(
            Path::new("C:\\a b\\vibe_hook.exe"),
            "http://h:1",
            TokenSource::EnvVar,
        )
        .unwrap();
        // Path with spaces should produce some form of quoting (shlex handles platform-specific escaping)
        // Just verify the path and space are present and the command is structurally sound
        assert!(
            cmd.contains("vibe_hook.exe"),
            "executable name should be in command"
        );
        assert!(
            cmd.contains("a b"),
            "spaced path component should be present"
        );
        // URL should be present and properly handled
        assert!(cmd.contains("--url") && cmd.contains("http://h:1"));
        // Token should reference env var, not embedded
        assert!(cmd.contains("$VIBE_MONITOR_TOKEN"));
        assert!(cmd.contains("--token \"$VIBE_MONITOR_TOKEN\""));
    }

    #[test]
    fn command_escaping_prevents_injection() {
        // Test that special characters in paths are properly escaped
        // and that token is NOT in the command (uses env var instead)
        let cmd = build_command(
            Path::new("/usr/bin/vibe'; rm -rf /"),
            "http://localhost:5151?foo=bar",
            TokenSource::EnvVar,
        )
        .unwrap();
        // Most important: token must use env var, not be embedded literal
        assert!(cmd.contains("$VIBE_MONITOR_TOKEN"));
        assert!(cmd.contains("--token \"$VIBE_MONITOR_TOKEN\""));
        // Verify the command structure is intact
        assert!(cmd.contains("--url"));
        assert!(cmd.contains("vibe"));
        // The injection payload (path with ; rm -rf) should be quoted.
        // When quoted, the semicolon becomes a literal character, not a command separator.
        // Verify it's quoted (either single or double quotes contain the dangerous parts)
        let path_quoted = cmd.contains("\"/usr/bin/vibe'; rm -rf /\"")
            || cmd.contains("'/usr/bin/vibe'\\'''; rm -rf /'");
        assert!(
            path_quoted,
            "injection path should be quoted to prevent execution in: {cmd}"
        );
    }

    #[test]
    fn config_mode_registers_the_config_path_and_no_token_or_env_var() {
        let cmd = build_command(
            Path::new("/opt/cydrust/vibe_hook"),
            "http://127.0.0.1:5151",
            TokenSource::Config(Path::new("/home/dev/.config/cydrust/config.toml")),
        )
        .unwrap();
        assert_eq!(
            cmd,
            "/opt/cydrust/vibe_hook --url http://127.0.0.1:5151 \
             --config /home/dev/.config/cydrust/config.toml"
        );
        assert!(!cmd.contains("--token"));
        assert!(!cmd.contains("VIBE_MONITOR_TOKEN"));
    }

    #[test]
    fn config_mode_quotes_a_config_path_with_spaces_and_metacharacters() {
        let cmd = build_command(
            Path::new("/opt/vibe_hook"),
            "http://h:1",
            TokenSource::Config(Path::new(
                "/Users/a b/Application Support/c; rm -rf ~/config.toml",
            )),
        )
        .unwrap();
        let parts = shlex::split(&cmd).expect("command must stay shell-parseable");
        assert_eq!(parts.len(), 5, "path must survive as ONE argument: {cmd}");
        assert_eq!(
            parts[4],
            "/Users/a b/Application Support/c; rm -rf ~/config.toml"
        );
    }
}
