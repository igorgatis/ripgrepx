//! User config, loaded from a TOML file.
//!
//! Location: `$RGX_CONFIG` (verbatim), else `$XDG_CONFIG_HOME/rgx/config.toml` (`%APPDATA%` on
//! Windows), else `~/.config/rgx/config.toml`. A missing or unreadable file yields the default
//! config; a present but malformed (or invalid) file is a hard error so typos don't silently fall
//! back to defaults.

use serde::Deserialize;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::paths::{non_empty, win_var};

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Base directory for the rebuildable cache (index + socket). `$RGX_CACHE_DIR` overrides this.
    pub cache_dir: Option<PathBuf>,
}

impl Config {
    /// The process-wide config, loaded once from disk.
    pub fn get() -> &'static Config {
        static CONFIG: OnceLock<Config> = OnceLock::new();
        CONFIG.get_or_init(load)
    }
}

fn load() -> Config {
    let path = config_path(
        non_empty(std::env::var_os("RGX_CONFIG")),
        non_empty(std::env::var_os("XDG_CONFIG_HOME").or_else(win_var("APPDATA"))),
        non_empty(std::env::var_os("HOME").or_else(win_var("USERPROFILE"))),
    );
    let Some(path) = path else {
        return Config::default();
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => return Config::default(),
    };
    let result = parse(&text).map_err(|e| e.to_string()).and_then(validate);
    result.unwrap_or_else(|e| {
        eprintln!("rgx: invalid config at {}: {e}", path.display());
        std::process::exit(2);
    })
}

/// Reject values that would misbehave downstream. `cache_dir` must be absolute: a relative (or
/// empty) base resolves against the cwd, which would put rgx's state inside the indexed repo.
fn validate(cfg: Config) -> Result<Config, String> {
    if let Some(dir) = &cfg.cache_dir
        && !dir.is_absolute()
    {
        return Err(format!("cache_dir must be an absolute path, got {:?}", dir));
    }
    Ok(cfg)
}

/// Where the config file lives, given the relevant environment.
pub fn config_path(
    rgx_config: Option<OsString>,
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(p) = rgx_config {
        return Some(PathBuf::from(p));
    }
    xdg_config_home
        .map(PathBuf::from)
        .or_else(|| home.map(|h| PathBuf::from(h).join(".config")))
        .map(|base| base.join("rgx").join("config.toml"))
}

/// Parse config text, rejecting unknown keys.
pub fn parse(text: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn config_path_precedence() {
        assert_eq!(
            config_path(os("/etc/rgx.toml"), os("/xdg"), os("/home/u")),
            Some(PathBuf::from("/etc/rgx.toml"))
        );
        assert_eq!(
            config_path(None, os("/xdg"), os("/home/u")),
            Some(PathBuf::from("/xdg/rgx/config.toml"))
        );
        assert_eq!(
            config_path(None, None, os("/home/u")),
            Some(PathBuf::from("/home/u/.config/rgx/config.toml"))
        );
        assert_eq!(config_path(None, None, None), None);
    }

    #[test]
    fn parses_cache_dir() {
        let cfg = parse("cache_dir = \"/tmp/rgx-cache\"").unwrap();
        assert_eq!(cfg.cache_dir, Some(PathBuf::from("/tmp/rgx-cache")));
    }

    #[test]
    fn empty_config_is_default() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn unknown_key_is_error() {
        assert!(parse("nope = 1").is_err());
    }

    #[test]
    fn validate_rejects_non_absolute_cache_dir() {
        let abs = parse("cache_dir = \"/tmp/c\"").unwrap();
        assert!(validate(abs).is_ok());
        let rel = parse("cache_dir = \"rel/c\"").unwrap();
        assert!(validate(rel).is_err());
        let empty = parse("cache_dir = \"\"").unwrap();
        assert!(validate(empty).is_err());
        assert!(validate(Config::default()).is_ok());
    }
}
