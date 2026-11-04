//! Stand-in backend. Replies and token counts are synthetic.
//!
//! The reply is the submitted text prefixed by `[stand-in]`. The screen
//! attaches the stand-in note. This backend does not call a model and it
//! does not use the network.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use super::{ChatRequest, CompletionBackend, ResponseEvent};
use crate::chat::transcript::TranscriptRole;

const CONTEXT_WINDOW: u64 = 128_000;

/// Stand-in backend. Replies and token counts are synthetic.
#[derive(Debug, Default)]
pub(crate) struct EchoBackend;

impl EchoBackend {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl CompletionBackend for EchoBackend {
    fn submit(&mut self, request: ChatRequest) -> Receiver<ResponseEvent> {
        let (sender, receiver) = mpsc::channel();
        let submitted = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == TranscriptRole::Operator)
            .map(|message| message.text.clone())
            .unwrap_or_default();
        let reply = format!("[stand-in] {submitted}");
        let characters = submitted.chars().count() as u64;
        let tokens = characters / 4;
        thread::spawn(move || {
            let pieces = delta_pieces(&reply);
            for (index, piece) in pieces.into_iter().enumerate() {
                if index > 0 {
                    thread::sleep(Duration::from_millis(20));
                }
                if sender.send(ResponseEvent::TextDelta(piece)).is_err() {
                    return;
                }
            }
            let usage = ResponseEvent::Usage {
                input_tokens: tokens,
                output_tokens: tokens,
                cached_input_tokens: None,
                cost_usd: None,
            };
            if sender.send(usage).is_err() {
                return;
            }
            if sender
                .send(ResponseEvent::ContextWindow(CONTEXT_WINDOW))
                .is_err()
            {
                return;
            }
            let _ = sender.send(ResponseEvent::Completed);
        });
        receiver
    }
}

fn delta_pieces(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 2 {
        return vec![chars.into_iter().collect()];
    }
    chars
        .chunks(4)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    use super::super::{ChatRequest, CompletionBackend, ResponseEvent, Selection};
    use super::EchoBackend;
    use crate::chat::thinking::ThinkingLevel;
    use crate::chat::transcript::{ChatMessage, TranscriptRole, STAND_IN_NOTE};

    #[test]
    fn echo_backend_streams_usage_then_completion_and_notes_the_first_reply() {
        let mut backend = EchoBackend::new();
        let request = ChatRequest {
            selection: Selection {
                gateway: "openai".to_string(),
                model: "gpt-6.1-sol".to_string(),
            },
            thinking: ThinkingLevel::Medium,
            messages: vec![ChatMessage {
                role: TranscriptRole::Operator,
                text: "hello!!!!".to_string(),
            }],
        };
        let first = collect(&backend.submit(request.clone()));
        assert!(first
            .deltas
            .concat_text()
            .starts_with("[stand-in] hello!!!!"));
        assert!(!first.deltas.concat_text().contains(STAND_IN_NOTE));
        assert!(matches!(
            first.kinds.as_slice(),
            [
                EventKind::Delta,
                ..,
                EventKind::Usage,
                EventKind::Window,
                EventKind::Completed
            ]
        ));
        assert!(first.deltas.len() > 1);
        match first.usage {
            Some(ResponseEvent::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                cost_usd,
            }) => {
                assert_eq!(input_tokens, 2);
                assert_eq!(output_tokens, 2);
                assert_eq!(cached_input_tokens, None);
                assert_eq!(cost_usd, None);
            }
            other => panic!("expected usage, got {other:?}"),
        }
        assert_eq!(first.window, Some(128_000));

        let second = collect(&backend.submit(request));
        assert!(!second.deltas.concat_text().contains(STAND_IN_NOTE));
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum EventKind {
        Delta,
        Usage,
        Window,
        Completed,
        Failed,
    }

    struct Collected {
        kinds: Vec<EventKind>,
        deltas: Vec<String>,
        usage: Option<ResponseEvent>,
        window: Option<u64>,
    }

    trait ConcatText {
        fn concat_text(&self) -> String;
    }

    impl ConcatText for Vec<String> {
        fn concat_text(&self) -> String {
            self.concat()
        }
    }

    fn collect(receiver: &Receiver<ResponseEvent>) -> Collected {
        let mut collected = Collected {
            kinds: Vec::new(),
            deltas: Vec::new(),
            usage: None,
            window: None,
        };
        loop {
            let event = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            match event {
                ResponseEvent::TextDelta(text) => {
                    collected.kinds.push(EventKind::Delta);
                    collected.deltas.push(text);
                }
                ResponseEvent::Usage {
                    input_tokens,
                    output_tokens,
                    cached_input_tokens,
                    cost_usd,
                } => {
                    collected.kinds.push(EventKind::Usage);
                    collected.usage = Some(ResponseEvent::Usage {
                        input_tokens,
                        output_tokens,
                        cached_input_tokens,
                        cost_usd,
                    });
                }
                ResponseEvent::ContextWindow(window) => {
                    collected.kinds.push(EventKind::Window);
                    collected.window = Some(window);
                }
                ResponseEvent::Completed => {
                    collected.kinds.push(EventKind::Completed);
                    break;
                }
                ResponseEvent::Failed(_) => {
                    collected.kinds.push(EventKind::Failed);
                    break;
                }
            }
        }
        collected
    }
}
