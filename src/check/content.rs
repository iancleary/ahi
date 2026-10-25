use std::collections::{BTreeSet, HashSet};

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

use super::{flag_values, Check, CheckContext, CheckOutcome, Finding};
use crate::git;

pub struct MergeConflict;
pub struct LargeFiles;
pub struct CaseConflict;
pub struct Symlinks;
pub struct JsonCheck;
pub struct YamlCheck;
pub struct TomlCheck;
pub struct TrojanSource;
pub struct WindowsReserved;
pub struct PrivateKey;
pub struct CommittedDotenv;

impl Check for MergeConflict {
    fn id(&self) -> &'static str {
        "ahi/merge-conflict"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["check-merge-conflict"]
    }
    fn name(&self) -> &'static str {
        "Check for merge conflicts"
    }
    fn description(&self) -> &'static str {
        "Reports conflict markers. Unlike upstream check-merge-conflict, ahi scans even when git is not in the middle of a merge."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["text"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("check-merge-conflict")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        const PATTERNS: &[&[u8]] = &[
            b"<<<<<<< ",
            b"======= ",
            b"=======\r\n",
            b"=======\n",
            b">>>>>>> ",
        ];
        let mut findings = Vec::new();
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            for (number, line) in split_lines(&bytes) {
                for pattern in PATTERNS {
                    if line.starts_with(pattern) {
                        let shown = String::from_utf8_lossy(pattern).trim().to_string();
                        findings.push(Finding::at(
                            file,
                            Some(number),
                            format!("merge conflict string `{shown}` found"),
                        ));
                    }
                }
            }
        }
        CheckOutcome::from(findings)
    }
}

impl Check for LargeFiles {
    fn id(&self) -> &'static str {
        "ahi/large-files"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["check-added-large-files"]
    }
    fn name(&self) -> &'static str {
        "Check for large files"
    }
    fn description(&self) -> &'static str {
        "Reports files whose size in KiB, rounded up, exceeds --maxkb (default 500). Every selected file is checked, which is upstream's --enforce-all behavior."
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("check-added-large-files")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let max_kb = flag_values(ctx.args, "--maxkb")
            .first()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(500);
        let mut findings = Vec::new();
        for file in files {
            let Ok(len) = ctx.len(file) else {
                continue;
            };
            let kb = len.div_ceil(1024);
            if kb > max_kb {
                findings.push(Finding::at(
                    file,
                    None,
                    format!("{file} ({kb} KB) exceeds {max_kb} KB"),
                ));
            }
        }
        CheckOutcome::from(findings)
    }
}

impl Check for CaseConflict {
    fn id(&self) -> &'static str {
        "ahi/case-conflict"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["check-case-conflict"]
    }
    fn name(&self) -> &'static str {
        "Check for case conflicts"
    }
    fn description(&self) -> &'static str {
        "Reports paths that differ only by case, including parent directories. That collision breaks a checkout on Windows and on default macOS volumes."
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("check-case-conflict")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let repo = match git::tracked_files(ctx.root) {
            Ok(files) => files,
            Err(err) => return CheckOutcome::from(vec![Finding::bare(err.to_string())]),
        };
        let added = git::added_files(ctx.root).unwrap_or_default();
        let conflicts = case_conflicts(&repo, files, &added);
        CheckOutcome::from(
            conflicts
                .into_iter()
                .map(|path| Finding::at(path, None, "case-insensitivity conflict found"))
                .collect(),
        )
    }
}

