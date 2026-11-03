//! `ahi.lock` version 1. The file is replaced by a temporary file and a rename.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_yaml::Value;

use crate::error::{Error, Result};
use crate::yaml_safe::check_config_yaml;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signature {
    Verified,
    Absent,
    Failed,
    Unverified,
}

impl Signature {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "verified" => Ok(Self::Verified),
            "absent" => Ok(Self::Absent),
            "failed" => Ok(Self::Failed),
            "unverified" => Ok(Self::Unverified),
            other => Err(Error::new(format!(
                "signature `{other}` is not verified, absent, failed, or unverified"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Absent => "absent",
            Self::Failed => "failed",
            Self::Unverified => "unverified",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lockfile {
    pub repos: Vec<LockedRepo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedRepo {
    pub repo: String,
    pub requested: String,
    pub commit: String,
    pub tree: String,
    pub digest: String,
    pub signature: Signature,
}

pub fn lock_path(root: &Path) -> PathBuf {
    root.join("ahi.lock")
}

pub fn load(root: &Path) -> Result<Lockfile> {
    let path = lock_path(root);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::new("ahi.lock is missing; run ahi lock"));
        }
        Err(err) => {
            return Err(Error::new(format!(
                "failed to read {}: {err}",
                path.display()
            )))
        }
    };
    parse(&text)
}

pub fn load_optional(root: &Path) -> Result<Option<Lockfile>> {
    let path = lock_path(root);
    if !path.exists() {
        return Ok(None);
    }
    load(root).map(Some)
}

pub fn parse(text: &str) -> Result<Lockfile> {
    check_config_yaml(text, "ahi.lock")?;
    let raw: RawLock =
        serde_yaml::from_str(text).map_err(|err| Error::new(format!("ahi.lock: {err}")))?;
    if raw.version != 1 {
        return Err(Error::new(format!(
            "ahi.lock version {} is not supported (expected 1)",
            raw.version
        )));
    }
    let mut repos = Vec::new();
    for repo in raw.repos {
        if !repo.dependencies.is_empty() {
            return Err(Error::new(format!(
                "lock entry `{}` lists dependencies, which v0.2 does not pin",
                repo.repo
            )));
        }
        repos.push(validate_repo(repo)?);
    }
    Ok(Lockfile { repos })
}

pub fn render(lock: &Lockfile) -> Result<String> {
    if lock.repos.is_empty() {
        return Ok("version: 1\nrepos: []\n".to_string());
    }
    let mut out = String::from("version: 1\nrepos:\n");
    for repo in &lock.repos {
        validate_repo_fields(repo)?;
        out.push_str("  - repo: ");
        out.push_str(&quote(&repo.repo));
        out.push('\n');
        out.push_str("    requested: ");
        out.push_str(&quote(&repo.requested));
        out.push('\n');
        out.push_str("    commit: ");
        out.push_str(&quote(&repo.commit));
        out.push('\n');
        out.push_str("    tree: ");
        out.push_str(&quote(&repo.tree));
        out.push('\n');
        out.push_str("    digest: ");
        out.push_str(&quote(&repo.digest));
        out.push('\n');
        out.push_str("    signature: ");
        out.push_str(&quote(repo.signature.as_str()));
        out.push('\n');
    }
    Ok(out)
}

/// Write `ahi.lock` and, when `config_text` is set, the config file.
///
/// The lock bytes are staged in `ahi.lock.pending` before the config is
/// replaced, so a failed config write leaves the previous lock in place.
/// Identical lock bytes skip the rename.
pub fn publish(
    root: &Path,
    config_path: &Path,
    lock_text: &str,
    config_text: Option<&str>,
) -> Result<()> {
    let lock_path = lock_path(root);
    let same_lock = lock_path.is_file()
        && fs::read_to_string(&lock_path)
            .map(|existing| existing == lock_text)
            .unwrap_or(false);
    if same_lock && config_text.is_none() {
        return Ok(());
    }
    if let Some(config_text) = config_text {
        let pending = root.join("ahi.lock.pending");
        write_atomic(&pending, lock_text.as_bytes())?;
        write_atomic(config_path, config_text.as_bytes())?;
        if same_lock {
            let _ = fs::remove_file(&pending);
        } else {
            fs::rename(&pending, &lock_path).map_err(|err| {
                Error::new(format!("failed to replace {}: {err}", lock_path.display()))
            })?;
        }
        return Ok(());
    }
    write_atomic(&lock_path, lock_text.as_bytes())
}

pub fn digest_hex(digest: &str) -> Result<&str> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(Error::new(format!(
            "digest `{digest}` must start with sha256:"
        )));
    };
    require_hex("digest", hex, 64)?;
    Ok(hex)
}

