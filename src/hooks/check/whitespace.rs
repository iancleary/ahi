use super::{flag_values, Check, CheckContext, CheckOutcome, Finding, Rewrite};

pub struct TrailingWhitespace;
pub struct EndOfFile;
pub struct LineEndings;

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
        "Reports lines with trailing whitespace. A Markdown double-space linebreak is kept when --markdown-linebreak-ext names that extension (or *). The check is report-only unless `ahi run --fix` or a hook argument `--fix` is set, which rewrites the file and fails the hook."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["text"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("trailing-whitespace")
    }
    fn fixes(&self) -> bool {
        true
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let exts = markdown_exts(ctx.args);
        let chars = flag_values(ctx.args, "--chars")
            .first()
            .map(|value| value.as_bytes().to_vec());
        let mut findings = Vec::new();
        let mut rewrites = Vec::new();
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            let markdown = is_markdown(file, &exts);
            if ctx.fix {
                if let Some(updated) = fix_trailing(&bytes, markdown, chars.as_deref()) {
                    findings.push(Finding::at(file, None, "fixed trailing whitespace"));
                    rewrites.push(Rewrite {
                        path: file.clone(),
                        bytes: updated,
                    });
                }
            } else {
                for (line_no, line) in lines(&bytes) {
                    if line_has_trailing(line, markdown, chars.as_deref()) {
                        findings.push(Finding::at(file, Some(line_no), "trailing whitespace"));
                    }
                }
            }
        }
        CheckOutcome::with_rewrites(findings, rewrites)
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
        "Reports a missing final newline, extra trailing newlines, or a file that is only newlines. Empty files are allowed. The check is report-only unless `ahi run --fix` or a hook argument `--fix` is set, which rewrites the file and fails the hook."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["text"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("end-of-file-fixer")
    }
    fn fixes(&self) -> bool {
        true
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let mut findings = Vec::new();
        let mut rewrites = Vec::new();
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            if let Some(updated) = fixed_end_of_file(&bytes) {
                if ctx.fix {
                    findings.push(Finding::at(file, None, "fixed end of file"));
                    rewrites.push(Rewrite {
                        path: file.clone(),
                        bytes: updated,
                    });
                } else {
                    findings.push(Finding::at(
                        file,
                        None,
                        "file must be empty or end with a single newline",
                    ));
                }
            }
        }
        CheckOutcome::with_rewrites(findings, rewrites)
    }
}

impl Check for LineEndings {
    fn id(&self) -> &'static str {
        "ahi/line-endings"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["mixed-line-ending"]
    }
    fn name(&self) -> &'static str {
        "Mixed Line Endings"
    }
    fn description(&self) -> &'static str {
        "Reports CR, LF, and CRLF mixed in one file. The check is report-only, like upstream --fix=no, unless `ahi run --fix` or a hook argument `--fix` (auto, lf, crlf, or cr) is set. --fix=no stays report-only."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["text"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("mixed-line-ending")
    }
    fn fixes(&self) -> bool {
        true
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let mode = line_ending_mode(ctx.args, ctx.fix);
        let mut findings = Vec::new();
        let mut rewrites = Vec::new();
        if let Some(mode) = mode {
            for file in files {
                let Ok(bytes) = ctx.read(file) else {
                    continue;
                };
                match apply_line_endings(&bytes, mode, ctx.fix) {
                    LineEndingResult::Clean => {}
                    LineEndingResult::Mixed => {
                        findings.push(Finding::at(file, None, "mixed line endings"));
                    }
                    LineEndingResult::Fixed(updated) => {
                        findings.push(Finding::at(file, None, "fixed mixed line endings"));
                        rewrites.push(Rewrite {
                            path: file.clone(),
                            bytes: updated,
                        });
                    }
                    LineEndingResult::BadMode(mode) => {
                        findings.push(Finding::bare(format!(
                            "--fix={mode} is not auto, no, lf, crlf, or cr"
                        )));
                        break;
                    }
                }
            }
        }
        CheckOutcome::with_rewrites(findings, rewrites)
    }
}

fn line_ending_mode(args: &[String], fix: bool) -> Option<&str> {
    let mut selected = None;
    for flag in ["--fix", "-f"] {
        if let Some(value) = flag_values(args, flag).last().copied() {
            selected = Some(value);
        }
    }
    selected.or(if fix { Some("auto") } else { Some("no") })
}

#[derive(Debug)]
enum LineEndingResult {
    Clean,
    Mixed,
    Fixed(Vec<u8>),
    BadMode(String),
}

fn apply_line_endings(bytes: &[u8], mode: &str, allow_write: bool) -> LineEndingResult {
    let cr = b"\r".as_slice();
    let crlf = b"\r\n".as_slice();
    let lf = b"\n".as_slice();
    let endings = [cr, crlf, lf];
    let mut counts = [0usize; 3];
    for line in split_keep(bytes) {
        for (index, ending) in endings.iter().enumerate() {
            if line.ends_with(ending) {
                counts[index] += 1;
                break;
            }
        }
    }
    let mixed = counts.iter().filter(|count| **count > 0).count() > 1;
    if mode == "no" || (mode == "auto" && !mixed) {
        return if mixed {
            LineEndingResult::Mixed
        } else {
            LineEndingResult::Clean
        };
    }
    if !allow_write {
        return if mixed {
            LineEndingResult::Mixed
        } else {
            LineEndingResult::Clean
        };
    }
    let target: &[u8] = if mode == "auto" {
        let mut best = lf;
        let mut best_count = 0usize;
        for (index, ending) in endings.iter().enumerate() {
            if counts[index] >= best_count {
                best = ending;
                best_count = counts[index];
            }
        }
        best
    } else {
        match mode {
            "cr" => cr,
            "crlf" => crlf,
            "lf" => lf,
            other => return LineEndingResult::BadMode(other.to_string()),
        }
    };
    if mode != "auto" {
        let others = match mode {
            "cr" => counts[1] + counts[2],
            "crlf" => counts[0] + counts[2],
            "lf" => counts[0] + counts[1],
            _ => 0,
        };
        if others == 0 {
            return LineEndingResult::Clean;
        }
    }
    LineEndingResult::Fixed(rewrite_endings(bytes, target))
}

