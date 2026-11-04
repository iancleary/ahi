//! Working directory, Git HEAD, and a read-only worktree-status snapshot.
//!
//! HEAD is read directly, including worktree `.git` indirection. Change counts
//! use Git's NUL-delimited porcelain format, without optional index writes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GitChanges {
    pub conflicted: usize,
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitStatus {
    pub label: String,
    /// None means status could not be read, not that the worktree is clean.
    pub changes: Option<GitChanges>,
}

/// Nerd Font platform logo, read once when a chat screen is created.
pub(crate) fn os_icon() -> &'static str {
    if cfg!(target_os = "macos") {
        ""
    } else if cfg!(target_os = "windows") {
        ""
    } else if cfg!(target_os = "linux") {
        let arch = fs::read_to_string("/etc/os-release")
            .map(|release| {
                release
                    .lines()
                    .any(|line| matches!(line, "ID=arch" | "ID=\"arch\""))
            })
            .unwrap_or(false);
        if arch {
            ""
        } else {
            ""
        }
    } else {
        ""
    }
}

/// Directory display, with home shortened to `~`, and the current Git snapshot.
pub(crate) fn current() -> (String, Option<GitStatus>) {
    let directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    (display_directory(&directory), read_git_status(&directory))
}

fn read_git_status(start: &Path) -> Option<GitStatus> {
    let label = read_git_label(start)?;
    let changes = Command::new("git")
        .args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ])
        .current_dir(start)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| parse_changes(&output.stdout));
    Some(GitStatus { label, changes })
}

fn parse_changes(porcelain: &[u8]) -> Option<GitChanges> {
    let mut changes = GitChanges::default();
    let mut entries = porcelain.split(|byte| *byte == 0);
    while let Some(entry) = entries.next() {
        if entry.is_empty() {
            continue;
        }
        let [x, y, b' ', _, ..] = entry else {
            return None;
        };
        match [*x, *y] {
            [b'?', b'?'] => changes.untracked += 1,
            [b'!', b'!'] => {}
            [b'D', b'D']
            | [b'A', b'U']
            | [b'U', b'D']
            | [b'U', b'A']
            | [b'D', b'U']
            | [b'A', b'A']
            | [b'U', b'U'] => changes.conflicted += 1,
            _ if b" MADRCT".contains(x) && b" MADRCT".contains(y) => {
                changes.staged += usize::from(*x != b' ');
                changes.unstaged += usize::from(*y != b' ');
            }
            _ => return None,
        }
        // Renames/copies have a second NUL-delimited pathname. Neither path
        // needs UTF-8 decoding, and spaces/newlines in filenames stay opaque.
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            entries.next().filter(|path| !path.is_empty())?;
        }
    }
    Some(changes)
}

/// Branch name, or a 7-character commit id when `HEAD` is detached.
pub(crate) fn read_git_label(start: &Path) -> Option<String> {
    let git_dir = find_git_directory(start)?;
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref: ") {
        let name = reference
            .trim()
            .strip_prefix("refs/heads/")
            .unwrap_or(reference.trim());
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    } else if head.len() >= 7 && head.chars().all(|character| character.is_ascii_hexdigit()) {
        Some(head.chars().take(7).collect())
    } else {
        None
    }
}

fn display_directory(path: &Path) -> String {
    if let Some(home) = home_directory() {
        if let Ok(rest) = path.strip_prefix(&home) {
            let rest = rest.display().to_string();
            if rest.is_empty() {
                return "~".to_string();
            }
            return format!("~{}{rest}", std::path::MAIN_SEPARATOR);
        }
    }
    path.display().to_string()
}

fn home_directory() -> Option<PathBuf> {
    for name in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(name) {
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }
    None
}

fn find_git_directory(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(directory) = current {
        let dot_git = directory.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        if dot_git.is_file() {
            return git_directory_from_file(&dot_git);
        }
        current = directory.parent();
    }
    None
}

