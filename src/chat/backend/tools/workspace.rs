//! Containment of tool paths to the working directory.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub(in crate::chat::backend) struct Workspace {
    root: PathBuf,
    /// ahi's own state (credentials, `models.json`, catalogs). Never readable,
    /// even when it lies inside the working directory.
    excluded: Vec<PathBuf>,
}

impl Workspace {
    pub(in crate::chat::backend) fn open(
        directory: &Path,
        state: &crate::directories::Directories,
    ) -> Result<Self> {
        let root = directory
            .canonicalize()
            .map_err(|_| Error::new("cannot resolve the working directory for tools"))?;
        // Every ahi directory is off limits, whichever category it holds.
        let excluded = [&state.config, &state.data, &state.state, &state.cache]
            .into_iter()
            .filter_map(|dir| dir.canonicalize().ok())
            .collect();
        Ok(Self { root, excluded })
    }

    pub(in crate::chat::backend) fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a normalized relative path. The canonical result must equal the
    /// lexical join, so no symbolic link is followed, inside or outside, and a
    /// differently cased spelling of a real path (which `realpath` corrects on
    /// case-insensitive filesystems) is refused rather than matched by policy.
    /// The remaining window between this check and the read is accepted for
    /// read-only tools; a sandbox, not this check, contains a hostile process.
    pub(super) fn resolve(&self, normalized: &str) -> std::result::Result<PathBuf, &'static str> {
        // Push components one at a time: a Windows verbatim (`\\?\`) root
        // keeps a joined `/` literally, which would never match canonically.
        let mut joined = self.root.clone();
        for part in normalized.split('/').filter(|part| !part.is_empty()) {
            joined.push(part);
        }
        let canonical = joined
            .canonicalize()
            .map_err(|_| "path does not exist or cannot be read")?;
        if canonical != joined || !canonical.starts_with(&self.root) {
            return Err("path must be spelled exactly as on disk inside the working directory; symbolic links and case variants are refused");
        }
        if self.excluded.iter().any(|dir| canonical.starts_with(dir)) {
            return Err("path is inside ahi's own directories, which tools cannot read");
        }
        Ok(canonical)
    }
}
