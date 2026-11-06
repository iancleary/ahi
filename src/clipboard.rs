//! Shared clipboard adapters and bounded terminal clipboard writes.
//! Never interpolate selected text into a command or error.
use base64::Engine;
use std::fs::{File, OpenOptions};
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const MAX_OSC52_ENCODED_LENGTH: usize = 100_000;

pub(crate) fn copy(text: &str) -> bool {
    let present = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let wayland = present("WAYLAND_DISPLAY");
    let x11 = present("DISPLAY");
    let termux = present("TERMUX_VERSION");
    let mut commands: Vec<&[&str]> = Vec::new();
    if cfg!(windows) {
        commands.push(&[
            "powershell",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ErrorActionPreference = 'Stop'; [Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false); Set-Clipboard -Value ([Console]::In.ReadToEnd())",
        ]);
    } else if cfg!(target_os = "macos") {
        commands.push(&["pbcopy"]);
    } else {
        if termux {
            commands.push(&["termux-clipboard-set"]);
        }
        if wayland {
            commands.push(&["wl-copy"]);
        }
        if x11 {
            commands.push(&["xclip", "-selection", "clipboard"]);
            commands.push(&["xsel", "--clipboard", "--input"]);
        }
    }

    // Finish local writes before OSC 52, so they cannot race the terminal.
    let mut copied = copy_with_commands(text, &commands);
    let mut osc52_emitted = false;
    if !copied && crate::wsl::is_wsl() {
        // Windows Terminal supports OSC 52; avoid starting PowerShell when possible.
        if present("WT_SESSION") {
            osc52_emitted = emit_osc52(text);
        }
        copied = osc52_emitted || copy_via_windows(text);
    }
    let remote = present("SSH_CONNECTION")
        || present("SSH_CLIENT")
        || present("MOSH_CONNECTION")
        || std::env::var("AHI_CLIPBOARD_OSC52").is_ok_and(|value| value == "1");
    let headless = cfg!(target_os = "linux") && !wayland && !x11 && !termux;
    if !osc52_emitted && (remote || (!copied && headless)) {
        // There is no OSC 52 acknowledgement. Success means the request was sent,
        // not that the terminal accepted it. Preserve any successful local write.
        copied = emit_osc52(text) || copied;
    }
    copied
}

fn emit_osc52(text: &str) -> bool {
    let stdout = std::io::stdout();
    if !stdout.is_terminal() {
        return false;
    }
    write_osc52(text, &mut stdout.lock()).is_ok()
}

fn write_osc52(text: &str, output: &mut impl Write) -> std::io::Result<()> {
    if text.len().div_ceil(3).saturating_mul(4) > MAX_OSC52_ENCODED_LENGTH {
        return Err(std::io::Error::other("clipboard exceeds OSC 52 size limit"));
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    // Hold stdout's lock over the entire sequence, including the terminator.
    write!(output, "\x1b]52;c;{encoded}\x07")?;
    output.flush()
}

struct ClipboardFile(PathBuf);

impl ClipboardFile {
    fn create(text: &str) -> std::io::Result<Self> {
        let mut random = [0; 16];
        getrandom::getrandom(&mut random)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let name = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
        let path = std::env::temp_dir().join(format!("ahi-wsl-clip-{name}.txt"));
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

impl Drop for ClipboardFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn copy_via_windows(text: &str) -> bool {
    let Ok(file) = ClipboardFile::create(text) else {
        return false;
    };
    let Some(path) = file.0.to_str() else {
        return false;
    };
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
    let path = path.trim();
    if path.is_empty() {
        return false;
    }
    // Only the path enters the script. Text stays in a private UTF-8 file to
    // avoid console code-page conversions and PowerShell pipeline newlines.
    let script = format!(
        "$ErrorActionPreference = 'Stop'; Set-Clipboard -Value ([System.IO.File]::ReadAllText('{}', [System.Text.Encoding]::UTF8))",
        path.replace('\'', "''")
    );
    run_command(
        &[
            "powershell.exe",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ],
        "",
        Duration::from_secs(5),
        Stdio::null(),
    )
    .is_some()
}

fn copy_with_commands(text: &str, commands: &[&[&str]]) -> bool {
    commands
        .iter()
        .any(|args| run_command(args, text, Duration::from_secs(5), Stdio::null()).is_some())
}

fn run_command(args: &[&str], text: &str, timeout: Duration, stdout: Stdio) -> Option<Vec<u8>> {
    let mut child = Command::new(args[0])
        .args(&args[1..])
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Piped handles are guaranteed by the spawn configuration above.
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take();
    let text = text.to_string();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = stdin.write_all(text.as_bytes());
        drop(stdin);
        let _ = sender.send(result.is_ok());
    });
    let output = stdout.map(|stdout| {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            // Only wslpath's short path is used; bound unexpected helper output.
            let result = stdout.take(65_536).read_to_end(&mut bytes);
            let _ = sender.send(result.map(|_| bytes));
        });
        receiver
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success()
                    && receiver
                        .recv_timeout(Duration::from_millis(100))
                        .unwrap_or(false)
                {
                    return match output {
                        Some(output) => output.recv_timeout(Duration::from_millis(100)).ok()?.ok(),
                        None => Some(Vec::new()),
                    };
                }
                return None;
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn windows_handoff_file_is_private_preserves_text_and_is_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;
        let file = ClipboardFile::create("hello 界🙂\n\n").unwrap();
        let path = file.0.clone();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello 界🙂\n\n");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn terminal_copy_encodes_text_and_refuses_oversized_payloads_without_writing() {
        let mut output = Vec::new();
        write_osc52("hello 界🙂\n", &mut output).unwrap();
        assert_eq!(output, b"\x1b]52;c;aGVsbG8g55WM8J+Zggo=\x07");
        output.clear();
        write_osc52(&"a".repeat(75_000), &mut output).unwrap();
        assert_eq!(output.len(), 100_008);
        output.clear();
        assert!(write_osc52(&"a".repeat(75_001), &mut output).is_err());
        assert!(output.is_empty());
    }

    #[test]
    fn clipboard_helpers_receive_exact_text_and_failures_are_reported() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("clipboard");
        let target = dir.join("copied.txt");
        let commands: &[&[&str]] = &[
            &["/nonexistent/clipboard-helper"],
            &[
                "/bin/sh",
                "-c",
                "cat > \"$1\"",
                "clipboard-test",
                target.to_str().unwrap(),
            ],
        ];
        let text = "hello 界e\u{301}🙂\nnot a command: $(false)\n";
        assert!(copy_with_commands(text, commands));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), text);
        assert!(!copy_with_commands(text, &[&["/bin/false"]]));
        assert!(run_command(
            &["/bin/sleep", "5"],
            "",
            Duration::from_millis(20),
            Stdio::null(),
        )
        .is_none());
        assert_eq!(
            run_command(
                &["/bin/sh", "-c", "printf 'C:/clipboard.txt'"],
                "",
                Duration::from_secs(1),
                Stdio::piped(),
            ),
            Some(b"C:/clipboard.txt".to_vec())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
