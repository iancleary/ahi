use std::sync::OnceLock;

use regex::Regex;
use serde_yaml::Value;

use super::{flag_values, Check, CheckContext, CheckOutcome, Finding};

pub struct GithubActions;
pub struct DockerfileCheck;
pub struct ShellStrict;

impl Check for GithubActions {
    fn id(&self) -> &'static str {
        "ahi/github-actions"
    }
    fn name(&self) -> &'static str {
        "GitHub Actions pinning"
    }
    fn description(&self) -> &'static str {
        "Requires actions and reusable workflows to be pinned to a full commit SHA (or an image digest), requires an explicit permissions block of read or none, and flags pull_request_target checkouts of the PR head or use of secrets in run steps. --allow-write=contents permits that one write permission."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["yaml"]
    }
    fn default_files(&self) -> &'static str {
        r"^\.github/workflows/[^/]+\.ya?ml$"
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let allow_write: Vec<String> = flag_values(ctx.args, "--allow-write")
            .into_iter()
            .map(str::to_string)
            .collect();
        let mut findings = Vec::new();
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            let value: Value = match serde_yaml::from_slice(&bytes) {
                Ok(value) => value,
                Err(err) => {
                    findings.push(Finding::at(file, None, format!("workflow YAML: {err}")));
                    continue;
                }
            };
            findings.extend(check_workflow(file, &value, &allow_write));
        }
        CheckOutcome::from(findings)
    }
}

impl Check for DockerfileCheck {
    fn id(&self) -> &'static str {
        "ahi/dockerfile"
    }
    fn name(&self) -> &'static str {
        "Dockerfile supply chain"
    }
    fn description(&self) -> &'static str {
        "Requires every FROM image except scratch to be pinned by sha256 digest, a non-root USER in the final stage, no curl|sh or wget|sh, a checksum next to curl/wget downloads, and version-pinned apt packages."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["dockerfile"]
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let mut findings = Vec::new();
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            let text = match std::str::from_utf8(&bytes) {
                Ok(text) => text,
                Err(_) => {
                    findings.push(Finding::at(file, None, "Dockerfile is not UTF-8"));
                    continue;
                }
            };
            findings.extend(check_dockerfile(file, text));
        }
        CheckOutcome::from(findings)
    }
}

impl Check for ShellStrict {
    fn id(&self) -> &'static str {
        "ahi/shell-strict"
    }
    fn name(&self) -> &'static str {
        "Shell strict mode"
    }
    fn description(&self) -> &'static str {
        "Bash and zsh scripts must set -euo pipefail (or the three options separately). POSIX sh must set -eu; pipefail is not required because it is not POSIX."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["shell"]
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let mut findings = Vec::new();
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            let Some(kind) = shell_kind(file, &text) else {
                continue;
            };
            let code = strip_comments(&text);
            match kind {
                ShellKind::Bash => {
                    if !bash_strict(&code) {
                        findings.push(Finding::at(
                            file,
                            Some(1),
                            "bash script is missing `set -euo pipefail` (or errexit, nounset, and pipefail)",
                        ));
                    }
                }
                ShellKind::Posix => {
                    if !posix_strict(&code) {
                        findings.push(Finding::at(
                            file,
                            Some(1),
                            "POSIX sh script is missing `set -eu` (pipefail is not required for sh)",
                        ));
                    }
                }
            }
        }
        CheckOutcome::from(findings)
    }
}

fn check_workflow(file: &str, value: &Value, allow_write: &[String]) -> Vec<Finding> {
    let Some(root) = value.as_mapping() else {
        return vec![Finding::at(file, None, "workflow root must be a mapping")];
    };
    let mut findings = Vec::new();
    let on = root
        .get(Value::String("on".to_string()))
        .or_else(|| root.get(Value::Bool(true)));
    let pr_target = on.map(uses_pull_request_target).unwrap_or(false);
    let mut uses = Vec::new();
    collect_uses(value, &mut uses);
    for uses_value in uses {
        if let Some(message) = uses_problem(&uses_value) {
            findings.push(Finding::at(file, None, message));
        }
    }
    match permissions_problem(root, allow_write) {
        Ok(()) => {}
        Err(message) => findings.push(Finding::at(file, None, message)),
    }
    if pr_target {
        if checkout_of_pr_head(value) {
            findings.push(Finding::at(
                file,
                None,
                "pull_request_target checks out the pull request head",
            ));
        }
        if run_uses_secrets(value) {
            findings.push(Finding::at(
                file,
                None,
                "pull_request_target run step references secrets",
            ));
        }
    }
    findings
}

