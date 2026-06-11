//! Client side: connect to the project's daemon, spawning it on first use.

use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Result, bail};

use crate::paths;
use crate::proto::{self, Request};

/// Send one request to the daemon for `root` (spawning it if needed), streaming the response to
/// `sink`. Returns the total number of bytes written.
pub fn request_stream(root: &Path, req: &Request, sink: &mut impl std::io::Write) -> Result<usize> {
    let sock = paths::socket_path(root);
    let mut stream = connect_or_spawn(root, &sock)?;
    proto::write_request(&mut stream, req)?;
    proto::read_stream(&mut stream, sink)
}

/// Send one request and collect the whole response into a `Vec` (for small responses).
pub fn request(root: &Path, req: &Request) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    request_stream(root, req, &mut out)?;
    Ok(out)
}

/// Like [`request`] but never spawns — returns `None` if no daemon is listening. For `stop`/`status`.
pub fn request_existing(root: &Path, req: &Request) -> Result<Option<Vec<u8>>> {
    let sock = paths::socket_path(root);
    match UnixStream::connect(&sock) {
        Ok(mut stream) => {
            proto::write_request(&mut stream, req)?;
            Ok(Some(proto::read_stream_to_vec(&mut stream)?))
        }
        Err(e) if e.kind() == ErrorKind::NotFound || e.kind() == ErrorKind::ConnectionRefused => {
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

/// Subscribe to the daemon's live status (spawning it if needed), invoking `render` with each status
/// frame as it arrives, until the daemon closes the stream (or the process is interrupted).
pub fn watch(root: &Path, mut render: impl FnMut(&[u8])) -> Result<()> {
    let sock = paths::socket_path(root);
    let mut stream = connect_or_spawn(root, &sock)?;
    proto::write_request(&mut stream, &Request::Watch)?;
    while let Some(frame) = proto::read_watch_frame(&mut stream)? {
        render(&frame);
    }
    Ok(())
}

fn connect_or_spawn(root: &Path, sock: &Path) -> Result<UnixStream> {
    if let Ok(s) = UnixStream::connect(sock) {
        return Ok(s);
    }
    spawn_daemon(root)?;
    for _ in 0..400 {
        std::thread::sleep(Duration::from_millis(25));
        if let Ok(s) = UnixStream::connect(sock) {
            return Ok(s);
        }
    }
    bail!("daemon did not come up for {}", root.display());
}

/// Spawn a detached background daemon (`rgx --server`) rooted at `root`.
pub fn spawn_daemon(root: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    Command::new(exe)
        .arg("--server")
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    Ok(())
}
