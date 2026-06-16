//! Daemon orchestration: wires the IPC server, the MCP server, the Hermes
//! REST client, and the health/reconnect loop together.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::Config;
use crate::desktop::notify;
use crate::hermes::sse::ChatEvent;
use crate::hermes::{DESKTOP_TITLE_PREFIX, HermesClient, HermesError, new_desktop_session_id};
use crate::ipc::protocol::{ClientMessage, DaemonMessage, status};
use crate::ipc::server::{Conn, IpcServer, MessageHandler};

/// Capacity of the shared broadcast channel (toasts + status events).
const BROADCAST_CAP: usize = 256;
/// How often to probe Hermes health.
const HEALTH_INTERVAL: Duration = Duration::from_secs(10);

/// Max length of a launcher response delivered as a desktop notification.
const NOTIFY_MAX_LEN: usize = 280;

/// Bridges local IPC requests to the Hermes REST API.
pub struct DaemonHandler {
    hermes: HermesClient,
    /// Last observed Hermes reachability (kept current by the health loop).
    hermes_up: Arc<AtomicBool>,
    /// Session bus, used to deliver launcher (ephemeral) chat responses as
    /// desktop notifications independent of whether the panel is open.
    dbus: Option<zbus::Connection>,
}

impl DaemonHandler {
    pub fn new(
        hermes: HermesClient,
        hermes_up: Arc<AtomicBool>,
        dbus: Option<zbus::Connection>,
    ) -> Self {
        Self {
            hermes,
            hermes_up,
            dbus,
        }
    }

    fn status_str(&self) -> &'static str {
        if self.hermes_up.load(Ordering::Relaxed) {
            status::CONNECTED
        } else {
            status::DISCONNECTED
        }
    }

    /// Create a fresh desktop session and return its id.
    async fn new_session(&self, title: Option<&str>) -> Result<String, HermesError> {
        let id = new_desktop_session_id();
        let title = title
            .map(str::to_string)
            .unwrap_or_else(|| format!("{DESKTOP_TITLE_PREFIX} {id}"));
        let session = self.hermes.create_session(Some(&id), Some(&title)).await?;
        Ok(session.id)
    }

    /// Handle a chat: resolve a session (create an ephemeral one if none given),
    /// stream the reply, and recover once from a reset (404) session.
    async fn handle_chat(
        &self,
        request_id: String,
        session_id: Option<String>,
        message: String,
        conn: &Conn,
        cancel: &CancellationToken,
    ) {
        // No session id => launcher (ephemeral). Such responses are also
        // delivered as a desktop notification so they're visible even when the
        // panel isn't open.
        let ephemeral = session_id.is_none();
        let sid = match session_id {
            Some(s) => s,
            None => match self.new_session(None).await {
                Ok(s) => s,
                Err(e) => return self.send_error(conn, &request_id, e.to_string()).await,
            },
        };
        let final_content = self
            .stream_chat(&request_id, sid, &message, conn, cancel, true)
            .await;

        if ephemeral
            && let (Some(content), Some(dbus)) = (final_content, &self.dbus)
            && !content.is_empty()
        {
            let body: String = content.chars().take(NOTIFY_MAX_LEN).collect();
            if let Err(e) = notify::send(dbus, "Roci", &body, notify::Urgency::Normal, None).await {
                warn!(error = %e, "failed to deliver launcher response notification");
            }
        }
    }

    /// Stream a chat turn, emitting Delta/ToolProgress/ChatComplete over `conn`.
    /// Returns the final assistant text on success, or `None` if it errored or
    /// was cancelled (in which case an Error was already sent).
    async fn stream_chat(
        &self,
        request_id: &str,
        session_id: String,
        message: &str,
        conn: &Conn,
        cancel: &CancellationToken,
        allow_reset_recovery: bool,
    ) -> Option<String> {
        let stream = match self.hermes.chat_stream(&session_id, message).await {
            Ok(s) => s,
            Err(HermesError::SessionNotFound) if allow_reset_recovery => {
                // The session was reset; mint a new one and retry once.
                match self.new_session(None).await {
                    Ok(new_id) => {
                        conn.send(DaemonMessage::SessionReset {
                            request_id: Some(request_id.to_string()),
                            old_id: session_id,
                            new_id: new_id.clone(),
                        })
                        .await;
                        return Box::pin(
                            self.stream_chat(request_id, new_id, message, conn, cancel, false),
                        )
                        .await;
                    }
                    Err(e) => {
                        self.send_error(conn, request_id, e.to_string()).await;
                        return None;
                    }
                }
            }
            Err(e) => {
                self.send_error(conn, request_id, e.to_string()).await;
                return None;
            }
        };

        tokio::pin!(stream);
        let mut final_content = String::new();
        let mut usage = None;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    self.send_error(conn, request_id, "cancelled".into()).await;
                    return None;
                }
                item = stream.next() => match item {
                    Some(ChatEvent::Delta(c)) => {
                        final_content.push_str(&c);
                        conn.send(DaemonMessage::Delta {
                            request_id: request_id.to_string(),
                            content: c,
                        }).await;
                    }
                    Some(ChatEvent::ToolProgress { tool_name, status }) => {
                        conn.send(DaemonMessage::ToolProgress {
                            request_id: request_id.to_string(),
                            tool_name,
                            status,
                        }).await;
                    }
                    // assistant.completed is authoritative for the final text.
                    Some(ChatEvent::AssistantCompleted { content }) => final_content = content,
                    Some(ChatEvent::RunCompleted { usage: u }) => usage = u,
                    Some(ChatEvent::Error(m)) => {
                        self.send_error(conn, request_id, m).await;
                        return None;
                    }
                    Some(ChatEvent::Done) | None => break,
                    Some(_) => {}
                }
            }
        }

        conn.send(DaemonMessage::ChatComplete {
            request_id: request_id.to_string(),
            content: final_content.clone(),
            usage,
        })
        .await;
        Some(final_content)
    }

    async fn send_error(&self, conn: &Conn, request_id: &str, message: String) {
        conn.send(DaemonMessage::Error {
            request_id: Some(request_id.to_string()),
            message,
        })
        .await;
    }
}

