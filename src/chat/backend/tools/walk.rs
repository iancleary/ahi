//! Bounded, deterministic enumeration of the files under a workspace
//! directory: sorted paths, no symbolic links followed, `.gitignore`
//! honored, ahi's own directories skipped, and a hard cap on entries
//! examined so a huge tree fails with a clear error instead of stalling.
//!
//! Layout is a structure of arrays: every relative path lives in one
//! arena, and each entry is one `u64` span word (arena offset, byte length,
//! and the directory and symlink flags in the top bits). No per-entry heap
//! objects, no hash tables, and nothing is cached between calls. Top-level
//! subdirectories are walked in parallel; one final sort of the span words
//! by their path bytes makes the result identical however the work split.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use super::ignore::{IgnoreFile, IgnoreStack};
use super::workspace::Workspace;

/// Entries examined (files and directories) before the walk gives up.
pub(super) const MAX_SCANNED: usize = 200_000;
/// Arena bytes a single walk may hold (well above `MAX_SCANNED` paths of
/// ordinary length); the span word reserves 40 bits for the offset.
const MAX_ARENA: usize = 1 << 30;
/// Longest relative path stored; the span word reserves 22 bits.
const MAX_PATH_LEN: usize = (1 << 22) - 1;
/// Top-level directories per thread below which one thread is used.
const PARALLEL_FROM: usize = 2;

const DIR_BIT: u64 = 1 << 63;
const SYMLINK_BIT: u64 = 1 << 62;
const LEN_SHIFT: u32 = 40;
const LEN_MASK: u64 = (1 << 22) - 1;
const OFFSET_MASK: u64 = (1 << 40) - 1;

#[inline]
fn span(offset: usize, len: usize, is_dir: bool, is_symlink: bool) -> u64 {
    (offset as u64 & OFFSET_MASK)
        | ((len as u64 & LEN_MASK) << LEN_SHIFT)
        | if is_dir { DIR_BIT } else { 0 }
        | if is_symlink { SYMLINK_BIT } else { 0 }
}

/// The result of one walk. Paths are `/`-separated and relative to the
/// workspace root, in ascending byte order.
#[derive(Debug, Default)]
pub(super) struct Tree {
    arena: String,
    spans: Vec<u64>,
}

/// One entry viewed through the arena; nothing is copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Entry<'a> {
    pub(super) relative: &'a str,
    pub(super) is_dir: bool,
    pub(super) is_symlink: bool,
}

impl Tree {
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.spans.len()
    }

    #[inline]
    fn decode(&self, word: u64) -> Entry<'_> {
        let offset = (word & OFFSET_MASK) as usize;
        let len = ((word >> LEN_SHIFT) & LEN_MASK) as usize;
        Entry {
            relative: &self.arena[offset..offset + len],
            is_dir: word & DIR_BIT != 0,
            is_symlink: word & SYMLINK_BIT != 0,
        }
    }

    pub(super) fn entries(&self) -> impl Iterator<Item = Entry<'_>> + '_ {
        self.spans.iter().map(|word| self.decode(*word))
    }

    /// Regular files only (not directories, not symbolic links).
    pub(super) fn files(&self) -> impl Iterator<Item = &str> + '_ {
        self.spans
            .iter()
            .filter(|word| **word & (DIR_BIT | SYMLINK_BIT) == 0)
            .map(|word| self.decode(*word).relative)
    }

    fn push(&mut self, relative: &str, is_dir: bool, is_symlink: bool) -> Result<(), &'static str> {
        if relative.len() > MAX_PATH_LEN || self.arena.len() + relative.len() > MAX_ARENA {
            return Err("too many files under this directory; narrow the path");
        }
        let offset = self.arena.len();
        self.arena.push_str(relative);
        self.spans
            .push(span(offset, relative.len(), is_dir, is_symlink));
        Ok(())
    }

    fn append(&mut self, other: Tree) {
        let base = self.arena.len();
        self.arena.push_str(&other.arena);
        self.spans
            .extend(other.spans.into_iter().map(|word| word + base as u64));
    }

    fn sort(&mut self) {
        let arena = &self.arena;
        let bytes = |word: &u64| {
            let offset = (*word & OFFSET_MASK) as usize;
            let len = ((*word >> LEN_SHIFT) & LEN_MASK) as usize;
            &arena.as_bytes()[offset..offset + len]
        };
        self.spans.sort_unstable_by(|a, b| bytes(a).cmp(bytes(b)));
    }
}

