//! Pairing of tool calls with their results, shared by every protocol's replay.
//!
//! Tool exchanges are replayed only when complete: an assistant call is sent
//! with the result that answers it, and a result is sent only after the
//! assistant message that requested it. A turn that failed between a request
//! and its results therefore never produces an invalid follow-up request.

use crate::chat::transcript::{ChatMessage, ToolPayload, TranscriptRole};

/// Fixed-size bit set over message indexes.
struct BitSet(Vec<u64>);

impl BitSet {
    fn new(len: usize) -> Self {
        Self(vec![0; len.div_ceil(64)])
    }

    fn set(&mut self, index: usize) {
        self.0[index / 64] |= 1 << (index % 64);
    }

    fn get(&self, index: usize) -> bool {
        self.0
            .get(index / 64)
            .is_some_and(|word| word & (1 << (index % 64)) != 0)
    }
}

/// Which calls and results form complete exchanges in a transcript.
pub(super) struct Exchanges {
    /// Result messages whose call was found in the preceding assistant message.
    paired_results: BitSet,
    /// `(assistant message index, position of the call in its batch)`, sorted.
    answered: Vec<(u32, u16)>,
}

impl Exchanges {
    pub(super) fn of(messages: &[ChatMessage]) -> Self {
        let mut paired_results = BitSet::new(messages.len());
        let mut answered = Vec::new();
        // The open assistant batch: its index and which positions are answered.
        let mut open: Option<(usize, &[crate::chat::backend::ToolCall], u64)> = None;
        for (index, message) in messages.iter().enumerate() {
            match (&message.role, &message.tool) {
                (TranscriptRole::Assistant, Some(ToolPayload::Calls { calls, .. })) => {
                    open = Some((index, calls, 0));
                }
                (TranscriptRole::Assistant | TranscriptRole::Operator, _) => open = None,
                (TranscriptRole::Tool { .. }, Some(ToolPayload::Result { call_id, .. })) => {
                    if let Some((assistant, calls, taken)) = &mut open {
                        // First unanswered call with this id; a batch has at
                        // most 32 calls, so a u64 occupancy mask suffices.
                        let position =
                            calls.iter().take(64).enumerate().position(|(p, call)| {
                                call.id == *call_id && *taken & (1 << p) == 0
                            });
                        if let Some(position) = position {
                            *taken |= 1 << position;
                            answered.push((*assistant as u32, position as u16));
                            paired_results.set(index);
                        }
                    }
                }
                _ => {}
            }
        }
        answered.sort_unstable();
        Self {
            paired_results,
            answered,
        }
    }

    /// Whether the call at `position` in the assistant message at `index` has
    /// a paired result.
    pub(super) fn is_answered(&self, index: usize, position: usize) -> bool {
        let Ok(index) = u32::try_from(index) else {
            return false;
        };
        let Ok(position) = u16::try_from(position) else {
            return false;
        };
        self.answered.binary_search(&(index, position)).is_ok()
    }

    /// Whether the tool result at `index` answers a call in the preceding
    /// assistant message.
    pub(super) fn is_paired_result(&self, index: usize) -> bool {
        self.paired_results.get(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::backend::tools::call;
    use serde_json::json;

    fn assistant(ids: &[&str]) -> ChatMessage {
        let mut message = ChatMessage::new(TranscriptRole::Assistant, "");
        message.tool = Some(ToolPayload::Calls {
            calls: ids
                .iter()
                .map(|id| call(id, "read_file", json!({"path": "a"})))
                .collect(),
            native: None,
        });
        message
    }

    fn result(id: &str) -> ChatMessage {
        let mut message = ChatMessage::new(TranscriptRole::Tool { exit_code: 0 }, "");
        message.tool = Some(ToolPayload::Result {
            call_id: id.into(),
            name: "read_file".into(),
            content: String::new(),
        });
        message
    }

    #[test]
    fn pairs_each_result_once_and_only_with_the_open_batch() {
        let transcript = vec![
            ChatMessage::new(TranscriptRole::Operator, "q"),
            assistant(&["a", "b"]),
            result("a"),
            result("a"),
            ChatMessage::new(TranscriptRole::Notice, "n"),
            result("unknown"),
            ChatMessage::new(TranscriptRole::Assistant, "answer"),
            result("b"),
        ];
        let exchanges = Exchanges::of(&transcript);
        assert!(exchanges.is_answered(1, 0));
        assert!(!exchanges.is_answered(1, 1));
        assert!(exchanges.is_paired_result(2));
        assert!(!exchanges.is_paired_result(3));
        assert!(!exchanges.is_paired_result(5));
        assert!(!exchanges.is_paired_result(7));
    }

    #[test]
    fn bitset_spans_words() {
        let mut set = BitSet::new(130);
        set.set(0);
        set.set(64);
        set.set(129);
        assert!(set.get(0) && set.get(64) && set.get(129));
        assert!(!set.get(1) && !set.get(65) && !set.get(500));
    }
}
