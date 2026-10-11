//! The worker protocol: the Python worker (`python -m mm_forge <step>`) writes one JSON object per line on stdout.
//!
//! ```text
//! {"type":"progress","done":3,"total":10,"note":"layer 3 of 10"}
//! {"type":"metric","name":"loss","value":1.25,"step":40}
//! {"type":"log","level":"info","message":"..."}
//! {"type":"artifact","name":"dusty-bf16","path":"...","bytes":123,"kind":"gguf"}
//! {"type":"result","data":{...}}                      (the last thing a step says, at most once)
//! {"type":"error","message":"...","code":"..."}       (the step failed)
//! ```
//!
//! A line that is not one of these (a stray `print`, a library's banner) is kept as plain text, never lost and never
//! an error.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Progress {
        done: f64,
        total: f64,
        note: String,
    },
    Metric {
        name: String,
        /// None when the worker sent NaN or an infinity (JSON has neither, so they arrive as strings).
        value: Option<f64>,
        step: Option<i64>,
    },
    Log {
        level: String,
        message: String,
    },
    Artifact {
        name: String,
        path: String,
        bytes: Option<u64>,
        kind: String,
    },
    Result(Value),
    Error {
        message: String,
        code: String,
    },
    /// Not a protocol line.
    Text(String),
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        Value::String(s) => s.parse::<f64>().ok().filter(|x| x.is_finite()),
        _ => None,
    }
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

pub fn parse_line(line: &str) -> Event {
    let line = line.trim();
    if !line.starts_with('{') {
        return Event::Text(line.to_string());
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Event::Text(line.to_string());
    };
    match v.get("type").and_then(Value::as_str) {
        Some("progress") => Event::Progress {
            done: v.get("done").and_then(num).unwrap_or(0.0),
            total: v.get("total").and_then(num).unwrap_or(0.0),
            note: text(&v, "note"),
        },
        Some("metric") => match v.get("name").and_then(Value::as_str) {
            Some(name) if !name.is_empty() => Event::Metric {
                name: name.to_string(),
                value: v.get("value").and_then(num),
                step: v.get("step").and_then(Value::as_i64),
            },
            _ => Event::Text(line.to_string()),
        },
        Some("log") => Event::Log {
            level: match text(&v, "level") {
                l if l.is_empty() => "info".into(),
                l => l,
            },
            message: text(&v, "message"),
        },
        Some("artifact") => Event::Artifact {
            name: text(&v, "name"),
            path: text(&v, "path"),
            bytes: v.get("bytes").and_then(Value::as_u64),
            kind: match text(&v, "kind") {
                k if k.is_empty() => "file".into(),
                k => k,
            },
        },
        Some("result") => Event::Result(v.get("data").cloned().unwrap_or(Value::Null)),
        Some("error") => Event::Error {
            message: text(&v, "message"),
            code: match text(&v, "code") {
                c if c.is_empty() => "failed".into(),
                c => c,
            },
        },
        _ => Event::Text(line.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn each_event_type_parses() {
        assert_eq!(
            parse_line(r#"{"type":"progress","done":3,"total":10,"note":"layer 3"}"#),
            Event::Progress {
                done: 3.0,
                total: 10.0,
                note: "layer 3".into()
            }
        );
        assert_eq!(
            parse_line(r#"{"type":"metric","name":"loss","value":1.25,"step":40}"#),
            Event::Metric {
                name: "loss".into(),
                value: Some(1.25),
                step: Some(40)
            }
        );
        assert_eq!(
            parse_line(r#"{"type":"log","message":"hi"}"#),
            Event::Log {
                level: "info".into(),
                message: "hi".into()
            }
        );
        assert_eq!(
            parse_line(r#"{"type":"artifact","name":"w","path":"p","bytes":12,"kind":"gguf"}"#),
            Event::Artifact {
                name: "w".into(),
                path: "p".into(),
                bytes: Some(12),
                kind: "gguf".into()
            }
        );
        assert_eq!(
            parse_line(r#"{"type":"result","data":{"ok":true}}"#),
            Event::Result(json!({"ok": true}))
        );
        assert_eq!(
            parse_line(r#"{"type":"error","message":"boom"}"#),
            Event::Error {
                message: "boom".into(),
                code: "failed".into()
            }
        );
    }

    #[test]
    fn nan_and_infinity_arrive_as_strings_and_become_no_value() {
        for v in ["\"nan\"", "\"inf\"", "\"-inf\"", "null"] {
            let e = parse_line(&format!(r#"{{"type":"metric","name":"loss","value":{v}}}"#));
            assert_eq!(
                e,
                Event::Metric {
                    name: "loss".into(),
                    value: None,
                    step: None
                }
            );
        }
        // a number sent as a string is still a number
        let e = parse_line(r#"{"type":"metric","name":"x","value":"2.5"}"#);
        assert!(matches!(e, Event::Metric { value: Some(v), .. } if v == 2.5));
    }

    #[test]
    fn anything_else_is_plain_text() {
        for l in [
            "",
            "hello",
            "{broken",
            r#"{"type":"unknown"}"#,
            r#"{"no":"type"}"#,
            r#"{"type":"metric"}"#,
            "[1,2]",
        ] {
            assert_eq!(parse_line(l), Event::Text(l.trim().to_string()), "{l}");
        }
    }

    #[test]
    fn whitespace_and_carriage_returns_are_trimmed() {
        assert!(matches!(
            parse_line("  {\"type\":\"result\",\"data\":1}\r\n"),
            Event::Result(_)
        ));
    }
}
