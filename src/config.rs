//! Injected environment, config directory, stored session and device id.
//!
//! Only [`Env::from_process`] reads the real process environment. Everything else takes an `&Env`,
//! so tests run in parallel with their own variables and their own config directory.
use crate::errors::{CliError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Default)]
pub struct Env {
    vars: BTreeMap<String, String>,
    pub config_dir: PathBuf,
}

// Values can be secrets, so Debug lists names only.
impl fmt::Debug for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Env")
            .field("vars", &self.vars.keys().collect::<Vec<_>>())
            .field("config_dir", &self.config_dir)
            .finish()
    }
}

/// `TAPHOME_CONFIG_DIR`, else `$HOME/.config/taphome` on unix, else `%APPDATA%\taphome` on Windows.
fn default_config_dir(get: impl Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(dir) = get("TAPHOME_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    #[cfg(unix)]
    let dir = get("HOME").map(|h| PathBuf::from(h).join(".config").join("taphome"));
    #[cfg(not(unix))]
    let dir = get("APPDATA").map(|a| PathBuf::from(a).join("taphome"));
    dir.unwrap_or_else(|| PathBuf::from(".taphome"))
}

impl Env {
    pub fn from_process() -> Env {
        let config_dir = default_config_dir(|k| std::env::var(k).ok());
        let vars = std::env::vars().filter(|(k, _)| k.starts_with("TAPHOME_")).collect();
        Env { vars, config_dir }
    }

    pub fn new<K: Into<String>, V: Into<String>>(config_dir: impl Into<PathBuf>, vars: impl IntoIterator<Item = (K, V)>) -> Env {
        Env {
            vars: vars.into_iter().map(|(k, v)| (k.into(), v.into())).collect(),
            config_dir: config_dir.into(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str).filter(|v| !v.is_empty())
    }

    pub fn set(&mut self, name: &str, value: &str) {
        self.vars.insert(name.to_string(), value.to_string());
    }

    pub fn remove(&mut self, name: &str) {
        self.vars.remove(name);
    }

    /// The value of `NAME`, else the trimmed contents of the file named by `NAME_FILE`.
    pub fn get_or_file(&self, name: &str) -> Result<Option<String>> {
        if let Some(v) = self.get(name) {
            return Ok(Some(v.to_string()));
        }
        let file_var = format!("{name}_FILE");
        match self.get(&file_var) {
            None => Ok(None),
            Some(path) => fs::read_to_string(path)
                .map(|s| Some(s.trim().to_string()).filter(|s| !s.is_empty()))
                .map_err(|e| CliError::new("missing_config", format!("cannot read the file named by {file_var}: {e}"))),
        }
    }
}

/// Connection settings from the environment. Each field is None when not configured.
#[derive(Clone, Default)]
pub struct Config {
    pub host: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
}

impl Config {
    pub fn load(env: &Env) -> Result<Config> {
        Ok(Config {
            host: env.get_or_file("TAPHOME_HOST")?,
            user: env.get_or_file("TAPHOME_USER")?,
            password: env.get_or_file("TAPHOME_PASSWORD")?,
        })
    }
}

// ---------------------------------------------------------------- files

fn io_error(what: &str, e: std::io::Error) -> CliError {
    CliError::internal(format!("{what}: {e}"))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(()) // shortcut: Windows has no mode bits, the profile directory ACL protects the file; upgrade when ACLs are needed
}

/// Creates the config directory (mode 700 on unix).
pub fn ensure_config_dir(env: &Env) -> Result<()> {
    fs::create_dir_all(&env.config_dir).map_err(|e| io_error("cannot create the config directory", e))?;
    set_mode(&env.config_dir, 0o700).map_err(|e| io_error("cannot restrict the config directory", e))
}

/// Writes a file with mode 600 (unix), atomically: temp file in the same directory, then rename.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let write = || -> std::io::Result<()> {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut file = opts.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        set_mode(&tmp, 0o600)?;
        fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        io_error("cannot write a private file", e)
    })
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub host: String,
    pub location_id: String,
    pub user: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_hash: Option<String>,
}

fn session_path(env: &Env) -> PathBuf {
    env.config_dir.join("session.json")
}

/// A missing or unreadable session file counts as no session.
pub fn load_session(env: &Env) -> Option<StoredSession> {
    serde_json::from_slice(&fs::read(session_path(env)).ok()?).ok()
}

pub fn save_session(env: &Env, session: &StoredSession) -> Result<()> {
    ensure_config_dir(env)?;
    let bytes = serde_json::to_vec(session).map_err(|e| CliError::internal(e.to_string()))?;
    write_private(&session_path(env), &bytes)
}

/// Returns whether a session file existed.
pub fn delete_session(env: &Env) -> Result<bool> {
    match fs::remove_file(session_path(env)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error("cannot delete the session file", e)),
    }
}

/// Random 16 hex characters, generated once and kept in `<config dir>/device-id`.
pub fn device_id(env: &Env) -> Result<String> {
    let path = env.config_dir.join("device-id");
    if let Some(id) = fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| s.len() == 16)
    {
        return Ok(id);
    }
    use std::hash::{BuildHasher, Hasher};
    // RandomState is seeded from the OS per instance: enough randomness for an id that only needs to be unique.
    let n = std::collections::hash_map::RandomState::new().build_hasher().finish();
    let id = format!("{n:016x}");
    ensure_config_dir(env)?;
    write_private(&path, format!("{id}\n").as_bytes())?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_override_wins() {
        let dir = default_config_dir(|k| (k == "TAPHOME_CONFIG_DIR").then(|| "/x/y".to_string()));
        assert_eq!(dir, PathBuf::from("/x/y"));
    }

    #[test]
    #[cfg(unix)]
    fn config_dir_from_home() {
        let dir = default_config_dir(|k| (k == "HOME").then(|| "/home/a".to_string()));
        assert_eq!(dir, PathBuf::from("/home/a/.config/taphome"));
    }
}
