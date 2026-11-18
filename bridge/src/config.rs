//! Shared config discovery for every binary in this crate.
//!
//! All four executables (`vibe-bridge`, `serial_bridge`, `install_hooks`,
//! `vibe_hook`) need the same two facts — the hub token and the hub address —
//! and used to each take them through their own flags. This module gives them
//! one lookup so a fresh install needs no flags, no hand-written file and no
//! exported environment variable.
//!
//! Lookup order (first existing file wins):
//!   1. an explicitly passed path — if it does not exist that is an error,
//!      never a silent fallback, so a typo cannot create a second config
//!   2. `./config.toml` — the pre-existing convention, kept so current setups
//!      behave exactly as before
//!   3. the per-user config file, see [`user_config_path`]
//!
//! `vibe-bridge` additionally *creates* (3) with a random token when nothing
//! is found; the other binaries only read.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// File name looked for in the working directory and the per-user directory.
pub const CONFIG_FILE: &str = "config.toml";

/// Overrides the per-user config *directory*. Mainly a portable seam for
/// tests, but also usable to keep several independent hubs on one account.
pub const CONFIG_DIR_ENV: &str = "CYDRUST_CONFIG_DIR";

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 5151;

/// `$CYDRUST_CONFIG_DIR/config.toml` if set, else the platform's per-user
/// config dir: `~/.config/cydrust/` (Linux), `~/Library/Application
/// Support/cydrust/` (macOS), `%APPDATA%\cydrust\` (Windows).
pub fn user_config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(CONFIG_DIR_ENV) {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join(CONFIG_FILE));
        }
    }
    dirs::config_dir().map(|d| d.join("cydrust").join(CONFIG_FILE))
}

/// Find an existing config without creating anything.
///
/// Returns `Ok(None)` when no explicit path was given and no config exists in
/// either default location. An explicit path that does not exist is an error.
pub fn find(explicit: Option<&str>) -> Result<Option<PathBuf>> {
    if let Some(p) = explicit {
        let path = PathBuf::from(p);
        return if path.is_file() {
            Ok(Some(path))
        } else {
            Err(anyhow!("config file not found: {}", path.display()))
        };
    }
    let local = PathBuf::from(CONFIG_FILE);
    if local.is_file() {
        return Ok(Some(local));
    }
    Ok(user_config_path().filter(|p| p.is_file()))
}

/// Like [`find`], but when nothing exists, write a fresh per-user config with
/// a random token. The bool is `true` when the file was just created.
pub fn find_or_create(explicit: Option<&str>) -> Result<(PathBuf, bool)> {
    if let Some(path) = find(explicit)? {
        return Ok((path, false));
    }
    let path = user_config_path().ok_or_else(|| {
        anyhow!("cannot determine a per-user config directory; pass a config path explicitly")
    })?;
    write_new(&path)?;
    Ok((path, true))
}

/// 256 bits from the OS RNG, hex-encoded (64 chars).
pub fn generate_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow!("OS random source unavailable: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn write_new(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let body = format!(
        "# Created automatically by vibe-bridge on first run.\n\
         # The token authenticates the device and the Claude Code hooks against\n\
         # the hub. Every cydrust binary reads it from this file — nothing to copy.\n\
         token = \"{}\"\n\
         \n\
         # 127.0.0.1 = this machine only (USB transport). For the WiFi firmware\n\
         # set this to 0.0.0.0 or the machine's LAN address.\n\
         host = \"{DEFAULT_HOST}\"\n\
         port = {DEFAULT_PORT}\n",
        generate_token()?
    );
    write_private(path, &body).with_context(|| format!("write {}", path.display()))
}

/// Create the file exclusively (never clobber) and owner-only where the
/// platform supports it: the token in it is a credential.
fn write_private(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(body.as_bytes())
}

/// What a client binary needs from a config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    pub token: Option<String>,
    /// Base URL to reach the hub from this machine, e.g. `http://127.0.0.1:5151`.
    pub url: String,
}

