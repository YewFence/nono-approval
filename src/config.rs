use std::fs;
use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use bytesize::ByteSize;
use nix::unistd::getuid;
use serde::{Deserialize, Serialize};
use tempfile::Builder;
use thiserror::Error;

use crate::broker::{DEFAULT_MAX_PENDING, DEFAULT_MAX_PER_SESSION, DEFAULT_REQUEST_TIMEOUT};
use crate::runtime_path::{RuntimePathError, ensure_owner_directory};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub schema_version: u32,
    pub webhook: WebhookConfig,
    pub approval: ApprovalConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookConfig {
    pub listen: SocketAddr,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalConfig {
    pub request_timeout: String,
    pub max_pending: usize,
    pub max_per_session: usize,
    pub max_body: String,
}

#[derive(Clone, Debug)]
pub struct ResolvedConfig {
    pub webhook_listen: SocketAddr,
    pub request_timeout: Duration,
    pub max_pending: usize,
    pub max_per_session: usize,
    pub max_body_bytes: usize,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration file does not exist; run `nono-approval setup` first: {0}")]
    Missing(PathBuf),
    #[error("configuration path must be a regular file owned by the current user: {0}")]
    UnsafeFile(PathBuf),
    #[error("configuration file permissions must be 0600: {0}")]
    UnsafePermissions(PathBuf),
    #[error("unsupported configuration schema version {0}")]
    SchemaVersion(u32),
    #[error("webhook listener must use a loopback IP address")]
    NonLoopback,
    #[error("pending limits must be greater than zero")]
    ZeroLimit,
    #[error("max_per_session must not exceed max_pending")]
    PerSessionExceedsGlobal,
    #[error("invalid request_timeout: {0}")]
    InvalidDuration(String),
    #[error("invalid max_body: {0}")]
    InvalidByteSize(String),
    #[error(transparent)]
    RuntimePath(#[from] RuntimePathError),
    #[error(transparent)]
    TomlDecode(#[from] toml::de::Error),
    #[error(transparent)]
    TomlEncode(#[from] toml::ser::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            webhook: WebhookConfig {
                listen: "127.0.0.1:17443".parse().expect("static address is valid"),
            },
            approval: ApprovalConfig {
                request_timeout: humantime::format_duration(DEFAULT_REQUEST_TIMEOUT).to_string(),
                max_pending: DEFAULT_MAX_PENDING,
                max_per_session: DEFAULT_MAX_PER_SESSION,
                max_body: "256KiB".to_owned(),
            },
        }
    }
}

impl ConfigFile {
    /// Validates and converts the serialized configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schemas, unsafe listeners, or invalid limits.
    pub fn resolve(&self) -> Result<ResolvedConfig, ConfigError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ConfigError::SchemaVersion(self.schema_version));
        }
        if !self.webhook.listen.ip().is_loopback() {
            return Err(ConfigError::NonLoopback);
        }
        if self.approval.max_pending == 0 || self.approval.max_per_session == 0 {
            return Err(ConfigError::ZeroLimit);
        }
        if self.approval.max_per_session > self.approval.max_pending {
            return Err(ConfigError::PerSessionExceedsGlobal);
        }
        let request_timeout = humantime::parse_duration(&self.approval.request_timeout)
            .map_err(|error| ConfigError::InvalidDuration(error.to_string()))?;
        let max_body =
            ByteSize::from_str(&self.approval.max_body).map_err(ConfigError::InvalidByteSize)?;
        let max_body_bytes = usize::try_from(max_body.as_u64())
            .map_err(|_| ConfigError::InvalidByteSize("value is too large".to_owned()))?;
        if max_body_bytes == 0 {
            return Err(ConfigError::InvalidByteSize(
                "value must be greater than zero".to_owned(),
            ));
        }
        Ok(ResolvedConfig {
            webhook_listen: self.webhook.listen,
            request_timeout,
            max_pending: self.approval.max_pending,
            max_per_session: self.approval.max_per_session,
            max_body_bytes,
        })
    }
}

/// Loads and validates an owner-only configuration file.
///
/// # Errors
///
/// Returns an error for missing, unsafe, malformed, or unsupported configuration.
pub fn load(path: &Path) -> Result<ConfigFile, ConfigError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ConfigError::Missing(path.to_path_buf()));
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != getuid().as_raw()
    {
        return Err(ConfigError::UnsafeFile(path.to_path_buf()));
    }
    if metadata.permissions().mode() & 0o777 != 0o600 {
        return Err(ConfigError::UnsafePermissions(path.to_path_buf()));
    }
    let config: ConfigFile = toml::from_str(&fs::read_to_string(path)?)?;
    config.resolve()?;
    Ok(config)
}

/// Creates the default config atomically, or validates an existing one.
///
/// # Errors
///
/// Returns an error when the directory or existing file is unsafe, or persistence fails.
pub fn setup(path: &Path) -> Result<ConfigFile, ConfigError> {
    if path.exists() {
        return load(path);
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config path has no parent"))?;
    ensure_owner_directory(parent)?;
    let config = ConfigFile::default();
    let contents = toml::to_string_pretty(&config)?;
    let mut temporary = Builder::new()
        .prefix(".config-")
        .permissions(fs::Permissions::from_mode(0o600))
        .tempfile_in(parent)?;
    temporary.write_all(contents.as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    load(path)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    use tempfile::tempdir;

    use super::{ConfigError, load, setup};

    #[test]
    fn setup_is_owner_only_and_idempotent() {
        let temporary = tempdir().unwrap();
        let path = temporary
            .path()
            .canonicalize()
            .unwrap()
            .join("config")
            .join("config.toml");
        setup(&path).unwrap();
        setup(&path).unwrap();
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn rejects_unknown_fields_and_unsafe_permissions() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("config.toml");
        fs::write(
            &path,
            "schema_version = 1\nunknown = true\n[webhook]\nlisten = '127.0.0.1:1'\n[approval]\nrequest_timeout = '1s'\nmax_pending = 1\nmax_per_session = 1\nmax_body = '1KiB'\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(load(&path), Err(ConfigError::TomlDecode(_))));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            load(&path),
            Err(ConfigError::UnsafePermissions(_))
        ));
    }
}
