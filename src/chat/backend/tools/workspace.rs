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

    /// Whether a canonical-or-lexical absolute path lies in one of ahi's own
    /// directories. Used by the walk to prune without canonicalizing every
    /// entry; `resolve` remains the check before any read.
    pub(super) fn is_excluded(&self, absolute: &Path) -> bool {
        self.excluded.iter().any(|dir| absolute.starts_with(dir))
    }

    /// Resolve a path for creation or replacement. The parent directory must
    /// resolve like any read path. Returns the on-disk path and whether a
    /// regular file is already there; a symbolic link or directory at the
    /// path is refused, so nothing is ever written through a link.
    pub(super) fn resolve_for_write(
        &self,
        normalized: &str,
    ) -> std::result::Result<(PathBuf, bool), &'static str> {
        let (parent, name) = normalized
            .rsplit_once('/')
            .map_or(("", normalized), |(parent, name)| (parent, name));
        if name.is_empty() {
            return Err("a file path is required");
        }
        let directory = self.resolve(parent)?;
        if !directory.is_dir() {
            return Err("parent is not a directory");
        }
        let path = directory.join(name);
        match std::fs::symlink_metadata(&path) {
            Err(_) => Ok((path, false)),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Err("path is a symbolic link; writes never follow links")
            }
            Ok(metadata) if metadata.is_file() => {
                // Existing file: the full read-path check applies, so a
                // case variant of a real name is refused here too.
                self.resolve(normalized)?;
                Ok((path, true))
            }
            Ok(_) => Err("path is not a regular file"),
        }
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