fn uses_pull_request_target(value: &Value) -> bool {
    match value {
        Value::String(text) => text == "pull_request_target",
        Value::Sequence(items) => items.iter().any(uses_pull_request_target),
        Value::Mapping(map) => map.keys().any(|key| match key {
            Value::String(text) => text == "pull_request_target",
            _ => false,
        }),
        _ => false,
    }
}

fn collect_uses(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Mapping(map) => {
            for (key, child) in map {
                if key.as_str() == Some("uses") {
                    if let Some(text) = child.as_str() {
                        out.push(text.to_string());
                    }
                }
                collect_uses(child, out);
            }
        }
        Value::Sequence(items) => {
            for item in items {
                collect_uses(item, out);
            }
        }
        _ => {}
    }
}

fn uses_problem(uses: &str) -> Option<String> {
    if uses.starts_with("./") {
        return None;
    }
    if let Some(image) = uses.strip_prefix("docker://") {
        let digest = image.split('@').nth(1).unwrap_or("");
        if let Some(hex) = digest.strip_prefix("sha256:") {
            if hex.len() == 64 && hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
                return None;
            }
        }
        return Some(format!(
            "docker action `{uses}` is not pinned by a sha256 digest"
        ));
    }
    let Some((left, right)) = uses.rsplit_once('@') else {
        return Some(format!(
            "action `{uses}` must be pinned to a full 40-character commit SHA"
        ));
    };
    if right.len() == 40 && right.chars().all(|ch| ch.is_ascii_hexdigit()) && left.contains('/') {
        return None;
    }
    Some(format!(
        "action `{uses}` must be pinned to a full 40-character commit SHA"
    ))
}

fn permissions_problem(
    root: &serde_yaml::Mapping,
    allow_write: &[String],
) -> std::result::Result<(), String> {
    let root_perms = root.get(Value::String("permissions".to_string()));
    if let Some(perms) = root_perms {
        check_perm_value(perms, allow_write)?;
    }
    let Some(jobs) = root.get(Value::String("jobs".to_string())) else {
        if root_perms.is_none() {
            return Err("workflow is missing permissions".to_string());
        }
        return Ok(());
    };
    let Some(jobs) = jobs.as_mapping() else {
        return Err("jobs must be a mapping".to_string());
    };
    if jobs.is_empty() && root_perms.is_none() {
        return Err("workflow is missing permissions".to_string());
    }
    for (name, job) in jobs {
        let job_perms = job
            .as_mapping()
            .and_then(|map| map.get(Value::String("permissions".to_string())));
        match job_perms {
            Some(perms) => check_perm_value(perms, allow_write)?,
            None if root_perms.is_none() => {
                return Err(format!(
                    "job `{}` is missing permissions",
                    name.as_str().unwrap_or("?")
                ));
            }
            None => {}
        }
    }
    Ok(())
}

fn check_perm_value(value: &Value, allow_write: &[String]) -> std::result::Result<(), String> {
    match value {
        Value::String(text) if text == "write-all" => {
            Err("permissions write-all is not least privilege".to_string())
        }
        Value::String(_) => {
            Err("permissions must be a mapping whose values are read or none".to_string())
        }
        Value::Mapping(map) if map.is_empty() => Ok(()),
        Value::Mapping(map) => {
            for (key, perm) in map {
                let name = key.as_str().unwrap_or("");
                let value = perm.as_str().unwrap_or("");
                if value == "read" || value == "none" {
                    continue;
                }
                if allow_write.iter().any(|allowed| allowed == name) {
                    continue;
                }
                return Err(format!("permission `{name}: {value}` is not read or none"));
            }
            Ok(())
        }
        other => Err(format!("permissions value `{other:?}` is not a mapping")),
    }
}

