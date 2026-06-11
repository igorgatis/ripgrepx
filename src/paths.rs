//! Where a project's daemon keeps its socket and index snapshot.
//!
//! State lives under the user cache dir (`$XDG_CACHE_HOME` or `~/.cache/rgx/<hash>`), keyed by the
//! canonical root path, so rgx never writes into the repo it indexes.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Canonicalize a root argument (defaulting to the current directory).
pub fn resolve_root(arg: Option<&str>) -> PathBuf {
    let raw = arg.unwrap_or(".");
    std::fs::canonicalize(raw).unwrap_or_else(|_| PathBuf::from(raw))
}

/// The per-root state directory, created on demand.
pub fn state_dir(root: &Path) -> PathBuf {
    let mut h = DefaultHasher::new();
    root.hash(&mut h);
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("rgx").join(format!("{:016x}", h.finish()))
}

pub fn socket_path(root: &Path) -> PathBuf {
    state_dir(root).join("daemon.sock")
}

pub fn snapshot_path(root: &Path) -> PathBuf {
    state_dir(root).join("index.bin")
}