impl MessageHandler for DaemonHandler {
    async fn handle(&self, msg: ClientMessage, conn: Conn, cancel: CancellationToken) {
        match msg {
            ClientMessage::Chat {
                request_id,
                session_id,
                message,
            } => {
                self.handle_chat(request_id, session_id, message, &conn, &cancel)
                    .await;
            }
            ClientMessage::SessionCreate { request_id, title } => {
                match self.new_session(title.as_deref()).await {
                    Ok(id) => {
                        conn.send(DaemonMessage::SessionCreated {
                            request_id,
                            session_id: id,
                            title,
                        })
                        .await
                    }
                    Err(e) => self.send_error(&conn, &request_id, e.to_string()).await,
                }
            }
            ClientMessage::SessionList { request_id } => match self.hermes.list_sessions().await {
                Ok(data) => {
                    conn.send(DaemonMessage::Sessions { request_id, data })
                        .await
                }
                Err(e) => self.send_error(&conn, &request_id, e.to_string()).await,
            },
            ClientMessage::SessionResume {
                request_id,
                session_id,
            } => match self.hermes.list_sessions().await {
                Ok(all) => {
                    let data = all.into_iter().filter(|s| s.id == session_id).collect();
                    conn.send(DaemonMessage::Sessions { request_id, data })
                        .await
                }
                Err(e) => self.send_error(&conn, &request_id, e.to_string()).await,
            },
            ClientMessage::Status { request_id } => {
                conn.send(DaemonMessage::Status {
                    hermes: self.status_str().to_string(),
                    daemon: status::READY.to_string(),
                    request_id: Some(request_id),
                })
                .await
            }
            // Subscribe and Cancel are handled by the IPC server itself.
            ClientMessage::Subscribe { .. } | ClientMessage::Cancel { .. } => {}
        }
    }
}

/// Periodically probe Hermes; broadcast a status event on every transition.
async fn health_loop(
    hermes: HermesClient,
    hermes_up: Arc<AtomicBool>,
    broadcast: broadcast::Sender<DaemonMessage>,
    shutdown: CancellationToken,
) {
    loop {
        let up = hermes.health().await;
        let was = hermes_up.swap(up, Ordering::Relaxed);
        if up != was {
            let hermes_status = if up {
                status::CONNECTED
            } else {
                status::DISCONNECTED
            };
            if up {
                info!("Hermes reachable");
            } else {
                warn!("Hermes unreachable");
            }
            let _ = broadcast.send(DaemonMessage::Status {
                hermes: hermes_status.to_string(),
                daemon: status::READY.to_string(),
                request_id: None,
            });
        }
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = tokio::time::sleep(HEALTH_INTERVAL) => {}
        }
    }
}