fn checkout_of_pr_head(value: &Value) -> bool {
    let Value::Mapping(map) = value else {
        if let Value::Sequence(items) = value {
            return items.iter().any(checkout_of_pr_head);
        }
        return false;
    };
    let uses = map
        .get(Value::String("uses".to_string()))
        .and_then(Value::as_str)
        .unwrap_or("");
    if uses.starts_with("actions/checkout@") {
        if let Some(with) = map
            .get(Value::String("with".to_string()))
            .and_then(Value::as_mapping)
        {
            if let Some(reference) = with
                .get(Value::String("ref".to_string()))
                .and_then(Value::as_str)
            {
                if reference.contains("pull_request.head")
                    || reference.contains("head.sha")
                    || reference.contains("head.ref")
                {
                    return true;
                }
            }
        }
    }
    map.values().any(checkout_of_pr_head)
}

fn run_uses_secrets(value: &Value) -> bool {
    match value {
        Value::Mapping(map) => {
            if let Some(run) = map
                .get(Value::String("run".to_string()))
                .and_then(Value::as_str)
            {
                if run.contains("secrets.") {
                    return true;
                }
            }
            map.values().any(run_uses_secrets)
        }
        Value::Sequence(items) => items.iter().any(run_uses_secrets),
        _ => false,
    }
}

fn check_dockerfile(file: &str, text: &str) -> Vec<Finding> {
    let lines = logical_lines(text);
    let mut findings = Vec::new();
    let mut stages: Vec<Vec<String>> = Vec::new();
    for (line_no, line) in &lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (instruction, rest) = split_instruction(trimmed);
        match instruction.to_ascii_uppercase().as_str() {
            "FROM" => {
                stages.push(Vec::new());
                if let Some(message) = from_problem(rest) {
                    findings.push(Finding::at(file, Some(*line_no), message));
                }
            }
            "USER" => {
                if let Some(stage) = stages.last_mut() {
                    stage.push(rest.trim().to_string());
                }
            }
            "RUN" => {
                if curl_pipe_sh(rest) {
                    findings.push(Finding::at(
                        file,
                        Some(*line_no),
                        "RUN pipes curl or wget into a shell",
                    ));
                }
                if downloads_without_checksum(rest) {
                    findings.push(Finding::at(
                        file,
                        Some(*line_no),
                        "download with curl or wget is missing a checksum check in the same RUN (a later layer is not accepted in v0.1)",
                    ));
                }
                for package in unpinned_apt(rest) {
                    findings.push(Finding::at(
                        file,
                        Some(*line_no),
                        format!("apt package `{package}` is not pinned with =version"),
                    ));
                }
            }
            _ => {}
        }
    }
    match stages.last() {
        None => findings.push(Finding::at(file, None, "Dockerfile has no FROM")),
        Some(users) => {
            let last = users.last().map(String::as_str);
            let user = last
                .map(|value| value.split(':').next().unwrap_or(value).trim())
                .unwrap_or("");
            if user.is_empty() || user == "root" || user == "0" {
                findings.push(Finding::at(
                    file,
                    None,
                    "final stage USER must be present and must not be root",
                ));
            }
        }
    }
    findings
}

fn logical_lines(text: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut start = 1u32;
    for (index, line) in text.lines().enumerate() {
        let line_no = (index as u32) + 1;
        if buf.is_empty() {
            start = line_no;
        }
        let trimmed = line.trim_end();
        if let Some(stripped) = trimmed.strip_suffix('\\') {
            buf.push_str(stripped);
            buf.push(' ');
        } else {
            buf.push_str(trimmed);
            out.push((start, std::mem::take(&mut buf)));
        }
    }
    if !buf.is_empty() {
        out.push((start, buf));
    }
    out
}

fn split_instruction(line: &str) -> (&str, &str) {
    let line = line.trim_start();
    match line.find(char::is_whitespace) {
        Some(index) => (&line[..index], line[index..].trim()),
        None => (line, ""),
    }
}

fn from_problem(rest: &str) -> Option<String> {
    let mut tokens = rest.split_whitespace().peekable();
    let mut image = None;
    while let Some(token) = tokens.next() {
        if token.eq_ignore_ascii_case("AS") {
            break;
        }
        if let Some(stripped) = token.strip_prefix("--platform=") {
            let _ = stripped;
            continue;
        }
        if token == "--platform" {
            let _ = tokens.next();
            continue;
        }
        image = Some(token);
        break;
    }
    let Some(image) = image else {
        return Some("FROM is missing an image".to_string());
    };
    if image == "scratch" {
        return None;
    }
    let digest = image.split('@').nth(1).unwrap_or("");
    if digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.chars().all(|ch| ch.is_ascii_hexdigit()))
    {
        return None;
    }
    Some(format!(
        "base image `{image}` is not pinned by a sha256 digest"
    ))
}

