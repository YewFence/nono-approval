use std::io;

use tokio::net::UnixStream;

#[cfg(target_os = "linux")]
/// Verifies that a control connection belongs to the daemon user.
///
/// # Errors
///
/// Returns an error when peer credentials are unavailable or the UID differs.
pub fn verify_owner(stream: &UnixStream) -> io::Result<()> {
    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
    use nix::unistd::getuid;

    let credentials = getsockopt(stream, PeerCredentials).map_err(io::Error::other)?;
    if credentials.uid() == getuid().as_raw() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "control peer UID does not match daemon owner",
        ))
    }
}

#[cfg(target_os = "macos")]
/// Verifies that a control connection belongs to the daemon user.
///
/// # Errors
///
/// Returns an error when peer credentials are unavailable or the UID differs.
pub fn verify_owner(stream: &UnixStream) -> io::Result<()> {
    use nix::sys::socket::{getsockopt, sockopt::LocalPeerPid};
    use nix::unistd::{getpeereid, getuid};

    let _peer_pid = getsockopt(stream, LocalPeerPid).map_err(io::Error::other)?;
    let (peer_uid, _) = getpeereid(stream).map_err(io::Error::other)?;
    if peer_uid == getuid() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "control peer UID does not match daemon owner",
        ))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
/// Rejects control peers on unsupported platforms.
///
/// # Errors
///
/// Always returns [`io::ErrorKind::Unsupported`].
pub fn verify_owner(_: &UnixStream) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "control peer identity is only implemented on Linux and macOS",
    ))
}
