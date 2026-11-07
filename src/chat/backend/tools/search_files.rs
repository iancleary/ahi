//! `search_files`: lines matching a regular expression or literal across the
//! text files under a directory. Output is `path:line:text`, files in sorted
//! order, lines ascending, bounded by match count and bytes.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use regex::bytes::RegexBuilder;

use super::glob::Glob;
use super::walk;
use super::workspace::Workspace;

pub(super) const DEFAULT_LIMIT: usize = 100;
pub(super) const MAX_LIMIT: usize = 500;
/// Output cap; the same as `read_file`.
pub(super) const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Files larger than this are skipped and counted.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Total bytes read across one call before the search stops.
const SCAN_BUDGET: u64 = 256 * 1024 * 1024;
/// A matched line longer than this is cut.
const MAX_LINE_CHARS: usize = 240;
/// Bytes sniffed for NUL to classify a file as binary.
const SNIFF: usize = 8 * 1024;

pub(super) struct Query<'a> {
    pub(super) pattern: &'a str,
    pub(super) literal: bool,
    pub(super) ignore_case: bool,
    pub(super) glob: Option<&'a str>,
    pub(super) limit: usize,
}

pub(super) fn run(
    workspace: &Workspace,
    directory: &Path,
    relative: &str,
    query: &Query<'_>,
) -> Result<String, &'static str> {
    if !directory.is_dir() {
        return Err("not a directory");
    }
    if query.pattern.is_empty() || query.pattern.len() > 512 {
        return Err("pattern must be 1 to 512 bytes");
    }
    let source = if query.literal {
        regex::escape(query.pattern)
    } else {
        query.pattern.to_string()
    };
    let regex = RegexBuilder::new(&source)
        .case_insensitive(query.ignore_case)
        .multi_line(false)
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()
        .map_err(|_| "invalid regular expression")?;
    let glob = match query.glob {
        Some(glob) => Some(Glob::new(glob).ok_or("invalid glob pattern")?),
        None => None,
    };
    let limit = query.limit.clamp(1, MAX_LIMIT);
    let entries = walk::walk(workspace, directory, relative)?;

    let mut out = String::new();
    let mut matches = 0usize;
    let mut shown = 0usize;
    let mut skipped = 0usize;
    let mut budget = SCAN_BUDGET;
    let mut exhausted = false;
    let mut buffer = Vec::new();
    'files: for entry in &entries {
        if entry.is_dir || entry.is_symlink {
            continue;
        }
        if let Some(glob) = &glob {
            let below = entry
                .relative
                .strip_prefix(relative)
                .and_then(|r| r.strip_prefix('/'))
                .unwrap_or(&entry.relative);
            if !glob.matches(below) {
                continue;
            }
        }
        let Ok(path) = workspace.resolve(&entry.relative) else {
            continue;
        };
        let Ok(metadata) = path.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        if metadata.len() > MAX_FILE_BYTES {
            skipped += 1;
            continue;
        }
        if metadata.len() > budget {
            exhausted = true;
            break;
        }
        buffer.clear();
        if File::open(&path)
            .and_then(|f| f.take(MAX_FILE_BYTES).read_to_end(&mut buffer))
            .is_err()
        {
            skipped += 1;
            continue;
        }
        budget -= buffer.len() as u64;
        if buffer[..buffer.len().min(SNIFF)].contains(&0) {
            skipped += 1;
            continue;
        }
        if !regex.is_match(&buffer) {
            continue;
        }
        let mut line_number = 0usize;
        for line in buffer.split(|b| *b == b'\n') {
            line_number += 1;
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if !regex.is_match(line) {
                continue;
            }
            matches += 1;
            if shown >= limit {
                continue;
            }
            let text = String::from_utf8_lossy(line);
            let text = text.trim_end();
            let mut rendered = format!("{}:{line_number}:", entry.relative);
            if text.chars().count() > MAX_LINE_CHARS {
                rendered.extend(text.chars().take(MAX_LINE_CHARS));
                rendered.push_str("...");
            } else {
                rendered.push_str(text);
            }
            if out.len() + rendered.len() + 1 > MAX_OUTPUT_BYTES {
                exhausted = true;
                break 'files;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&rendered);
            shown += 1;
        }
    }
    if matches == 0 && !exhausted {
        out.push_str("[no matches]");
    } else if matches > shown {
        out.push_str(&format!(
            "\n[{} more matches not shown; raise limit or narrow the path, glob, or pattern]",
            matches - shown
        ));
    }
    if exhausted {
        out.push_str("\n[search stopped at the output or read budget; narrow the path or glob]");
    }
    if skipped > 0 {
        out.push_str(&format!("\n[{skipped} binary or oversized files skipped]"));
    }
    Ok(out)
}
