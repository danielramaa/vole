use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// User settings from `~/.config/vole/config.toml`.
/// Only plain numbers here, because some of them reach commands run with sudo (ADR 0001).
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Cached versions of each installed package to keep. At least 1, so a downgrade stays possible.
    pub pacman_keep_versions: u32,
    /// Size the systemd journal is shrunk to, in MB.
    pub journal_max_mb: u32,
    /// Refresh interval of the status dashboard, in milliseconds.
    pub status_refresh_ms: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pacman_keep_versions: 2,
            journal_max_mb: 200,
            status_refresh_ms: 1000,
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("vole").join("config.toml"))
    }

    /// Loads the config file, or the defaults when it doesn't exist.
    pub fn load() -> Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let config: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if !(1..=10).contains(&self.pacman_keep_versions) {
            bail!("pacman_keep_versions must be between 1 and 10");
        }
        if !(50..=10_000).contains(&self.journal_max_mb) {
            bail!("journal_max_mb must be between 50 and 10000");
        }
        if !(250..=5000).contains(&self.status_refresh_ms) {
            bail!("status_refresh_ms must be between 250 and 5000");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.pacman_keep_versions, 2);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn rejects_out_of_range_values() {
        let config: Config = toml::from_str("journal_max_mb = 1").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_keeping_zero_package_versions() {
        let config: Config = toml::from_str("pacman_keep_versions = 0").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_text_and_unknown_keys() {
        assert!(toml::from_str::<Config>(r#"journal_max_mb = "200M; rm -rf /""#).is_err());
        assert!(toml::from_str::<Config>("paccache_args = \"-rk0\"").is_err());
    }
}
