//! Shared user configuration for the reporter and GUI.
//!
//! Reads a TOML file from the platform XDG config directory. On first run the
//! file is auto-created with the compiled-in defaults so users have a concrete
//! template to edit. Malformed files surface as errors at startup rather than
//! silently falling back.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const DEFAULT_SERVER_URL: &str = "http://localhost:7685";

const STARTER_FILE: &str = "\
# claude-session-monitor configuration
# See the project README or `--help` for the list of recognized keys.

[server]
url = \"http://localhost:7685\"
";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub url: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            url: DEFAULT_SERVER_URL.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine platform config directory")]
    NoConfigDir,
    #[error("io error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// Resolve the platform config-file path: `{config_dir}/claude-session-monitor/config.toml`.
pub fn default_path() -> Result<PathBuf, ConfigError> {
    let dirs = directories::ProjectDirs::from("", "", "claude-session-monitor")
        .ok_or(ConfigError::NoConfigDir)?;
    Ok(dirs.config_dir().join("config.toml"))
}

/// Load configuration from the platform default path, auto-creating on first run.
pub fn load() -> Result<Config, ConfigError> {
    load_from(&default_path()?)
}

/// Load configuration from an explicit path. Auto-creates a starter file when
/// the path does not exist; returns the parsed `Config` either way.
pub fn load_from(path: &Path) -> Result<Config, ConfigError> {
    if !path.exists() {
        write_starter(path)?;
    }
    let body = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&body).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn write_starter(path: &Path) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    // create_new so concurrent reporter+GUI startups don't race on the write.
    // If another process won the race we treat the file as already-present.
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => f
            .write_all(STARTER_FILE.as_bytes())
            .map_err(|source| ConfigError::Io {
                path: path.to_path_buf(),
                source,
            }),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn well_formed_file_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[server]\nurl = \"http://example:1234\"\n").unwrap();
        let config = load_from(&path).unwrap();
        assert_eq!(config.server.url, "http://example:1234");
    }

    #[test]
    fn missing_path_creates_default_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let config = load_from(&path).unwrap();
        assert_eq!(config.server.url, DEFAULT_SERVER_URL);
        assert!(path.exists());
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("[server]"));
        assert!(body.contains(DEFAULT_SERVER_URL));
    }

    #[test]
    fn auto_created_file_re_reads_without_change() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let first = load_from(&path).unwrap();
        let body_after_first = std::fs::read_to_string(&path).unwrap();
        let second = load_from(&path).unwrap();
        let body_after_second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first, second);
        assert_eq!(body_after_first, body_after_second);
    }

    #[test]
    fn malformed_toml_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "not = valid = toml").unwrap();
        let err = load_from(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn unknown_key_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[server]\nurl = \"http://x:1\"\nbogus = \"nope\"\n").unwrap();
        let err = load_from(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn wrong_type_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[server]\nurl = 123\n").unwrap();
        let err = load_from(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }
}
