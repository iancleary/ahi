//! Builtin checks. Implementations do not spawn processes and do not touch the network.
//!
//! Fixers rewrite a file only when `CheckContext::fix` is set. The runner sets
//! that for `ahi run --fix` and for a per-hook `--fix` argument on the three
//! fixer checks. A rewrite is atomic, does not follow a symlink, and is refused
//! when the file has unstaged changes.

mod content;
mod inspect;
mod supply;
mod whitespace;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use content::{
    CaseConflict, CommittedDotenv, JsonCheck, LargeFiles, MergeConflict, PrivateKey, Symlinks,
    TomlCheck, TrojanSource, WindowsReserved, YamlCheck,
};
use inspect::{
    AwsCredentials, ByteOrderMarker, ExecutableShebang, NoCommitToBranch, ShebangExecutable,
    VcsPermalinks, XmlCheck,
};
use supply::{DockerfileCheck, GithubActions, ShellStrict};
use whitespace::{EndOfFile, LineEndings, TrailingWhitespace};

use crate::error::{Error, Result};
use crate::paths::join_inside;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub severity: Severity,
    pub message: String,
}

impl Finding {
    pub fn at(path: impl Into<String>, line: Option<u32>, message: impl Into<String>) -> Self {
        Self {
            path: Some(path.into()),
            line,
            column: None,
            severity: Severity::Error,
            message: message.into(),
        }
    }

    pub fn bare(message: impl Into<String>) -> Self {
        Self {
            path: None,
            line: None,
            column: None,
            severity: Severity::Error,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    pub path: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOutcome {
    pub findings: Vec<Finding>,
    pub rewrites: Vec<Rewrite>,
}

impl CheckOutcome {
    pub fn ok() -> Self {
        Self {
            findings: Vec::new(),
            rewrites: Vec::new(),
        }
    }

    pub fn from(findings: Vec<Finding>) -> Self {
        Self {
            findings,
            rewrites: Vec::new(),
        }
    }

    pub fn with_rewrites(findings: Vec<Finding>, rewrites: Vec<Rewrite>) -> Self {
        Self { findings, rewrites }
    }
}

pub struct CheckContext<'a> {
    pub root: &'a Path,
    pub args: &'a [String],
    pub git_modes: &'a BTreeMap<String, String>,
    /// When true, fixer checks may rewrite files. Report-only checks ignore it.
    pub fix: bool,
}

impl CheckContext<'_> {
    pub fn read(&self, rel: &str) -> Result<Vec<u8>> {
        let path = join_inside(self.root, rel)?;
        let meta =
            std::fs::symlink_metadata(&path).map_err(|err| Error::new(format!("{rel}: {err}")))?;
        if meta.file_type().is_symlink() {
            return Err(Error::new(format!("{rel}: refusing to follow a symlink")));
        }
        read_nofollow(&path).map_err(|err| Error::new(format!("{rel}: {err}")))
    }

    pub fn len(&self, rel: &str) -> Result<u64> {
        let path = join_inside(self.root, rel)?;
        let meta =
            std::fs::symlink_metadata(&path).map_err(|err| Error::new(format!("{rel}: {err}")))?;
        Ok(meta.len())
    }
}

pub(crate) fn write_nofollow(root: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    let path = path_without_symlinks(root, rel)?;
    let meta =
        std::fs::symlink_metadata(&path).map_err(|err| Error::new(format!("{rel}: {err}")))?;
    if meta.file_type().is_symlink() {
        return Err(Error::new(format!("{rel}: refusing to follow a symlink")));
    }
    if !meta.is_file() {
        return Err(Error::new(format!("{rel}: refusing to rewrite a non-file")));
    }
    atomic_replace(&path, bytes).map_err(|err| Error::new(format!("{rel}: {err}")))
}

/// Refuse a path whose own name, or any parent inside the repo, is a symlink.
fn path_without_symlinks(root: &Path, rel: &str) -> Result<std::path::PathBuf> {
    let path = join_inside(root, rel)?;
    let mut cursor = root.to_path_buf();
    for component in std::path::Path::new(rel).components() {
        let std::path::Component::Normal(part) = component else {
            continue;
        };
        cursor.push(part);
        let meta = std::fs::symlink_metadata(&cursor)
            .map_err(|err| Error::new(format!("{rel}: {err}")))?;
        if meta.file_type().is_symlink() {
            return Err(Error::new(format!("{rel}: refusing to follow a symlink")));
        }
    }
    Ok(path)
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let parent = parent.unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing file name")
    })?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let tmp_name = format!(
        ".{}.ahi-tmp-{}-{nonce}",
        file_name.to_string_lossy(),
        std::process::id()
    );
    let tmp = parent.join(tmp_name);
    let result = (|| {
        let mut file = create_excl_nofollow(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        let meta = std::fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "refusing to replace a symlink",
            ));
        }
        if !meta.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "refusing to replace a non-file",
            ));
        }
        replace_file(&tmp, path)?;
        #[cfg(unix)]
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn create_excl_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    }
}

