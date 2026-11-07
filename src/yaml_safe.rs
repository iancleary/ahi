//! YAML for ahi, on `yaml-rust2` alone: a hardening pass that rejects
//! aliases, anchors, and duplicate keys before typed deserialization; a
//! loader that bridges the parsed tree to `serde_json::Value` so the typed
//! configs deserialize with serde's own messages; and a multi-document
//! validator for the `check-yaml` builtin, which allows aliases but still
//! refuses duplicate keys, as upstream `check-yaml` does.
//!
//! Aliases would let a small config expand without bound. Duplicate keys are
//! easy to miss; ahi treats them as errors.

use serde::de::DeserializeOwned;
use yaml_rust2::parser::{Event, Parser};
use yaml_rust2::{Yaml, YamlLoader};

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

/// Parse one YAML document. Zero documents is `Null`; more than one is an
/// error. Callers that want the alias and duplicate-key refusals run
/// `check_config_yaml` first.
pub(crate) fn load_single(src: &str, origin: &str) -> Result<Yaml> {
    let mut documents =
        YamlLoader::load_from_str(src).map_err(|err| Error::new(format!("{origin}: {err}")))?;
    match documents.len() {
        0 => Ok(Yaml::Null),
        1 => Ok(documents.remove(0)),
        _ => Err(Error::new(format!(
            "{origin}: expected a single YAML document"
        ))),
    }
}

/// The first document of a YAML stream, or `Null` for an empty one. For
/// files ahi checks rather than configures itself with: aliases resolve,
/// later duplicate keys win, as in most loaders.
pub(crate) fn load_first_document(src: &str) -> std::result::Result<Yaml, String> {
    let mut documents = YamlLoader::load_from_str(src).map_err(|err| err.to_string())?;
    Ok(if documents.is_empty() {
        Yaml::Null
    } else {
        documents.remove(0)
    })
}

/// Deserialize an ahi config after `check_config_yaml` accepted it. The
/// tree goes through `serde_json::Value`, so field errors read exactly as
/// serde reports them (`unknown field`, `missing field`, `invalid type`).
pub(crate) fn typed<T: DeserializeOwned>(src: &str, origin: &str) -> Result<T> {
    let yaml = load_single(src, origin)?;
    let json = to_json(&yaml).map_err(|message| Error::new(format!("{origin}: {message}")))?;
    serde_json::from_value(json).map_err(|err| {
        // serde_json appends a position that refers to no document here.
        let text = err.to_string();
        let mut text = text.split(" at line ").next().unwrap_or(&text).to_string();
        // YAML types unquoted `true`, `yes`, `null`, and numbers as scalars,
        // not strings; nothing is coerced, so say what to do.
        if text.contains("invalid type") && text.contains("expected a string") {
            text.push_str(
                " (quote the value: YAML reads unquoted true, null, and numbers as non-strings)",
            );
        }
        Error::new(format!("{origin}: {text}"))
    })
}

/// Lossless for everything a config may contain. Mapping keys become
/// strings (an integer or boolean key is written in its YAML form), since
/// every ahi config keys on names. Floats that are not finite, aliases, and
/// bad values are errors.
pub(crate) fn to_json(yaml: &Yaml) -> std::result::Result<serde_json::Value, String> {
    use serde_json::Value;
    Ok(match yaml {
        Yaml::Null => Value::Null,
        Yaml::Boolean(value) => Value::Bool(*value),
        Yaml::Integer(value) => Value::Number((*value).into()),
        Yaml::Real(text) => {
            let number: f64 = text
                .parse()
                .map_err(|_| format!("invalid number `{text}`"))?;
            serde_json::Number::from_f64(number)
                .map(Value::Number)
                .ok_or_else(|| format!("number `{text}` is not finite"))?
        }
        Yaml::String(text) => Value::String(text.clone()),
        Yaml::Array(items) => Value::Array(
            items
                .iter()
                .map(to_json)
                .collect::<std::result::Result<Vec<_>, String>>()?,
        ),
        Yaml::Hash(map) => {
            let mut object = serde_json::Map::with_capacity(map.len());
            for (key, value) in map {
                let key = match key {
                    Yaml::String(text) => text.clone(),
                    Yaml::Integer(value) => value.to_string(),
                    Yaml::Boolean(value) => value.to_string(),
                    Yaml::Real(text) => text.clone(),
                    Yaml::Null => "null".to_string(),
                    _ => return Err("complex YAML keys are not allowed".to_string()),
                };
                object.insert(key, to_json(value)?);
            }
            Value::Object(object)
        }
        Yaml::Alias(_) => return Err("YAML aliases are not allowed".to_string()),
        Yaml::BadValue => return Err("invalid YAML value".to_string()),
    })
}

