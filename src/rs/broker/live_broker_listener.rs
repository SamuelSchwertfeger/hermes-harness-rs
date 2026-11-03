//! Bounded, private, externally managed `AF_UNIX` broker socket lifecycle.
#![cfg(unix)]

use std::fs;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::live_capability_broker::{BrokerDeniedError, CapabilityBroker, serve_connection};

/// Largest accepted listen backlog.
pub const MAX_BACKLOG: usize = 128;
/// Upper bound on how long a stop request can go unnoticed while idle.
pub const ACCEPT_POLL_SECONDS: f64 = 0.2;
/// Mode applied to the socket file.
pub const SOCKET_MODE: u32 = 0o600;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Unsafe broker socket lifecycle.
#[derive(Debug, thiserror::Error)]
pub enum BrokerListenerError {
    /// The listener configuration or socket path is unsafe.
    #[error("{0}")]
    Unsafe(String),
    /// Filesystem or socket failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// A peer request failed in a way that Python does not contain per peer.
    #[error(transparent)]
    Peer(#[from] BrokerDeniedError),
}

impl BrokerDeniedError {
    fn kind(&self) -> &'static str {
        match self {
            Self::Denied(_) => "Denied",
            Self::Adapter(_) => "Adapter",
            Self::Sqlite(_) => "Sqlite",
            Self::Io(_) => "Io",
            Self::Encode(_) => "Encode",
        }
    }
}

fn unsafe_err(message: &str) -> BrokerListenerError {
    BrokerListenerError::Unsafe(message.to_owned())
}

/// Limits and identity for one listener.
#[derive(Debug, Clone)]
pub struct ListenerSettings {
    /// Socket path; must not exist.
    pub path: PathBuf,
    /// UID allowed to submit requests.
    pub expected_uid: u32,
    /// Listen backlog.
    pub backlog: usize,
    /// Maximum connections served before returning.
    pub max_connections: usize,
    /// Set once the listener accepts connections.
    pub ready: Option<Arc<AtomicBool>>,
}

impl ListenerSettings {
    /// Settings with the Python defaults (`backlog=8`, `max_connections=32`).
    #[must_use]
    pub const fn new(path: PathBuf, expected_uid: u32) -> Self {
        Self {
            path,
            expected_uid,
            backlog: 8,
            max_connections: 32,
            ready: None,
        }
    }
}

///
/// # Errors
/// Socket ownership, permissions, peer identity, framing, or socket I/O fails validation.
fn validate_path(settings: &ListenerSettings) -> Result<(), BrokerListenerError> {
    let path = settings.path.as_path();
    if settings.max_connections < 1 || !(1..=MAX_BACKLOG).contains(&settings.backlog) {
        return Err(unsafe_err("Invalid listener limits"));
    }
    if crate::util::lstat(path).is_ok() || path.exists() {
        return Err(unsafe_err("Refusing existing socket path"));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    if crate::util::lstat(parent).is_ok_and(|m| m.file_type().is_symlink()) || !parent.is_dir() {
        return Err(unsafe_err("Socket directory must exist and be real"));
    }
    if fs::metadata(parent)?.permissions().mode() & 0o002 != 0 {
        return Err(unsafe_err("World-writable socket directory rejected"));
    }
    Ok(())
}

///
/// # Errors
/// Socket ownership, permissions, peer identity, framing, or socket I/O fails validation.
fn handle_peer(
    mut conn: UnixStream,
    broker: &mut CapabilityBroker,
    uid: u32,
) -> Result<(), BrokerListenerError> {
    match serve_connection(&mut conn, broker, uid) {
        Err(err) if !err.is_peer_rejection() => Err(err.into()),
        Err(err) => {
            log::warn!("Broker peer request rejected ({})", err.kind());
            Ok(())
        }
        Ok(()) => Ok(()),
    }
}

///
/// # Errors
/// Socket ownership, permissions, peer identity, framing, or socket I/O fails validation.
fn accept_peers(
    listener: &UnixListener,
    broker: &mut CapabilityBroker,
    settings: &ListenerSettings,
    stop: &AtomicBool,
) -> Result<usize, BrokerListenerError> {
    let mut handled = 0;
    while !stop.load(Ordering::SeqCst) && handled < settings.max_connections {
        match listener.accept() {
            Ok((conn, _)) => {
                conn.set_nonblocking(false)?;
                handled += 1;
                handle_peer(conn, broker, settings.expected_uid)?;
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                log::debug!("Broker accept polling timeout");
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(handled)
}

/// Serve bounded peers; preserve existing paths and unlink owned socket.
///
/// # Errors
/// Socket ownership, permissions, peer identity, framing, or socket I/O fails validation.
pub fn serve_until(
    settings: &ListenerSettings,
    broker: &mut CapabilityBroker,
    stop: &AtomicBool,
) -> Result<usize, BrokerListenerError> {
    validate_path(settings)?;
    let path = settings.path.as_path();
    let socket = rustix::net::socket_with(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::STREAM,
        rustix::net::SocketFlags::CLOEXEC,
        None,
    )
    .map_err(io::Error::from)?;
    rustix::net::SocketAddrUnix::new(path)
        .and_then(|addr| rustix::net::bind_unix(&socket, &addr))
        .map_err(io::Error::from)?;
    let outcome = run_listener(socket, settings, broker, stop);
    let removed = match fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        Ok(()) | Err(_) => Ok(()),
    };
    let handled = outcome?;
    removed?;
    Ok(handled)
}

/// Like Python: chmod the bound socket before `listen`, so no peer can connect earlier.
fn run_listener(
    socket: OwnedFd,
    settings: &ListenerSettings,
    broker: &mut CapabilityBroker,
    stop: &AtomicBool,
) -> Result<usize, BrokerListenerError> {
    fs::set_permissions(&settings.path, fs::Permissions::from_mode(SOCKET_MODE))?;
    let backlog = i32::try_from(settings.backlog).unwrap_or(i32::MAX);
    rustix::net::listen(&socket, backlog).map_err(io::Error::from)?;
    let listener = UnixListener::from(socket);
    listener.set_nonblocking(true)?;
    if let Some(ready) = &settings.ready {
        ready.store(true, Ordering::SeqCst);
    }
    accept_peers(&listener, broker, settings, stop)
}
