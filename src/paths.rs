//! Where a project's daemon keeps its socket and index snapshot.
//!
//! State lives under the user cache dir (`$RGX_CACHE_DIR`, else the config file's `cache_dir`, else
//! `$XDG_CACHE_HOME/rgx`, else `~/.cache/rgx`), with a `<hash>` subdir keyed by the canonical root
//! path, so rgx never writes into the repo it indexes.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::config::Config;

/// Canonicalize a root argument (defaulting to the current directory).
pub fn resolve_root(arg: Option<&str>) -> PathBuf {
    let raw = arg.unwrap_or(".");
    std::fs::canonicalize(raw).unwrap_or_else(|_| PathBuf::from(raw))
}

/// The per-root state directory, created on demand.
///
/// `$RGX_CACHE_DIR` overrides the base and is used verbatim; otherwise the config file's `cache_dir`,
/// then `$XDG_CACHE_HOME/rgx`, then `~/.cache/rgx`, then a temp dir.
pub fn state_dir(root: &Path) -> PathBuf {
    let mut h = DefaultHasher::new();
    root.hash(&mut h);
    let hash = format!("{:016x}", h.finish());
    // Windows has no XDG/HOME convention, so fall back to the standard per-user cache roots there.
    // An exported-but-empty var is treated as unset, so a stray `FOO=` never yields a relative base.
    let xdg = non_empty(std::env::var_os("XDG_CACHE_HOME").or_else(win_var("LOCALAPPDATA")));
    let home = non_empty(std::env::var_os("HOME").or_else(win_var("USERPROFILE")));
    let rgx_cache = non_empty(std::env::var_os("RGX_CACHE_DIR"));
    let cfg = Config::get().cache_dir.clone();
    let base = cache_base(rgx_cache, cfg, xdg, home);
    base.join(hash)
}

#[cfg(windows)]
pub(crate) fn win_var(name: &'static str) -> impl FnOnce() -> Option<std::ffi::OsString> {
    move || std::env::var_os(name)
}

#[cfg(not(windows))]
pub(crate) fn win_var(_name: &'static str) -> impl FnOnce() -> Option<std::ffi::OsString> {
    || None
}

/// Treat an exported-but-empty environment value as unset, so `FOO=` never resolves to a path.
pub(crate) fn non_empty(v: Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    v.filter(|s| !s.is_empty())
}

fn cache_base(
    rgx_cache_dir: Option<std::ffi::OsString>,
    config_cache_dir: Option<PathBuf>,
    xdg_cache_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    if let Some(dir) = rgx_cache_dir {
        return PathBuf::from(dir);
    }
    if let Some(dir) = config_cache_dir {
        return dir;
    }
    xdg_cache_home
        .map(PathBuf::from)
        .or_else(|| home.map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("rgx")
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
        let cfg = || Some(PathBuf::from("/cfg/cache"));
        assert_eq!(
            cache_base(os("/custom/rgx"), cfg(), os("/xdg"), os("/home/u")),
            PathBuf::from("/custom/rgx")
        );
        assert_eq!(
            cache_base(None, cfg(), os("/xdg"), os("/home/u")),
            PathBuf::from("/cfg/cache")
        );
        assert_eq!(
            cache_base(None, None, os("/xdg"), os("/home/u")),
            PathBuf::from("/xdg/rgx")
        );
        assert_eq!(
            cache_base(None, None, None, os("/home/u")),
            PathBuf::from("/home/u/.cache/rgx")
        );
    }
}