impl Check for Symlinks {
    fn id(&self) -> &'static str {
        "ahi/symlinks"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["check-symlinks"]
    }
    fn name(&self) -> &'static str {
        "Check for symlinks"
    }
    fn description(&self) -> &'static str {
        "Reports a broken symlink, and a symlink whose target escapes the repository root. The escape check is stricter than upstream check-symlinks."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["symlink"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("check-symlinks")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let mut findings = Vec::new();
        for file in files {
            let Ok(path) = crate::paths::join_inside(ctx.root, file) else {
                findings.push(Finding::at(file, None, "path escapes the repository"));
                continue;
            };
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.file_type().is_symlink() {
                continue;
            }
            if std::fs::metadata(&path).is_err() {
                findings.push(Finding::at(file, None, "broken symlink"));
            }
            if let Ok(target) = std::fs::read_link(&path) {
                let combined = if target.is_absolute() {
                    target
                } else {
                    path.parent().unwrap_or(ctx.root).join(target)
                };
                let lexical = lexical_normalize(&combined);
                if !lexical.starts_with(ctx.root) {
                    findings.push(Finding::at(
                        file,
                        None,
                        "symlink target escapes the repository",
                    ));
                }
            }
        }
        CheckOutcome::from(findings)
    }
}

impl Check for JsonCheck {
    fn id(&self) -> &'static str {
        "ahi/check-json"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["check-json"]
    }
    fn name(&self) -> &'static str {
        "Check JSON"
    }
    fn description(&self) -> &'static str {
        "Reports JSON that does not parse, including duplicate object keys."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["json"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("check-json")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        parse_each(ctx, files, "JSON", |bytes| {
            if bytes.len() > MAX_PARSE {
                return Err("file is larger than 4 MiB".to_string());
            }
            let mut de = serde_json::Deserializer::from_slice(bytes);
            let value = JsonValue::deserialize(&mut de).map_err(|err| err.to_string())?;
            de.end().map_err(|err| err.to_string())?;
            let _ = value;
            Ok(())
        })
    }
}

impl Check for YamlCheck {
    fn id(&self) -> &'static str {
        "ahi/check-yaml"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["check-yaml"]
    }
    fn name(&self) -> &'static str {
        "Check YAML"
    }
    fn description(&self) -> &'static str {
        "Reports YAML that does not parse. Aliases are allowed in files under check; they are rejected only in ahi's own config."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["yaml"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("check-yaml")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        parse_each(ctx, files, "YAML", |bytes| {
            if bytes.len() > MAX_PARSE {
                return Err("file is larger than 4 MiB".to_string());
            }
            for document in serde_yaml::Deserializer::from_slice(bytes) {
                let value =
                    serde_yaml::Value::deserialize(document).map_err(|err| err.to_string())?;
                let _ = value;
            }
            Ok(())
        })
    }
}

impl Check for TomlCheck {
    fn id(&self) -> &'static str {
        "ahi/check-toml"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["check-toml"]
    }
    fn name(&self) -> &'static str {
        "Check TOML"
    }
    fn description(&self) -> &'static str {
        "Reports TOML that does not parse."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["toml"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("check-toml")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        parse_each(ctx, files, "TOML", |bytes| {
            if bytes.len() > MAX_PARSE {
                return Err("file is larger than 4 MiB".to_string());
            }
            let text = std::str::from_utf8(bytes).map_err(|_| "file is not UTF-8".to_string())?;
            toml::from_str::<toml::Value>(text)
                .map(|_| ())
                .map_err(|err| err.to_string())
        })
    }
}

impl Check for TrojanSource {
    fn id(&self) -> &'static str {
        "ahi/trojan-source"
    }
    fn name(&self) -> &'static str {
        "Trojan Source"
    }
    fn description(&self) -> &'static str {
        "Reports Unicode bidi controls (U+202A–U+202E and U+2066–U+2069) and a documented set of Latin/Cyrillic/Greek homoglyphs on the same line. This is not the full Unicode confusables database."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["text"]
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        let mut findings = Vec::new();
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            for (number, line) in text.lines().enumerate() {
                let line_no = (number as u32) + 1;
                if let Some(ch) = line.chars().find(|ch| BIDI.contains(ch)) {
                    findings.push(Finding::at(
                        file,
                        Some(line_no),
                        format!("bidirectional control U+{:04X}", ch as u32),
                    ));
                }
                let latin = line.chars().any(|ch| ch.is_ascii_alphabetic());
                if latin {
                    if let Some(ch) = line.chars().find(|ch| HOMOGLYPHS.contains(ch)) {
                        findings.push(Finding::at(
                            file,
                            Some(line_no),
                            format!("homoglyph `{ch}` mixed with Latin letters"),
                        ));
                    }
                }
            }
        }
        CheckOutcome::from(findings)
    }
}