fn git_directory_from_file(dot_git: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(dot_git).ok()?;
    let line = text.lines().find(|line| line.starts_with("gitdir:"))?;
    let path = line["gitdir:".len()..].trim();
    let git_dir = PathBuf::from(path);
    let parent = dot_git.parent()?;
    if git_dir.is_absolute() {
        Some(git_dir)
    } else {
        Some(parent.join(git_dir))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn location_row_shows_branch_detached_commit_or_directory_only() {
        let repo = crate::fixture_directory::create_unique_fixture_directory("chat-git");
        git(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("file"), "a\n").unwrap();
        git(&repo, &["add", "file"]);
        git(&repo, &["commit", "-m", "init"]);
        let branch = read_git_status(&repo);
        assert_eq!(
            branch,
            Some(GitStatus {
                label: "main".to_string(),
                changes: Some(GitChanges::default())
            })
        );
        let row = crate::chat::status::render_location_row(80, "", "/tmp/ahi", branch.as_ref())
            .to_string();
        assert_eq!(row, "  /tmp/ahi on   main");

        let sha = git_stdout(&repo, &["rev-parse", "HEAD"]);
        let short = sha.trim().chars().take(7).collect::<String>();
        git(&repo, &["checkout", "--detach", "HEAD"]);
        assert_eq!(read_git_label(&repo).as_deref(), Some(short.as_str()));
        let detached = crate::chat::status::render_location_row(
            80,
            "",
            "/tmp/ahi",
            read_git_status(&repo).as_ref(),
        )
        .to_string();
        assert_eq!(detached, format!("  /tmp/ahi on   {short}"));

        let outside = crate::fixture_directory::create_unique_fixture_directory("chat-nogit");
        assert!(read_git_label(&outside).is_none());
        let row = crate::chat::status::render_location_row(80, "", "/tmp/plain", None).to_string();
        assert_eq!(row, "  /tmp/plain");

        let shortened = crate::chat::status::render_location_row(
            12,
            "",
            "/home/user/src/ahi",
            branch.as_ref(),
        )
        .to_string();
        assert!(shortened.starts_with('…'), "{shortened}");
        assert!(shortened.ends_with(" main"), "{shortened}");
        assert_eq!(shortened.chars().count(), 12);

        let work = repo.join("work");
        fs::create_dir(&work).unwrap();
        fs::write(work.join(".git"), "gitdir: ../.git\n").unwrap();
        assert_eq!(read_git_label(&work).as_deref(), Some(short.as_str()));
        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn git_snapshot_counts_changes_and_tracks_branch_changes_without_index_writes() {
        let repo = crate::fixture_directory::create_unique_fixture_directory("chat-git-status");
        git(&repo, &["init", "-b", "main"]);
        for name in ["both", "gone", "old name"] {
            fs::write(repo.join(name), "base\n").unwrap();
        }
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "base"]);
        fs::write(repo.join("both"), "staged\n").unwrap();
        git(&repo, &["add", "both"]);
        fs::write(repo.join("both"), "working\n").unwrap();
        fs::remove_file(repo.join("gone")).unwrap();
        git(&repo, &["mv", "old name", "new name"]);
        fs::write(repo.join("untracked file"), "new\n").unwrap();
        let index_before = fs::read(repo.join(".git/index")).unwrap();
        let snapshot = read_git_status(&repo).unwrap();
        assert_eq!(
            snapshot,
            GitStatus {
                label: "main".to_string(),
                changes: Some(GitChanges {
                    staged: 2,
                    unstaged: 2,
                    untracked: 1,
                    conflicted: 0
                })
            }
        );
        assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index_before);
        assert_eq!(
            crate::chat::status::render_location_row(100, "", "/tmp/ahi", Some(&snapshot))
                .to_string(),
            "  /tmp/ahi on   main +2 !2 ?1"
        );
        git(&repo, &["checkout", "-b", "tool-branch"]);
        let next = read_git_status(&repo).unwrap();
        assert_eq!(next.label, "tool-branch");
        assert_eq!(
            next.changes,
            Some(GitChanges {
                staged: 2,
                unstaged: 2,
                untracked: 1,
                conflicted: 0
            })
        );
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "clean"]);
        assert_eq!(
            read_git_status(&repo),
            Some(GitStatus {
                label: "tool-branch".to_string(),
                changes: Some(GitChanges::default())
            })
        );
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn porcelain_handles_conflicts_rename_paths_and_unusual_filename_bytes() {
        assert_eq!(
            parse_changes(b"UU conflict\0AA added\0DD deleted\0"),
            Some(GitChanges {
                conflicted: 3,
                ..GitChanges::default()
            })
        );
        assert_eq!(
            parse_changes(b"R  new\nname\0old\nname\0 M modified\0?? non-utf8-\xff\0"),
            Some(GitChanges {
                staged: 1,
                unstaged: 1,
                untracked: 1,
                conflicted: 0
            })
        );
        assert_eq!(parse_changes(b"R  missing-source\0"), None);
        assert_eq!(parse_changes(b"not porcelain\0"), None);
        assert_eq!(parse_changes(b""), Some(GitChanges::default()));
    }

    fn git(directory: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(directory)
            .env("GIT_AUTHOR_NAME", "Ahi Test")
            .env("GIT_AUTHOR_EMAIL", "ahi@example.com")
            .env("GIT_COMMITTER_NAME", "Ahi Test")
            .env("GIT_COMMITTER_EMAIL", "ahi@example.com")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn git_stdout(directory: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(directory)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout).unwrap()
    }
}
