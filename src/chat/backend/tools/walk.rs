//! Bounded, deterministic enumeration of the files under a workspace
//! directory: sorted names, no symbolic links followed, `.gitignore`
//! honored, ahi's own directories skipped, and a hard cap on entries
//! examined so a huge tree fails with a clear error instead of stalling.

use std::fs;
use std::path::Path;

use super::ignore::{IgnoreFile, IgnoreStack};
use super::workspace::Workspace;

/// Entries examined (files and directories) before the walk gives up.
pub(super) const MAX_SCANNED: usize = 200_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entry {
    /// `/`-separated path relative to the workspace root.
    pub(super) relative: String,
    pub(super) is_dir: bool,
    pub(super) is_symlink: bool,
}

/// Every non-ignored entry below `directory` (a resolved workspace path
/// whose relative form is `relative`), depth-first in sorted order. Symbolic
/// links are reported but never entered. `.gitignore` files above
/// `directory` are not consulted: a search rooted in a subdirectory sees
/// what that subdirectory's own ignore files say.
pub(super) fn walk(
    workspace: &Workspace,
    directory: &Path,
    relative: &str,
) -> Result<Vec<Entry>, &'static str> {
    let mut out = Vec::new();
    let mut ignore = IgnoreStack::default();
    ignore.push("", IgnoreFile::load(directory));
    let mut scanned = 0usize;
    // Explicit stack of (relative dir, absolute dir); children are pushed in
    // reverse sorted order so they pop in sorted order.
    let mut stack = vec![(String::new(), directory.to_path_buf())];
    while let Some((rel_dir, abs_dir)) = stack.pop() {
        ignore.pop_to(&rel_dir);
        if !rel_dir.is_empty() {
            ignore.push(&rel_dir, IgnoreFile::load(&abs_dir));
        }
        let read = fs::read_dir(&abs_dir).map_err(|_| "cannot read a directory in the tree")?;
        let mut children = Vec::new();
        for entry in read {
            scanned += 1;
            if scanned > MAX_SCANNED {
                return Err("too many files under this directory; narrow the path");
            }
            let entry = entry.map_err(|_| "cannot read a directory in the tree")?;
            let kind = entry
                .file_type()
                .map_err(|_| "cannot read a directory in the tree")?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue; // not representable; a policy key must be exact
            };
            if name.contains('/') || name.chars().any(char::is_control) {
                continue;
            }
            let child_rel = if rel_dir.is_empty() {
                name.to_string()
            } else {
                format!("{rel_dir}/{name}")
            };
            let is_symlink = kind.is_symlink();
            let is_dir = kind.is_dir();
            if ignore.ignored(&child_rel, is_dir) {
                continue;
            }
            let abs = entry.path();
            if is_dir && workspace.is_excluded(&abs) {
                continue;
            }
            children.push((child_rel, abs, is_dir, is_symlink));
        }
        children.sort_by(|a, b| a.0.cmp(&b.0));
        for (child_rel, abs, is_dir, is_symlink) in children.iter().rev() {
            if *is_dir && !*is_symlink {
                stack.push((child_rel.clone(), abs.clone()));
            }
        }
        for (child_rel, _, is_dir, is_symlink) in children {
            out.push(Entry {
                relative: join(relative, &child_rel),
                is_dir,
                is_symlink,
            });
        }
    }
    // Depth-first order from an explicit stack interleaves siblings and
    // descendants; present one sorted list.
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(out)
}

fn join(base: &str, rest: &str) -> String {
    if base.is_empty() {
        rest.to_string()
    } else {
        format!("{base}/{rest}")
    }
}
