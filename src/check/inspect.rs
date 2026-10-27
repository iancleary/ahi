//! Offline file inspections that do not need a language runtime.
//!
//! `ahi/aws-credentials` looks for key shapes in the files under check. It does
//! not read `~/.aws` or the process environment. That is a deliberate divergence
//! from upstream `detect-aws-credentials`, which compares file bytes to secrets
//! loaded from the operator's credential files.

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{flag_values, Check, CheckContext, CheckOutcome, Finding};
use crate::git;

pub struct ShebangExecutable;
pub struct ExecutableShebang;
pub struct NoCommitToBranch;
pub struct XmlCheck;
pub struct AwsCredentials;
pub struct VcsPermalinks;
pub struct ByteOrderMarker;

impl Check for ShebangExecutable {
    fn id(&self) -> &'static str {
        "ahi/shebang-executable"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["check-executables-have-shebangs"]
    }
    fn name(&self) -> &'static str {
        "Check Executables Have Shebangs"
    }
    fn description(&self) -> &'static str {
        "An executable text file must start with #!. Tracked files use the git index mode so Windows and WSL agree. Upstream check-executables-have-shebangs."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["text", "executable"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("check-executables-have-shebangs")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let mut findings = Vec::new();
        for file in files {
            if !is_executable(ctx, file) {
                continue;
            }
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            if !has_shebang(&bytes) {
                findings.push(Finding::at(
                    file,
                    None,
                    "marked executable but has no shebang (chmod -x, or git add --chmod=-x on Windows)",
                ));
            }
        }
        CheckOutcome::from(findings)
    }
}

impl Check for ExecutableShebang {
    fn id(&self) -> &'static str {
        "ahi/executable-shebang"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["check-shebang-scripts-are-executable"]
    }
    fn name(&self) -> &'static str {
        "Check Shebang Scripts Are Executable"
    }
    fn description(&self) -> &'static str {
        "A tracked text file that starts with #! must be executable in the git index. Untracked files are skipped, matching upstream check-shebang-scripts-are-executable."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["text"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("check-shebang-scripts-are-executable")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let mut findings = Vec::new();
        for file in files {
            if !ctx.git_modes.contains_key(file) {
                continue;
            }
            if is_executable(ctx, file) {
                continue;
            }
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            if has_shebang(&bytes) {
                findings.push(Finding::at(
                    file,
                    None,
                    "has a shebang but is not marked executable (chmod +x, or git add --chmod=+x on Windows)",
                ));
            }
        }
        CheckOutcome::from(findings)
    }
}

impl Check for NoCommitToBranch {
    fn id(&self) -> &'static str {
        "ahi/no-commit-to-branch"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["no-commit-to-branch"]
    }
    fn name(&self) -> &'static str {
        "Don't Commit to Branch"
    }
    fn description(&self) -> &'static str {
        "Fails when the current branch is one of --branch (default main and master) or matches --pattern from the start of the name. A detached HEAD passes. This matches upstream no-commit-to-branch."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["file"]
    }
    fn default_always_run(&self) -> bool {
        true
    }
    fn default_pass_filenames(&self) -> bool {
        false
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("no-commit-to-branch")
    }
    fn check(&self, ctx: &CheckContext<'_>, _files: &[String]) -> CheckOutcome {
        let branches = configured_branches(ctx.args);
        let patterns = flag_values(ctx.args, "--pattern")
            .into_iter()
            .chain(flag_values(ctx.args, "-p"))
            .map(str::to_string)
            .collect::<Vec<_>>();
        let branch = match git::current_branch(ctx.root) {
            Ok(Some(branch)) => branch,
            Ok(None) => return CheckOutcome::ok(),
            Err(err) => {
                return CheckOutcome::from(vec![Finding::bare(format!(
                    "could not read the current branch: {err}"
                ))]);
            }
        };
        if branches.iter().any(|name| name == &branch) {
            return CheckOutcome::from(vec![Finding::bare(format!(
                "refusing to commit to branch `{branch}`"
            ))]);
        }
        for pattern in patterns {
            let regex = match regex::Regex::new(&pattern) {
                Ok(regex) => regex,
                Err(err) => {
                    return CheckOutcome::from(vec![Finding::bare(format!(
                        "--pattern `{pattern}` is not supported ({err})"
                    ))]);
                }
            };
            if regex.find(&branch).is_some_and(|found| found.start() == 0) {
                return CheckOutcome::from(vec![Finding::bare(format!(
                    "refusing to commit to branch `{branch}` matching `{pattern}`"
                ))]);
            }
        }
        CheckOutcome::ok()
    }
}

