//! Reject YAML aliases, anchors, and duplicate keys before typed deserialization.
//!
//! Aliases would let a small config expand without bound. Duplicate keys are
//! easy to miss; ahi treats them as errors.

use yaml_rust2::parser::{Event, Parser};

use crate::error::{Error, Result};

const MAX_EVENTS: usize = 10_000;
const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy)]
enum Phase {
    Key,
    Value,
}

enum Frame {
    Map { keys: Vec<String>, phase: Phase },
    Seq,
}

pub fn check_config_yaml(src: &str, origin: &str) -> Result<()> {
    if src.len() > 1024 * 1024 {
        return Err(Error::new(format!("{origin}: config exceeds 1 MiB")));
    }
    let mut parser = Parser::new_from_str(src);
    let mut frames: Vec<Frame> = Vec::new();
    let mut events = 0usize;
    let mut documents = 0usize;

    loop {
        let (event, marker) = match parser.next_token() {
            Ok(pair) => pair,
            Err(err) => {
                return Err(Error::new(format!("{origin}: {err}")));
            }
        };
        events += 1;
        if events > MAX_EVENTS {
            return Err(Error::new(format!(
                "{origin}: YAML document is too large to parse"
            )));
        }
        let line = marker.line() + 1;
        let col = marker.col() + 1;
        let fail = |message: String| Error::new(format!("{origin}:{line}:{col}: {message}"));

        match event {
            Event::StreamEnd => break,
            Event::DocumentStart => {
                documents += 1;
                if documents > 1 {
                    return Err(fail(
                        "ahi config must be a single YAML document".to_string(),
                    ));
                }
            }
            Event::Alias(_) => {
                return Err(fail(
                    "YAML aliases and anchors are not allowed in ahi config".to_string(),
                ));
            }
            Event::MappingStart(anchor, _) => {
                if anchor != 0 {
                    return Err(fail(
                        "YAML aliases and anchors are not allowed in ahi config".to_string(),
                    ));
                }
                if let Some(Frame::Map {
                    phase: Phase::Key, ..
                }) = frames.last()
                {
                    return Err(fail("complex YAML keys are not allowed".to_string()));
                }
                if frames.len() >= MAX_DEPTH {
                    return Err(fail("YAML nesting is too deep".to_string()));
                }
                frames.push(Frame::Map {
                    keys: Vec::new(),
                    phase: Phase::Key,
                });
            }
            Event::SequenceStart(anchor, _) => {
                if anchor != 0 {
                    return Err(fail(
                        "YAML aliases and anchors are not allowed in ahi config".to_string(),
                    ));
                }
                if let Some(Frame::Map {
                    phase: Phase::Key, ..
                }) = frames.last()
                {
                    return Err(fail("complex YAML keys are not allowed".to_string()));
                }
                if frames.len() >= MAX_DEPTH {
                    return Err(fail("YAML nesting is too deep".to_string()));
                }
                frames.push(Frame::Seq);
            }
            Event::MappingEnd | Event::SequenceEnd => {
                frames.pop();
                if let Some(Frame::Map { phase, .. }) = frames.last_mut() {
                    if matches!(*phase, Phase::Value) {
                        *phase = Phase::Key;
                    }
                }
            }
            Event::Scalar(value, style, anchor, _) => {
                let _ = style;
                if anchor != 0 {
                    return Err(fail(
                        "YAML aliases and anchors are not allowed in ahi config".to_string(),
                    ));
                }
                if let Some(Frame::Map { keys, phase }) = frames.last_mut() {
                    match *phase {
                        Phase::Key => {
                            if keys.iter().any(|existing| existing == &value) {
                                return Err(fail(format!("duplicate YAML key `{value}`")));
                            }
                            keys.push(value);
                            *phase = Phase::Value;
                        }
                        Phase::Value => {
                            *phase = Phase::Key;
                        }
                    }
                }
            }
            Event::Nothing | Event::StreamStart | Event::DocumentEnd => {}
        }
    }
    Ok(())
}
