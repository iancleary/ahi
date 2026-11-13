//! The one parser for a connect command: `ahi exec --via`, `ahi tui --via`,
//! and `AHI_DELEGATE_VIA`. The words come from the hook runner's `shlex`,
//! POSIX `shlex.split` without a shell: quotes and backslashes, no
//! variables, no globs, no operators; a path with a space is quoted and
//! stays one token. This module adds the bounds a connect command has.

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
    let argv = crate::shlex::split(command).map_err(|err| {
        format!(
            "the connect command: {}",
            err.to_string().replace("entry ", "")
        )
    })?;
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
        assert!(split("a 'b").unwrap_err().contains("unclosed single quote"));
        assert!(split("a \"b")
            .unwrap_err()
            .contains("unclosed double quote"));
        assert!(split("a \\").unwrap_err().contains("trailing backslash"));
        assert!(split("a\0b").unwrap_err().contains("NUL"));
        assert!(split(&"x ".repeat(MAX_TOKENS + 1))
            .unwrap_err()
            .contains("tokens"));
        assert!(split(&"y".repeat(MAX_BYTES + 1))
            .unwrap_err()
            .contains("bytes"));
    }
}
