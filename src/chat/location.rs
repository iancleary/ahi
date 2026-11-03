//! Working directory and the git label read from `HEAD`.
//!
//! This does not spawn `git`. A worktree `.git` file is followed when it
//! contains a `gitdir:` line.

use std::fs;
use std::path::{Path, PathBuf};

/// Directory display, with the home directory shortened to `~`, and the git label.
pub(crate) fn current() -> (String, Option<String>) {
    let directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    (display_directory(&directory), read_git_label(&directory))
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
        let branch = read_git_label(&repo);
        assert_eq!(branch.as_deref(), Some("main"));
        let row = crate::chat::status::render_location_row(80, "/tmp/ahi", branch.as_deref());
        assert!(row.ends_with("(main)"), "{row}");

        let sha = git_stdout(&repo, &["rev-parse", "HEAD"]);
        let short = sha.trim().chars().take(7).collect::<String>();
        git(&repo, &["checkout", "--detach", "HEAD"]);
        assert_eq!(read_git_label(&repo).as_deref(), Some(short.as_str()));
        let detached = crate::chat::status::render_location_row(80, "/tmp/ahi", Some(&short));
        assert!(detached.contains(&format!("({short})")), "{detached}");
        assert!(!detached.contains("(main)"), "{detached}");

        let outside = crate::fixture_directory::create_unique_fixture_directory("chat-nogit");
        assert!(read_git_label(&outside).is_none());
        let row = crate::chat::status::render_location_row(80, "/tmp/plain", None);
        assert_eq!(row, "/tmp/plain");
        assert!(!row.contains('('));

        let shortened =
            crate::chat::status::render_location_row(12, "/home/user/src/ahi", Some("main"));
        assert!(shortened.starts_with('…'), "{shortened}");
        assert!(shortened.ends_with("(main)"), "{shortened}");
        assert_eq!(shortened.chars().count(), 12);

        let work = repo.join("work");
        fs::create_dir(&work).unwrap();
        fs::write(work.join(".git"), "gitdir: ../.git\n").unwrap();
        assert_eq!(read_git_label(&work).as_deref(), Some(short.as_str()));
        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(outside);
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
