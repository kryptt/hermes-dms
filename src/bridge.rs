//! Gateway bridge: the daemon side of the Hermes "desktop" platform.
//!
//! A Hermes platform-plugin adapter (running in the Hermes pod) dials out to a
//! `/gateway` WebSocket on this daemon's existing HTTP server (the same one that
//! serves MCP, reusing its Bearer auth). Panel chat messages are pushed to the
//! adapter as `inbound` frames; the adapter — having run the message through the
//! full gateway pipeline (slash commands, per-session model override, …) — pushes
//! `draft` (growing full text), `send` (final), and `typing` frames back.
//!
//! The [`BridgeHub`] owns the single adapter connection and routes per-`chat_id`
//! response frames to whichever panel chat is awaiting them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Daemon → adapter frame.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToAdapter {
    /// A user message from the desktop; the adapter turns it into a gateway
    /// `MessageEvent` for `chat_id`.
    Inbound {
        chat_id: String,
        text: String,
        message_id: String,
    },
}

/// Adapter → daemon frame.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromAdapter {
    /// Growing full assistant text (from the adapter's `send_draft`); the panel
    /// *replaces* the streaming bubble with `text` (not append).
    Draft { chat_id: String, text: String },
    /// Final assistant message for the turn.
    Send { chat_id: String, text: String },
    /// Typing indicator.
    Typing { chat_id: String },
}

impl FromAdapter {
    fn chat_id(&self) -> &str {
        match self {
            FromAdapter::Draft { chat_id, .. }
            | FromAdapter::Send { chat_id, .. }
            | FromAdapter::Typing { chat_id } => chat_id,
        }
    }
}

/// Owns the single in-pod adapter connection and per-chat response routing.
#[derive(Default)]
pub struct BridgeHub {
    /// Sender into the connected adapter's WS write task (None = not connected).
    adapter: Mutex<Option<mpsc::UnboundedSender<ToAdapter>>>,
    /// chat_id → sink for response frames awaited by an in-flight panel chat.
    routes: Mutex<HashMap<String, mpsc::UnboundedSender<FromAdapter>>>,
}

impl BridgeHub {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_adapter(&self) -> std::sync::MutexGuard<'_, Option<mpsc::UnboundedSender<ToAdapter>>> {
        self.adapter.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_routes(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, mpsc::UnboundedSender<FromAdapter>>> {
        self.routes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// True iff a desktop-platform adapter is currently connected.
    pub fn is_connected(&self) -> bool {
        self.lock_adapter().is_some()
    }

    fn set_adapter(&self, tx: mpsc::UnboundedSender<ToAdapter>) {
        *self.lock_adapter() = Some(tx);
    }

    fn clear_adapter(&self) {
        *self.lock_adapter() = None;
    }

    /// Register a route for `chat_id` and return the receiver an in-flight chat
    /// reads response frames from. Replaces any prior route for that id.
    pub fn open_chat(&self, chat_id: &str) -> mpsc::UnboundedReceiver<FromAdapter> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.lock_routes().insert(chat_id.to_string(), tx);
        rx
    }

    /// Drop the route for `chat_id` (call when the chat turn completes).
    pub fn close_chat(&self, chat_id: &str) {
        self.lock_routes().remove(chat_id);
    }

    /// Push a user message to the adapter. Returns false if no adapter is
    /// connected (caller surfaces a "desktop platform offline" error).
    pub fn send_inbound(&self, chat_id: &str, text: &str, message_id: &str) -> bool {
        let guard = self.lock_adapter();
        match guard.as_ref() {
            Some(tx) => tx
                .send(ToAdapter::Inbound {
                    chat_id: chat_id.to_string(),
                    text: text.to_string(),
                    message_id: message_id.to_string(),
                })
                .is_ok(),
            None => false,
        }
    }

    /// Route an adapter response frame to the chat awaiting it. Frames for an
    /// unknown chat_id (no in-flight turn) are dropped.
    fn dispatch(&self, frame: FromAdapter) {
        let chat_id = frame.chat_id().to_string();
        let routes = self.lock_routes();
        if let Some(tx) = routes.get(&chat_id) {
            // Receiver gone (turn already finished) → drop silently.
            let _ = tx.send(frame);
        }
    }
}

/// axum handler for `GET /gateway` (WebSocket upgrade). Bearer auth is applied
/// by the shared middleware on the router.
pub async fn gateway_ws_handler(ws: WebSocketUpgrade, State(hub): State<Arc<BridgeHub>>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, hub))
}