impl Check for WindowsReserved {
    fn id(&self) -> &'static str {
        "ahi/windows-reserved-names"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["check-illegal-windows-names"]
    }
    fn name(&self) -> &'static str {
        "Check Windows reserved names"
    }
    fn description(&self) -> &'static str {
        "Reports path components Windows rejects: device names (CON, PRN, AUX, NUL, COM1–COM9, LPT1–LPT9, including superscripts), trailing dots or spaces, and characters <>:\"|?*."
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("check-illegal-windows-names")
    }
    fn check(&self, _ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        CheckOutcome::from(
            files
                .iter()
                .filter_map(|file| {
                    windows_reserved_reason(file).map(|reason| Finding::at(file, None, reason))
                })
                .collect(),
        )
    }
}

impl Check for PrivateKey {
    fn id(&self) -> &'static str {
        "ahi/private-key"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["detect-private-key"]
    }
    fn name(&self) -> &'static str {
        "Detect private key"
    }
    fn description(&self) -> &'static str {
        "Reports private-key headers. The marker list is the one in pre-commit-hooks detect_private_key.py as of 2026-10-25. This is not a general entropy scanner."
    }
    fn default_types(&self) -> &'static [&'static str] {
        &["text"]
    }
    fn upstream(&self) -> Option<&'static str> {
        Some("detect-private-key")
    }
    fn check(&self, ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        // Markers copied from pre-commit-hooks pre_commit_hooks/detect_private_key.py.
        const MARKERS: &[&[u8]] = &[
            b"BEGIN RSA PRIVATE KEY",
            b"BEGIN DSA PRIVATE KEY",
            b"BEGIN EC PRIVATE KEY",
            b"BEGIN OPENSSH PRIVATE KEY",
            b"BEGIN PRIVATE KEY",
            b"PuTTY-User-Key-File-2",
            b"BEGIN SSH2 ENCRYPTED PRIVATE KEY",
            b"BEGIN PGP PRIVATE KEY BLOCK",
            b"BEGIN ENCRYPTED PRIVATE KEY",
            b"BEGIN OpenVPN Static key V1",
        ];
        let mut findings = Vec::new();
        for file in files {
            let Ok(bytes) = ctx.read(file) else {
                continue;
            };
            if MARKERS.iter().any(|marker| contains_slice(&bytes, marker)) {
                findings.push(Finding::at(file, None, "private key found"));
            }
        }
        CheckOutcome::from(findings)
    }
}

impl Check for CommittedDotenv {
    fn id(&self) -> &'static str {
        "ahi/committed-dotenv"
    }
    fn name(&self) -> &'static str {
        "Committed dotenv file"
    }
    fn description(&self) -> &'static str {
        "Reports a file named .env or .env.* other than .env.example, .env.sample, and .env.template. Files such as versions/tool.env are not dotenv files and are not flagged."
    }
    fn check(&self, _ctx: &CheckContext<'_>, files: &[String]) -> CheckOutcome {
        CheckOutcome::from(
            files
                .iter()
                .filter(|file| is_dotenv(file))
                .map(|file| {
                    Finding::at(
                        file,
                        None,
                        "dotenv file should not be committed (keep secrets out of git; an example file is allowed)",
                    )
                })
                .collect(),
        )
    }
}

const MAX_PARSE: usize = 4 * 1024 * 1024;

const BIDI: &[char] = &[
    '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}',
    '\u{2069}',
];