/// Lenient reader for the client side: missing `host`/`port` fall back to the
/// hub's own defaults. `None` only if the file is unreadable or not TOML.
pub fn read_client(path: &Path) -> Option<ClientConfig> {
    let text = std::fs::read_to_string(path).ok()?;
    let val: toml::Value = toml::from_str(&text).ok()?;
    let token = val
        .get("token")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let host = val
        .get("host")
        .and_then(|h| h.as_str())
        .unwrap_or(DEFAULT_HOST);
    // A bind-all address is only meaningful to the server; clients on the
    // same machine reach it over loopback.
    let host = if host == "0.0.0.0" {
        DEFAULT_HOST
    } else {
        host
    };
    let port = val
        .get("port")
        .and_then(|p| p.as_integer())
        .unwrap_or(i64::from(DEFAULT_PORT));
    Some(ClientConfig {
        token,
        url: format!("http://{host}:{port}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // CONFIG_DIR_ENV and the working directory are process-global.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct DirGuard {
        old: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl DirGuard {
        fn point_at(dir: &Path) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let old = std::env::var(CONFIG_DIR_ENV).ok();
            std::env::set_var(CONFIG_DIR_ENV, dir);
            Self { old, _lock: lock }
        }
    }
    impl Drop for DirGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
                None => std::env::remove_var(CONFIG_DIR_ENV),
            }
        }
    }

    #[test]
    fn generated_tokens_are_64_hex_chars_and_differ() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn first_run_creates_a_config_the_hub_and_clients_can_both_read() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("cydrust");
        let _g = DirGuard::point_at(&dir);

        let (path, created) = find_or_create(None).unwrap();
        assert!(created);
        assert_eq!(path, dir.join(CONFIG_FILE));

        let client = read_client(&path).unwrap();
        assert_eq!(client.token.as_deref().map(str::len), Some(64));
        assert_eq!(client.url, "http://127.0.0.1:5151");
    }

    #[test]
    fn second_run_reuses_the_config_and_keeps_the_token() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DirGuard::point_at(tmp.path());

        let (path, created) = find_or_create(None).unwrap();
        assert!(created);
        let first = read_client(&path).unwrap().token;

        let (again, created) = find_or_create(None).unwrap();
        assert!(!created);
        assert_eq!(again, path);
        assert_eq!(read_client(&again).unwrap().token, first);
    }

    #[cfg(unix)]
    #[test]
    fn created_config_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let _g = DirGuard::point_at(tmp.path());

        let (path, _) = find_or_create(None).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the token is a credential");
    }

    #[test]
    fn explicit_missing_path_is_an_error_and_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DirGuard::point_at(tmp.path());
        let missing = tmp.path().join("typo.toml");

        assert!(find(Some(missing.to_str().unwrap())).is_err());
        assert!(find_or_create(Some(missing.to_str().unwrap())).is_err());
        assert!(!missing.exists());
        assert!(!tmp.path().join(CONFIG_FILE).exists());
    }

    #[test]
    fn explicit_existing_path_wins_over_the_user_config() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DirGuard::point_at(tmp.path());
        find_or_create(None).unwrap();

        let other = tmp.path().join("other.toml");
        std::fs::write(&other, "token = \"explicit-token\"\n").unwrap();
        let found = find(Some(other.to_str().unwrap())).unwrap().unwrap();
        assert_eq!(
            read_client(&found).unwrap().token.as_deref(),
            Some("explicit-token")
        );
    }

    #[test]
    fn find_without_any_config_is_none_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DirGuard::point_at(tmp.path());
        // Only meaningful when the test's cwd has no config.toml of its own.
        if !Path::new(CONFIG_FILE).exists() {
            assert_eq!(find(None).unwrap(), None);
        }
    }

    #[test]
    fn read_client_maps_bind_all_to_loopback_and_defaults_missing_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("c.toml");

        std::fs::write(&p, "token = \"t\"\nhost = \"0.0.0.0\"\nport = 6000\n").unwrap();
        assert_eq!(read_client(&p).unwrap().url, "http://127.0.0.1:6000");

        std::fs::write(&p, "token = \"t\"\n").unwrap();
        assert_eq!(read_client(&p).unwrap().url, "http://127.0.0.1:5151");

        std::fs::write(&p, "token = \"\"\n").unwrap();
        assert_eq!(
            read_client(&p).unwrap().token,
            None,
            "empty token is no token"
        );

        std::fs::write(&p, "not toml at all [[[").unwrap();
        assert_eq!(read_client(&p), None);
    }
}
