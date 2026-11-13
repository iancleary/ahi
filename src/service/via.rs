//! The one parser for a connect command: `ahi exec --via`, `ahi tui --via`,
//! and `AHI_DELEGATE_VIA`. Shell-like words without a shell: single quotes
//! keep everything, double quotes keep everything but `\"` and `\\`, a
//! backslash outside quotes keeps the next character. No variables, no
//! globs, no operators; a path with a space is quoted and stays one token.

/// Most tokens a connect command may have, the same bound as `run_command`.
pub(crate) const MAX_TOKENS: usize = 64;
/// Most bytes a connect command may have.
pub(crate) const MAX_BYTES: usize = 8 * 1024;

/// Split `command` into argv. The refusal names what is wrong.
pub(crate) fn split(command: &str) -> Result<Vec<String>, String> {
    if command.len() > MAX_BYTES {
        return Err(format!(
            "the connect command is longer than {MAX_BYTES} bytes"
        ));
    }
    if command.contains('\0') {
        return Err("the connect command contains NUL".to_string());
    }
    let mut argv = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if in_word {
                    argv.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("the connect command has an unclosed '".to_string()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => {
                                return Err("the connect command has an unclosed \"".to_string())
                            }
                        },
                        Some(c) => word.push(c),
                        None => return Err("the connect command has an unclosed \"".to_string()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => word.push(c),
                    None => return Err("the connect command ends with a backslash".to_string()),
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        argv.push(word);
    }
    if argv.is_empty() {
        return Err("the connect command is empty".to_string());
    }
    if argv.len() > MAX_TOKENS {
        return Err(format!(
            "the connect command has more than {MAX_TOKENS} tokens"
        ));
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_split_like_a_shell_would_without_being_one() {
        assert_eq!(
            split("hc connect --worker rust-ci").unwrap(),
            ["hc", "connect", "--worker", "rust-ci"]
        );
        assert_eq!(
            split("ssh box '/opt/my tools/ahi' connect").unwrap(),
            ["ssh", "box", "/opt/my tools/ahi", "connect"]
        );
        assert_eq!(
            split(r#"ssh box "/opt/my \"tools\"/ahi" connect"#).unwrap(),
            ["ssh", "box", "/opt/my \"tools\"/ahi", "connect"]
        );
        assert_eq!(
            split(r"ssh box /opt/my\ tools/ahi connect").unwrap(),
            ["ssh", "box", "/opt/my tools/ahi", "connect"]
        );
        assert_eq!(split("  a\t'' b  ").unwrap(), ["a", "", "b"]);
        assert_eq!(split("\"$HOME\"").unwrap(), ["$HOME"], "no expansion");
        assert!(split("").unwrap_err().contains("empty"));
        assert!(split("   ").unwrap_err().contains("empty"));
        assert!(split("a 'b").unwrap_err().contains("unclosed '"));
        assert!(split("a \"b").unwrap_err().contains("unclosed \""));
        assert!(split("a \\").unwrap_err().contains("backslash"));
        assert!(split("a\0b").unwrap_err().contains("NUL"));
        assert!(split(&"x ".repeat(MAX_TOKENS + 1))
            .unwrap_err()
            .contains("tokens"));
        assert!(split(&"y".repeat(MAX_BYTES + 1))
            .unwrap_err()
            .contains("bytes"));
    }
}