async fn handle_socket(socket: WebSocket, hub: Arc<BridgeHub>) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ToAdapter>();
    hub.set_adapter(tx);
    info!("desktop platform adapter connected");

    // Write task: serialize queued ToAdapter frames out to the adapter.
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let json = match serde_json::to_string(&frame) {
                Ok(j) => j,
                Err(e) => {
                    warn!(error = %e, "dropping unserializable bridge frame");
                    continue;
                }
            };
            if sink.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });

    // Read loop: parse FromAdapter frames and route them.
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(Message::Text(t)) => match serde_json::from_str::<FromAdapter>(&t) {
                Ok(frame) => hub.dispatch(frame),
                Err(e) => warn!(error = %e, "ignoring malformed adapter frame"),
            },
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {} // ping/pong/binary: ignore (axum answers ping automatically)
        }
    }

    hub.clear_adapter();
    writer.abort();
    info!("desktop platform adapter disconnected");
}

#[cfg(test)]
impl BridgeHub {
    /// Test-only: simulate a connected adapter; returns the inbound receiver the
    /// fake adapter reads `ToAdapter` frames from.
    pub(crate) fn inject_adapter(&self) -> mpsc::UnboundedReceiver<ToAdapter> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.set_adapter(tx);
        rx
    }

    /// Test-only: route a frame as if it arrived from the adapter.
    pub(crate) fn test_dispatch(&self, frame: FromAdapter) {
        self.dispatch(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn send_inbound_false_without_adapter() {
        let hub = BridgeHub::new();
        assert!(!hub.is_connected());
        assert!(!hub.send_inbound("desktop:1", "hi", "m1"));
    }

    #[tokio::test]
    async fn inbound_reaches_adapter_and_responses_route_to_chat() {
        let hub = BridgeHub::new();
        // Simulate a connected adapter.
        let (atx, mut arx) = mpsc::unbounded_channel::<ToAdapter>();
        hub.set_adapter(atx);
        assert!(hub.is_connected());

        // A panel chat opens a route and sends a message.
        let mut chat = hub.open_chat("desktop:1");
        assert!(hub.send_inbound("desktop:1", "hello", "m1"));
        assert_eq!(
            arx.recv().await.unwrap(),
            ToAdapter::Inbound {
                chat_id: "desktop:1".into(),
                text: "hello".into(),
                message_id: "m1".into(),
            }
        );

        // Adapter streams a draft then a final send; both route to this chat.
        hub.dispatch(FromAdapter::Draft {
            chat_id: "desktop:1".into(),
            text: "He".into(),
        });
        hub.dispatch(FromAdapter::Send {
            chat_id: "desktop:1".into(),
            text: "Hello there".into(),
        });
        assert_eq!(
            chat.recv().await.unwrap(),
            FromAdapter::Draft {
                chat_id: "desktop:1".into(),
                text: "He".into()
            }
        );
        assert_eq!(
            chat.recv().await.unwrap(),
            FromAdapter::Send {
                chat_id: "desktop:1".into(),
                text: "Hello there".into()
            }
        );

        // A frame for an unrouted chat is dropped without panicking.
        hub.dispatch(FromAdapter::Typing {
            chat_id: "desktop:other".into(),
        });
        hub.close_chat("desktop:1");
    }
}
