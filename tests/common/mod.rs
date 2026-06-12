//! Shared test helpers for differential checks against a real `rg`.
//!
//! The pinned ripgrep (see `mise.toml`) is on `PATH` under `mise run test`. When `rg` isn't available
//! (a bare `cargo test` on a machine without it), the locator returns `None` and the differential
//! tests skip rather than fail — so they never *require* ripgrep, but always run it in CI.

use std::path::Path;
use std::process::Command;

/// An `rg` Command with `RIPGREP_CONFIG_PATH` stripped: ripgrep reads flags (`--hidden`,
/// `--no-ignore`, …) from that file, but rgx's walk never does, so leaving it set would diverge the
/// oracle from rgx — silently masking or inventing failures. Everything else (global/parent gitignore)
/// is read identically by both sides, so it stays as-is.
fn rg_cmd() -> Command {
    let mut c = Command::new("rg");
    c.env_remove("RIPGREP_CONFIG_PATH");
    c
}

/// `Some("rg")` if a runnable `rg` is on `PATH`, else `None`.
pub fn rg() -> Option<&'static str> {
    match rg_cmd().arg("--version").output() {
        Ok(out) if out.status.success() => Some("rg"),
        _ => None,
    }
}

/// `rg --files` run with cwd `dir`: the files ripgrep would search, as sorted `/`-separated paths.
pub fn rg_files(dir: &Path) -> Vec<String> {
    let out = rg_cmd()
        .arg("--files")
        .current_dir(dir)
        .output()
        .expect("run rg --files");
    // exit 0 = files listed, 1 = none matched; anything else is a real failure.
    assert!(
        out.status.success() || out.status.code() == Some(1),
        "rg --files failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.replace('\\', "/"))
        .collect();
    v.sort();
    v
}
