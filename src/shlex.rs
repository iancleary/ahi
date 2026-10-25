//! Split a hook `entry` the way POSIX `shlex.split` does.
//!
//! This does not invoke a shell. `$VAR`, backticks, and `$(...)` stay literal
//! text. Operators such as `|`, `>`, and `;` stay literal arguments.

use crate::error::{Error, Result};

pub fn split(input: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_token = false;
    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(inner) => current.push(inner),
                        None => {
                            return Err(Error::new(
                                "entry has an unclosed single quote; ahi does not invoke a shell",
                            ));
                        }
                    }
                }
            }
            '"' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(escaped @ ('"' | '\\' | '$' | '`' | '\n')) => {
                                if escaped != '\n' {
                                    current.push(escaped);
                                }
                            }
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => {
                                return Err(Error::new(
                                    "entry has a trailing backslash; ahi does not invoke a shell",
                                ));
                            }
                        },
                        Some(inner) => current.push(inner),
                        None => {
                            return Err(Error::new(
                                "entry has an unclosed double quote; ahi does not invoke a shell",
                            ));
                        }
                    }
                }
            }
            '\\' => {
                in_token = true;
                match chars.next() {
                    Some('\n') => {}
                    Some(escaped) => current.push(escaped),
                    None => {
                        return Err(Error::new(
                            "entry has a trailing backslash; ahi does not invoke a shell",
                        ));
                    }
                }
            }
            ch if ch.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            other => {
                in_token = true;
                current.push(other);
            }
        }
    }
    if in_token {
        tokens.push(current);
    }
    if tokens.is_empty() {
        return Err(Error::new("entry is empty after shell-style splitting"));
    }
    if tokens.iter().any(|token| token.contains('\0')) {
        return Err(Error::new(
            "entry contains a NUL after shell-style splitting",
        ));
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_cargo_entries_without_a_shell() {
        assert_eq!(
            split("cargo fmt --all --").unwrap(),
            vec!["cargo", "fmt", "--all", "--"]
        );
        assert_eq!(
            split("cargo clippy --all-targets --all-features -- -Dclippy::all").unwrap(),
            vec![
                "cargo",
                "clippy",
                "--all-targets",
                "--all-features",
                "--",
                "-Dclippy::all"
            ]
        );
    }

    #[test]
    fn quotes_and_escapes_are_literal() {
        assert_eq!(
            split(r#"echo "hello world""#).unwrap(),
            vec!["echo", "hello world"]
        );
        assert_eq!(split("echo 'a b'").unwrap(), vec!["echo", "a b"]);
        assert_eq!(split(r"echo a\ b").unwrap(), vec!["echo", "a b"]);
        assert_eq!(split("echo $HOME").unwrap(), vec!["echo", "$HOME"]);
        assert_eq!(split("echo $(id)").unwrap(), vec!["echo", "$(id)"]);
        assert_eq!(split("echo `id`").unwrap(), vec!["echo", "`id`"]);
        assert_eq!(
            split("echo hello > out").unwrap(),
            vec!["echo", "hello", ">", "out"]
        );
    }

    #[test]
    fn rejects_unclosed_quotes() {
        assert!(split("echo \"hello").is_err());
        assert!(split("echo 'hello").is_err());
    }
}
