//! `write_file`: create a file, or replace one when `overwrite` is set.
//! Writes are atomic (temporary sibling, fsync, rename), never follow a
//! symbolic link, and report content hashes for the audit log.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// Largest file `edit_file` will read and rewrite.
pub(super) const MAX_EDIT_BYTES: u64 = 8 * 1024 * 1024;
/// Largest `content` or `new` accepted.
pub(super) const MAX_WRITE_BYTES: usize = 8 * 1024 * 1024;

/// What changed, for the audit log and the transcript. Hashes, never text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriteReport {
    pub(crate) before: Option<String>,
    pub(crate) after: String,
    pub(crate) lines_added: usize,
    pub(crate) lines_removed: usize,
}

/// The texts a write replaced, for the transcript's diff. Not logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriteTexts {
    pub(crate) before: Option<String>,
    pub(crate) after: String,
}

impl WriteReport {
    pub(super) fn replace(
        path: &Path,
        before: &[u8],
        after: &[u8],
        lines_added: usize,
        lines_removed: usize,
    ) -> Result<Self, &'static str> {
        crate::hooks::check::atomic_replace(path, after).map_err(|_| "cannot write the file")?;
        Ok(Self {
            before: Some(hex(before)),
            after: hex(after),
            lines_added,
            lines_removed,
        })
    }
}

/// `overwrite: false` fails when the file exists; `true` requires it to
/// exist and be a regular file. `exists` is what the caller resolved.
pub(super) fn run(
    path: &Path,
    content: &str,
    overwrite: bool,
    exists: bool,
) -> Result<(String, WriteReport, WriteTexts), &'static str> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lines = line_count(content);
    if !overwrite {
        if exists {
            return Err("file exists; set overwrite to replace it");
        }
        crate::hooks::check::create_new(path, content.as_bytes())
            .map_err(|_| "cannot create the file")?;
        return Ok((
            format!("created {name}: {lines} lines"),
            WriteReport {
                before: None,
                after: hex(content.as_bytes()),
                lines_added: lines,
                lines_removed: 0,
            },
            WriteTexts {
                before: None,
                after: content.to_string(),
            },
        ));
    }
    if !exists {
        return Err("file does not exist; overwrite applies to an existing file");
    }
    let (before_text, before) = read_text(path)?;
    let removed = line_count(&before_text);
    let report = WriteReport::replace(path, &before, content.as_bytes(), lines, removed)?;
    Ok((
        format!("replaced {name}: {lines} lines (was {removed})"),
        report,
        WriteTexts {
            before: Some(before_text),
            after: content.to_string(),
        },
    ))
}

/// Read a regular UTF-8 text file without following a symbolic link.
pub(super) fn read_text(path: &Path) -> Result<(String, Vec<u8>), &'static str> {
    let metadata = path
        .symlink_metadata()
        .map_err(|_| "cannot read file metadata")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("not a regular file");
    }
    if metadata.len() > MAX_EDIT_BYTES {
        return Err("file is larger than 8 MiB");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|_| "cannot read file")?;
    if bytes.contains(&0) {
        return Err("file is not UTF-8 text");
    }
    let text = String::from_utf8(bytes.clone()).map_err(|_| "file is not UTF-8 text")?;
    Ok((text, bytes))
}

/// Lines in `text`: newline count, plus one for a final unterminated line.
pub(super) fn line_count(text: &str) -> usize {
    let newlines = text.matches('\n').count();
    if text.is_empty() || text.ends_with('\n') {
        newlines
    } else {
        newlines + 1
    }
}

fn hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
