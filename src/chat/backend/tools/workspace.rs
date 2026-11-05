//! Containment of tool paths to the working directory.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub(in crate::chat::backend) struct Workspace {
    root: PathBuf,
    /// ahi's own state (credentials, `models.json`, catalogs). Never readable,
    /// even when it lies inside the working directory.
    excluded: Option<PathBuf>,
}

impl Workspace {
    pub(in crate::chat::backend) fn open(directory: &Path, state: &Path) -> Result<Self> {
        let root = directory
            .canonicalize()
            .map_err(|_| Error::new("cannot resolve the working directory for tools"))?;
        Ok(Self {
            root,
            excluded: state.canonicalize().ok(),
        })
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
        if self
            .excluded
            .as_ref()
            .is_some_and(|state| canonical.starts_with(state))
        {
            return Err("path is inside ahi's state directory, which tools cannot read");
        }
        Ok(canonical)
    }
}
