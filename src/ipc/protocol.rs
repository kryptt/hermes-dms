//! JSON-lines IPC protocol shared between the daemon and `hermes-dms-ctl`.
//!
//! Every message is a single line of JSON with a `type` discriminator. Client
//! requests carry a `request_id` so the daemon can correlate streamed
//! responses; broadcast events (toasts, status changes) omit it.

use serde::{Deserialize, Serialize};

/// A request sent from a local client (panel/launcher/ctl) to the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Send a chat message. `session_id` is optional — when omitted the daemon
    /// creates a fresh ephemeral Hermes session (launcher semantics).
    Chat {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        message: String,
    },
    /// Create a new desktop session (returns `SessionCreated`).
    SessionCreate {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// List known sessions (returns `Sessions`).
    SessionList { request_id: String },
    /// Resume an existing session by id (returns `Sessions` with one entry,
    /// or `Error` if the session has been reset).
    SessionResume {
        request_id: String,
        session_id: String,
    },
    /// Cancel the in-flight chat for the given request id.
    Cancel { request_id: String },
    /// Query daemon/Hermes status (returns a `Status` event addressed to the
    /// requester).
    Status { request_id: String },
    /// Opt into broadcast events (toasts, status changes). Used by the panel.
    Subscribe {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
}

impl ClientMessage {
    /// The correlation id, if this message carries one.
    pub fn request_id(&self) -> Option<&str> {
        match self {
            ClientMessage::Chat { request_id, .. }
            | ClientMessage::SessionCreate { request_id, .. }
            | ClientMessage::SessionList { request_id }
            | ClientMessage::SessionResume { request_id, .. }
            | ClientMessage::Cancel { request_id }
            | ClientMessage::Status { request_id } => Some(request_id),
            ClientMessage::Subscribe { request_id } => request_id.as_deref(),
        }
    }
}

/// A response or event sent from the daemon back to a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMessage {
    /// Incremental assistant output for an in-flight chat.
    Delta { request_id: String, content: String },
    /// A tool invocation progress update during a chat run.
    ToolProgress {
        request_id: String,
        tool_name: String,
        status: String,
    },
    /// A chat run finished. `content` is the final assistant message.
    ChatComplete {
        request_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<serde_json::Value>,
    },
    /// Response to `SessionCreate`.
    SessionCreated {
        request_id: String,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// Response to `SessionList` / `SessionResume`.
    Sessions {
        request_id: String,
        data: Vec<SessionInfo>,
    },
    /// The daemon transparently rotated a reset session; the panel should
    /// adopt `new_id` and show a subtle "session refreshed" indicator.
    SessionReset {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        old_id: String,
        new_id: String,
    },
    /// A broadcast toast (e.g. a launcher response or a remote-action notice).
    Toast {
        title: String,
        body: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    /// A broadcast (or requester-addressed) status snapshot.
    Status {
        hermes: String,
        daemon: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    /// An error, optionally tied to a request.
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        message: String,
    },
}

/// Minimal session descriptor surfaced to clients. Hermes returns more fields;
/// unknown keys are ignored on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Connection status values used in `DaemonMessage::Status`.
pub mod status {
    pub const CONNECTED: &str = "connected";
    pub const DISCONNECTED: &str = "disconnected";
    pub const READY: &str = "ready";
    pub const STARTING: &str = "starting";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every client message round-trips through JSON-lines.
    #[test]
    fn client_message_roundtrip() {
        let cases = vec![
            ClientMessage::Chat {
                request_id: "abc".into(),
                session_id: Some("desktop_1".into()),
                message: "open firefox".into(),
            },
            ClientMessage::Chat {
                request_id: "abc2".into(),
                session_id: None,
                message: "hi".into(),
            },
            ClientMessage::SessionCreate {
                request_id: "def".into(),
                title: Some("[Desktop] notes".into()),
            },
            ClientMessage::SessionList {
                request_id: "ghi".into(),
            },
            ClientMessage::SessionResume {
                request_id: "jkl".into(),
                session_id: "desktop_1".into(),
            },
            ClientMessage::Cancel {
                request_id: "mno".into(),
            },
            ClientMessage::Status {
                request_id: "pqr".into(),
            },
            ClientMessage::Subscribe { request_id: None },
        ];
        for msg in cases {
            let line = serde_json::to_string(&msg).unwrap();
            assert!(!line.contains('\n'), "JSON-lines must be single-line");
            let back: ClientMessage = serde_json::from_str(&line).unwrap();
            assert_eq!(msg, back);
        }
    }

    /// `chat` with no `session_id` omits the field on the wire (launcher case).
    #[test]
    fn chat_without_session_omits_field() {
        let msg = ClientMessage::Chat {
            request_id: "x".into(),
            session_id: None,
            message: "hi".into(),
        };
        let line = serde_json::to_string(&msg).unwrap();
        assert!(!line.contains("session_id"));
    }

    /// A bare `{"type":"subscribe"}` (no request_id) deserializes.
    #[test]
    fn bare_subscribe_parses() {
        let msg: ClientMessage = serde_json::from_str(r#"{"type":"subscribe"}"#).unwrap();
        assert_eq!(msg, ClientMessage::Subscribe { request_id: None });
    }

    /// Every daemon message round-trips, and broadcast events omit request_id.
    #[test]
    fn daemon_message_roundtrip() {
        let cases = vec![
            DaemonMessage::Delta {
                request_id: "abc".into(),
                content: "Sure".into(),
            },
            DaemonMessage::ToolProgress {
                request_id: "abc".into(),
                tool_name: "desktop_launch_app".into(),
                status: "running".into(),
            },
            DaemonMessage::ChatComplete {
                request_id: "abc".into(),
                content: "Done.".into(),
                usage: Some(serde_json::json!({"tokens": 10})),
            },
            DaemonMessage::Toast {
                title: "Roci via Telegram".into(),
                body: "Focused Firefox".into(),
                icon: None,
            },
            DaemonMessage::Status {
                hermes: status::CONNECTED.into(),
                daemon: status::READY.into(),
                request_id: None,
            },
            DaemonMessage::Error {
                request_id: None,
                message: "Hermes unreachable".into(),
            },
        ];
        for msg in cases {
            let line = serde_json::to_string(&msg).unwrap();
            let back: DaemonMessage = serde_json::from_str(&line).unwrap();
            assert_eq!(msg, back);
        }
    }

    /// A toast serializes without a request_id field (broadcast).
    #[test]
    fn toast_is_broadcast() {
        let line = serde_json::to_string(&DaemonMessage::Toast {
            title: "t".into(),
            body: "b".into(),
            icon: None,
        })
        .unwrap();
        assert!(!line.contains("request_id"));
    }
}