/// Cyrillic and Greek letters that share a glyph with a Latin letter.
/// This is a documented subset, not Unicode TR39 confusables.
const HOMOGLYPHS: &[char] = &[
    'А', 'В', 'Е', 'К', 'М', 'Н', 'О', 'Р', 'С', 'Т', 'Х', 'а', 'е', 'о', 'р', 'с', 'у', 'х', 'і',
    'ј', 'ѕ', 'Α', 'Β', 'Ε', 'Ζ', 'Η', 'Ι', 'Κ', 'Μ', 'Ν', 'Ο', 'Ρ', 'Τ', 'Υ', 'Χ', 'α', 'ο', 'ρ',
    'υ',
];

pub fn windows_reserved_reason(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    for component in path.split('/').filter(|part| !part.is_empty()) {
        if let Some(reason) = component_reason(component) {
            return Some(reason);
        }
    }
    None
}

fn component_reason(component: &str) -> Option<String> {
    if component
        .chars()
        .any(|ch| matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') || ch.is_control())
    {
        return Some(format!(
            "component `{component}` contains a character Windows forbids"
        ));
    }
    if component.ends_with(['.', ' ', '\t']) {
        return Some(format!("component `{component}` ends with a dot or space"));
    }
    let stem = component.split('.').next().unwrap_or(component);
    if is_device(stem) {
        return Some(format!(
            "component `{component}` uses reserved Windows device name `{stem}`"
        ));
    }
    None
}

fn is_device(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    for prefix in ["COM", "LPT"] {
        if let Some(rest) = upper.strip_prefix(prefix) {
            let mut chars = rest.chars();
            if let (Some(ch), None) = (chars.next(), chars.next()) {
                if ch.is_ascii_digit() || matches!(ch, '\u{00B9}' | '\u{00B2}' | '\u{00B3}') {
                    return true;
                }
            }
        }
    }
    false
}

pub fn case_conflicts(repo_files: &[String], selected: &[String], added: &[String]) -> Vec<String> {
    let mut repo: BTreeSet<String> = repo_files.iter().cloned().collect();
    repo.extend(parents_of(&repo));
    let mut relevant: BTreeSet<String> = selected
        .iter()
        .cloned()
        .chain(added.iter().cloned())
        .collect();
    relevant.extend(parents_of(&relevant));
    for path in &relevant {
        repo.remove(path);
    }
    let repo_lower: BTreeSet<String> = repo.iter().map(|path| path.to_lowercase()).collect();
    let relevant_lower: BTreeSet<String> =
        relevant.iter().map(|path| path.to_lowercase()).collect();
    let mut conflicts: BTreeSet<String> =
        repo_lower.intersection(&relevant_lower).cloned().collect();
    let mut seen = relevant_lower;
    for filename in &relevant {
        let lower = filename.to_lowercase();
        if !seen.remove(&lower) {
            conflicts.insert(lower);
        }
    }
    if conflicts.is_empty() {
        return Vec::new();
    }
    let mut names: Vec<String> = repo
        .union(&relevant)
        .filter(|path| conflicts.contains(&path.to_lowercase()))
        .cloned()
        .collect();
    names.sort();
    names
}

fn parents_of(files: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for file in files {
        let mut parts: Vec<&str> = file.split('/').filter(|part| !part.is_empty()).collect();
        parts.pop();
        while !parts.is_empty() {
            out.insert(parts.join("/"));
            parts.pop();
        }
    }
    out
}

fn is_dotenv(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    if base == ".env" {
        return true;
    }
    if let Some(rest) = base.strip_prefix(".env.") {
        return !matches!(rest, "example" | "sample" | "template");
    }
    false
}

fn parse_each(
    ctx: &CheckContext<'_>,
    files: &[String],
    what: &str,
    parse: impl Fn(&[u8]) -> std::result::Result<(), String>,
) -> CheckOutcome {
    let mut findings = Vec::new();
    for file in files {
        match ctx.read(file) {
            Ok(bytes) => {
                if let Err(err) = parse(&bytes) {
                    findings.push(Finding::at(
                        file,
                        None,
                        format!("{what} parse error: {err}"),
                    ));
                }
            }
            Err(err) => findings.push(Finding::at(file, None, err.to_string())),
        }
    }
    CheckOutcome::from(findings)
}

fn split_lines(bytes: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut number = 1u32;
    while start < bytes.len() {
        let rel = bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| start + index + 1)
            .unwrap_or(bytes.len());
        out.push((number, &bytes[start..rel]));
        number += 1;
        start = rel;
    }
    out
}

