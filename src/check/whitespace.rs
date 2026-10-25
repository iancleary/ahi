use super::{flag_values, Check, CheckContext, CheckOutcome, Finding};

pub struct TrailingWhitespace;
pub struct EndOfFile;

impl Check for TrailingWhitespace {
    fn id(&self) -> &'static str {
        "ahi/trailing-whitespace"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["trailing-whitespace"]
    }
    fn name(&self) -> &'static str {
        "Trim Trailing Whitespace"
    }
    fn description(&self) -> &'static str {
        "Reports lines with trailing whitespace. A Markdown double-space linebreak is kept when --markdown-linebreak-ext names that extension (or *). v0.1 does not rewrite files."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["text"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("trailing-whitespace")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let exts = markdown_exts(ctx.args);
        let chars = flag_values(ctx.args, "--chars")
            .first()
            .map(|value| value.as_bytes().to_vec());
        let mut findings = Vec::new();
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            let markdown = is_markdown(file, &exts);
            for (line_no, line) in lines(&bytes) {
                if line_has_trailing(line, markdown, chars.as_deref()) {
                    findings.push(Finding::at(file, Some(line_no), "trailing whitespace"));
                }
            }
        }
        CheckOutcome::from(findings)
    }
}

impl Check for EndOfFile {
    fn id(&self) -> &'static str {
        "ahi/end-of-file"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["end-of-file-fixer"]
    }
    fn name(&self) -> &'static str {
        "Fix End of Files"
    }
    fn description(&self) -> &'static str {
        "Reports a missing final newline, extra trailing newlines, or a file that is only newlines. Empty files are allowed. v0.1 does not rewrite files."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["text"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("end-of-file-fixer")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let mut findings = Vec::new();
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            if end_of_file_needs_fix(&bytes) {
                findings.push(Finding::at(
                    file,
                    None,
                    "file must be empty or end with a single newline",
                ));
            }
        }
        CheckOutcome::from(findings)
    }
}

fn markdown_exts(args: &[String]) -> Vec<String> {
    let mut exts = Vec::new();
    for value in flag_values(args, "--markdown-linebreak-ext") {
        for part in value.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if part == "*" {
                exts.push("*".to_string());
            } else {
                exts.push(format!(
                    ".{}",
                    part.trim_start_matches('.').to_ascii_lowercase()
                ));
            }
        }
    }
    exts
}

fn is_markdown(path: &str, exts: &[String]) -> bool {
    if exts.iter().any(|ext| ext == "*") {
        return true;
    }
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let ext = name
        .rsplit_once('.')
        .map(|(_, ext)| format!(".{ext}"))
        .unwrap_or_default();
    exts.iter().any(|candidate| candidate == &ext)
}

fn lines(bytes: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut number = 1u32;
    if bytes.is_empty() {
        return out;
    }
    while start < bytes.len() {
        let rest = &bytes[start..];
        let rel = rest.iter().position(|byte| *byte == b'\n');
        let end = match rel {
            Some(index) => start + index + 1,
            None => bytes.len(),
        };
        out.push((number, &bytes[start..end]));
        number += 1;
        start = end;
    }
    out
}

fn line_has_trailing(line: &[u8], markdown: bool, chars: Option<&[u8]>) -> bool {
    let (body, eol) = split_eol(line);
    let fixed = if markdown && !body.iter().all(|byte| is_ascii_ws(*byte)) && body.ends_with(b"  ")
    {
        let mut trimmed = rstrip(&body[..body.len() - 2], chars);
        trimmed.extend_from_slice(b"  ");
        trimmed.extend_from_slice(eol);
        trimmed
    } else {
        let mut trimmed = rstrip(body, chars);
        trimmed.extend_from_slice(eol);
        trimmed
    };
    fixed.as_slice() != line
}

fn split_eol(line: &[u8]) -> (&[u8], &[u8]) {
    if line.ends_with(b"\r\n") {
        (&line[..line.len() - 2], &line[line.len() - 2..])
    } else if line.ends_with(b"\n") || line.ends_with(b"\r") {
        (&line[..line.len() - 1], &line[line.len() - 1..])
    } else {
        (line, b"")
    }
}

fn rstrip(bytes: &[u8], chars: Option<&[u8]>) -> Vec<u8> {
    let mut end = bytes.len();
    while end > 0 && should_strip(bytes[end - 1], chars) {
        end -= 1;
    }
    bytes[..end].to_vec()
}

fn should_strip(byte: u8, chars: Option<&[u8]>) -> bool {
    match chars {
        Some(chars) => chars.contains(&byte),
        None => is_ascii_ws(byte),
    }
}

fn is_ascii_ws(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Same cases as pre-commit-hooks `end_of_file_fixer.fix_file`, without writing.
pub fn end_of_file_needs_fix(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let mut tell = bytes.len();
    let mut last = bytes[tell - 1];
    if last != b'\n' && last != b'\r' {
        return true;
    }
    loop {
        if tell == 1 {
            return true;
        }
        tell -= 2;
        last = bytes[tell];
        tell += 1;
        if last != b'\n' && last != b'\r' {
            break;
        }
    }
    let remaining = &bytes[tell..];
    for sequence in [b"\n".as_slice(), b"\r\n".as_slice(), b"\r".as_slice()] {
        if remaining == sequence {
            return false;
        } else if remaining.starts_with(sequence) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eof_cases_match_upstream_fixer() {
        assert!(!end_of_file_needs_fix(b""));
        assert!(end_of_file_needs_fix(b"a"));
        assert!(!end_of_file_needs_fix(b"a\n"));
        assert!(end_of_file_needs_fix(b"a\n\n"));
        assert!(!end_of_file_needs_fix(b"a\r\n"));
        assert!(end_of_file_needs_fix(b"a\r\n\r\n"));
        assert!(end_of_file_needs_fix(b"\n"));
        assert!(end_of_file_needs_fix(b"\n\n"));
    }

    #[test]
    fn markdown_keeps_two_spaces_only_when_asked() {
        assert!(line_has_trailing(b"hello  \n", false, None));
        assert!(!line_has_trailing(b"hello  \n", true, None));
        assert!(line_has_trailing(b"hello \n", true, None));
        assert!(!line_has_trailing(b"hello\n", false, None));
    }
}
