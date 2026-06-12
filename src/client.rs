//! Client side: connect to the project's daemon, spawning it on first use.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Result, bail};

use crate::proto::{self, Request};
use crate::transport::{self, Stream};

/// Send one request to the daemon for `root` (spawning it if needed), streaming the response to
/// `sink`. Returns the total number of bytes written.
pub fn request_stream(root: &Path, req: &Request, sink: &mut impl std::io::Write) -> Result<usize> {
    let mut stream = connect_or_spawn(root)?;
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
    match transport::connect(root)? {
        Some(mut stream) => {
            proto::write_request(&mut stream, req)?;
            Ok(Some(proto::read_stream_to_vec(&mut stream)?))
        }
        None => Ok(None),
    }
}

/// Park a pagination cursor blob in `root`'s daemon (spawning it if needed); returns the short token
/// to print in place of the blob.
pub fn store_cursor(root: &Path, blob: Vec<u8>) -> Result<String> {
    let out = request(root, &Request::CursorStore { blob })?;
    Ok(String::from_utf8(out)?)
}

/// Redeem a pagination token at `root`'s daemon. `Ok(None)` means it expired or was already used —
/// the daemon replies with an empty frame, or there's no daemon at all (so no stored cursors). Uses a
/// connect-only request: a stale token must never spawn a fresh daemon (and a cold index build) just
/// to discover it's gone.
pub fn take_cursor(root: &Path, token: &str) -> Result<Option<Vec<u8>>> {
    let reply = request_existing(
        root,
        &Request::CursorTake {
            token: token.to_string(),
        },
    )?;
    Ok(reply.filter(|blob| !blob.is_empty()))
}

/// Subscribe to the daemon's live status (spawning it if needed), invoking `render` with each status
/// frame as it arrives, until the daemon closes the stream (or the process is interrupted).
pub fn watch(root: &Path, mut render: impl FnMut(&[u8])) -> Result<()> {
    let mut stream = connect_or_spawn(root)?;
    proto::write_request(&mut stream, &Request::Watch)?;
    while let Some(frame) = proto::read_watch_frame(&mut stream)? {
        render(&frame);
    }
    Ok(())
}

fn connect_or_spawn(root: &Path) -> Result<Stream> {
    if let Some(s) = transport::connect(root)? {
        return Ok(s);
    }
    spawn_daemon(root)?;
    for _ in 0..400 {
        std::thread::sleep(Duration::from_millis(25));
        if let Some(s) = transport::connect(root)? {
            return Ok(s);
        }
    }
    bail!("daemon did not come up for {}", root.display());
}

/// Poll until no daemon is listening for `root`, up to ~5s. Used by `restart` after a Shutdown so the
/// fresh daemon isn't spawned while the old one still holds the endpoint (it would exit as a no-op).
/// Returns `true` once the endpoint is free, `false` on timeout.
pub fn wait_until_stopped(root: &Path) -> bool {
    for _ in 0..200 {
        if let Ok(None) = transport::connect(root) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// Spawn a detached background daemon (`rgx --server`) rooted at `root`.
pub fn spawn_daemon(root: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("--server")
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut cmd);
    cmd.spawn()?;
    Ok(())
}

/// Put the daemon in its own process group so it outlives this client.
#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
    cmd.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000);
}