fn replace_file(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::rename(tmp, dest)
    }
    #[cfg(not(unix))]
    {
        let backup_name = format!(
            ".{}.ahi-bak-{}",
            dest.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_string()),
            std::process::id()
        );
        let backup = dest
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(backup_name);
        std::fs::rename(dest, &backup)?;
        if let Err(err) = std::fs::rename(tmp, dest) {
            let _ = std::fs::rename(&backup, dest);
            return Err(err);
        }
        let _ = std::fs::remove_file(&backup);
        Ok(())
    }
}

fn read_nofollow(path: &Path) -> std::io::Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::read(path)
    }
}

pub trait Check: Send + Sync {
    fn id(&self) -> &'static str;
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn default_types(&self) -> &'static [&'static str] {
        &["file"]
    }
    fn default_files(&self) -> &'static str {
        ""
    }
    fn upstream(&self) -> Option<&'static str> {
        None
    }
    /// `ahi run --fix` or a per-hook `--fix` argument may write.
    fn fixes(&self) -> bool {
        false
    }
    /// Used when the config omits `always_run`.
    fn default_always_run(&self) -> bool {
        false
    }
    /// Used when the config omits `pass_filenames`.
    fn default_pass_filenames(&self) -> bool {
        true
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome;
}

/// Whether this hook invocation is allowed to rewrite files.
pub(crate) fn writes_enabled(check: &dyn Check, args: &[String], cli_fix: bool) -> bool {
    if !check.fixes() {
        return false;
    }
    if check.id() == "ahi/line-endings" {
        if let Some(mode) = flag_values(args, "--fix")
            .last()
            .copied()
            .or_else(|| flag_values(args, "-f").last().copied())
        {
            return matches!(mode, "auto" | "lf" | "crlf" | "cr");
        }
    }
    if args.iter().any(|arg| arg == "--fix") {
        return true;
    }
    cli_fix
}

pub fn all_checks() -> &'static [&'static dyn Check] {
    static REG: OnceLock<Vec<&'static dyn Check>> = OnceLock::new();
    REG.get_or_init(|| {
        vec![
            Box::leak(Box::new(TrailingWhitespace)),
            Box::leak(Box::new(EndOfFile)),
            Box::leak(Box::new(MergeConflict)),
            Box::leak(Box::new(LargeFiles)),
            Box::leak(Box::new(CaseConflict)),
            Box::leak(Box::new(Symlinks)),
            Box::leak(Box::new(JsonCheck)),
            Box::leak(Box::new(YamlCheck)),
            Box::leak(Box::new(TomlCheck)),
            Box::leak(Box::new(TrojanSource)),
            Box::leak(Box::new(WindowsReserved)),
            Box::leak(Box::new(PrivateKey)),
            Box::leak(Box::new(LineEndings)),
            Box::leak(Box::new(ShebangExecutable)),
            Box::leak(Box::new(ExecutableShebang)),
            Box::leak(Box::new(NoCommitToBranch)),
            Box::leak(Box::new(XmlCheck)),
            Box::leak(Box::new(AwsCredentials)),
            Box::leak(Box::new(VcsPermalinks)),
            Box::leak(Box::new(ByteOrderMarker)),
            Box::leak(Box::new(GithubActions)),
            Box::leak(Box::new(DockerfileCheck)),
            Box::leak(Box::new(ShellStrict)),
            Box::leak(Box::new(CommittedDotenv)),
        ]
    })
    .as_slice()
}

pub fn builtin_by_id(id: &str) -> Option<&'static dyn Check> {
    all_checks()
        .iter()
        .copied()
        .find(|check| check.id() == id || check.aliases().contains(&id))
}

pub(crate) fn flag_values<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
    let mut values = Vec::new();
    let mut iter = args.iter();
    let with_eq = format!("{flag}=");
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix(&with_eq) {
            values.push(value);
        } else if arg == flag {
            if let Some(value) = iter.next() {
                values.push(value.as_str());
            }
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_flag_enables_only_the_three_fixers() {
        let eof = builtin_by_id("ahi/end-of-file").unwrap();
        let yaml = builtin_by_id("ahi/check-yaml").unwrap();
        let lines = builtin_by_id("ahi/line-endings").unwrap();
        let bom = builtin_by_id("ahi/byte-order-marker").unwrap();
        assert!(!writes_enabled(eof, &[], false));
        assert!(writes_enabled(eof, &[], true));
        assert!(writes_enabled(eof, &["--fix".to_string()], false));
        assert!(!writes_enabled(yaml, &["--fix".to_string()], true));
        assert!(!writes_enabled(bom, &[], true));
        assert!(!writes_enabled(lines, &["--fix=no".to_string()], true));
        assert!(writes_enabled(lines, &["--fix=lf".to_string()], false));
    }

    #[test]
    fn atomic_write_replaces_a_file_and_refuses_a_symlink() {
        let dir = std::env::temp_dir().join(format!(
            "ahi-write-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), b"old\n").unwrap();
        write_nofollow(&dir, "notes.txt", b"new\n").unwrap();
        assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), b"new\n");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("notes.txt", dir.join("link.txt")).unwrap();
            let err = write_nofollow(&dir, "link.txt", b"nope\n").unwrap_err();
            assert!(err.to_string().contains("symlink"), "{err}");
            assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), b"new\n");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
