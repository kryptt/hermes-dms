//! reqwest client for the Hermes platform API server (port 8642).

use std::time::Duration;

use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::json;

use super::sse::{ChatEvent, parse_event};
use crate::ipc::protocol::SessionInfo;

#[derive(Debug, thiserror::Error)]
pub enum HermesError {
    #[error("HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("authentication failed (401): check the Hermes API key")]
    Auth,
    #[error("session not found (404)")]
    SessionNotFound,
    #[error("Hermes returned {status}: {body}")]
    Status { status: u16, body: String },
}

/// Client for the Hermes session/chat REST API.
#[derive(Clone)]
pub struct HermesClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl HermesClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Result<Self, HermesError> {
        // No global request timeout — the chat stream is long-lived. Per-call
        // timeouts are applied to the non-streaming requests instead.
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(4)
            .build()?;
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Ok(Self {
            http,
            base_url,
            api_key: api_key.into(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// `GET /health` — true iff Hermes answers 2xx. Never errors (used by the
    /// health-check loop).
    pub async fn health(&self) -> bool {
        match self
            .http
            .get(self.url("/health"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    /// `POST /api/sessions` — create a session, optionally with a client id
    /// (e.g. a `desktop_` prefixed id) and title.
    pub async fn create_session(
        &self,
        id: Option<&str>,
        title: Option<&str>,
    ) -> Result<SessionInfo, HermesError> {
        let mut body = serde_json::Map::new();
        if let Some(id) = id {
            body.insert("id".into(), json!(id));
        }
        if let Some(title) = title {
            body.insert("title".into(), json!(title));
        }
        let resp = self
            .http
            .post(self.url("/api/sessions"))
            .bearer_auth(&self.api_key)
            .json(&body)
            .timeout(Duration::from_secs(15))
            .send()
            .await?;
        let resp = check_status(resp).await?;

        #[derive(Deserialize)]
        struct Wrap {
            session: SessionInfo,
        }
        let wrap: Wrap = resp.json().await?;
        Ok(wrap.session)
    }

    /// `GET /api/sessions` — list sessions (newest first).
    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>, HermesError> {
        let resp = self
            .http
            .get(self.url("/api/sessions"))
            .bearer_auth(&self.api_key)
            .timeout(Duration::from_secs(15))
            .send()
            .await?;
        let resp = check_status(resp).await?;

        #[derive(Deserialize)]
        struct Wrap {
            data: Vec<SessionInfo>,
        }
        Ok(resp.json::<Wrap>().await?.data)
    }

    /// `POST /api/sessions/{id}/chat/stream` — send a message and stream the
    /// reply. Returns [`HermesError::SessionNotFound`] (404) if the session was
    /// reset, so the caller can mint a fresh one. The returned stream is
    /// consumed until [`ChatEvent::Done`].
    pub async fn chat_stream(
        &self,
        session_id: &str,
        message: &str,
    ) -> Result<impl Stream<Item = ChatEvent>, HermesError> {
        let resp = self
            .http
            .post(self.url(&format!("/api/sessions/{session_id}/chat/stream")))
            .bearer_auth(&self.api_key)
            .json(&json!({ "message": message }))
            .send()
            .await?;
        let resp = check_status(resp).await?;

        let stream = resp.bytes_stream().eventsource().map(|res| match res {
            Ok(ev) => parse_event(&ev.event, &ev.data),
            Err(e) => ChatEvent::Error(format!("SSE stream error: {e}")),
        });
        Ok(stream)
    }
}

/// Translate a non-success HTTP status into a typed error.
async fn check_status(resp: reqwest::Response) -> Result<reqwest::Response, HermesError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    match status {
        StatusCode::UNAUTHORIZED => Err(HermesError::Auth),
        StatusCode::NOT_FOUND => Err(HermesError::SessionNotFound),
        s => {
            let body = resp.text().await.unwrap_or_default();
            Err(HermesError::Status {
                status: s.as_u16(),
                body,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn health_true_on_200_false_on_500() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = HermesClient::new(server.uri(), "k").unwrap();
        assert!(client.health().await);

        let server2 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server2)
            .await;
        let client2 = HermesClient::new(server2.uri(), "k").unwrap();
        assert!(!client2.health().await);
    }

    #[tokio::test]
    async fn create_session_sends_bearer_and_parses_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/sessions"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "object": "hermes.session",
                "session": {"id": "desktop_1_abcd", "title": "[Desktop] test", "source": "api_server", "model": "x"}
            })))
            .mount(&server)
            .await;
        let client = HermesClient::new(server.uri(), "secret").unwrap();
        let s = client
            .create_session(Some("desktop_1_abcd"), Some("[Desktop] test"))
            .await
            .unwrap();
        assert_eq!(s.id, "desktop_1_abcd");
        assert_eq!(s.source.as_deref(), Some("api_server"));
    }

    #[tokio::test]
    async fn auth_failure_maps_to_auth_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/sessions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": {"message": "Invalid API key"}})))
            .mount(&server)
            .await;
        let client = HermesClient::new(server.uri(), "bad").unwrap();
        assert!(matches!(client.list_sessions().await, Err(HermesError::Auth)));
    }

    #[tokio::test]
    async fn chat_on_missing_session_maps_to_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/sessions/desktop_gone/chat/stream"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": {"message": "no session"}})))
            .mount(&server)
            .await;
        let client = HermesClient::new(server.uri(), "k").unwrap();
        let res = client.chat_stream("desktop_gone", "hi").await;
        assert!(matches!(res.err(), Some(HermesError::SessionNotFound)));
    }

    #[tokio::test]
    async fn list_sessions_parses_data_array() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/sessions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "list",
                "data": [
                    {"id": "desktop_1_aaaa", "title": "[Desktop] a", "source": "api_server"},
                    {"id": "telegram_9", "source": "telegram"}
                ]
            })))
            .mount(&server)
            .await;
        let client = HermesClient::new(server.uri(), "k").unwrap();
        let sessions = client.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "desktop_1_aaaa");
    }

    #[tokio::test]
    async fn chat_stream_parses_sse_event_sequence() {
        let server = MockServer::start().await;
        let sse_body = concat!(
            "event: run.started\ndata: {\"user_message\":{}}\n\n",
            "event: assistant.delta\ndata: {\"delta\":\"Hel\"}\n\n",
            "event: assistant.delta\ndata: {\"delta\":\"lo\"}\n\n",
            ": keepalive\n\n",
            "event: tool.started\ndata: {\"tool_name\":\"desktop_launch_app\"}\n\n",
            "event: assistant.completed\ndata: {\"content\":\"Hello\"}\n\n",
            "event: run.completed\ndata: {\"usage\":{\"input_tokens\":3}}\n\n",
            "event: done\ndata: {}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/api/sessions/desktop_1/chat/stream"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_body),
            )
            .mount(&server)
            .await;
        let client = HermesClient::new(server.uri(), "k").unwrap();
        let stream = client.chat_stream("desktop_1", "hi").await.unwrap();
        let events: Vec<ChatEvent> = stream.collect().await;

        // Deltas concatenate to the streamed text.
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                ChatEvent::Delta(d) => Some(d.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello");
        // The keepalive comment produced no event.
        assert!(events.contains(&ChatEvent::AssistantCompleted { content: "Hello".into() }));
        assert!(events.contains(&ChatEvent::Done));
        assert!(events.iter().any(|e| matches!(e, ChatEvent::ToolProgress { .. })));
    }
}
