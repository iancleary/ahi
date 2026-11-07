//! `search_files`: lines matching a regular expression or literal across the
//! text files under a directory. Output is `path:line:text`, files in sorted
//! order, lines ascending, bounded by match count and bytes.
//!
//! Files are scanned in parallel. Each file costs three system calls (open,
//! fstat, one read) and the regex runs once over the whole buffer; line
//! numbers come from counting newlines between hits. Each thread renders its
//! hits straight into one buffer per file, so merging is a concatenation in
//! file order and the output never depends on how the work was split.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use regex::bytes::{Regex, RegexBuilder};

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
/// Files per thread below which one thread is used.
const CHUNK: usize = 64;

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
    let tree = walk::walk(workspace, directory, relative)?;
    // Candidate files in sorted order. The walk never followed a symbolic
    // link and pruned ahi's directories.
    let files: Vec<&str> = tree
        .files()
        .filter(|path| match &glob {
            Some(glob) => {
                let below = path
                    .strip_prefix(relative)
                    .and_then(|r| r.strip_prefix('/'))
                    .unwrap_or(path);
                glob.matches(below)
            }
            None => true,
        })
        .collect();

    let budget = AtomicU64::new(SCAN_BUDGET);
    let scanned: Vec<FileResult> = if files.len() <= CHUNK {
        let mut scanner = Scanner::new(workspace, &regex, &budget);
        files.iter().map(|path| scanner.scan(path)).collect()
    } else {
        let threads = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .clamp(1, 16);
        let per_thread = files.len().div_ceil(threads).max(CHUNK);
        thread::scope(|scope| {
            let handles: Vec<_> = files
                .chunks(per_thread)
                .map(|chunk| {
                    let regex = &regex;
                    let budget = &budget;
                    scope.spawn(move || {
                        let mut scanner = Scanner::new(workspace, regex, budget);
                        chunk
                            .iter()
                            .map(|path| scanner.scan(path))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().unwrap_or_default())
                .collect()
        })
    };

    let mut out = String::new();
    let mut matches = 0usize;
    let mut shown = 0usize;
    let mut skipped = 0usize;
    let mut exhausted = false;
    'files: for result in scanned {
        let hits = match result {
            FileResult::Skipped => {
                skipped += 1;
                continue;
            }
            FileResult::Exhausted => {
                exhausted = true;
                break;
            }
            FileResult::Clean => continue,
            FileResult::Hits(hits) => hits,
        };
        matches += hits.ends.len();
        let mut start = 0usize;
        for end in hits.ends {
            let end = end as usize;
            let line = &hits.rendered[start..end];
            start = end;
            if shown >= limit {
                continue;
            }
            if out.len() + line.len() + 1 > MAX_OUTPUT_BYTES {
                exhausted = true;
                break 'files;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
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

/// A file's matching lines, pre-rendered as `path:line:text` runs in one
/// buffer, with the end offset of each run. Structure of arrays: no
/// per-hit allocation.
#[derive(Default)]
struct Hits {
    rendered: String,
    ends: Vec<u32>,
}

enum FileResult {
    /// Not a regular file, binary, oversized, or unreadable.
    Skipped,
    /// The read budget ran out before this file.
    Exhausted,
    Clean,
    Hits(Hits),
}

/// Per-thread scanner: the path and file buffers are reused across files.
struct Scanner<'a> {
    workspace: &'a Workspace,
    regex: &'a Regex,
    budget: &'a AtomicU64,
    path: PathBuf,
    buffer: Vec<u8>,
}

impl<'a> Scanner<'a> {
    fn new(workspace: &'a Workspace, regex: &'a Regex, budget: &'a AtomicU64) -> Self {
        Self {
            workspace,
            regex,
            budget,
            path: PathBuf::new(),
            buffer: Vec::new(),
        }
    }

    fn scan(&mut self, relative: &str) -> FileResult {
        walk::absolute(self.workspace, relative, &mut self.path);
        let Ok(mut file) = File::open(&self.path) else {
            return FileResult::Skipped;
        };
        // fstat on the open descriptor: no second path lookup.
        let Ok(metadata) = file.metadata() else {
            return FileResult::Skipped;
        };
        if !metadata.is_file() {
            return FileResult::Skipped;
        }
        let len = metadata.len();
        if len > MAX_FILE_BYTES {
            return FileResult::Skipped;
        }
        if !reserve(self.budget, len) {
            return FileResult::Exhausted;
        }
        self.buffer.clear();
        self.buffer.resize(len as usize, 0);
        // One read for the whole file; a file that grew or shrank between
        // fstat and read is read as far as it goes.
        let mut filled = 0usize;
        while filled < self.buffer.len() {
            match file.read(&mut self.buffer[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => return FileResult::Skipped,
            }
        }
        self.buffer.truncate(filled);
        let buffer = &self.buffer[..];
        if buffer[..buffer.len().min(SNIFF)].contains(&0) {
            return FileResult::Skipped;
        }
        let mut hits = Hits::default();
        let mut line_number = 1usize;
        let mut counted_to = 0usize;
        let mut last_line_start = usize::MAX;
        for found in self.regex.find_iter(buffer) {
            let start = found.start();
            line_number += buffer[counted_to..start]
                .iter()
                .filter(|b| **b == b'\n')
                .count();
            counted_to = start;
            let line_start = buffer[..start]
                .iter()
                .rposition(|b| *b == b'\n')
                .map_or(0, |i| i + 1);
            if line_start == last_line_start {
                continue; // a second match on the same line
            }
            last_line_start = line_start;
            let line_end = buffer[start..]
                .iter()
                .position(|b| *b == b'\n')
                .map_or(buffer.len(), |i| start + i);
            let line = &buffer[line_start..line_end];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let text = String::from_utf8_lossy(line);
            let text = text.trim_end();
            hits.rendered.push_str(relative);
            hits.rendered.push(':');
            hits.rendered.push_str(&line_number.to_string());
            hits.rendered.push(':');
            if text.chars().count() > MAX_LINE_CHARS {
                hits.rendered.extend(text.chars().take(MAX_LINE_CHARS));
                hits.rendered.push_str("...");
            } else {
                hits.rendered.push_str(text);
            }
            hits.ends.push(hits.rendered.len() as u32);
        }
        if hits.ends.is_empty() {
            FileResult::Clean
        } else {
            FileResult::Hits(hits)
        }
    }
}

/// Take `len` bytes from the shared budget; false when it would overdraw.
fn reserve(budget: &AtomicU64, len: u64) -> bool {
    let mut remaining = budget.load(Ordering::Relaxed);
    loop {
        if len > remaining {
            return false;
        }
        match budget.compare_exchange_weak(
            remaining,
            remaining - len,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(actual) => remaining = actual,
        }
    }
}