/// Build the on-disk path of an entry into `buffer` by pushing components
/// onto the canonical workspace root. No symbolic link is followed because
/// the walk never descended through one.
pub(super) fn absolute(workspace: &Workspace, relative: &str, buffer: &mut PathBuf) {
    buffer.clear();
    buffer.push(workspace.root());
    for part in relative.split('/').filter(|part| !part.is_empty()) {
        buffer.push(part);
    }
}

/// Every non-ignored entry below `directory` (a resolved workspace path
/// whose relative form is `relative`). Symbolic links are reported but never
/// entered. `.gitignore` files above `directory` are not consulted: a search
/// rooted in a subdirectory sees what that subdirectory's own ignore files
/// say.
pub(super) fn walk(
    workspace: &Workspace,
    directory: &Path,
    relative: &str,
) -> Result<Tree, &'static str> {
    let scanned = AtomicUsize::new(0);
    let root_rules = IgnoreFile::load(directory);
    let mut ignore = IgnoreStack::default();
    ignore.push("", root_rules.clone());

    // The top level is read once, here; its subdirectories are the units of
    // parallel work.
    let mut tree = Tree::default();
    let mut children = Vec::new();
    read_children(directory, "", &ignore, workspace, &scanned, &mut children)?;
    let mut subdirectories: Vec<(String, PathBuf)> = Vec::new();
    for child in children.drain(..) {
        tree.push(&join(relative, &child.rel), child.is_dir, child.is_symlink)?;
        if child.is_dir && !child.is_symlink {
            subdirectories.push((child.rel, child.abs));
        }
    }

    let threads = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, 16);
    if subdirectories.len() < PARALLEL_FROM * 2 || threads == 1 {
        let mut walker = Walker::new(workspace, relative, &root_rules, &scanned);
        for (rel, abs) in subdirectories {
            walker.descend(rel, abs)?;
        }
        tree.append(walker.tree);
    } else {
        // Static split of the top-level directories; each thread walks its
        // share depth-first with its own ignore stack. Sorting afterwards
        // removes any trace of the split.
        let per_thread = subdirectories.len().div_ceil(threads).max(PARALLEL_FROM);
        let parts = thread::scope(|scope| {
            let handles: Vec<_> = subdirectories
                .chunks(per_thread)
                .map(|chunk| {
                    let root_rules = &root_rules;
                    let scanned = &scanned;
                    scope.spawn(move || -> Result<Tree, &'static str> {
                        let mut walker = Walker::new(workspace, relative, root_rules, scanned);
                        for (rel, abs) in chunk {
                            walker.descend(rel.clone(), abs.clone())?;
                        }
                        Ok(walker.tree)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().map_err(|_| "walk thread failed")?)
                .collect::<Result<Vec<Tree>, &'static str>>()
        })?;
        for part in parts {
            tree.append(part);
        }
    }
    tree.sort();
    Ok(tree)
}

struct Child {
    rel: String,
    abs: PathBuf,
    is_dir: bool,
    is_symlink: bool,
}

/// One thread's depth-first walk below a set of top-level directories.
struct Walker<'a> {
    workspace: &'a Workspace,
    relative: &'a str,
    scanned: &'a AtomicUsize,
    ignore: IgnoreStack,
    tree: Tree,
    children: Vec<Child>,
    stack: Vec<(String, PathBuf)>,
}

