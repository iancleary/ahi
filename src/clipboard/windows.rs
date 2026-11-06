//! Windows clipboard through PowerShell, for native Windows consoles and WSL.
//! Text travels in a private UTF-8 file: console code pages, `clip.exe` and
//! PowerShell pipelines would otherwise re-encode it or append newlines.
use super::command::run_command;
use base64::Engine;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

pub(super) struct HandoffFile(pub(super) PathBuf);

impl HandoffFile {
    pub(super) fn create(text: &str) -> std::io::Result<Self> {
        let mut random = [0; 16];
        getrandom::getrandom(&mut random)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let name = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
        let path = std::env::temp_dir().join(format!("ahi-clip-{name}.txt"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file: File = options.open(&path)?;
        let temporary = Self(path);
        file.write_all(text.as_bytes())?;
        Ok(temporary)
    }
}

impl Drop for HandoffFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Copies through `powershell.exe`. Under WSL the file path is translated with
/// `wslpath` first; on native Windows the temporary path is already a Windows path.
pub(super) fn copy(text: &str, translate_path: bool) -> bool {
    let Ok(file) = HandoffFile::create(text) else {
        return false;
    };
    let Some(path) = file.0.to_str() else {
        return false;
    };
    let path = if translate_path {
        let Some(output) = run_command(
            &["wslpath", "-w", path],
            "",
            Duration::from_secs(1),
            Stdio::piped(),
        ) else {
            return false;
        };
        let Ok(path) = String::from_utf8(output) else {
            return false;
        };
        path.trim().to_string()
    } else {
        path.to_string()
    };
    if path.is_empty() {
        return false;
    }
    run_command(
        &[
            "powershell.exe",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script(&path),
        ],
        "",
        Duration::from_secs(5),
        Stdio::null(),
    )
    .is_some()
}

/// Only the path enters the script, as a single-quoted PowerShell literal.
fn script(path: &str) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'; Set-Clipboard -Value ([System.IO.File]::ReadAllText('{}', [System.Text.Encoding]::UTF8))",
        path.replace('\'', "''")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_quotes_the_path_and_nothing_else() {
        assert_eq!(
            script(r"C:\it's\clip.txt"),
            "$ErrorActionPreference = 'Stop'; Set-Clipboard -Value ([System.IO.File]::ReadAllText('C:\\it''s\\clip.txt', [System.Text.Encoding]::UTF8))"
        );
    }

    #[cfg(unix)]
    #[test]
    fn handoff_file_is_private_preserves_text_and_is_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;
        let file = HandoffFile::create("hello 界🙂\n\n").unwrap();
        let path = file.0.clone();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello 界🙂\n\n");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(file);
        assert!(!path.exists());
    }
}
