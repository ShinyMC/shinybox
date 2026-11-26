use std::path::{Path, PathBuf};
use std::time::Duration;

use bytesize::ByteSize;
use serde::Deserialize;

const MINUTE: Duration = Duration::from_secs(60);

/// Runtime configuration, read from a TOML file and overridable from the command line.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Socket address the HTTP server listens on.
    pub bind: String,
    /// Directory holding the SQLite database and the uploaded files.
    pub data_dir: PathBuf,
    /// Base URL used in share links, e.g. `https://box.example.com`.
    /// When unset, links are built from the incoming request.
    pub public_url: Option<String>,
    /// When set, uploads require `Authorization: Bearer <token>`.
    pub upload_token: Option<String>,
    /// Maximum combined size of all files in one box.
    pub max_upload_size: ByteSize,
    /// Maximum number of files in one box.
    pub max_files: usize,
    /// Lifetime of a box when the uploader does not choose one.
    #[serde(with = "humantime_serde")]
    pub default_expiry: Duration,
    /// Longest lifetime an uploader may choose.
    #[serde(with = "humantime_serde")]
    pub max_expiry: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8080".into(),
            data_dir: "data".into(),
            public_url: None,
            upload_token: None,
            max_upload_size: ByteSize::mb(10),
            max_files: 3,
            default_expiry: Duration::from_secs(24 * 60 * 60),
            max_expiry: Duration::from_secs(7 * 24 * 60 * 60),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("invalid config file {path}: {source}")]
    Parse { path: PathBuf, source: toml::de::Error },
    #[error("invalid config: {0}")]
    Invalid(&'static str),
}

impl Config {
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })
    }

    /// Normalises optional values and rejects settings the server cannot work with.
    pub fn validated(mut self) -> Result<Self, ConfigError> {
        self.public_url = non_empty(self.public_url).map(|url| url.trim_end_matches('/').to_owned());
        self.upload_token = non_empty(self.upload_token);

        if self.max_files == 0 {
            return Err(ConfigError::Invalid("max-files must be at least 1"));
        }
        if self.max_upload_size.as_u64() == 0 {
            return Err(ConfigError::Invalid("max-upload-size must be greater than zero"));
        }
        if self.max_expiry < MINUTE {
            return Err(ConfigError::Invalid("max-expiry must be at least one minute"));
        }
        if self.default_expiry < MINUTE || self.default_expiry > self.max_expiry {
            return Err(ConfigError::Invalid(
                "default-expiry must be between one minute and max-expiry",
            ));
        }
        Ok(self)
    }

    /// The upload limit in decimal units, e.g. `10.0 MB`.
    pub fn max_upload_size_label(&self) -> String {
        self.max_upload_size.display().si().to_string()
    }

    /// Checks an uploader-chosen lifetime against the configured bounds.
    pub fn accepts_expiry(&self, expiry: Duration) -> bool {
        (MINUTE..=self.max_expiry).contains(&expiry)
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kebab_case_file() {
        let config: Config = toml::from_str(
            r#"
            max-upload-size = "25 MB"
            max-files = 5
            default-expiry = "1h"
            max-expiry = "3d"
            "#,
        )
        .unwrap();

        assert_eq!(config.max_upload_size, ByteSize::mb(25));
        assert_eq!(config.max_files, 5);
        assert_eq!(config.default_expiry, Duration::from_secs(3600));
        assert_eq!(config.bind, Config::default().bind);
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("max-file-size = 10").is_err());
    }

    #[test]
    fn validation_normalises_and_checks_bounds() {
        let config = Config {
            public_url: Some("https://box.example.com/".into()),
            upload_token: Some("  ".into()),
            ..Config::default()
        }
        .validated()
        .unwrap();
        assert_eq!(config.public_url.as_deref(), Some("https://box.example.com"));
        assert_eq!(config.upload_token, None);

        let too_long_default = Config {
            default_expiry: Duration::from_secs(30 * 24 * 60 * 60),
            ..Config::default()
        };
        assert!(too_long_default.validated().is_err());
    }
}
