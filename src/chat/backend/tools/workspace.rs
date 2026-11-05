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
    /// lexical join, so no symbolic link is followed, inside or outside.
    /// The remaining window between this check and the read is accepted for
    /// read-only tools; a sandbox, not this check, contains a hostile process.
    pub(super) fn resolve(&self, normalized: &str) -> std::result::Result<PathBuf, &'static str> {
        let joined = if normalized.is_empty() {
            self.root.clone()
        } else {
            self.root.join(normalized)
        };
        let canonical = joined
            .canonicalize()
            .map_err(|_| "path does not exist or cannot be read")?;
        if canonical != joined || !canonical.starts_with(&self.root) {
            return Err("path is not a canonical path inside the working directory; symbolic links are not followed");
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
