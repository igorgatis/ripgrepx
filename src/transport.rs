//! The daemon transport, abstracted so the same protocol runs over the best local IPC per platform.
//!
//! - **Unix** (`mac`/`linux`): an AF_UNIX socket — byte-for-byte the original implementation, so
//!   behavior and performance on those platforms are unchanged.
//! - **Windows**: a loopback TCP connection whose port the daemon publishes in the state dir (Rust's
//!   `std` has no Windows `UnixStream`). `set_nodelay` keeps small-request latency low.
//!
//! The `tcp-transport` feature forces the TCP path on any platform, so the Windows transport can be
//! exercised and tested on Unix.

use std::io;
use std::path::Path;

use anyhow::Result;

#[cfg(all(unix, not(feature = "tcp-transport")))]
mod imp {
    use std::io::{self, ErrorKind};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    use anyhow::Result;

    use crate::paths;

    pub type Listener = UnixListener;
    pub type Stream = UnixStream;

    fn endpoint(root: &Path) -> PathBuf {
        paths::state_dir(root).join("daemon.sock")
    }

    /// Connect to the daemon, spawning nothing. `Ok(None)` means no live daemon owns this root.
    pub fn connect(root: &Path) -> io::Result<Option<Stream>> {
        match UnixStream::connect(endpoint(root)) {
            Ok(s) => Ok(Some(s)),
            Err(e)
                if e.kind() == ErrorKind::NotFound || e.kind() == ErrorKind::ConnectionRefused =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Bind the per-root endpoint, taking ownership. `Ok(None)` means a live daemon already owns it;
    /// a stale socket (no listener) is removed and rebound.
    pub fn bind(root: &Path) -> Result<Option<Listener>> {
        let sock = endpoint(root);
        match UnixListener::bind(&sock) {
            Ok(l) => Ok(Some(l)),
            Err(e) if e.kind() == ErrorKind::AddrInUse => {
                if UnixStream::connect(&sock).is_ok() {
                    Ok(None)
                } else {
                    std::fs::remove_file(&sock).ok();
                    Ok(Some(UnixListener::bind(&sock)?))
                }
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn accept(listener: &Listener) -> io::Result<Stream> {
        listener.accept().map(|(s, _)| s)
    }

    pub fn cleanup(root: &Path) {
        let _ = std::fs::remove_file(endpoint(root));
    }
}

#[cfg(any(windows, feature = "tcp-transport"))]
mod imp {
    use std::io::{self, ErrorKind};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};

    use anyhow::Result;

    use crate::paths;

    pub type Listener = TcpListener;
    pub type Stream = TcpStream;

    fn port_file(root: &Path) -> PathBuf {
        paths::state_dir(root).join("daemon.port")
    }

    fn read_port(root: &Path) -> Option<u16> {
        std::fs::read_to_string(port_file(root))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Connect to the daemon, spawning nothing. `Ok(None)` means no live daemon owns this root (no
    /// port published, or nothing listening on it).
    pub fn connect(root: &Path) -> io::Result<Option<Stream>> {
        let Some(port) = read_port(root) else {
            return Ok(None);
        };
        match TcpStream::connect((Ipv4Addr::LOCALHOST, port)) {
            Ok(s) => {
                s.set_nodelay(true).ok();
                Ok(Some(s))
            }
            Err(e) if e.kind() == ErrorKind::ConnectionRefused => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Bind a loopback port and publish it. `Ok(None)` means a live daemon already answers for this
    /// root. (Loopback-only, so it does not trip the Windows firewall prompt.)
    pub fn bind(root: &Path) -> Result<Option<Listener>> {
        if connect(root)?.is_some() {
            return Ok(None);
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let dir = paths::state_dir(root);
        std::fs::create_dir_all(&dir)?;
        // Publish the port atomically so a connecting client never reads a half-written file.
        let tmp = dir.join(format!("daemon.port.{}", std::process::id()));
        std::fs::write(&tmp, port.to_string())?;
        std::fs::rename(&tmp, port_file(root))?;
        Ok(Some(listener))
    }

    pub fn accept(listener: &Listener) -> io::Result<Stream> {
        let (s, _) = listener.accept()?;
        s.set_nodelay(true).ok();
        Ok(s)
    }

    pub fn cleanup(root: &Path) {
        let _ = std::fs::remove_file(port_file(root));
    }
}

pub use imp::{Listener, Stream};

/// Connect to the daemon for `root` without spawning one; `Ok(None)` if none is live.
pub fn connect(root: &Path) -> io::Result<Option<Stream>> {
    imp::connect(root)
}

/// Take ownership of `root`'s endpoint; `Ok(None)` if a live daemon already owns it.
pub fn bind(root: &Path) -> Result<Option<Listener>> {
    imp::bind(root)
}

/// Block until one client connects, returning its stream.
pub fn accept(listener: &Listener) -> io::Result<Stream> {
    imp::accept(listener)
}

/// Remove the endpoint's on-disk artifact (socket file / published port) on shutdown.
pub fn cleanup(root: &Path) {
    imp::cleanup(root)
}
