//! Builtin checks. Implementations do not spawn processes and do not touch the network.

mod content;
mod supply;
mod whitespace;

use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use content::{
    CaseConflict, CommittedDotenv, JsonCheck, LargeFiles, MergeConflict, PrivateKey, Symlinks,
    TomlCheck, TrojanSource, WindowsReserved, YamlCheck,
};
use supply::{DockerfileCheck, GithubActions, ShellStrict};
use whitespace::{EndOfFile, TrailingWhitespace};

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
pub struct CheckOutcome {
    pub findings: Vec<Finding>,
}

impl CheckOutcome {
    pub fn ok() -> Self {
        Self {
            findings: Vec::new(),
        }
    }

    pub fn from(findings: Vec<Finding>) -> Self {
        Self { findings }
    }
}

pub struct CheckContext<'a> {
    pub root: &'a Path,
    pub args: &'a [String],
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

fn read_nofollow(path: &Path) -> std::io::Result<Vec<u8>> {
    #[cfg(unix)]
    {
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
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome;
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
