//! Where a project's daemon keeps its socket and index snapshot.
//!
//! State lives under the user cache dir (`$RGX_CACHE_DIR`, else `$XDG_CACHE_HOME/rgx`, else
//! `~/.cache/rgx`), with a `<hash>` subdir keyed by the canonical root path, so rgx never writes
//! into the repo it indexes.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Canonicalize a root argument (defaulting to the current directory).
pub fn resolve_root(arg: Option<&str>) -> PathBuf {
    let raw = arg.unwrap_or(".");
    std::fs::canonicalize(raw).unwrap_or_else(|_| PathBuf::from(raw))
}

/// The per-root state directory, created on demand.
///
/// `$RGX_CACHE_DIR` overrides the base and is used verbatim; otherwise it falls back to
/// `$XDG_CACHE_HOME/rgx`, then `~/.cache/rgx`, then a temp dir.
pub fn state_dir(root: &Path) -> PathBuf {
    let mut h = DefaultHasher::new();
    root.hash(&mut h);
    let hash = format!("{:016x}", h.finish());
    let base = cache_base(
        std::env::var_os("RGX_CACHE_DIR"),
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::var_os("HOME"),
    );
    base.join(hash)
}

fn cache_base(
    rgx_cache_dir: Option<std::ffi::OsString>,
    xdg_cache_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    if let Some(dir) = rgx_cache_dir {
        return PathBuf::from(dir);
    }
    xdg_cache_home
        .map(PathBuf::from)
        .or_else(|| home.map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("rgx")
}

pub fn socket_path(root: &Path) -> PathBuf {
    state_dir(root).join("daemon.sock")
}

pub fn snapshot_path(root: &Path) -> PathBuf {
    state_dir(root).join("index.bin")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn cache_base_precedence() {
        assert_eq!(
            cache_base(os("/custom/rgx"), os("/xdg"), os("/home/u")),
            PathBuf::from("/custom/rgx")
        );
        assert_eq!(
            cache_base(None, os("/xdg"), os("/home/u")),
            PathBuf::from("/xdg/rgx")
        );
        assert_eq!(
            cache_base(None, None, os("/home/u")),
            PathBuf::from("/home/u/.cache/rgx")
        );
    }
}
