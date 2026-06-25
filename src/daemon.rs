//! Daemon orchestration: wires the IPC server, the MCP server, the Hermes
//! REST client, and the health/reconnect loop together.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::broadcast;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::Config;
use crate::bridge::{BridgeHub, FromAdapter};
use crate::desktop::notify;
use crate::hermes::sse::ChatEvent;
use crate::hermes::{
    DESKTOP_TITLE_PREFIX, HermesClient, HermesError, is_desktop_session, new_desktop_session_id,
};
use crate::ipc::protocol::{ClientMessage, DaemonMessage, SessionInfo, status};
use crate::ipc::server::{Conn, IpcServer, MessageHandler};
use crate::ollama::OllamaRouterClient;

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
    /// ollama-router client for the model picker (None if no token configured).
    ollama: Option<OllamaRouterClient>,
    /// Model chosen via SetModel; passed when creating new sessions. None = use
    /// Hermes's configured default.
    selected_model: Arc<Mutex<Option<String>>>,
    /// Desktop-platform bridge. When an adapter is connected, panel chats route
    /// through it (full gateway pipeline: slash commands, model override, …)
    /// instead of the api_server REST path.
    bridge: Arc<BridgeHub>,
}

impl DaemonHandler {
    pub fn new(
        hermes: HermesClient,
        hermes_up: Arc<AtomicBool>,
        dbus: Option<zbus::Connection>,
        ollama: Option<OllamaRouterClient>,
        bridge: Arc<BridgeHub>,
    ) -> Self {
        Self {
            hermes,
            hermes_up,
            dbus,
            ollama,
            selected_model: Arc::new(Mutex::new(None)),
            bridge,
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
    ///
    /// The id-based title is always unique. A caller-provided title (e.g. the
    /// panel's fixed `[Desktop] panel`) can collide with an existing session,
    /// which Hermes rejects with a 400 `invalid_title`; we recover by retrying
    /// once with the guaranteed-unique id title.
    async fn new_session(&self, title: Option<&str>) -> Result<SessionInfo, HermesError> {
        let id = new_desktop_session_id();
        let unique = format!("{DESKTOP_TITLE_PREFIX} {id}");
        let chosen = title.unwrap_or(unique.as_str());
        let model = self
            .selected_model
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match self
            .hermes
            .create_session(Some(&id), Some(chosen), model.as_deref())
            .await
        {
            Ok(session) => Ok(session),
            Err(HermesError::Status { status: 400, body })
                if body.contains("invalid_title") && chosen != unique =>
            {
                self.hermes
                    .create_session(Some(&id), Some(&unique), model.as_deref())
                    .await
            }
            Err(e) => Err(e),
        }
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
        // Panel chat (has a conversation/chat id) routes through the desktop
        // platform bridge when an adapter is connected — that's the path that
        // gets slash commands (/model) and per-session model overrides. Falls
        // back to the api_server REST path below when no adapter is connected.
        if let Some(chat_id) = session_id.clone()
            && self.bridge.is_connected()
        {
            return self
                .chat_via_bridge(request_id, chat_id, message, conn, cancel)
                .await;
        }

        // No session id => launcher (ephemeral). Such responses are also
        // delivered as a desktop notification so they're visible even when the
        // panel isn't open.
        let ephemeral = session_id.is_none();
        let sid = match session_id {
            Some(s) => s,
            None => match self.new_session(None).await {
                Ok(s) => s.id,
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

    /// Route a panel chat through the desktop-platform bridge: push the message
    /// to the adapter (keyed by `chat_id`) and relay its `draft`/`send` frames
    /// back as `Draft`/`ChatComplete`. The adapter runs the message through the
    /// full gateway pipeline, so `/model` etc. work here.
    async fn chat_via_bridge(
        &self,
        request_id: String,
        chat_id: String,
        message: String,
        conn: &Conn,
        cancel: &CancellationToken,
    ) {
        let mut frames = UnboundedReceiverStream::new(self.bridge.open_chat(&chat_id));
        if !self.bridge.send_inbound(&chat_id, &message, &request_id) {
            self.bridge.close_chat(&chat_id);
            return self
                .send_error(conn, &request_id, "desktop platform offline".into())
                .await;
        }
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    self.send_error(conn, &request_id, "cancelled".into()).await;
                    break;
                }
                item = frames.next() => match item {
                    Some(FromAdapter::Draft { text, .. }) => {
                        conn.send(DaemonMessage::Draft {
                            request_id: request_id.clone(),
                            content: text,
                        }).await;
                    }
                    Some(FromAdapter::Send { text, .. }) => {
                        conn.send(DaemonMessage::ChatComplete {
                            request_id: request_id.clone(),
                            content: text,
                        }).await;
                        break;
                    }
                    Some(FromAdapter::Typing { .. }) => {} // panel already shows busy
                    None => {
                        // Adapter disconnected mid-turn.
                        self.send_error(conn, &request_id, "desktop platform disconnected".into()).await;
                        break;
                    }
                }
            }
        }
        self.bridge.close_chat(&chat_id);
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
                    Ok(s) => {
                        let new_id = s.id;
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
                    Ok(s) => {
                        conn.send(DaemonMessage::SessionCreated {
                            request_id,
                            session_id: s.id,
                            title: s.title.or(title),
                            model: s.model,
                        })
                        .await
                    }
                    Err(e) => self.send_error(&conn, &request_id, e.to_string()).await,
                }
            }
            // Only this daemon's own (desktop_*) sessions — the panel shouldn't
            // surface Telegram/email sessions in its switcher.
            ClientMessage::SessionList { request_id } => match self.hermes.list_sessions().await {
                Ok(all) => {
                    let data = all
                        .into_iter()
                        .filter(|s| is_desktop_session(&s.id))
                        .collect();
                    conn.send(DaemonMessage::Sessions { request_id, data })
                        .await
                }
                Err(e) => self.send_error(&conn, &request_id, e.to_string()).await,
            },
            ClientMessage::SessionMessages {
                request_id,
                session_id,
            } => match self.hermes.get_messages(&session_id).await {
                Ok(data) => {
                    conn.send(DaemonMessage::Messages {
                        request_id,
                        session_id,
                        data,
                    })
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
            ClientMessage::ModelList { request_id } => match &self.ollama {
                Some(ollama) => match ollama.list_models().await {
                    Ok(mut data) => {
                        let active = self
                            .selected_model
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .clone();
                        for m in &mut data {
                            m.active = active.as_deref() == Some(m.id.as_str());
                        }
                        conn.send(DaemonMessage::Models { request_id, data }).await
                    }
                    Err(e) => self.send_error(&conn, &request_id, e.to_string()).await,
                },
                None => {
                    self.send_error(&conn, &request_id, "model picker not configured".into())
                        .await
                }
            },
            ClientMessage::SetModel { model, .. } => {
                *self
                    .selected_model
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(model);
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
    // The desktop platform adapter (in the Hermes pod) connects to `/gateway`
    // on the same HTTP server; the hub routes panel chats ↔ adapter frames.
    // (U3 will hand this hub to the IPC handler to reroute panel chat through it.)
    let bridge = Arc::new(BridgeHub::new());

    let mcp_addr = config.mcp_listen_addr;
    let mcp_auth = config.mcp_auth_token.clone();
    let mcp_tx = broadcast_tx.clone();
    let mcp_shutdown = shutdown.child_token();
    let mcp_dbus = dbus.clone();
    let mcp_bridge = bridge.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::mcp::serve(
            mcp_addr,
            mcp_auth,
            mcp_dbus,
            mcp_tx,
            mcp_bridge,
            mcp_shutdown,
        )
        .await
        {
            error!(error = %e, "MCP server exited with error");
        }
    });

    // Model picker (optional): needs a token to reach ollama-router via Traefik.
    let ollama = config.ollama_router_token.as_ref().and_then(|tok| {
        OllamaRouterClient::new(config.ollama_router_url.clone(), Some(tok.clone()))
            .map_err(
                |e| warn!(error = %e, "ollama-router client init failed; model picker disabled"),
            )
            .ok()
    });

    // Foreground: IPC server.
    let handler = Arc::new(DaemonHandler::new(hermes, hermes_up, dbus, ollama, bridge));
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
            None,
            Arc::new(BridgeHub::new()),
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

    /// Hermes requires unique session titles: a fixed caller title (the panel's
    /// `[Desktop] panel`) collides on the second create with a 400 invalid_title.
    /// The daemon must recover by retrying with its unique id-based title — the
    /// field bug that silently hung the panel.
    #[tokio::test]
    async fn new_session_recovers_from_title_collision() {
        use std::collections::HashSet;
        use std::sync::Mutex as StdMutex;
        use wiremock::{Request, Respond};

        // Mock Hermes: 201 on a first-seen title, 400 invalid_title on a repeat.
        struct UniqueTitle {
            seen: StdMutex<HashSet<String>>,
        }
        impl Respond for UniqueTitle {
            fn respond(&self, req: &Request) -> ResponseTemplate {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let title = body["title"].as_str().unwrap_or_default().to_string();
                let id = body["id"].as_str().unwrap_or("x").to_string();
                if !self.seen.lock().unwrap().insert(title.clone()) {
                    return ResponseTemplate::new(400).set_body_json(json!({
                        "error": {
                            "message": format!("Title '{title}' is already in use"),
                            "type": "invalid_request_error",
                            "code": "invalid_title"
                        }
                    }));
                }
                ResponseTemplate::new(201).set_body_json(json!({
                    "object": "hermes.session",
                    "session": {"id": id, "title": title, "source": "api_server"}
                }))
            }
        }

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/sessions"))
            .respond_with(UniqueTitle {
                seen: StdMutex::new(HashSet::new()),
            })
            .mount(&server)
            .await;

        let hermes = HermesClient::new(server.uri(), "k").unwrap();
        let handler = DaemonHandler::new(
            hermes,
            Arc::new(AtomicBool::new(true)),
            None,
            None,
            Arc::new(BridgeHub::new()),
        );

        // First create with the panel's fixed title succeeds.
        let a = handler.new_session(Some("[Desktop] panel")).await.unwrap();
        // Second with the SAME fixed title 400s; without retry this would Err and
        // the .unwrap() would panic. Recovery makes it succeed with a fresh id.
        let b = handler.new_session(Some("[Desktop] panel")).await.unwrap();
        assert_ne!(a.id, b.id);
    }

    /// With a connected desktop adapter, a panel chat (has a chat id) routes
    /// through the bridge — not the api_server REST path — and the adapter's
    /// draft/send frames come back as Draft/ChatComplete.
    #[tokio::test]
    async fn panel_chat_routes_through_bridge_when_adapter_connected() {
        // Hermes URL is unused on the bridge path; point it at a dead address.
        let hermes = HermesClient::new("http://127.0.0.1:1", "k").unwrap();
        let bridge = Arc::new(BridgeHub::new());
        let mut inbound = bridge.inject_adapter();
        let handler = Arc::new(DaemonHandler::new(
            hermes,
            Arc::new(AtomicBool::new(true)),
            None,
            None,
            bridge.clone(),
        ));
        let shutdown = CancellationToken::new();
        let socket = std::env::temp_dir().join(format!(
            "hermes-dms-bridge-test-{}.sock",
            uuid::Uuid::new_v4()
        ));
        let (btx, _) = broadcast::channel(16);
        let ipc = IpcServer::new(handler, btx);
        let run_socket = socket.clone();
        let run_shutdown = shutdown.clone();
        tokio::spawn(async move {
            ipc.run(run_socket, run_shutdown).await.unwrap();
        });

        let mut client = connect(&socket).await;
        client
            .send(&ClientMessage::Chat {
                request_id: "r1".into(),
                session_id: Some("desktop:1".into()),
                message: "hi".into(),
            })
            .await
            .unwrap();

        // The adapter receives the inbound message for this chat id.
        match inbound.recv().await.unwrap() {
            crate::bridge::ToAdapter::Inbound { chat_id, text, .. } => {
                assert_eq!(chat_id, "desktop:1");
                assert_eq!(text, "hi");
            }
        }

        // The adapter streams a draft then a final send.
        bridge.test_dispatch(FromAdapter::Draft {
            chat_id: "desktop:1".into(),
            text: "He".into(),
        });
        bridge.test_dispatch(FromAdapter::Send {
            chat_id: "desktop:1".into(),
            text: "Hello there".into(),
        });

        let mut saw_draft = false;
        let final_content = loop {
            match client.next_message().await.unwrap() {
                Some(DaemonMessage::Draft { content, .. }) => {
                    saw_draft = true;
                    assert_eq!(content, "He");
                }
                Some(DaemonMessage::ChatComplete { content, .. }) => break content,
                Some(DaemonMessage::Error { message, .. }) => panic!("unexpected error: {message}"),
                Some(_) => {}
                None => panic!("connection closed early"),
            }
        };
        assert!(saw_draft);
        assert_eq!(final_content, "Hello there");
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
