//! Daemon configuration.
//!
//! Loaded from a TOML file (default `~/.config/hermes-dms/config.toml`, mode
//! 0600). The Hermes API key may instead be supplied via the `HERMES_API_KEY`
//! environment variable (e.g. a systemd credential), which always wins over
//! the file so the secret can be kept out of config when desired.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// rh-anine's VLAN20 address — bind the MCP server here, not 0.0.0.0, so the
/// desktop tools are only reachable over the cluster-internal network.
pub const DEFAULT_MCP_LISTEN_ADDR: &str = "10.20.0.3:9721";

/// Hermes API server default. The reachable address from bare-metal rh-anine
/// must be confirmed at deploy time (ClusterIP DNAT vs. pod IP vs. LAN IP).
pub const DEFAULT_HERMES_API_URL: &str = "http://hermes.ai.svc.cluster.local:8642";

/// Resolved, validated configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub hermes_api_url: String,
    pub hermes_api_key: String,
    pub mcp_listen_addr: SocketAddr,
    pub socket_path: PathBuf,
}

/// As parsed from TOML. Every field is optional; defaults are applied during
/// [`Config::resolve`].
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    pub hermes_api_url: Option<String>,
    pub hermes_api_key: Option<String>,
    pub mcp_listen_addr: Option<String>,
    pub socket_path: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parsing config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid mcp_listen_addr {value:?}: {source}")]
    BadAddr {
        value: String,
        source: std::net::AddrParseError,
    },
    #[error(
        "no Hermes API key: set `hermes_api_key` in the config file or the HERMES_API_KEY env var"
    )]
    MissingKey,
}

impl Config {
    /// Default config-file path, honouring `XDG_CONFIG_HOME` then `HOME`.
    pub fn default_path() -> Option<PathBuf> {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(xdg).join("hermes-dms/config.toml"));
        }
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/hermes-dms/config.toml"))
    }

    /// Default Unix socket path under `$XDG_RUNTIME_DIR` (local tmpfs), falling
    /// back to `/tmp` when the runtime dir is unset.
    pub fn default_socket_path() -> PathBuf {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        base.join("hermes-dms.sock")
    }

    /// Load and resolve config from `path`, applying the `HERMES_API_KEY`
    /// environment override.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let raw: RawConfig = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        Self::resolve(raw, std::env::var("HERMES_API_KEY").ok())
    }

    /// Pure resolution of raw config + env key into a validated [`Config`].
    pub fn resolve(raw: RawConfig, env_key: Option<String>) -> Result<Self, ConfigError> {
        let key = env_key
            .filter(|k| !k.is_empty())
            .or_else(|| raw.hermes_api_key.filter(|k| !k.is_empty()))
            .ok_or(ConfigError::MissingKey)?;

        let addr_str = raw
            .mcp_listen_addr
            .unwrap_or_else(|| DEFAULT_MCP_LISTEN_ADDR.to_string());
        let mcp_listen_addr =
            addr_str
                .parse::<SocketAddr>()
                .map_err(|source| ConfigError::BadAddr {
                    value: addr_str,
                    source,
                })?;

        Ok(Config {
            hermes_api_url: raw
                .hermes_api_url
                .unwrap_or_else(|| DEFAULT_HERMES_API_URL.to_string()),
            hermes_api_key: key,
            mcp_listen_addr,
            socket_path: raw
                .socket_path
                .map(PathBuf::from)
                .unwrap_or_else(Self::default_socket_path),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_with_key() -> RawConfig {
        RawConfig {
            hermes_api_key: Some("filekey".into()),
            ..Default::default()
        }
    }

    /// Missing optional fields fall back to documented defaults.
    #[test]
    fn defaults_applied() {
        let cfg = Config::resolve(raw_with_key(), None).unwrap();
        assert_eq!(cfg.hermes_api_url, DEFAULT_HERMES_API_URL);
        assert_eq!(
            cfg.mcp_listen_addr,
            DEFAULT_MCP_LISTEN_ADDR.parse().unwrap()
        );
        assert!(cfg.socket_path.ends_with("hermes-dms.sock"));
    }

    /// The env key wins over the file key.
    #[test]
    fn env_key_overrides_file() {
        let cfg = Config::resolve(raw_with_key(), Some("envkey".into())).unwrap();
        assert_eq!(cfg.hermes_api_key, "envkey");
    }

    /// An empty env key is ignored and the file key is used.
    #[test]
    fn empty_env_key_falls_back_to_file() {
        let cfg = Config::resolve(raw_with_key(), Some(String::new())).unwrap();
        assert_eq!(cfg.hermes_api_key, "filekey");
    }

    /// No key anywhere is a hard error.
    #[test]
    fn missing_key_errors() {
        let err = Config::resolve(RawConfig::default(), None).unwrap_err();
        assert!(matches!(err, ConfigError::MissingKey));
    }

    /// A malformed listen address produces a clear error, not a panic.
    #[test]
    fn bad_addr_errors() {
        let raw = RawConfig {
            mcp_listen_addr: Some("not-an-addr".into()),
            ..raw_with_key()
        };
        let err = Config::resolve(raw, None).unwrap_err();
        assert!(matches!(err, ConfigError::BadAddr { .. }));
    }

    /// A full TOML document parses and resolves.
    #[test]
    fn full_toml_parses() {
        let toml = r#"
            hermes_api_url = "http://10.43.0.5:8642"
            hermes_api_key = "abc123"
            mcp_listen_addr = "10.20.0.3:9721"
            socket_path = "/run/user/1000/hermes-dms.sock"
        "#;
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let cfg = Config::resolve(raw, None).unwrap();
        assert_eq!(cfg.hermes_api_url, "http://10.43.0.5:8642");
        assert_eq!(cfg.hermes_api_key, "abc123");
        assert_eq!(cfg.socket_path, PathBuf::from("/run/user/1000/hermes-dms.sock"));
    }

    /// Unknown keys in the config file are rejected (typo protection).
    #[test]
    fn unknown_key_rejected() {
        let toml = r#"hermes_api_key = "x"
            bogus_field = true
        "#;
        assert!(toml::from_str::<RawConfig>(toml).is_err());
    }
}