impl<'a> Walker<'a> {
    fn new(
        workspace: &'a Workspace,
        relative: &'a str,
        root_rules: &IgnoreFile,
        scanned: &'a AtomicUsize,
    ) -> Self {
        let mut ignore = IgnoreStack::default();
        ignore.push("", root_rules.clone());
        Self {
            workspace,
            relative,
            scanned,
            ignore,
            tree: Tree::default(),
            children: Vec::new(),
            stack: Vec::new(),
        }
    }

    fn descend(&mut self, rel: String, abs: PathBuf) -> Result<(), &'static str> {
        self.stack.push((rel, abs));
        while let Some((rel_dir, abs_dir)) = self.stack.pop() {
            self.ignore.pop_to(&rel_dir);
            self.ignore.push(&rel_dir, IgnoreFile::load(&abs_dir));
            read_children(
                &abs_dir,
                &rel_dir,
                &self.ignore,
                self.workspace,
                self.scanned,
                &mut self.children,
            )?;
            for child in self.children.drain(..) {
                self.tree.push(
                    &join(self.relative, &child.rel),
                    child.is_dir,
                    child.is_symlink,
                )?;
                if child.is_dir && !child.is_symlink {
                    self.stack.push((child.rel, child.abs));
                }
            }
        }
        Ok(())
    }
}

/// Read one directory into `out` (cleared first), applying the ignore rules
/// and the scan cap. Names that cannot be a policy key (non-UTF-8, control
/// characters, or a `/`) are skipped.
fn read_children(
    abs_dir: &Path,
    rel_dir: &str,
    ignore: &IgnoreStack,
    workspace: &Workspace,
    scanned: &AtomicUsize,
    out: &mut Vec<Child>,
) -> Result<(), &'static str> {
    out.clear();
    let read = fs::read_dir(abs_dir).map_err(|_| "cannot read a directory in the tree")?;
    for entry in read {
        if scanned.fetch_add(1, Ordering::Relaxed) + 1 > MAX_SCANNED {
            return Err("too many files under this directory; narrow the path");
        }
        let entry = entry.map_err(|_| "cannot read a directory in the tree")?;
        let kind = entry
            .file_type()
            .map_err(|_| "cannot read a directory in the tree")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.contains('/') || name.chars().any(char::is_control) {
            continue;
        }
        let rel = join(rel_dir, name);
        let is_symlink = kind.is_symlink();
        let is_dir = kind.is_dir();
        if ignore.ignored(&rel, is_dir) {
            continue;
        }
        let abs = entry.path();
        if is_dir && workspace.is_excluded(&abs) {
            continue;
        }
        out.push(Child {
            rel,
            abs,
            is_dir,
            is_symlink,
        });
    }
    Ok(())
}

fn join(base: &str, rest: &str) -> String {
    if base.is_empty() {
        rest.to_string()
    } else {
        let mut joined = String::with_capacity(base.len() + 1 + rest.len());
        joined.push_str(base);
        joined.push('/');
        joined.push_str(rest);
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_words_round_trip_offset_length_and_flags() {
        let mut tree = Tree::default();
        tree.push("a/b", true, false).unwrap();
        tree.push("c", false, true).unwrap();
        tree.push("", false, false).unwrap();
        let entries: Vec<_> = tree.entries().collect();
        assert_eq!(
            entries,
            [
                Entry {
                    relative: "a/b",
                    is_dir: true,
                    is_symlink: false
                },
                Entry {
                    relative: "c",
                    is_dir: false,
                    is_symlink: true
                },
                Entry {
                    relative: "",
                    is_dir: false,
                    is_symlink: false
                },
            ]
        );
        assert_eq!(tree.files().collect::<Vec<_>>(), [""]);
        tree.sort();
        assert_eq!(
            tree.entries().map(|e| e.relative).collect::<Vec<_>>(),
            ["", "a/b", "c"]
        );
        let mut other = Tree::default();
        other.push("z", false, false).unwrap();
        tree.append(other);
        assert_eq!(tree.len(), 4);
        assert_eq!(tree.entries().last().unwrap().relative, "z");
    }
}
