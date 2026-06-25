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
use crate::hermes::{HermesClient, new_desktop_session_id};
use crate::ipc::protocol::{ClientMessage, DaemonMessage, status};
use crate::ipc::server::{Conn, IpcServer, MessageHandler};
use crate::ollama::OllamaRouterClient;

/// Capacity of the shared broadcast channel (toasts + status events).
const BROADCAST_CAP: usize = 256;
/// How often to probe Hermes health.
const HEALTH_INTERVAL: Duration = Duration::from_secs(10);

/// Max length of a launcher response delivered as a desktop notification.
const NOTIFY_MAX_LEN: usize = 280;

/// Routes local IPC requests: chat through the desktop-platform bridge, and
/// session list/history + health through the Hermes REST client.
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
    /// Desktop-platform bridge. All chats route through it (full gateway
    /// pipeline: slash commands, per-session model override, …).
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

    /// Handle a chat: route it through the desktop-platform bridge (the gateway
    /// pipeline — slash commands, per-session model override, …). A panel chat
    /// carries its conversation `chat_id` (`session_id`); a launcher chat has
    /// none, so we mint an ephemeral `chat_id` and deliver the final reply as a
    /// desktop notification (the launcher closes immediately, so the panel
    /// stream alone would never be seen).
    ///
    /// The gateway owns the session keyed by `chat_id`; there is no REST
    /// pre-creation. When the conversation lands a session, it shows up in the
    /// switcher via `source == "desktop"`.
    async fn handle_chat(
        &self,
        request_id: String,
        session_id: Option<String>,
        message: String,
        conn: &Conn,
        cancel: &CancellationToken,
    ) {
        let notify_on_complete = session_id.is_none();
        let chat_id = session_id.unwrap_or_else(new_desktop_session_id);
        self.chat_via_bridge(request_id, chat_id, message, conn, cancel, notify_on_complete)
            .await;
    }

    /// Route a chat through the desktop-platform bridge: push the message to the
    /// adapter (keyed by `chat_id`) and relay its `draft`/`send` frames back as
    /// `Draft`/`ChatComplete`. The adapter runs the message through the full
    /// gateway pipeline, so `/model` etc. work here.
    ///
    /// When `notify_on_complete` is set (launcher chats), the final reply is
    /// also delivered as a desktop notification.
    async fn chat_via_bridge(
        &self,
        request_id: String,
        chat_id: String,
        message: String,
        conn: &Conn,
        cancel: &CancellationToken,
        notify_on_complete: bool,
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
                        if notify_on_complete {
                            self.notify_launcher(&text).await;
                        }
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

    /// Deliver a launcher reply as a desktop notification, so it's visible
    /// without the panel open. No-op without a session bus or on empty text.
    async fn notify_launcher(&self, content: &str) {
        let (Some(dbus), false) = (&self.dbus, content.is_empty()) else {
            return;
        };
        let body: String = content.chars().take(NOTIFY_MAX_LEN).collect();
        if let Err(e) = notify::send(dbus, "Roci", &body, notify::Urgency::Normal, None).await {
            warn!(error = %e, "failed to deliver launcher response notification");
        }
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
            // No REST pre-creation: the gateway owns desktop sessions and mints
            // one (keyed by this chat_id) on the first bridge message. We just
            // hand the panel a fresh chat_id to use as its conversation handle,
            // tagged with the daemon's currently-selected model for display.
            ClientMessage::SessionCreate { request_id, title } => {
                let model = self
                    .selected_model
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                conn.send(DaemonMessage::SessionCreated {
                    request_id,
                    session_id: new_desktop_session_id(),
                    title,
                    model,
                })
                .await
            }
            // Only desktop-platform sessions — the panel shouldn't surface
            // Telegram/email sessions in its switcher. The gateway tags adapter
            // sessions with `source == "desktop"`.
            //
            // ponytail: switcher *resume* replays history read-only; the gateway
            // keys sessions by the originating chat_id (not exposed by the API),
            // so continuing a resumed session forks a new thread. Pre-existing
            // limitation — fix by persisting chat_ids panel-side if it matters.
            ClientMessage::SessionList { request_id } => match self.hermes.list_sessions().await {
                Ok(all) => {
                    let data = all
                        .into_iter()
                        .filter(|s| s.source.as_deref() == Some("desktop"))
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
    use std::path::Path;

    /// With a connected desktop adapter, a panel chat (has a chat id) routes
    /// through the bridge and the adapter's draft/send frames come back as
    /// Draft/ChatComplete.
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

    /// A launcher chat (no session id) also routes through the bridge: the
    /// daemon mints an ephemeral `desktop_` chat_id and relays the adapter's
    /// final send as ChatComplete. (Notification delivery is a no-op here — no
    /// session bus is wired in the test.)
    #[tokio::test]
    async fn launcher_chat_routes_through_bridge_with_minted_chat_id() {
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
            "hermes-dms-launcher-test-{}.sock",
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
                session_id: None,
                message: "hi".into(),
            })
            .await
            .unwrap();

        // The adapter receives the inbound message under a minted chat_id.
        let chat_id = match inbound.recv().await.unwrap() {
            crate::bridge::ToAdapter::Inbound { chat_id, text, .. } => {
                assert_eq!(text, "hi");
                assert!(chat_id.starts_with("desktop_"), "minted chat_id: {chat_id}");
                chat_id
            }
        };

        bridge.test_dispatch(FromAdapter::Send {
            chat_id,
            text: "Hello there".into(),
        });

        let final_content = loop {
            match client.next_message().await.unwrap() {
                Some(DaemonMessage::ChatComplete { content, .. }) => break content,
                Some(DaemonMessage::Error { message, .. }) => panic!("unexpected error: {message}"),
                Some(_) => {}
                None => panic!("connection closed early"),
            }
        };
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