fn configured_branches(args: &[String]) -> Vec<String> {
    let listed = flag_values(args, "--branch")
        .into_iter()
        .chain(flag_values(args, "-b"))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if listed.is_empty() {
        vec!["master".to_string(), "main".to_string()]
    } else {
        listed
    }
}

impl Check for XmlCheck {
    fn id(&self) -> &'static str {
        "ahi/check-xml"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["check-xml"]
    }
    fn name(&self) -> &'static str {
        "Check Xml"
    }
    fn description(&self) -> &'static str {
        "Reports XML that is not well-formed. External entities are not resolved and no network request is made. Upstream check-xml."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["xml"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("check-xml")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let mut findings = Vec::new();
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            if let Err(err) = parse_xml(&bytes) {
                findings.push(Finding::at(file, None, format!("xml parse error: {err}")));
            }
        }
        CheckOutcome::from(findings)
    }
}

fn parse_xml(bytes: &[u8]) -> std::result::Result<(), String> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().check_end_names = true;
    let mut saw_root = false;
    let mut depth = 0i32;
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => {
                return if saw_root && depth == 0 {
                    Ok(())
                } else if !saw_root {
                    Err("missing root element".to_string())
                } else {
                    Err("unclosed element".to_string())
                };
            }
            Ok(Event::Start(_)) => {
                if depth == 0 && saw_root {
                    return Err("multiple root elements".to_string());
                }
                saw_root = true;
                depth += 1;
            }
            Ok(Event::End(_)) => {
                depth -= 1;
                if depth < 0 {
                    return Err("unexpected end element".to_string());
                }
            }
            Ok(Event::Empty(_)) => {
                if depth == 0 && saw_root {
                    return Err("multiple root elements".to_string());
                }
                saw_root = true;
            }
            Ok(Event::DocType(body)) => {
                let text = String::from_utf8_lossy(&body);
                let upper = text.to_ascii_uppercase();
                if upper.contains("SYSTEM") || upper.contains("PUBLIC") || upper.contains("ENTITY")
                {
                    return Err(
                        "doctype with external or entity declarations is refused".to_string()
                    );
                }
            }
            Ok(_) => {}
            Err(err) => return Err(err.to_string()),
        }
    }
}

impl Check for AwsCredentials {
    fn id(&self) -> &'static str {
        "ahi/aws-credentials"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["detect-aws-credentials"]
    }
    fn name(&self) -> &'static str {
        "Detect Aws Credentials"
    }
    fn description(&self) -> &'static str {
        "Reports AWS access key ids (AKIA, ASIA, ABIA, ACCA plus 16 uppercase letters or digits) and 40-character secret, session, or security tokens assigned on the same line. Does not read ~/.aws, credential files, or the environment. Upstream detect-aws-credentials instead searches for secrets it loaded from those files; ahi cannot do that inside a sandbox without a home directory."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["text"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("detect-aws-credentials")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let mut findings = Vec::new();
        if ctx
            .args
            .iter()
            .any(|arg| arg == "--credentials-file" || arg.starts_with("--credentials-file="))
        {
            findings.push(Finding::bare(
                "ahi does not read --credentials-file or ~/.aws; remove that argument",
            ));
        }
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            if let Some(line) = access_key_line(&bytes) {
                findings.push(Finding::at(file, Some(line), "AWS access key id pattern"));
            }
            if let Some(line) = secret_key_line(&bytes) {
                findings.push(Finding::at(file, Some(line), "AWS secret key assignment"));
            }
        }
        CheckOutcome::from(findings)
    }
}

