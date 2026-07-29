use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use nix::unistd::getuid;
use thiserror::Error;

#[cfg(target_os = "linux")]
const UNIX_SOCKET_PATH_LIMIT: usize = 107;
#[cfg(target_os = "macos")]
const UNIX_SOCKET_PATH_LIMIT: usize = 103;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const UNIX_SOCKET_PATH_LIMIT: usize = 103;

#[derive(Clone, Debug)]
pub struct ProjectPaths {
    pub config_file: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub control_socket: PathBuf,
}

#[derive(Debug, Error)]
pub enum RuntimePathError {
    #[error("could not resolve platform project directories")]
    Unavailable,
    #[error("runtime path is not owned by the current user: {0}")]
    WrongOwner(PathBuf),
    #[error("runtime path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("runtime path must not be a symlink: {0}")]
    Symlink(PathBuf),
    #[error("control socket path is too long for this platform: {0}")]
    SocketPathTooLong(PathBuf),
    #[error("unsafe existing control socket path: {0}")]
    UnsafeSocket(PathBuf),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl ProjectPaths {
    /// Resolves native config, state, and runtime paths.
    ///
    /// # Errors
    ///
    /// Returns an error if the current platform cannot provide project directories.
    pub fn resolve() -> Result<Self, RuntimePathError> {
        let project = ProjectDirs::from("dev", "YewFence", "nono-approval")
            .ok_or(RuntimePathError::Unavailable)?;
        let state_dir = project
            .state_dir()
            .unwrap_or_else(|| project.data_local_dir())
            .to_path_buf();
        let runtime_dir = project.runtime_dir().map_or_else(
            || project.data_local_dir().join("runtime"),
            Path::to_path_buf,
        );
        let control_socket = runtime_dir.join("control.sock");
        validate_socket_path(&control_socket)?;
        Ok(Self {
            config_file: project.config_dir().join("config.toml"),
            state_dir,
            runtime_dir,
            control_socket,
        })
    }
}

/// Creates or validates an owner-only directory.
///
/// # Errors
///
/// Returns an error for symlinks, unexpected file types, wrong ownership, or I/O failures.
pub fn ensure_owner_directory(path: &Path) -> Result<(), RuntimePathError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(RuntimePathError::Symlink(path.to_path_buf()));
            }
            if !metadata.is_dir() {
                return Err(RuntimePathError::NotDirectory(path.to_path_buf()));
            }
            if metadata.uid() != getuid().as_raw() {
                return Err(RuntimePathError::WrongOwner(path.to_path_buf()));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(path)?,
        Err(error) => return Err(error.into()),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Validates that a Unix socket path fits the target platform ABI.
///
/// # Errors
///
/// Returns an error when the encoded path exceeds `sockaddr_un.sun_path`.
pub fn validate_socket_path(path: &Path) -> Result<(), RuntimePathError> {
    if path.as_os_str().as_encoded_bytes().len() > UNIX_SOCKET_PATH_LIMIT {
        Err(RuntimePathError::SocketPathTooLong(path.to_path_buf()))
    } else {
        Ok(())
    }
}

/// Removes an inactive owner-controlled socket path.
///
/// # Errors
///
/// Returns an error when the existing entry is not a socket, is a symlink, or appears active.
pub async fn remove_stale_socket(path: &Path) -> Result<(), RuntimePathError> {
    use std::os::unix::fs::FileTypeExt as _;

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_socket()
        || metadata.uid() != getuid().as_raw()
    {
        return Err(RuntimePathError::UnsafeSocket(path.to_path_buf()));
    }
    match tokio::net::UnixStream::connect(path).await {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            fs::remove_file(path)?;
            Ok(())
        }
        Ok(_) | Err(_) => Err(RuntimePathError::UnsafeSocket(path.to_path_buf())),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use tempfile::tempdir;

    use super::ensure_owner_directory;

    #[test]
    fn creates_owner_only_directory() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("runtime");
        ensure_owner_directory(&path).unwrap();
        let mode = path.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