fn contains_slice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn lexical_normalize(path: &std::path::Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[derive(Debug)]
enum JsonValue {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

impl<'de> Deserialize<'de> for JsonValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Vis;
        impl<'de> Visitor<'de> for Vis {
            type Value = JsonValue;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("any JSON value")
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<JsonValue, E> {
                Ok(JsonValue::Null)
            }
            fn visit_none<E: de::Error>(self) -> std::result::Result<JsonValue, E> {
                Ok(JsonValue::Null)
            }
            fn visit_bool<E: de::Error>(self, _value: bool) -> std::result::Result<JsonValue, E> {
                Ok(JsonValue::Bool)
            }
            fn visit_i64<E: de::Error>(self, _value: i64) -> std::result::Result<JsonValue, E> {
                Ok(JsonValue::Number)
            }
            fn visit_u64<E: de::Error>(self, _value: u64) -> std::result::Result<JsonValue, E> {
                Ok(JsonValue::Number)
            }
            fn visit_f64<E: de::Error>(self, _value: f64) -> std::result::Result<JsonValue, E> {
                Ok(JsonValue::Number)
            }
            fn visit_str<E: de::Error>(self, _value: &str) -> std::result::Result<JsonValue, E> {
                Ok(JsonValue::String)
            }
            fn visit_string<E: de::Error>(
                self,
                _value: String,
            ) -> std::result::Result<JsonValue, E> {
                Ok(JsonValue::String)
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<JsonValue, A::Error> {
                while seq.next_element::<JsonValue>()?.is_some() {}
                Ok(JsonValue::Array)
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<JsonValue, A::Error> {
                let mut keys = HashSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key.clone()) {
                        return Err(de::Error::custom(format!("duplicate key `{key}`")));
                    }
                    let _value: JsonValue = map.next_value()?;
                }
                Ok(JsonValue::Object)
            }
        }
        deserializer.deserialize_any(Vis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_windows_names() {
        assert!(windows_reserved_reason("src/main.rs").is_none());
        assert!(windows_reserved_reason("dir/CON").unwrap().contains("CON"));
        assert!(windows_reserved_reason("dir/con.txt")
            .unwrap()
            .contains("con"));
        assert!(windows_reserved_reason("dir/COM1").is_some());
        assert!(windows_reserved_reason("dir/file.")
            .unwrap()
            .contains("dot"));
        assert!(windows_reserved_reason("dir/a:b")
            .unwrap()
            .contains("forbids"));
    }

    #[test]
    fn case_conflict_detects_pair() {
        let conflicts = case_conflicts(&["src/Foo.rs".into()], &["src/foo.rs".into()], &[]);
        assert!(conflicts
            .iter()
            .any(|path| path.eq_ignore_ascii_case("src/foo.rs")));
    }

    #[test]
    fn dotenv_names() {
        assert!(is_dotenv(".env"));
        assert!(is_dotenv("app/.env.local"));
        assert!(!is_dotenv(".env.example"));
        assert!(!is_dotenv("versions/tool.env"));
    }

    #[test]
    fn large_file_boundary() {
        assert_eq!(0u64.div_ceil(1024), 0);
        assert_eq!(500 * 1024, 512_000);
        assert!(500 * 1024_u64.div_ceil(1024) == 500);
        assert!((500 * 1024 + 1u64).div_ceil(1024) == 501);
    }
}
