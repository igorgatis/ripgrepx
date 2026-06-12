//! File filtering for `-g`/`--glob` and `-t`/`-T`/`--type[-not]`: narrowing *which* files are
//! searched, exactly as ripgrep does — these are ripgrep's own `ignore` matchers (`overrides` for
//! globs, `types` for the built-in type registry), so the resulting file set matches `rg`'s.
//!
//! Unlike `--hidden`/`--no-ignore` (which add files and force a full scan), filters only *remove*
//! files, so the trigram index stays in play: the candidate set is filtered down (`FileFilter`), and
//! the fallback walk gets the same matchers (`configure_walk`). The raw `FilterSpec` is what travels
//! over the daemon wire and in the pagination cursor; it compiles to the matchers on each side.

use std::path::Path;

use anyhow::Result;
use ignore::WalkBuilder;
use ignore::overrides::{Override, OverrideBuilder};
use ignore::types::{Types, TypesBuilder};

/// The raw filter request: repeatable `-g` globs (a leading `!` negates) and `-t`/`-T` type names.
/// Serializable (wire + cursor); compiles to matchers via [`FilterSpec::compile`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterSpec {
    pub globs: Vec<String>,
    pub types: Vec<String>,
    pub type_nots: Vec<String>,
}

impl FilterSpec {
    pub fn is_empty(&self) -> bool {
        self.globs.is_empty() && self.types.is_empty() && self.type_nots.is_empty()
    }

    fn build_override(&self, root: &Path) -> Result<Option<Override>> {
        if self.globs.is_empty() {
            return Ok(None);
        }
        let mut b = OverrideBuilder::new(root);
        for g in &self.globs {
            b.add(g)?;
        }
        Ok(Some(b.build()?))
    }

    fn build_types(&self) -> Result<Option<Types>> {
        if self.types.is_empty() && self.type_nots.is_empty() {
            return Ok(None);
        }
        let mut b = TypesBuilder::new();
        b.add_defaults();
        for t in &self.types {
            b.select(t);
        }
        for t in &self.type_nots {
            b.negate(t);
        }
        Ok(Some(b.build()?))
    }

    /// Compile to a [`FileFilter`] for filtering a candidate set (the index path). `root` anchors the
    /// globs. Errors on a bad glob or an unknown `-t` type name (same as ripgrep).
    pub fn compile(&self, root: &Path) -> Result<FileFilter> {
        Ok(FileFilter {
            ov: self.build_override(root)?,
            types: self.build_types()?,
        })
    }

    /// Apply the same matchers to a `WalkBuilder` (the fallback/full-scan path), so the walk yields
    /// exactly the files `rg` would for these flags.
    pub fn configure_walk(&self, root: &Path, wb: &mut WalkBuilder) -> Result<()> {
        if let Some(ov) = self.build_override(root)? {
            wb.overrides(ov);
        }
        if let Some(types) = self.build_types()? {
            wb.types(types);
        }
        Ok(())
    }
}

/// Compiled matchers that decide whether a candidate file survives the filter.
pub struct FileFilter {
    ov: Option<Override>,
    types: Option<Types>,
}

impl FileFilter {
    /// Whether `path` (a file) passes the filter: not glob-excluded and not type-excluded. Mirrors the
    /// walk's file-level decision (the index candidates already cleared gitignore/hidden at build).
    pub fn matched(&self, path: &Path) -> bool {
        if let Some(ov) = &self.ov
            && ov.matched(path, false).is_ignore()
        {
            return false;
        }
        if let Some(types) = &self.types
            && types.matched(path, false).is_ignore()
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn type_filter_keeps_only_selected() {
        let spec = FilterSpec {
            types: vec!["rust".into()],
            ..Default::default()
        };
        let f = spec.compile(Path::new("/repo")).unwrap();
        assert!(f.matched(&p("/repo/src/main.rs")));
        assert!(!f.matched(&p("/repo/src/main.py")));
    }

    #[test]
    fn type_not_excludes_selected() {
        let spec = FilterSpec {
            type_nots: vec!["rust".into()],
            ..Default::default()
        };
        let f = spec.compile(Path::new("/repo")).unwrap();
        assert!(!f.matched(&p("/repo/a.rs")));
        assert!(f.matched(&p("/repo/a.py")));
    }

    #[test]
    fn glob_include_and_negate() {
        let inc = FilterSpec {
            globs: vec!["*.rs".into()],
            ..Default::default()
        };
        let f = inc.compile(Path::new("/repo")).unwrap();
        assert!(f.matched(&p("/repo/a.rs")));
        assert!(!f.matched(&p("/repo/a.txt")));

        let exc = FilterSpec {
            globs: vec!["!*.rs".into()],
            ..Default::default()
        };
        let f = exc.compile(Path::new("/repo")).unwrap();
        assert!(!f.matched(&p("/repo/a.rs")));
        assert!(f.matched(&p("/repo/a.txt")));
    }

    #[test]
    fn empty_filter_passes_everything() {
        let f = FilterSpec::default().compile(Path::new("/repo")).unwrap();
        assert!(f.matched(&p("/repo/anything.xyz")));
        assert!(FilterSpec::default().is_empty());
    }

    #[test]
    fn unknown_type_errors() {
        let spec = FilterSpec {
            types: vec!["bogustype".into()],
            ..Default::default()
        };
        assert!(spec.compile(Path::new("/repo")).is_err());
    }
}
