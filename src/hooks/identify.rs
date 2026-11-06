//! File type tags.
//!
//! The extension, filename, and interpreter tables are vendored from
//! pre-commit/identify (MIT). Mode tags and the text/binary heuristic follow
//! `identify.tags_from_path`. Tracked files take their executable bit from the
//! git index so Windows and WSL agree.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;

#[path = "identify_tables.rs"]
mod identify_tables;

use identify_tables::{ALL_TAGS, EXTENSIONS, EXTENSIONS_NEED_BINARY_CHECK, INTERPRETERS, NAMES};

pub fn is_known_tag(tag: &str) -> bool {
    ALL_TAGS.binary_search(&tag).is_ok()
}

/// Classify `path`. `git_mode` is the index mode (`100644`, `100755`, ...) when the path is tracked.
pub fn classify(path: &Path, git_mode: Option<&str>) -> std::io::Result<BTreeSet<&'static str>> {
    let meta = std::fs::symlink_metadata(path)?;
    let file_type = meta.file_type();
    if file_type.is_dir() {
        return Ok(set(&["directory"]));
    }
    if file_type.is_symlink() {
        return Ok(set(&["symlink"]));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if file_type.is_socket() {
            return Ok(set(&["socket"]));
        }
    }

    let mut tags = set(&["file"]);
    let executable = is_executable(path, git_mode);
    if executable {
        tags.insert("executable");
    } else {
        tags.insert("non-executable");
    }

    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let from_name = tags_from_filename(name);
    if from_name.is_empty() {
        if executable {
            if let Some(interpreter) = shebang_interpreter(path) {
                tags.extend(tags_from_interpreter(&interpreter));
            }
        }
    } else {
        tags.extend(from_name);
    }

    if !tags.contains("text") && !tags.contains("binary") {
        let text = file_is_text(path).unwrap_or(false);
        tags.insert(if text { "text" } else { "binary" });
    }
    Ok(tags)
}

pub fn tags_from_filename(filename: &str) -> Vec<&'static str> {
    let mut candidates = Vec::new();
    candidates.push(filename);
    for part in filename.split('.') {
        candidates.push(part);
    }
    for candidate in candidates {
        if let Ok(index) = NAMES.binary_search_by_key(&candidate, |entry| entry.0) {
            return NAMES[index].1.to_vec();
        }
    }
    if let Some((_, ext)) = filename.rsplit_once('.') {
        let ext = ext.to_ascii_lowercase();
        if let Ok(index) = EXTENSIONS.binary_search_by_key(&ext.as_str(), |entry| entry.0) {
            return EXTENSIONS[index].1.to_vec();
        }
        if let Ok(index) =
            EXTENSIONS_NEED_BINARY_CHECK.binary_search_by_key(&ext.as_str(), |entry| entry.0)
        {
            return EXTENSIONS_NEED_BINARY_CHECK[index].1.to_vec();
        }
    }
    Vec::new()
}

fn tags_from_interpreter(interpreter: &str) -> Vec<&'static str> {
    let name = interpreter
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(interpreter);
    let mut name = name.to_string();
    while !name.is_empty() {
        if let Ok(index) = INTERPRETERS.binary_search_by_key(&name.as_str(), |entry| entry.0) {
            return INTERPRETERS[index].1.to_vec();
        }
        match name.rfind('.') {
            Some(dot) => name.truncate(dot),
            None => break,
        }
    }
    Vec::new()
}

fn is_executable(path: &Path, git_mode: Option<&str>) -> bool {
    if let Some(mode) = git_mode {
        return mode
            .chars()
            .last()
            .is_some_and(|ch| matches!(ch, '1' | '3' | '5' | '7'));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        shebang_interpreter(path).is_some()
    }
}

fn file_is_text(path: &Path) -> std::io::Result<bool> {
    let mut file = File::open(path)?;
    let mut buf = [0u8; 1024];
    let n = file.read(&mut buf)?;
    Ok(is_text_sample(&buf[..n]))
}

pub fn is_text_sample(sample: &[u8]) -> bool {
    sample.iter().all(|byte| {
        matches!(byte, 7 | 8 | 9 | 10 | 11 | 12 | 13 | 27)
            || (0x20..0x7f).contains(byte)
            || *byte >= 0x80
    })
}

fn shebang_interpreter(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut buf = [0u8; 256];
    let n = file.read(&mut buf).ok()?;
    let line = buf[..n].split(|byte| *byte == b'\n').next().unwrap_or(&[]);
    let line = std::str::from_utf8(line).ok()?.trim();
    let rest = line.strip_prefix("#!")?;
    if rest
        .chars()
        .any(|ch| !ch.is_ascii() || ch.is_ascii_control())
    {
        return None;
    }
    let mut parts = rest.split_whitespace();
    let first = parts.next()?;
    let interpreter = if first.ends_with("/env") || first == "env" {
        let next = parts.next()?;
        if next == "-S" {
            parts.next()?
        } else {
            next
        }
    } else {
        first
    };
    Some(interpreter.to_string())
}

fn set(tags: &[&'static str]) -> BTreeSet<&'static str> {
    tags.iter().copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn known_tags_include_mode_and_python() {
        assert!(is_known_tag("python"));
        assert!(is_known_tag("file"));
        assert!(!is_known_tag("not-a-real-tag"));
    }

    #[test]
    fn classifies_python_and_directory() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("classify");
        let file = dir.join("main.py");
        let mut handle = File::create(&file).unwrap();
        writeln!(handle, "print('hi')").unwrap();
        let tags = classify(&file, None).unwrap();
        assert!(tags.contains("python"));
        assert!(tags.contains("text"));
        assert!(tags.contains("file"));
        assert!(tags.contains("non-executable"));
        let dir_tags = classify(&dir, None).unwrap();
        assert_eq!(
            dir_tags.iter().copied().collect::<Vec<_>>(),
            vec!["directory"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_mode_overrides_filesystem_bit() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("git-mode");
        let file = dir.join("tool.sh");
        std::fs::write(&file, "echo hi\n").unwrap();
        let tags = classify(&file, Some("100755")).unwrap();
        assert!(tags.contains("executable"));
        assert!(tags.contains("shell"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn text_sample_rejects_nul() {
        assert!(is_text_sample(b"hello\n"));
        assert!(!is_text_sample(b"hel\0lo"));
    }
}