fn curl_pipe() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(curl|wget)\b[^|\n]*\|\s*(sudo\s+)?(ba)?sh\b").expect("regex")
    })
}

fn curl_pipe_sh(run: &str) -> bool {
    curl_pipe().is_match(run)
}

fn downloads_without_checksum(run: &str) -> bool {
    let lower = run.to_ascii_lowercase();
    let fetches = (lower.contains("curl") || lower.contains("wget"))
        && (lower.contains("http://") || lower.contains("https://"));
    if !fetches {
        return false;
    }
    let verified = ["sha256sum", "sha384sum", "sha512sum", "shasum"]
        .iter()
        .any(|tool| lower.contains(tool))
        || lower.contains("cosign verify");
    !verified
}

fn unpinned_apt(run: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in run.split([';', '&']) {
        let lower = part.to_ascii_lowercase();
        let marker = if lower.contains("apt-get install") {
            "apt-get install"
        } else if lower.contains("apt install") {
            "apt install"
        } else {
            continue;
        };
        let Some(index) = lower.find(marker) else {
            continue;
        };
        let after = &part[index + marker.len()..];
        for token in after.split_whitespace() {
            if token.starts_with('-') || token == "&&" {
                continue;
            }
            if token.contains('=') {
                continue;
            }
            out.push(token.to_string());
        }
    }
    out
}

enum ShellKind {
    Bash,
    Posix,
}

fn shell_kind(path: &str, text: &str) -> Option<ShellKind> {
    let first = text.lines().next().unwrap_or("").trim();
    let shebang = first.starts_with("#!");
    if path.ends_with(".bash")
        || (shebang && first.contains("bash"))
        || (shebang && first.contains("zsh"))
    {
        return Some(ShellKind::Bash);
    }
    if path.ends_with(".sh") || shebang {
        return Some(ShellKind::Posix);
    }
    Some(ShellKind::Posix)
}

fn strip_comments(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn bash_strict(code: &str) -> bool {
    if code.contains("set -euo pipefail") {
        return true;
    }
    has_option(code, "errexit", 'e')
        && has_option(code, "nounset", 'u')
        && code.contains("pipefail")
}

fn posix_strict(code: &str) -> bool {
    has_option(code, "errexit", 'e') && has_option(code, "nounset", 'u')
}

fn has_option(code: &str, long: &str, short: char) -> bool {
    if code.contains(&format!("set -o {long}")) {
        return true;
    }
    code.lines().any(|line| {
        let line = line.trim();
        let Some(flags) = line.split_whitespace().nth(1) else {
            return false;
        };
        line.starts_with("set -") && flags.starts_with('-') && flags.contains(short)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_pin_rules() {
        assert!(
            uses_problem("actions/checkout@0123456789abcdef0123456789abcdef01234567").is_none()
        );
        assert!(uses_problem("actions/checkout@v4")
            .unwrap()
            .contains("full 40"));
        assert!(uses_problem("./.github/actions/build").is_none());
        assert!(uses_problem("docker://alpine@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").is_none());
    }

    #[test]
    fn dockerfile_pins_and_user() {
        let bad = "FROM alpine:3.19\nRUN curl https://example.com/x | sh\n";
        let findings = check_dockerfile("Dockerfile", bad);
        assert!(findings.iter().any(|f| f.message.contains("digest")));
        assert!(findings.iter().any(|f| f.message.contains("pipes curl")));
        assert!(findings.iter().any(|f| f.message.contains("USER")));
        let digest = "a".repeat(64);
        let good = format!("FROM alpine@sha256:{digest}\nUSER nobody\n");
        assert!(check_dockerfile("Dockerfile", &good).is_empty());
    }

    #[test]
    fn shell_rules() {
        assert!(bash_strict("set -euo pipefail\n"));
        assert!(!bash_strict("set -eu\n"));
        assert!(posix_strict("set -eu\n"));
        assert!(!posix_strict("echo hi\n"));
    }
}
