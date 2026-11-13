//! The Program Status Protocol (OSC 7501): one escape sequence by which a
//! program tells the terminal multiplexer that holds it what it is doing.
//!
//! herdr-core stores what a program reports, verbatim, and shows it in its
//! sidebar; it never guesses from screen text. ahi reports the moments a
//! coordinator cares about: a turn is running, a tool call waits on a
//! person, a turn finished or failed. Nothing is read back, so this is a
//! write-only format string with no dependency and no negotiation.
//!
//! The sequence is written only when stdout is a terminal. A terminal that
//! does not know OSC 7501 ignores it, as terminals ignore any unknown OSC.
//!
//! Wire format, from herdr-core's `status.rs`:
//!
//! ```text
//! ESC ] 7501 ; state=<state>[:kind=<kind>][:app=<app>][:title=<base64>][:msg=<base64>] ESC \
//! ```

use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex};

/// What the program says it is doing. The words are the protocol's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Open, nothing running.
    Idle,
    /// A turn is running: the model is answering or a tool is executing.
    Working,
    /// The last turn completed.
    Done,
    /// Waiting on a person; `kind` says why.
    Blocked,
    /// The last turn failed, timed out, or was cancelled.
    Error,
    /// Remove the record: the program is leaving.
    Clear,
}

impl State {
    fn word(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
            Self::Clear => "clear",
        }
    }
}

/// Why a program is blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A tool call waits for the operator's approval.
    Permission,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Self::Permission => "permission",
        }
    }
}

/// One report, before encoding.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report<'a> {
    pub state: Option<State>,
    pub kind: Option<Kind>,
    /// What the program is working on, for the sidebar row; `""` omits it.
    pub title: &'a str,
    /// One line about the current step; `""` omits it.
    pub msg: &'a str,
}

/// Longest `msg` sent, in bytes, cut at a character boundary. A sidebar row
/// has no room for more and the transcript holds the whole text.
const MESSAGE_LIMIT: usize = 120;

/// The escape sequence for a report. `Clear` carries no other field.
pub fn encode(report: &Report<'_>) -> String {
    let Some(state) = report.state else {
        return String::new();
    };
    let mut fields = vec![format!("state={}", state.word())];
    if state != State::Clear {
        if let Some(kind) = report.kind {
            fields.push(format!("kind={}", kind.word()));
        }
        fields.push("app=ahi".to_string());
        if !report.title.is_empty() {
            fields.push(format!("title={}", base64(report.title.as_bytes())));
        }
        let msg = first_line(report.msg);
        if !msg.is_empty() {
            fields.push(format!("msg={}", base64(msg.as_bytes())));
        }
    }
    format!("\x1b]7501;{}\x1b\\", fields.join(":"))
}

/// The first non-blank line of `text`, trimmed and cut to [`MESSAGE_LIMIT`].
fn first_line(text: &str) -> &str {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if line.len() <= MESSAGE_LIMIT {
        return line;
    }
    let mut end = MESSAGE_LIMIT;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

/// Standard base64 with padding, as herdr-core decodes it. Written out here
/// because ahi has no direct `base64` dependency and this is the one use.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Where reports go: stdout when it is a terminal, a buffer in tests, or
/// nowhere. Cheap to clone; one per screen.
#[derive(Clone)]
pub struct Reporter {
    sink: Sink,
}

#[derive(Clone)]
enum Sink {
    Silent,
    Stdout,
    Buffer(Arc<Mutex<Vec<u8>>>),
}

impl Reporter {
    /// Reports to stdout when stdout is a terminal, otherwise nowhere.
    pub fn for_stdout() -> Self {
        let sink = if io::stdout().is_terminal() {
            Sink::Stdout
        } else {
            Sink::Silent
        };
        Self { sink }
    }

    /// Never writes. For a program whose stdout carries data.
    pub fn silent() -> Self {
        Self { sink: Sink::Silent }
    }

    /// Collects every sequence in `buffer`, for tests.
    pub fn buffered(buffer: Arc<Mutex<Vec<u8>>>) -> Self {
        Self {
            sink: Sink::Buffer(buffer),
        }
    }

    /// Write one report. A failed write is ignored: status is advisory and
    /// the terminal is about to be redrawn anyway.
    pub fn report(&self, report: &Report<'_>) {
        let encoded = encode(report);
        if encoded.is_empty() {
            return;
        }
        match &self.sink {
            Sink::Silent => {}
            Sink::Stdout => {
                let mut stdout = io::stdout().lock();
                let _ = stdout.write_all(encoded.as_bytes());
                let _ = stdout.flush();
            }
            Sink::Buffer(buffer) => {
                if let Ok(mut buffer) = buffer.lock() {
                    buffer.extend_from_slice(encoded.as_bytes());
                }
            }
        }
    }

    pub fn state(&self, state: State, title: &str, msg: &str) {
        self.report(&Report {
            state: Some(state),
            kind: None,
            title,
            msg,
        });
    }

    pub fn blocked(&self, kind: Kind, title: &str, msg: &str) {
        self.report(&Report {
            state: Some(State::Blocked),
            kind: Some(kind),
            title,
            msg,
        });
    }

    pub fn clear(&self) {
        self.report(&Report {
            state: Some(State::Clear),
            ..Report::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64("ahi · é".as_bytes()), "YWhpIMK3IMOp");
    }

    #[test]
    fn reports_encode_as_herdr_core_decodes_them() {
        assert_eq!(
            encode(&Report {
                state: Some(State::Working),
                kind: None,
                title: "ahi/gpt",
                msg: "run_command cargo test",
            }),
            "\x1b]7501;state=working:app=ahi:title=YWhpL2dwdA==:msg=cnVuX2NvbW1hbmQgY2FyZ28gdGVzdA==\x1b\\"
        );
        assert_eq!(
            encode(&Report {
                state: Some(State::Blocked),
                kind: Some(Kind::Permission),
                title: "",
                msg: "write_file src/a.rs",
            }),
            "\x1b]7501;state=blocked:kind=permission:app=ahi:msg=d3JpdGVfZmlsZSBzcmMvYS5ycw==\x1b\\"
        );
        assert_eq!(
            encode(&Report {
                state: Some(State::Idle),
                ..Report::default()
            }),
            "\x1b]7501;state=idle:app=ahi\x1b\\"
        );
        // Clear removes the record and carries nothing else.
        assert_eq!(
            encode(&Report {
                state: Some(State::Clear),
                kind: Some(Kind::Permission),
                title: "t",
                msg: "m",
            }),
            "\x1b]7501;state=clear\x1b\\"
        );
        assert_eq!(encode(&Report::default()), "");
    }

    #[test]
    fn the_message_is_one_trimmed_line_cut_at_a_character_boundary() {
        assert_eq!(first_line("\n\n  second  \nthird"), "second");
        let long = format!("{}é", "x".repeat(MESSAGE_LIMIT - 1));
        let cut = first_line(&long);
        assert_eq!(cut, "x".repeat(MESSAGE_LIMIT - 1));
        assert!(cut.len() <= MESSAGE_LIMIT);
    }

    #[test]
    fn a_buffered_reporter_collects_sequences_and_a_silent_one_writes_nothing() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let reporter = Reporter::buffered(buffer.clone());
        reporter.state(State::Working, "", "");
        reporter.blocked(Kind::Permission, "", "x");
        reporter.clear();
        let written = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        assert_eq!(written.matches("\x1b]7501;").count(), 3);
        assert!(written.ends_with("state=clear\x1b\\"));
        // Silent is what a non-terminal stdout gets; it must not panic or
        // print.
        Reporter::silent().state(State::Done, "t", "m");
    }
}
