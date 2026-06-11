//! Shared rendering for `rgx --server status` (and `watch`). Used by the daemon with live in-RAM
//! stats, and by the CLI when no daemon is running — in which case it still reports the on-disk
//! index location, size, and age, read straight from the snapshot file.

use std::path::Path;

/// Everything the status block can show. In-RAM fields are `None` when no daemon is running.
pub struct Status<'a> {
    pub root: &'a Path,
    pub snapshot: &'a Path,
    pub running: bool,
    /// "ready" or "building N / M files"; only when the daemon is running.
    pub state: Option<String>,
    pub files: Option<usize>,
    pub trigrams: Option<usize>,
    pub memory_bytes: Option<u64>,
}

impl Status<'_> {
    pub fn render(&self) -> String {
        let row = |label: &str, value: &str| format!("  {label:<9} {value}\n");
        let mut s = String::from("rgx index status\n\n");
        s.push_str(&row("root", &self.root.display().to_string()));

        // Daemon up -> show live state; daemon down -> say so. Stats are shown either way (loaded
        // from the snapshot when there's no daemon).
        if self.running {
            if let Some(state) = &self.state {
                s.push_str(&row("state", state));
            }
        } else {
            s.push_str(&row("daemon", "not running (run a search to start it)"));
        }
        if let Some(f) = self.files {
            s.push_str(&row("files", &human_count(f as u64)));
        }
        if let Some(t) = self.trigrams {
            s.push_str(&row("trigrams", &human_count(t as u64)));
        }
        if let Some(m) = self.memory_bytes {
            s.push_str(&row("index", &human_bytes(m)));
        }

        // On-disk snapshot: size + last-sync age, then its location — shown even with no daemon.
        match std::fs::metadata(self.snapshot) {
            Ok(m) => {
                let age = m
                    .modified()
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .map(|d| format!("last synced {} ago", human_duration(d.as_secs())))
                    .unwrap_or_else(|| "on disk".into());
                s.push_str(&row(
                    "snapshot",
                    &format!("{} ({age})", human_bytes(m.len())),
                ));
            }
            Err(_) => s.push_str(&row("snapshot", "not built yet")),
        }
        s.push_str(&format!("            {}\n", self.snapshot.display()));
        s
    }
}

/// Human-friendly counts: `758`, `93.6k`, `1.5m` (one decimal, lowercase k/m suffixes).
pub fn human_count(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{:.1}m", n as f64 / 1_000_000.0)
    }
}

pub fn human_bytes(n: u64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn human_duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{}s", secs / 60, secs % 60),
        3600..=86399 => format!("{}h{}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d{}h", secs / 86400, (secs % 86400) / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_use_k_and_m_suffixes() {
        assert_eq!(human_count(758), "758");
        assert_eq!(human_count(93_596), "93.6k");
        assert_eq!(human_count(549_600), "549.6k");
        assert_eq!(human_count(1_500_000), "1.5m");
    }

    #[test]
    fn no_daemon_status_shows_snapshot_location() {
        let block = Status {
            root: Path::new("/repo"),
            snapshot: Path::new("/cache/rgx/abc/index.bin"),
            running: false,
            state: None,
            files: None,
            trigrams: None,
            memory_bytes: None,
        }
        .render();
        assert!(block.contains("daemon    not running"));
        assert!(block.contains("/cache/rgx/abc/index.bin"));
        assert!(block.contains("not built yet")); // file doesn't exist in test
    }
}