fn validate_repo(raw: RawRepo) -> Result<LockedRepo> {
    let repo = LockedRepo {
        repo: raw.repo,
        requested: raw.requested,
        commit: raw.commit,
        tree: raw.tree,
        digest: raw.digest,
        signature: Signature::parse(&raw.signature)?,
    };
    validate_repo_fields(&repo)?;
    Ok(repo)
}

fn validate_repo_fields(repo: &LockedRepo) -> Result<()> {
    require_single_line("repo", &repo.repo)?;
    require_single_line("requested", &repo.requested)?;
    require_hex("commit", &repo.commit, 40)?;
    require_hex("tree", &repo.tree, 40)?;
    digest_hex(&repo.digest)?;
    Ok(())
}

fn require_single_line(field: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.chars().any(|ch| ch.is_control()) {
        return Err(Error::new(format!(
            "{field} is empty or contains a control character"
        )));
    }
    Ok(())
}

fn require_hex(field: &str, value: &str, len: usize) -> Result<()> {
    if value.len() == len
        && value
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
    {
        return Ok(());
    }
    Err(Error::new(format!(
        "{field} must be {len} lowercase hex characters"
    )))
}

fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(format!("path {} has no parent directory", path.display())))?;
    let mut tmp_name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    tmp_name.push(".tmp");
    let tmp = parent.join(tmp_name);
    {
        let mut file = File::create(&tmp)
            .map_err(|err| Error::new(format!("failed to create {}: {err}", tmp.display())))?;
        file.write_all(bytes)
            .map_err(|err| Error::new(format!("failed to write {}: {err}", tmp.display())))?;
        file.sync_all()
            .map_err(|err| Error::new(format!("failed to fsync {}: {err}", tmp.display())))?;
    }
    fs::rename(&tmp, path)
        .map_err(|err| Error::new(format!("failed to replace {}: {err}", path.display())))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLock {
    version: u64,
    repos: Vec<RawRepo>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepo {
    repo: String,
    requested: String,
    commit: String,
    tree: String,
    digest: String,
    signature: String,
    #[serde(default)]
    dependencies: Vec<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_fields_and_rejects_unknown_keys() {
        let lock = Lockfile {
            repos: vec![LockedRepo {
                repo: "https://example.com/hooks".to_string(),
                requested: "v1.2.3".to_string(),
                commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
                tree: "abcdefabcdefabcdefabcdefabcdefabcdefabcd".to_string(),
                digest: "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                    .to_string(),
                signature: Signature::Absent,
            }],
        };
        let text = render(&lock).unwrap();
        let parsed = parse(&text).unwrap();
        assert_eq!(parsed, lock);
        let err = parse("version: 1\nrepos: []\nextra: true\n").unwrap_err();
        assert!(err.to_string().contains("extra") || err.to_string().contains("unknown"));
    }

    #[test]
    fn identical_bytes_skip_the_replace() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("lockfile-same");
        let path = lock_path(&dir);
        let text = "version: 1\nrepos: []\n";
        publish(&dir, &dir.join("unused"), text, None).unwrap();
        let first = fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        publish(&dir, &dir.join("unused"), text, None).unwrap();
        let second = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(first, second);
        let _ = fs::remove_dir_all(&dir);
    }
}
