//! `.gitignore` semantics for the in-process walk, implemented here so that
//! results do not depend on a git binary or on the operator's global
//! excludes. Supported: comments, blank lines, `!` negation, a trailing `/`
//! for directory-only patterns, a leading `/` or an inner `/` for patterns
//! anchored to the `.gitignore`'s directory, and the glob syntax of
//! [`super::glob`]. Later patterns win, as in git. `.git` is always ignored.

use std::fs;
use std::path::Path;

use super::glob::Glob;

#[derive(Debug, Clone)]
struct Rule {
    glob: Glob,
    negated: bool,
    directory_only: bool,
}

/// The rules of one `.gitignore`, matched against paths relative to its
/// directory.
#[derive(Debug, Clone, Default)]
pub(super) struct IgnoreFile {
    rules: Vec<Rule>,
}

impl IgnoreFile {
    /// Read `<directory>/.gitignore`; a missing or unreadable file is empty.
    /// The file is capped at 64 KiB so a hostile ignore file cannot stall
    /// the walk.
    pub(super) fn load(directory: &Path) -> Self {
        let path = directory.join(".gitignore");
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            return Self::default();
        };
        if !metadata.is_file() || metadata.len() > 64 * 1024 {
            return Self::default();
        }
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(_) => Self::default(),
        }
    }

    pub(super) fn parse(text: &str) -> Self {
        let mut rules = Vec::new();
        for raw in text.lines() {
            let line = raw.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (negated, body) = match line.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, line.strip_prefix('\\').unwrap_or(line)),
            };
            let (directory_only, body) = match body.strip_suffix('/') {
                Some(rest) => (true, rest),
                None => (false, body),
            };
            if body.is_empty() {
                continue;
            }
            // A pattern with a slash anywhere but the end is anchored to
            // this directory; a bare name matches at any depth below it.
            let anchored = body.contains('/');
            let source = if anchored {
                format!("/{}", body.trim_start_matches('/'))
            } else {
                body.to_string()
            };
            if let Some(glob) = Glob::new(&source) {
                rules.push(Rule {
                    glob,
                    negated,
                    directory_only,
                });
            }
        }
        Self { rules }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// `Some(true)` ignored, `Some(false)` explicitly re-included, `None`
    /// when no rule mentions the path. `relative` is `/`-separated and
    /// relative to this file's directory.
    fn decide(&self, relative: &str, is_dir: bool) -> Option<bool> {
        let mut verdict = None;
        for rule in &self.rules {
            if rule.directory_only && !is_dir {
                continue;
            }
            if rule.glob.matches(relative) {
                verdict = Some(!rule.negated);
            }
        }
        verdict
    }
}

/// The `.gitignore` files above the current directory of a walk, innermost
/// last. Each entry carries the depth at which it applies so paths can be
/// re-rooted to it.
#[derive(Debug, Clone, Default)]
pub(super) struct IgnoreStack {
    /// (directory relative to the walk root, rules)
    files: Vec<(String, IgnoreFile)>,
}

impl IgnoreStack {
    pub(super) fn push(&mut self, directory: &str, file: IgnoreFile) {
        if !file.is_empty() {
            self.files.push((directory.to_string(), file));
        }
    }

    pub(super) fn pop_to(&mut self, directory: &str) {
        self.files.retain(|(dir, _)| {
            dir.is_empty() || directory == dir || directory.starts_with(&format!("{dir}/"))
        });
    }

    /// Whether `relative` (to the walk root) is ignored. The innermost
    /// `.gitignore` that mentions the path decides; `.git` always is.
    pub(super) fn ignored(&self, relative: &str, is_dir: bool) -> bool {
        let name = relative.rsplit('/').next().unwrap_or(relative);
        if name == ".git" {
            return true;
        }
        for (dir, file) in self.files.iter().rev() {
            let scoped = if dir.is_empty() {
                relative
            } else {
                match relative
                    .strip_prefix(dir.as_str())
                    .and_then(|r| r.strip_prefix('/'))
                {
                    Some(rest) => rest,
                    None => continue,
                }
            };
            if let Some(ignored) = file.decide(scoped, is_dir) {
                return ignored;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitignore_rules_follow_git_semantics() {
        let file = IgnoreFile::parse(
            "# build output\ntarget/\n*.log\n!keep.log\n/root-only.txt\ndocs/*.tmp\nbuild\n",
        );
        let mut stack = IgnoreStack::default();
        stack.push("", file);
        assert!(stack.ignored("target", true));
        assert!(!stack.ignored("target", false), "directory-only pattern");
        assert!(stack.ignored("a/b/target", true));
        assert!(stack.ignored("x/y.log", false));
        assert!(!stack.ignored("x/keep.log", false), "negation wins later");
        assert!(stack.ignored("root-only.txt", false));
        assert!(!stack.ignored("sub/root-only.txt", false), "anchored");
        assert!(stack.ignored("docs/a.tmp", false));
        assert!(
            !stack.ignored("docs/deep/a.tmp", false),
            "single star stops at /"
        );
        assert!(stack.ignored("build", true));
        assert!(stack.ignored("src/build", false), "bare name at any depth");
        assert!(stack.ignored(".git", true), "always");
        assert!(!stack.ignored("src/main.rs", false));
    }

    #[test]
    fn nested_files_are_scoped_and_innermost_wins() {
        let mut stack = IgnoreStack::default();
        stack.push("", IgnoreFile::parse("*.gen\n"));
        stack.push("src", IgnoreFile::parse("!*.gen\nsecret/\n"));
        assert!(stack.ignored("lib/a.gen", false));
        assert!(!stack.ignored("src/a.gen", false), "inner re-include");
        assert!(stack.ignored("src/secret", true));
        assert!(!stack.ignored("other/secret", true), "scoped to src");
        stack.pop_to("other");
        assert!(stack.ignored("src/a.gen", false), "src rules popped");
    }
}
