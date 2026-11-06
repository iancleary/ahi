//! Bounded execution of clipboard helper processes.
//! Text reaches a helper only through its stdin or a private file, never argv.
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Output from helpers that report back (only `wslpath`) is bounded.
const MAX_OUTPUT_BYTES: u64 = 65_536;

/// Runs `args[0]` with `text` on stdin. `Some(stdout)` means the process exited
/// successfully within `timeout` and consumed all of `text`; `None` is any failure.
pub(super) fn run_command(
    args: &[&str],
    text: &str,
    timeout: Duration,
    stdout: Stdio,
) -> Option<Vec<u8>> {
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
            let result = stdout.take(MAX_OUTPUT_BYTES).read_to_end(&mut bytes);
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

/// Tries each writer in order; the first successful exit wins.
pub(super) fn copy_with_commands(text: &str, commands: &[&[&str]]) -> bool {
    commands
        .iter()
        .any(|args| run_command(args, text, Duration::from_secs(5), Stdio::null()).is_some())
}

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
