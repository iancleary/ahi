//! OSC 52 terminal clipboard writes. The terminal never acknowledges, so
//! success means the request was sent, not that the terminal accepted it.
use base64::Engine;
use std::io::{IsTerminal, Write};

const MAX_ENCODED_LENGTH: usize = 100_000;

pub(super) fn emit(text: &str) -> bool {
    let stdout = std::io::stdout();
    if !stdout.is_terminal() {
        return false;
    }
    write(text, &mut stdout.lock()).is_ok()
}

fn write(text: &str, output: &mut impl Write) -> std::io::Result<()> {
    if text.len().div_ceil(3).saturating_mul(4) > MAX_ENCODED_LENGTH {
        return Err(std::io::Error::other("clipboard exceeds OSC 52 size limit"));
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    // Hold stdout's lock over the entire sequence, including the terminator.
    write!(output, "\x1b]52;c;{encoded}\x07")?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_copy_encodes_text_and_refuses_oversized_payloads_without_writing() {
        let mut output = Vec::new();
        write("hello 界🙂\n", &mut output).unwrap();
        assert_eq!(output, b"\x1b]52;c;aGVsbG8g55WM8J+Zggo=\x07");
        output.clear();
        write(&"a".repeat(75_000), &mut output).unwrap();
        assert_eq!(output.len(), 100_008);
        output.clear();
        assert!(write(&"a".repeat(75_001), &mut output).is_err());
        assert!(output.is_empty());
    }
}