/// `check-yaml`: every document must parse. The loader rejects a repeated
/// key in a mapping, as upstream `check-yaml` does; aliases and anchors are
/// allowed, since this is someone else's file.
pub(crate) fn validate_documents(src: &str) -> std::result::Result<(), String> {
    YamlLoader::load_from_str(src)
        .map(drop)
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod bridge_tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Sample {
        version: u32,
        name: String,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        ratio: Option<f64>,
        #[serde(default)]
        enabled: bool,
    }

    #[test]
    fn typed_configs_deserialize_with_serde_messages() {
        let sample: Sample = typed(
            "version: 1\nname: ahi\ntags: [a, b]\nratio: 0.5\nenabled: true\n",
            "t",
        )
        .unwrap();
        assert_eq!(
            sample,
            Sample {
                version: 1,
                name: "ahi".into(),
                tags: vec!["a".into(), "b".into()],
                ratio: Some(0.5),
                enabled: true
            }
        );
        let unknown = typed::<Sample>("version: 1\nname: x\nextra: 1\n", "t").unwrap_err();
        assert!(
            unknown.to_string().starts_with("t: unknown field `extra`"),
            "{unknown}"
        );
        let missing = typed::<Sample>("version: 1\n", "t").unwrap_err();
        assert!(
            missing.to_string().contains("missing field `name`"),
            "{missing}"
        );
        let wrong = typed::<Sample>("version: one\nname: x\n", "t").unwrap_err();
        assert!(wrong.to_string().contains("invalid type"), "{wrong}");
        let empty = typed::<Option<Sample>>("", "t").unwrap();
        assert_eq!(empty, None);
        let two = typed::<Sample>("---\nversion: 1\nname: a\n---\nversion: 2\nname: b\n", "t")
            .unwrap_err();
        assert!(two.to_string().contains("single YAML document"), "{two}");
    }

    #[test]
    fn the_bridge_keeps_types_and_order_and_refuses_non_finite_numbers() {
        let yaml = load_single(
            "b: 2\na: [1, 2.5, true, null, text, 'quoted 1']\n3: int key\nyes: string in 1.2\n",
            "t",
        )
        .unwrap();
        let json = to_json(&yaml).unwrap();
        let object = json.as_object().unwrap();
        assert_eq!(
            object.keys().collect::<Vec<_>>(),
            ["b", "a", "3", "yes"],
            "insertion order kept"
        );
        assert_eq!(
            json["a"],
            serde_json::json!([1, 2.5, true, null, "text", "quoted 1"])
        );
        assert_eq!(json["3"], "int key");
        assert_eq!(json["yes"], "string in 1.2", "YAML 1.2: yes is a string");
        let inf = load_single("x: .inf\n", "t").unwrap();
        assert!(to_json(&inf).is_err(), "infinity has no JSON form");
        let typed_bool = typed::<Vec<String>>("[true]", "t").unwrap_err().to_string();
        assert!(typed_bool.contains("quote the value"), "{typed_bool}");
    }

    #[test]
    fn document_validation_allows_aliases_but_not_duplicate_keys() {
        assert_eq!(validate_documents("a: 1\nb: 2\n"), Ok(()));
        assert_eq!(
            validate_documents("base: &b {x: 1}\nderived: *b\n"),
            Ok(()),
            "aliases are fine in files under check"
        );
        assert_eq!(
            validate_documents("---\na: 1\n---\na: 2\n"),
            Ok(()),
            "multi-document"
        );
        assert_eq!(validate_documents(""), Ok(()));
        let dup = validate_documents("a: 1\nb:\n  c: 1\n  c: 2\n").unwrap_err();
        assert!(
            dup.contains("duplicated key") && dup.contains("line 4"),
            "{dup}"
        );
        assert!(validate_documents("a: [1, 2\n").is_err());
        assert_eq!(
            validate_documents("? [complex, key]\n: value\nother: 1\n"),
            Ok(()),
            "a sequence key is not tracked but does not break the walk"
        );
    }
}
