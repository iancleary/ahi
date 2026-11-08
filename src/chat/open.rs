//! Opening what the operator clicks: a URL in the system browser, or a file
//! reference in an editor. Both build an argv and spawn it detached with no
//! shell, so a URL or path is always one argument. Routing is a pure function
//! of the environment and the preference so tests cover it without spawning.

use std::process::{Command, Stdio};

use crate::clipboard::{Environment, Platform};

/// Built-in editor templates by the name the operator types in `/editor`.
/// `{path}`, `{line}`, `{column}` are replaced; `{line}` and `{column}`
/// default to 1 when the reference carries none.
const EDITORS: &[(&str, &[&str])] = &[
    ("code", &["code", "-g", "{path}:{line}:{column}"]),
    ("vscode", &["code", "-g", "{path}:{line}:{column}"]),
    ("cursor", &["cursor", "-g", "{path}:{line}:{column}"]),
    ("zed", &["zed", "{path}:{line}:{column}"]),
    ("subl", &["subl", "{path}:{line}:{column}"]),
    ("sublime", &["subl", "{path}:{line}:{column}"]),
    ("idea", &["idea", "--line", "{line}", "{path}"]),
    ("nvim", &["nvim", "+{line}", "{path}"]),
    ("vim", &["vim", "+{line}", "{path}"]),
    ("hx", &["hx", "{path}:{line}:{column}"]),
    ("helix", &["hx", "{path}:{line}:{column}"]),
];

/// Editors that run in the terminal and would fight the chat screen for it.
const TERMINAL_EDITORS: &[&str] = &["nvim", "vim", "vi", "hx", "nano", "emacs"];

/// The argv that opens `url`, or `None` on a platform with no known opener.
/// Only `http` and `https` schemes are accepted; anything else is refused
/// before a program is named, so a model cannot steer a click to `file:`.
pub(crate) fn url_argv(env: &Environment, url: &str) -> Option<Vec<String>> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let url = url.to_string();
    Some(match env.platform {
        Platform::MacOs => vec!["open".into(), url],
        Platform::Windows => vec!["rundll32".into(), "url.dll,FileProtocolHandler".into(), url],
        Platform::Linux if env.wsl => vec!["wslview".into(), url],
        Platform::Linux | Platform::Other => vec!["xdg-open".into(), url],
    })
}

/// How an editor was chosen, for the notice after a click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Editor {
    Argv(Vec<String>),
    /// The preference names a terminal editor, which cannot open over the
    /// chat screen. Shown instead of spawning.
    Terminal(String),
    None,
}

/// Resolve the editor preference, or `$AHI_EDITOR`, `$VISUAL`, `$EDITOR`,
/// then `code` when found on `PATH`. `probe` reports whether a program is
/// on `PATH`, injected so tests do not depend on the host.
pub(crate) fn editor_argv(
    preference: Option<&str>,
    env_var: impl Fn(&str) -> Option<String>,
    probe: impl Fn(&str) -> bool,
    path: &str,
    line: Option<u32>,
    column: Option<u32>,
) -> Editor {
    let template = preference
        .map(str::to_string)
        .or_else(|| env_var("AHI_EDITOR"))
        .or_else(|| env_var("VISUAL"))
        .or_else(|| env_var("EDITOR"))
        .or_else(|| probe("code").then(|| "code".to_string()));
    let Some(template) = template else {
        return Editor::None;
    };
    let tokens: Vec<String> = match crate::shlex::split(&template) {
        Ok(tokens) if !tokens.is_empty() => tokens,
        _ => return Editor::None,
    };
    let tokens: Vec<String> = if tokens.len() == 1 {
        match EDITORS.iter().find(|(name, _)| *name == tokens[0]) {
            Some((_, argv)) => argv.iter().map(|t| t.to_string()).collect(),
            // A bare program with no template: path, then line as a `+N`
            // argument that most editors accept.
            None => vec![tokens[0].clone(), "{path}".into()],
        }
    } else {
        tokens
    };
    let program = std::path::Path::new(&tokens[0])
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if TERMINAL_EDITORS.contains(&program.as_str()) {
        return Editor::Terminal(program);
    }
    let line = line.unwrap_or(1).to_string();
    let column = column.unwrap_or(1).to_string();
    Editor::Argv(
        tokens
            .iter()
            .map(|token| {
                token
                    .replace("{path}", path)
                    .replace("{line}", &line)
                    .replace("{column}", &column)
            })
            .collect(),
    )
}