fn rewrite_endings(bytes: &[u8], ending: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for line in split_keep(bytes) {
        let body = rstrip_ending(line);
        out.extend_from_slice(body);
        out.extend_from_slice(ending);
    }
    out
}

fn rstrip_ending(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r') {
        end -= 1;
    }
    &line[..end]
}

fn split_keep(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\r' {
            let next = index + 1;
            if next < bytes.len() && bytes[next] == b'\n' {
                out.push(&bytes[start..next + 1]);
                start = next + 1;
                index = start;
            } else {
                out.push(&bytes[start..next]);
                start = next;
                index = start;
            }
        } else if bytes[index] == b'\n' {
            out.push(&bytes[start..index + 1]);
            start = index + 1;
            index = start;
        } else {
            index += 1;
        }
    }
    if start < bytes.len() {
        out.push(&bytes[start..]);
    }
    out
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
    process_line(line, markdown, chars).as_slice() != line
}

fn process_line(line: &[u8], markdown: bool, chars: Option<&[u8]>) -> Vec<u8> {
    let (body, eol) = split_eol(line);
    if markdown && !body.iter().all(|byte| is_ascii_ws(*byte)) && body.ends_with(b"  ") {
        let mut trimmed = rstrip(&body[..body.len() - 2], chars);
        trimmed.extend_from_slice(b"  ");
        trimmed.extend_from_slice(eol);
        trimmed
    } else {
        let mut trimmed = rstrip(body, chars);
        trimmed.extend_from_slice(eol);
        trimmed
    }
}

fn fix_trailing(bytes: &[u8], markdown: bool, chars: Option<&[u8]>) -> Option<Vec<u8>> {
    if bytes.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut changed = false;
    for (_, line) in lines(bytes) {
        let fixed = process_line(line, markdown, chars);
        if fixed.as_slice() != line {
            changed = true;
        }
        out.extend_from_slice(&fixed);
    }
    changed.then_some(out)
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

/// Same cases as pre-commit-hooks `end_of_file_fixer.fix_file`.
fn fixed_end_of_file(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.is_empty() {
        return None;
    }
    let mut data = bytes.to_vec();
    let mut pos = data.len() - 1;
    let mut last = data[pos];
    pos += 1;
    if last != b'\n' && last != b'\r' {
        data.push(b'\n');
        return Some(data);
    }
    while last == b'\n' || last == b'\r' {
        if pos == 1 {
            return Some(Vec::new());
        }
        pos -= 2;
        last = data[pos];
        pos += 1;
    }
    let position = pos;
    let remaining = &data[position..];
    for sequence in [b"\n".as_slice(), b"\r\n".as_slice(), b"\r".as_slice()] {
        if remaining == sequence {
            return None;
        } else if remaining.starts_with(sequence) {
            data.truncate(position + sequence.len());
            return Some(data);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eof_cases_match_upstream_fixer() {
        assert!(fixed_end_of_file(b"").is_none());
        assert_eq!(fixed_end_of_file(b"a").as_deref(), Some(b"a\n".as_slice()));
        assert!(fixed_end_of_file(b"a\n").is_none());
        assert_eq!(
            fixed_end_of_file(b"a\n\n").as_deref(),
            Some(b"a\n".as_slice())
        );
        assert!(fixed_end_of_file(b"a\r\n").is_none());
        assert_eq!(
            fixed_end_of_file(b"a\r\n\r\n").as_deref(),
            Some(b"a\r\n".as_slice())
        );
        assert_eq!(fixed_end_of_file(b"\n").as_deref(), Some(b"".as_slice()));
        assert_eq!(fixed_end_of_file(b"\n\n").as_deref(), Some(b"".as_slice()));
    }

    #[test]
    fn markdown_keeps_two_spaces_only_when_asked() {
        assert!(line_has_trailing(b"hello  \n", false, None));
        assert!(!line_has_trailing(b"hello  \n", true, None));
        assert!(line_has_trailing(b"hello \n", true, None));
        assert!(!line_has_trailing(b"hello\n", false, None));
    }

    #[test]
    fn line_endings_ignore_a_single_style_and_flag_a_mix() {
        assert!(matches!(
            apply_line_endings(b"a\nb\n", "no", false),
            LineEndingResult::Clean
        ));
        assert!(matches!(
            apply_line_endings(b"a\r\nb\r\n", "no", false),
            LineEndingResult::Clean
        ));
        assert!(matches!(
            apply_line_endings(b"a\nb\r\n", "no", false),
            LineEndingResult::Mixed
        ));
        match apply_line_endings(b"a\nb\r\n", "lf", true) {
            LineEndingResult::Fixed(bytes) => assert_eq!(bytes, b"a\nb\n"),
            other => panic!("expected a rewrite, got a different result: {other:?}"),
        }
    }

    #[test]
    fn trailing_fix_removes_the_space_and_keeps_a_markdown_break() {
        assert_eq!(
            fix_trailing(b"hello \n", false, None).as_deref(),
            Some(b"hello\n".as_slice())
        );
        assert!(fix_trailing(b"hello  \n", true, None).is_none());
    }
}