/// Run the daemon until `shutdown` fires: MCP server + health loop in the
/// background, IPC server in the foreground.
pub async fn run(config: Config, shutdown: CancellationToken) -> std::io::Result<()> {
    let hermes = HermesClient::new(config.hermes_api_url.clone(), config.hermes_api_key.clone())
        .map_err(|e| std::io::Error::other(format!("building Hermes client: {e}")))?;

    // Session D-Bus is optional: tools degrade gracefully without it.
    let dbus = match zbus::Connection::session().await {
        Ok(c) => Some(c),
        Err(e) => {
            warn!(error = %e, "no D-Bus session bus; desktop_notify will be unavailable");
            None
        }
    };

    let (broadcast_tx, _) = broadcast::channel::<DaemonMessage>(BROADCAST_CAP);
    let hermes_up = Arc::new(AtomicBool::new(false));

    // Background: health/reconnect loop.
    tokio::spawn(health_loop(
        hermes.clone(),
        hermes_up.clone(),
        broadcast_tx.clone(),
        shutdown.child_token(),
    ));

    // Background: MCP HTTP server (shares the broadcast channel for toasts).
    let mcp_addr = config.mcp_listen_addr;
    let mcp_tx = broadcast_tx.clone();
    let mcp_shutdown = shutdown.child_token();
    let mcp_dbus = dbus.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::mcp::serve(mcp_addr, mcp_dbus, mcp_tx, mcp_shutdown).await {
            error!(error = %e, "MCP server exited with error");
        }
    });

    // Foreground: IPC server.
    let handler = Arc::new(DaemonHandler::new(hermes, hermes_up, dbus));
    let ipc = IpcServer::new(handler, broadcast_tx);
    ipc.run(config.socket_path.clone(), shutdown).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::IpcClient;
    use serde_json::json;
    use std::path::Path;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// End-to-end through the daemon core: a Chat IPC request creates a session
    /// on (mock) Hermes, streams deltas back, and finishes with ChatComplete.
    #[tokio::test]
    async fn chat_request_bridges_to_hermes_and_streams_back() {
        let server = MockServer::start().await;
        // Session creation.
        Mock::given(method("POST"))
            .and(path("/api/sessions"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "object": "hermes.session",
                "session": {"id": "desktop_xyz", "source": "api_server"}
            })))
            .mount(&server)
            .await;
        // Chat stream.
        let sse = concat!(
            "event: assistant.delta\ndata: {\"delta\":\"Hi\"}\n\n",
            "event: assistant.completed\ndata: {\"content\":\"Hi there\"}\n\n",
            "event: run.completed\ndata: {\"usage\":{\"t\":1}}\n\n",
            "event: done\ndata: {}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/api/sessions/desktop_xyz/chat/stream"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse),
            )
            .mount(&server)
            .await;

        let hermes = HermesClient::new(server.uri(), "k").unwrap();
        let (btx, _) = broadcast::channel(16);
        let handler = Arc::new(DaemonHandler::new(
            hermes,
            Arc::new(AtomicBool::new(true)),
            None,
        ));
        let shutdown = CancellationToken::new();
        let socket = std::env::temp_dir().join(format!(
            "hermes-dms-daemon-test-{}.sock",
            uuid::Uuid::new_v4()
        ));
        let ipc = IpcServer::new(handler, btx);
        let run_socket = socket.clone();
        let run_shutdown = shutdown.clone();
        tokio::spawn(async move {
            ipc.run(run_socket, run_shutdown).await.unwrap();
        });

        // Connect and send a chat with no session id (ephemeral).
        let mut client = connect(&socket).await;
        client
            .send(&ClientMessage::Chat {
                request_id: "r1".into(),
                session_id: None,
                message: "hi".into(),
            })
            .await
            .unwrap();

        let mut saw_delta = false;
        let final_content = loop {
            match client.next_message().await.unwrap() {
                Some(DaemonMessage::Delta { content, .. }) => {
                    saw_delta = true;
                    assert_eq!(content, "Hi");
                }
                Some(DaemonMessage::ChatComplete { content, .. }) => break content,
                Some(DaemonMessage::Error { message, .. }) => panic!("unexpected error: {message}"),
                Some(_) => {}
                None => panic!("connection closed early"),
            }
        };
        assert!(saw_delta);
        assert_eq!(final_content, "Hi there");
        shutdown.cancel();
    }

    async fn connect(path: &Path) -> IpcClient {
        for _ in 0..50 {
            if let Ok(c) = IpcClient::connect(path).await {
                return c;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("daemon socket never came up");
    }
}