fn access_key_line(bytes: &[u8]) -> Option<u32> {
    const PREFIXES: &[&[u8]] = &[b"AKIA", b"ASIA", b"ABIA", b"ACCA"];
    let mut line = 1u32;
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\n' {
            line += 1;
            index += 1;
            continue;
        }
        if index + 20 <= bytes.len()
            && PREFIXES
                .iter()
                .any(|prefix| bytes[index..].starts_with(prefix))
            && bytes[index..index + 20]
                .iter()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            && boundary_before(bytes, index)
            && boundary_after(bytes, index + 20)
        {
            return Some(line);
        }
        index += 1;
    }
    None
}

fn boundary_before(bytes: &[u8], index: usize) -> bool {
    index == 0 || !is_key_char(bytes[index - 1])
}

fn boundary_after(bytes: &[u8], index: usize) -> bool {
    index >= bytes.len() || !is_key_char(bytes[index])
}

fn is_key_char(byte: u8) -> bool {
    byte.is_ascii_uppercase() || byte.is_ascii_digit()
}

fn secret_key_line(bytes: &[u8]) -> Option<u32> {
    let text = String::from_utf8_lossy(bytes);
    for (offset, line) in text.split_inclusive('\n').enumerate() {
        let lower = line.to_ascii_lowercase();
        let Some(name_at) = [
            "aws_secret_access_key",
            "aws_session_token",
            "aws_security_token",
        ]
        .iter()
        .find_map(|name| lower.find(name)) else {
            continue;
        };
        let rest =
            line[name_at..].trim_start_matches(|ch: char| ch.is_ascii_alphabetic() || ch == '_');
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix(['=', ':']) else {
            continue;
        };
        let rest = rest
            .trim_start()
            .trim_matches(|ch| ch == '"' || ch == '\'' || ch == ' ');
        let token = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '+' | '='))
            .collect::<String>();
        if token.len() == 40 {
            return Some((offset + 1) as u32);
        }
    }
    None
}

impl Check for VcsPermalinks {
    fn id(&self) -> &'static str {
        "ahi/vcs-permalinks"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["check-vcs-permalinks"]
    }
    fn name(&self) -> &'static str {
        "Check Vcs Permalinks"
    }
    fn description(&self) -> &'static str {
        "Reports GitHub blob links whose revision is not a 4 to 64 digit hex commit and that point at a line number. A 40-hex permalink is allowed. --additional-github-domain adds a host. Upstream check-vcs-permalinks."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["text"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("check-vcs-permalinks")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let mut domains = vec!["github.com".to_string()];
        for extra in flag_values(ctx.args, "--additional-github-domain") {
            if !extra.is_empty() && !domains.iter().any(|have| have == extra) {
                domains.push(extra.to_string());
            }
        }
        let mut findings = Vec::new();
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            for (offset, line) in text.split_inclusive('\n').enumerate() {
                if domains.iter().any(|domain| non_permalink(line, domain)) {
                    findings.push(Finding::at(
                        file,
                        Some((offset + 1) as u32),
                        "non-permanent github link; press y on the GitHub page for a permalink",
                    ));
                }
            }
        }
        CheckOutcome::from(findings)
    }
}

fn non_permalink(line: &str, domain: &str) -> bool {
    let marker = format!("https://{domain}/");
    let mut rest = line;
    while let Some(start) = rest.find(&marker) {
        let after = &rest[start + marker.len()..];
        if let Some(blob) = after.find("/blob/") {
            let owner_repo = &after[..blob];
            if owner_repo
                .split('/')
                .filter(|part| !part.is_empty())
                .count()
                == 2
                && !owner_repo.contains(' ')
            {
                let tail = &after[blob + "/blob/".len()..];
                if let Some(rev) = revision_of(tail) {
                    if !is_hex_commit(rev) && has_line_anchor(tail) {
                        return true;
                    }
                }
            }
        }
        rest = &rest[start + marker.len()..];
    }
    false
}

fn revision_of(tail: &str) -> Option<&str> {
    let end = tail.find(['/', ' ', '#', '\n', '\r']).unwrap_or(tail.len());
    let rev = &tail[..end];
    if rev.is_empty() || rev.contains('.') {
        None
    } else {
        Some(rev)
    }
}

