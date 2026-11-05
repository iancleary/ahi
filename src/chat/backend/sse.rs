//! Bounded server-sent-events framing shared by every streaming protocol.
//!
//! One reader owns one line buffer and one event buffer for the life of a
//! response; nothing is allocated per line. Protocol modules interpret the
//! payloads and decide what a terminal event is; this module only frames.

use std::io::{BufRead, BufReader, Read};

/// Longest single SSE line or joined event payload accepted.
pub(super) const FRAME_CAP: u64 = 1024 * 1024;
/// Longest response stream accepted.
pub(super) const STREAM_CAP: u64 = 16 * 1024 * 1024;
/// Bytes read past a terminal event so a keep-alive connection can be reused.
const DRAIN_CAP: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SseError {
    /// The transport failed mid-stream.
    Interrupted,
    /// A line or event exceeded `FRAME_CAP`, or the stream exceeded `STREAM_CAP`.
    TooLarge,
    /// Clean end of stream. Whether that is an error is the protocol's call.
    Eof,
    /// With strict prefix checking, the first non-empty line was not SSE and
    /// started with this byte (`{`, `[`, or `<`).
    NotSse(u8),
}

pub(super) struct SseReader<R: Read> {
    reader: BufReader<R>,
    line: Vec<u8>,
    data: Vec<u8>,
    total: u64,
    received_data: bool,
    /// Reject a body that opens like JSON or markup instead of SSE.
    strict_prefix: bool,
    /// `data` holds the event returned by the previous call; clear it first.
    ready: bool,
}

impl<R: Read> SseReader<R> {
    pub(super) fn new(reader: R, strict_prefix: bool) -> Self {
        Self {
            reader: BufReader::new(reader),
            line: Vec::with_capacity(1024),
            data: Vec::with_capacity(4096),
            total: 0,
            received_data: false,
            strict_prefix,
            ready: false,
        }
    }

    /// Whether any `data:` line has arrived yet.
    pub(super) fn received_data(&self) -> bool {
        self.received_data
    }

    /// The next complete event payload: the `data:` lines of one event joined
    /// with `\n`, without the trailing newline. Other fields are ignored.
    pub(super) fn next_event(&mut self) -> Result<&[u8], SseError> {
        if self.ready {
            self.data.clear();
            self.ready = false;
        }
        loop {
            self.line.clear();
            let size = self
                .reader
                .by_ref()
                .take(FRAME_CAP + 1)
                .read_until(b'\n', &mut self.line)
                .map_err(|_| SseError::Interrupted)? as u64;
            self.total += size;
            if size > FRAME_CAP || self.total > STREAM_CAP {
                return Err(SseError::TooLarge);
            }
            if size == 0 {
                return Err(SseError::Eof);
            }
            while matches!(self.line.last(), Some(b'\n' | b'\r')) {
                self.line.pop();
            }
            if self.line.is_empty() {
                if self.data.is_empty() {
                    continue;
                }
                if self.data.last() == Some(&b'\n') {
                    self.data.pop();
                }
                self.ready = true;
                return Ok(&self.data);
            }
            if let Some(value) = self.line.strip_prefix(b"data:") {
                let value = value.strip_prefix(b" ").unwrap_or(value);
                if self.data.len() + value.len() + 1 > FRAME_CAP as usize {
                    return Err(SseError::TooLarge);
                }
                self.received_data = true;
                self.data.extend_from_slice(value);
                self.data.push(b'\n');
            } else if self.strict_prefix && !self.received_data {
                if let Some(first @ (b'{' | b'[' | b'<')) = self
                    .line
                    .iter()
                    .copied()
                    .find(|byte| !byte.is_ascii_whitespace())
                {
                    return Err(SseError::NotSse(first));
                }
            }
        }
    }

    /// Read what follows a terminal event, bounded, so an HTTP client that
    /// pools connections sees the body consumed. Errors here are irrelevant.
    pub(super) fn drain(mut self) {
        let mut remaining = DRAIN_CAP;
        let mut chunk = [0_u8; 4096];
        while remaining > 0 {
            match self.reader.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => remaining = remaining.saturating_sub(n),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(body: &str, strict: bool) -> (Vec<String>, SseError) {
        let mut reader = SseReader::new(body.as_bytes(), strict);
        let mut out = Vec::new();
        loop {
            match reader.next_event() {
                Ok(event) => out.push(String::from_utf8(event.to_vec()).unwrap()),
                Err(error) => return (out, error),
            }
        }
    }

    #[test]
    fn frames_join_data_lines_and_ignore_other_fields() {
        let (out, end) = events(
            "event: x\r\ndata: a\r\ndata:b\r\n\r\n: comment\n\ndata: [DONE]\n\n",
            false,
        );
        assert_eq!(out, ["a\nb", "[DONE]"]);
        assert_eq!(end, SseError::Eof);
    }

    #[test]
    fn strict_prefix_rejects_json_and_markup_bodies_only_before_data() {
        assert_eq!(events("{\"error\":1}\n", true).1, SseError::NotSse(b'{'));
        assert_eq!(events("<html>\n", true).1, SseError::NotSse(b'<'));
        assert_eq!(events("{\"error\":1}\n", false).1, SseError::Eof);
        let (out, _) = events("data: 1\n\n{x}\n\n", true);
        assert_eq!(out, ["1"]);
    }

    #[test]
    fn caps_apply_to_lines_and_joined_events() {
        let long = format!("data: {}\n\n", "x".repeat(FRAME_CAP as usize + 1));
        assert_eq!(events(&long, false).1, SseError::TooLarge);
        let half = "x".repeat(FRAME_CAP as usize / 2 + 1);
        let joined = format!("data: {half}\ndata: {half}\n\n");
        assert_eq!(events(&joined, false).1, SseError::TooLarge);
    }
}
