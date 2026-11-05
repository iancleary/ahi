//! Shared clipboard adapters. Never interpolate selected text into a command or error.
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(all(test, unix))]
mod tests {
    use super::*;

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
        std::fs::remove_dir_all(dir).unwrap();
    }
}

pub(crate) fn copy(text: &str) -> bool {
    let commands: &[&[&str]] = if cfg!(windows) {
        &[&[
            "powershell",
            "-NoProfile",
            "-Command",
            "$input | Set-Clipboard",
        ]]
    } else if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else {
        &[
            &["wl-copy"],
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
        ]
    };
    copy_with_commands(text, commands)
}

fn copy_with_commands(text: &str, commands: &[&[&str]]) -> bool {
    for args in commands {
        let Ok(mut child) = Command::new(args[0])
            .args(&args[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            continue;
        };
        let text = text.to_string();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(stdin.write_all(text.as_bytes()).is_ok());
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success()
                        && receiver
                            .recv_timeout(Duration::from_millis(100))
                            .unwrap_or(false)
                    {
                        return true;
                    }
                    break;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
    }
    false
}