fn is_hex_commit(rev: &str) -> bool {
    (4..=64).contains(&rev.len()) && rev.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn has_line_anchor(tail: &str) -> bool {
    let Some(hash) = tail.find("#L") else {
        return false;
    };
    let digits = &tail[hash + 2..];
    digits.chars().next().is_some_and(|ch| ch.is_ascii_digit())
}

impl Check for ByteOrderMarker {
    fn id(&self) -> &'static str {
        "ahi/byte-order-marker"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["fix-byte-order-marker"]
    }
    fn name(&self) -> &'static str {
        "Fix Utf-8 Byte Order Marker"
    }
    fn description(&self) -> &'static str {
        "Reports a leading UTF-8 BOM (EF BB BF). The check is report-only. `ahi run --fix` does not strip the marker."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["text"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("fix-byte-order-marker")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let mut findings = Vec::new();
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
                findings.push(Finding::at(file, Some(1), "utf-8 byte-order marker"));
            }
        }
        CheckOutcome::from(findings)
    }
}

fn has_shebang(bytes: &[u8]) -> bool {
    bytes.starts_with(b"#!")
}

fn is_executable(ctx: &CheckContext<'_>, file: &str) -> bool {
    let Some(mode) = ctx.git_modes.get(file) else {
        return false;
    };
    mode.chars()
        .last()
        .is_some_and(|ch| matches!(ch, '1' | '3' | '5' | '7'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;

    fn ctx<'a>(
        root: &'a Path,
        args: &'a [String],
        modes: &'a BTreeMap<String, String>,
        fix: bool,
    ) -> CheckContext<'a> {
        CheckContext {
            root,
            args,
            git_modes: modes,
            fix,
        }
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        crate::fixture_directory::create_unique_fixture_directory(label)
    }

    #[test]
    fn shebang_checks_follow_git_mode() {
        let dir = temp_dir("shebang");
        std::fs::write(dir.join("run"), b"#!/bin/sh\necho\n").unwrap();
        std::fs::write(dir.join("tool"), b"echo\n").unwrap();
        std::fs::write(dir.join("plain.txt"), b"hello\n").unwrap();
        let mut modes = BTreeMap::new();
        modes.insert("run".to_string(), "100755".to_string());
        modes.insert("tool".to_string(), "100755".to_string());
        modes.insert("plain.txt".to_string(), "100644".to_string());
        let args = Vec::new();
        let context = ctx(&dir, &args, &modes, false);
        let executable = ShebangExecutable.check(
            &context,
            &[
                "run".to_string(),
                "tool".to_string(),
                "plain.txt".to_string(),
            ],
        );
        assert_eq!(executable.findings.len(), 1);
        assert_eq!(executable.findings[0].path.as_deref(), Some("tool"));
        let scripts =
            ExecutableShebang.check(&context, &["run".to_string(), "plain.txt".to_string()]);
        assert!(scripts.findings.is_empty());
        let mut plain_modes = BTreeMap::new();
        plain_modes.insert("run".to_string(), "100644".to_string());
        let context = ctx(&dir, &args, &plain_modes, false);
        let missing = ExecutableShebang.check(&context, &["run".to_string()]);
        assert_eq!(missing.findings.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn xml_accepts_well_formed_and_rejects_a_broken_tag() {
        let dir = temp_dir("xml");
        std::fs::write(dir.join("ok.xml"), b"<root attr=\"x\">1 &lt; 2</root>\n").unwrap();
        std::fs::write(dir.join("bad.xml"), b"<root>\n").unwrap();
        let args = Vec::new();
        let modes = BTreeMap::new();
        let context = ctx(&dir, &args, &modes, false);
        let ok = XmlCheck.check(&context, &["ok.xml".to_string()]);
        assert!(ok.findings.is_empty(), "{ok:?}");
        let bad = XmlCheck.check(&context, &["bad.xml".to_string()]);
        assert_eq!(bad.findings.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn aws_patterns_skip_near_misses_and_unlabeled_secrets() {
        let dir = temp_dir("aws");
        std::fs::write(
            dir.join("bad.txt"),
            b"id=AKIAIOSFODNN7EXAMPLE\naws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ok.txt"),
            b"short AKIAIOSFODNN7EXAMPL\nnotAKIAIOSFODNN7EXAMPLEX\naws_secret_access_key = short\nwJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\n",
        )
        .unwrap();
        let args = Vec::new();
        let modes = BTreeMap::new();
        let context = ctx(&dir, &args, &modes, false);
        let bad = AwsCredentials.check(&context, &["bad.txt".to_string()]);
        assert_eq!(bad.findings.len(), 2, "{bad:?}");
        let ok = AwsCredentials.check(&context, &["ok.txt".to_string()]);
        assert!(ok.findings.is_empty(), "{ok:?}");
        let args = vec!["--credentials-file".to_string(), "/tmp/nope".to_string()];
        let context = ctx(&dir, &args, &modes, false);
        let refused = AwsCredentials.check(&context, &["ok.txt".to_string()]);
        assert!(refused
            .findings
            .iter()
            .any(|finding| finding.message.contains("does not read")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn permalinks_allow_a_commit_and_flag_a_branch() {
        let dir = temp_dir("permalink");
        let sha = "0123456789abcdef0123456789abcdef01234567";
        std::fs::write(
            dir.join("doc.md"),
            format!(
                "ok https://github.com/org/repo/blob/{sha}/src/lib.rs#L10\nbad https://github.com/org/repo/blob/main/src/lib.rs#L10\nnope https://github.com/org/repo/blob/main/README.md\n"
            )
            .as_bytes(),
        )
        .unwrap();
        let args = Vec::new();
        let modes = BTreeMap::new();
        let context = ctx(&dir, &args, &modes, false);
        let outcome = VcsPermalinks.check(&context, &["doc.md".to_string()]);
        assert_eq!(outcome.findings.len(), 1, "{outcome:?}");
        assert_eq!(outcome.findings[0].line, Some(2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bom_is_report_only_until_fix() {
        let dir = temp_dir("bom");
        std::fs::write(dir.join("bom.txt"), b"\xef\xbb\xbfhello\n").unwrap();
        std::fs::write(dir.join("plain.txt"), b"hello\n").unwrap();
        let args = Vec::new();
        let modes = BTreeMap::new();
        let report = ctx(&dir, &args, &modes, false);
        let found =
            ByteOrderMarker.check(&report, &["bom.txt".to_string(), "plain.txt".to_string()]);
        assert_eq!(found.findings.len(), 1);
        assert!(found.rewrites.is_empty());
        let fix = ctx(&dir, &args, &modes, true);
        let rewritten = ByteOrderMarker.check(&fix, &["bom.txt".to_string()]);
        assert!(rewritten.rewrites.is_empty());
        assert_eq!(rewritten.findings.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn branch_defaults_skip_a_feature_branch() {
        let dir = temp_dir("branch");
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "{args:?}");
        };
        git(&["init", "-q", "--template="]);
        git(&["config", "user.email", "ahi@example.com"]);
        git(&["config", "user.name", "ahi"]);
        std::fs::write(dir.join("f"), b"a\n").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-qm", "init"]);
        git(&["checkout", "-q", "-b", "feature"]);
        let args = Vec::new();
        let modes = BTreeMap::new();
        let context = ctx(&dir, &args, &modes, false);
        let ok = NoCommitToBranch.check(&context, &[]);
        assert!(ok.findings.is_empty(), "{ok:?}");
        git(&["checkout", "-q", "-b", "main"]);
        let denied = NoCommitToBranch.check(&context, &[]);
        assert_eq!(denied.findings.len(), 1);
        let args = vec!["--pattern".to_string(), "release".to_string()];
        git(&["checkout", "-q", "-b", "my-release"]);
        let context = ctx(&dir, &args, &modes, false);
        let not_prefix = NoCommitToBranch.check(&context, &[]);
        assert!(
            not_prefix.findings.is_empty(),
            "pattern is anchored at the start: {not_prefix:?}"
        );
        git(&["checkout", "-q", "-b", "release-1"]);
        let prefixed = NoCommitToBranch.check(&context, &[]);
        assert_eq!(prefixed.findings.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
