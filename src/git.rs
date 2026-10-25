//! Git file lists. Commands match pre-commit's `git.py` for the three selection modes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};
use crate::filter::normalize_slashes;

#[derive(Debug, Clone)]
pub struct GitFiles {
    pub paths: Vec<String>,
    pub modes: BTreeMap<String, String>,
    pub unstaged: bool,
}

pub fn repo_root(start: &Path) -> Result<PathBuf> {
    let output = git(start, &["rev-parse", "--show-toplevel"])?;
    let text = String::from_utf8(output)
        .map_err(|_| Error::new("git rev-parse returned a path that is not UTF-8"))?;
    Ok(PathBuf::from(text.trim()))
}

pub fn staged_files(root: &Path) -> Result<GitFiles> {
    if has_unmerged(root)? {
        return Err(Error::new(
            "Unmerged files. Resolve before committing.".to_string(),
        ));
    }
    let paths = nul_paths(
        root,
        &[
            "diff",
            "--staged",
            "--name-only",
            "--no-ext-diff",
            "-z",
            "--diff-filter=ACMRTUXB",
        ],
    )?;
    let unstaged = !nul_paths(root, &["diff", "--name-only", "--no-ext-diff", "-z"])?.is_empty();
    Ok(GitFiles {
        paths,
        modes: file_modes(root)?,
        unstaged,
    })
}

pub fn all_files(root: &Path) -> Result<GitFiles> {
    let paths = nul_paths(root, &["ls-files", "-z", "--deduplicate"])?;
    Ok(GitFiles {
        paths,
        modes: file_modes(root)?,
        unstaged: false,
    })
}

pub fn changed_files(root: &Path, from: &str, to: &str) -> Result<GitFiles> {
    let three = format!("{from}...{to}");
    let paths = match git(
        root,
        &["diff", "--name-only", "--no-ext-diff", "-z", &three],
    ) {
        Ok(bytes) => split_nul(&bytes)?,
        Err(_) => nul_paths(
            root,
            &[
                "diff",
                "--name-only",
                "--no-ext-diff",
                "-z",
                &format!("{from}..{to}"),
            ],
        )?,
    };
    Ok(GitFiles {
        paths,
        modes: file_modes(root)?,
        unstaged: false,
    })
}

pub fn explicit_files(root: &Path, files: &[String]) -> Result<GitFiles> {
    let paths = files.iter().map(|path| normalize_slashes(path)).collect();
    Ok(GitFiles {
        paths,
        modes: file_modes(root).unwrap_or_default(),
        unstaged: false,
    })
}

pub fn added_files(root: &Path) -> Result<Vec<String>> {
    nul_paths(
        root,
        &[
            "diff",
            "--staged",
            "--name-only",
            "--no-ext-diff",
            "-z",
            "--diff-filter=A",
        ],
    )
}

pub fn tracked_files(root: &Path) -> Result<Vec<String>> {
    nul_paths(root, &["ls-files", "-z"])
}

/// `git symbolic-ref HEAD`. `Ok(None)` is a detached HEAD, which is not a branch.
pub fn current_branch(root: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .current_dir(root)
        .output()
        .map_err(|err| Error::new(format!("failed to run git: {err}")))?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| Error::new("git symbolic-ref returned a name that is not UTF-8"))?;
    let text = text.trim();
    let name = text.strip_prefix("refs/heads/").unwrap_or(text);
    if name.is_empty() {
        Ok(None)
    } else {
        Ok(Some(name.to_string()))
    }
}

pub fn unstaged_paths(root: &Path) -> Result<BTreeSet<String>> {
    Ok(
        nul_paths(root, &["diff", "--name-only", "--no-ext-diff", "-z"])?
            .into_iter()
            .collect(),
    )
}

pub fn add_paths(root: &Path, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["add".to_string(), "--".to_string()];
    args.extend(paths.iter().cloned());
    let output = Command::new("git")
        .args(&args)
        .current_dir(root)
        .output()
        .map_err(|err| Error::new(format!("failed to run git: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::new(format!("git add failed: {}", stderr.trim())));
    }
    Ok(())
}

fn file_modes(root: &Path) -> Result<BTreeMap<String, String>> {
    let bytes = git(root, &["ls-files", "-z", "--stage"])?;
    let mut modes = BTreeMap::new();
    for entry in bytes.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let entry = std::str::from_utf8(entry)
            .map_err(|_| Error::new("git ls-files --stage returned a non-UTF-8 path"))?;
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let mode = meta.split_whitespace().next().unwrap_or("");
        if !mode.is_empty() {
            modes.insert(normalize_slashes(path), mode.to_string());
        }
    }
    Ok(modes)
}

fn has_unmerged(root: &Path) -> Result<bool> {
    let output = git(root, &["diff", "--name-only", "--diff-filter=U"])?;
    Ok(!output.iter().all(|byte| byte.is_ascii_whitespace()))
}

fn nul_paths(root: &Path, args: &[&str]) -> Result<Vec<String>> {
    let bytes = git(root, args)?;
    split_nul(&bytes)
}

fn split_nul(bytes: &[u8]) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for entry in bytes.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let text = std::str::from_utf8(entry)
            .map_err(|_| Error::new("git returned a path that is not UTF-8"))?;
        if text.contains('\n') {
            return Err(Error::new(format!(
                "refusing a git path that contains a newline: {text:?}"
            )));
        }
        paths.push(normalize_slashes(text));
    }
    Ok(paths)
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|err| Error::new(format!("failed to run git: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::new(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(output.stdout)
}

pub fn git_version() -> Result<String> {
    let output = Command::new("git")
        .arg("--version")
        .output()
        .map_err(|err| Error::new(format!("failed to run git: {err}")))?;
    if !output.status.success() {
        return Err(Error::new("git --version failed"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
