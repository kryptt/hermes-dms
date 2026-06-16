//! Parser for the Hermes `chat/stream` SSE event protocol.
//!
//! Hermes emits `event: <name>\ndata: <json>\n\n` frames. The exact names and
//! payload fields were taken from `gateway/platforms/api_server.py`
//! (`_run_and_signal`): note `assistant.delta` carries its text in `delta`
//! (not `content`), and the final assistant text arrives in
//! `assistant.completed.content`.

use serde_json::Value;

/// A normalized chat-stream event.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    RunStarted,
    MessageStarted,
    /// Incremental assistant text.
    Delta(String),
    /// Tool lifecycle / reasoning progress. `status` is the raw event name.
    ToolProgress {
        tool_name: String,
        status: String,
    },
    /// Final assistant message text.
    AssistantCompleted {
        content: String,
    },
    /// Run finished; carries the opaque usage object if present.
    RunCompleted {
        usage: Option<Value>,
    },
    /// Server-reported error mid-run.
    Error(String),
    /// Terminal sentinel.
    Done,
    /// Any other event name (ignored by the consumer).
    Other(String),
}

fn str_field(json: &Value, key: &str) -> String {
    json.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Map a raw SSE (event name, data) pair to a [`ChatEvent`]. Malformed JSON
/// degrades gracefully (treated as empty payload) rather than erroring.
pub fn parse_event(name: &str, data: &str) -> ChatEvent {
    let json: Value = serde_json::from_str(data).unwrap_or(Value::Null);
    match name {
        "run.started" => ChatEvent::RunStarted,
        "message.started" => ChatEvent::MessageStarted,
        "assistant.delta" => ChatEvent::Delta(str_field(&json, "delta")),
        "tool.started" | "tool.completed" | "tool.failed" | "tool.progress" => {
            let tool_name = json
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            ChatEvent::ToolProgress {
                tool_name,
                status: name.to_string(),
            }
        }
        "assistant.completed" => ChatEvent::AssistantCompleted {
            content: str_field(&json, "content"),
        },
        "run.completed" => ChatEvent::RunCompleted {
            usage: json.get("usage").cloned(),
        },
        "error" => {
            let msg = json
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string();
            ChatEvent::Error(msg)
        }
        "done" => ChatEvent::Done,
        other => ChatEvent::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_uses_delta_field_not_content() {
        let ev = parse_event("assistant.delta", r#"{"message_id":"m","delta":"Hello"}"#);
        assert_eq!(ev, ChatEvent::Delta("Hello".into()));
    }

    #[test]
    fn tool_events_carry_name_and_status() {
        let ev = parse_event(
            "tool.started",
            r#"{"tool_name":"desktop_launch_app","preview":null}"#,
        );
        assert_eq!(
            ev,
            ChatEvent::ToolProgress {
                tool_name: "desktop_launch_app".into(),
                status: "tool.started".into()
            }
        );
    }

    #[test]
    fn assistant_completed_extracts_content() {
        let ev = parse_event(
            "assistant.completed",
            r#"{"content":"Done. Firefox is open.","completed":true}"#,
        );
        assert_eq!(
            ev,
            ChatEvent::AssistantCompleted {
                content: "Done. Firefox is open.".into()
            }
        );
    }

    #[test]
    fn run_completed_captures_usage() {
        let ev = parse_event("run.completed", r#"{"usage":{"input_tokens":5}}"#);
        match ev {
            ChatEvent::RunCompleted { usage: Some(u) } => {
                assert_eq!(u["input_tokens"], 5);
            }
            other => panic!("expected RunCompleted with usage, got {other:?}"),
        }
    }

    #[test]
    fn error_event_extracts_message() {
        let ev = parse_event("error", r#"{"message":"boom"}"#);
        assert_eq!(ev, ChatEvent::Error("boom".into()));
    }

    #[test]
    fn done_is_terminal() {
        assert_eq!(parse_event("done", "{}"), ChatEvent::Done);
    }

    #[test]
    fn unknown_event_is_other() {
        assert_eq!(
            parse_event("something.weird", "{}"),
            ChatEvent::Other("something.weird".into())
        );
    }

    #[test]
    fn malformed_json_degrades_to_empty() {
        assert_eq!(
            parse_event("assistant.delta", "not json"),
            ChatEvent::Delta(String::new())
        );
    }
}