/// Spawn `argv` detached, with no shell and no inherited stdio. `Err` is a
/// short message for a notice.
pub(crate) fn spawn(argv: &[String]) -> Result<(), String> {
    let (program, args) = argv.split_first().ok_or("nothing to run")?;
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|_| format!("cannot start {program}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(platform: Platform, wsl: bool) -> Environment {
        Environment {
            platform,
            wayland: false,
            x11: false,
            termux: false,
            wsl,
            windows_terminal: false,
            remote: false,
        }
    }

    #[test]
    fn urls_open_with_the_platform_opener_and_only_for_http() {
        let url = "https://example.com/a?b=c";
        assert_eq!(
            url_argv(&env(Platform::MacOs, false), url),
            Some(vec!["open".to_string(), url.to_string()])
        );
        assert_eq!(
            url_argv(&env(Platform::Linux, false), url).unwrap()[0],
            "xdg-open"
        );
        assert_eq!(
            url_argv(&env(Platform::Linux, true), url).unwrap()[0],
            "wslview"
        );
        assert_eq!(
            url_argv(&env(Platform::Windows, false), url).unwrap()[0],
            "rundll32"
        );
        assert_eq!(
            url_argv(&env(Platform::MacOs, false), "file:///etc/passwd"),
            None
        );
        assert_eq!(
            url_argv(&env(Platform::MacOs, false), "javascript:alert(1)"),
            None
        );
        assert_eq!(url_argv(&env(Platform::MacOs, false), "https://a b"), None);
    }

    #[test]
    fn editors_resolve_from_preference_environment_then_probe() {
        let none = |_: &str| None;
        let code_present = |p: &str| p == "code";
        assert_eq!(
            editor_argv(None, none, code_present, "src/a.rs", Some(12), Some(5)),
            Editor::Argv(vec!["code".into(), "-g".into(), "src/a.rs:12:5".into()])
        );
        assert_eq!(
            editor_argv(None, none, |_| false, "src/a.rs", None, None),
            Editor::None
        );
        assert_eq!(
            editor_argv(Some("zed"), none, |_| false, "a.rs", None, None),
            Editor::Argv(vec!["zed".into(), "a.rs:1:1".into()])
        );
        assert_eq!(
            editor_argv(
                Some("myed --goto {line} {path}"),
                none,
                |_| false,
                "a.rs",
                Some(3),
                None
            ),
            Editor::Argv(vec![
                "myed".into(),
                "--goto".into(),
                "3".into(),
                "a.rs".into()
            ])
        );
        let visual = |name: &str| (name == "VISUAL").then(|| "nvim".to_string());
        assert_eq!(
            editor_argv(None, visual, |_| false, "a.rs", None, None),
            Editor::Terminal("nvim".into())
        );
        let ahi = |name: &str| match name {
            "AHI_EDITOR" => Some("cursor".to_string()),
            "VISUAL" => Some("nvim".to_string()),
            _ => None,
        };
        assert!(matches!(
            editor_argv(None, ahi, |_| false, "a.rs", None, None),
            Editor::Argv(argv) if argv[0] == "cursor"
        ));
        assert_eq!(
            editor_argv(Some("unknown-ed"), none, |_| false, "a.rs", None, None),
            Editor::Argv(vec!["unknown-ed".into(), "a.rs".into()])
        );
    }
}
