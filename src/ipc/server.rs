//! Unix-domain-socket server for local clients (panel, launcher, ctl).
//!
//! Each connection gets a reader task and a writer task. Requests carrying a
//! `request_id` are dispatched to the [`MessageHandler`] on a spawned task so a
//! long streaming chat doesn't block a concurrent `cancel` on the same
//! connection. `subscribe` opts a connection into broadcast events.

use std::collections::HashMap;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::protocol::{ClientMessage, DaemonMessage};

/// Capacity of the per-connection outbound queue.
const CONN_QUEUE: usize = 128;

/// Handles client requests, emitting zero or more responses via [`Conn`].
///
/// The returned future is `Send` so the server can spawn it, letting each
/// request stream independently of other traffic on the connection.
pub trait MessageHandler: Send + Sync + 'static {
    fn handle(
        &self,
        msg: ClientMessage,
        conn: Conn,
        cancel: CancellationToken,
    ) -> impl Future<Output = ()> + Send;
}

/// A handle for sending responses/events back over a single connection.
#[derive(Clone)]
pub struct Conn {
    tx: mpsc::Sender<DaemonMessage>,
}

impl Conn {
    /// Queue a message for this connection. Drops silently if the client has
    /// disconnected — one dead client must not affect the daemon or others.
    pub async fn send(&self, msg: DaemonMessage) {
        if self.tx.send(msg).await.is_err() {
            debug!("dropping message for disconnected client");
        }
    }
}

/// The IPC server. Generic over the request handler so it can be unit-tested
/// with a stub independent of the Hermes client.
pub struct IpcServer<H> {
    handler: Arc<H>,
    broadcast: broadcast::Sender<DaemonMessage>,
}

impl<H: MessageHandler> IpcServer<H> {
    pub fn new(handler: Arc<H>, broadcast: broadcast::Sender<DaemonMessage>) -> Self {
        Self { handler, broadcast }
    }

    /// Bind the socket and serve until `shutdown` fires. Removes a stale socket
    /// from a previous run, refuses to clobber a live one, and cleans up the
    /// socket file on exit.
    pub async fn run(
        self,
        socket_path: PathBuf,
        shutdown: CancellationToken,
    ) -> std::io::Result<()> {
        prepare_socket_path(&socket_path).await?;
        let listener = UnixListener::bind(&socket_path)?;
        tokio::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).await?;
        info!(path = %socket_path.display(), "IPC socket listening");

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("IPC server shutting down");
                    break;
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _addr)) => {
                            let handler = self.handler.clone();
                            let broadcast_rx = self.broadcast.subscribe();
                            let conn_shutdown = shutdown.child_token();
                            tokio::spawn(async move {
                                handle_connection(stream, handler, broadcast_rx, conn_shutdown).await;
                            });
                        }
                        Err(e) => {
                            error!(error = %e, "IPC accept failed");
                        }
                    }
                }
            }
        }

        // Best-effort cleanup; ignore if already gone.
        let _ = tokio::fs::remove_file(&socket_path).await;
        Ok(())
    }
}

/// Remove a leftover socket file, but refuse to bind over one that still has a
/// live listener behind it (another running daemon).
async fn prepare_socket_path(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    match UnixStream::connect(path).await {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!(
                "a hermes-dms daemon is already listening on {}",
                path.display()
            ),
        )),
        Err(_) => {
            warn!(path = %path.display(), "removing stale socket from previous run");
            tokio::fs::remove_file(path).await
        }
    }
}

async fn handle_connection<H: MessageHandler>(
    stream: UnixStream,
    handler: Arc<H>,
    mut broadcast_rx: broadcast::Receiver<DaemonMessage>,
    shutdown: CancellationToken,
) {
    let (read_half, mut write_half) = stream.into_split();
    let (conn_tx, mut conn_rx) = mpsc::channel::<DaemonMessage>(CONN_QUEUE);
    let subscribed = Arc::new(AtomicBool::new(false));

    // Writer task: serialize outbound messages + (when subscribed) broadcasts.
    let writer_subscribed = subscribed.clone();
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = conn_rx.recv() => {
                    match msg {
                        Some(msg) => {
                            if write_line(&mut write_half, &msg).await.is_err() {
                                break;
                            }
                        }
                        None => break, // all senders dropped: connection is done
                    }
                }
                bcast = broadcast_rx.recv() => {
                    match bcast {
                        Ok(msg) if writer_subscribed.load(Ordering::Relaxed) => {
                            if write_line(&mut write_half, &msg).await.is_err() {
                                break;
                            }
                        }
                        Ok(_) => {} // not subscribed: ignore
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(skipped = n, "client lagged on broadcast events");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            // broadcast source gone; keep serving direct messages
                        }
                    }
                }
            }
        }
    });

    // Reader loop: parse JSON-lines and dispatch.
    let conn = Conn { tx: conn_tx };
    let in_flight: Arc<Mutex<HashMap<String, CancellationToken>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let mut lines = BufReader::new(read_half).lines();

    loop {
        let line = tokio::select! {
            _ = shutdown.cancelled() => break,
            res = lines.next_line() => match res {
                Ok(Some(line)) => line,
                Ok(None) => break,          // client closed
                Err(e) => { debug!(error = %e, "read error, closing connection"); break; }
            },
        };

        if line.trim().is_empty() {
            continue;
        }

        let msg: ClientMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                conn.send(DaemonMessage::Error {
                    request_id: None,
                    message: format!("malformed request: {e}"),
                })
                .await;
                continue;
            }
        };

        match msg {
            ClientMessage::Subscribe { .. } => {
                subscribed.store(true, Ordering::Relaxed);
                debug!("client subscribed to broadcast events");
            }
            ClientMessage::Cancel { request_id } => {
                if let Some(token) = in_flight.lock().await.get(&request_id) {
                    token.cancel();
                    debug!(request_id, "cancelled in-flight request");
                }
            }
            other => {
                let request_id = other.request_id().map(String::from);
                let token = shutdown.child_token();
                if let Some(id) = &request_id {
                    in_flight.lock().await.insert(id.clone(), token.clone());
                }
                let handler = handler.clone();
                let conn = conn.clone();
                let in_flight = in_flight.clone();
                tokio::spawn(async move {
                    handler.handle(other, conn, token).await;
                    if let Some(id) = request_id {
                        in_flight.lock().await.remove(&id);
                    }
                });
            }
        }
    }

    // Reader done: drop the sender so the writer task can finish.
    drop(conn);
    let _ = writer.await;
}

async fn write_line<W>(w: &mut W, msg: &DaemonMessage) -> std::io::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let mut buf = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    buf.push(b'\n');
    w.write_all(&buf).await?;
    w.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::protocol::status;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    /// Stub handler: echoes a chat as a delta + completion, answers status.
    struct Echo;
    impl MessageHandler for Echo {
        async fn handle(&self, msg: ClientMessage, conn: Conn, cancel: CancellationToken) {
            match msg {
                ClientMessage::Chat {
                    request_id,
                    message,
                    ..
                } => {
                    conn.send(DaemonMessage::Delta {
                        request_id: request_id.clone(),
                        content: message.clone(),
                    })
                    .await;
                    conn.send(DaemonMessage::ChatComplete {
                        request_id,
                        content: message,
                    })
                    .await;
                }
                ClientMessage::Status { request_id } => {
                    conn.send(DaemonMessage::Status {
                        hermes: status::CONNECTED.into(),
                        daemon: status::READY.into(),
                        request_id: Some(request_id),
                    })
                    .await;
                }
                // A request that blocks until cancelled, then reports back.
                ClientMessage::SessionList { request_id } => {
                    cancel.cancelled().await;
                    conn.send(DaemonMessage::Error {
                        request_id: Some(request_id),
                        message: "cancelled".into(),
                    })
                    .await;
                }
                _ => {}
            }
        }
    }

    async fn read_msg(stream: &mut UnixStream) -> DaemonMessage {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = stream.read(&mut byte).await.unwrap();
            if n == 0 {
                panic!("connection closed before a full line");
            }
            if byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
        }
        serde_json::from_slice(&buf).unwrap()
    }

    async fn send_msg(stream: &mut UnixStream, msg: &ClientMessage) {
        let mut line = serde_json::to_vec(msg).unwrap();
        line.push(b'\n');
        stream.write_all(&line).await.unwrap();
    }

    fn start_server() -> (PathBuf, CancellationToken, broadcast::Sender<DaemonMessage>) {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("hermes-dms-test-{}.sock", uuid::Uuid::new_v4()));
        let shutdown = CancellationToken::new();
        let (btx, _) = broadcast::channel(16);
        let server = IpcServer::new(Arc::new(Echo), btx.clone());
        let run_path = path.clone();
        let run_shutdown = shutdown.clone();
        tokio::spawn(async move {
            server.run(run_path, run_shutdown).await.unwrap();
        });
        (path, shutdown, btx)
    }

    async fn connect(path: &Path) -> UnixStream {
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(path).await {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("server never came up");
    }

    #[tokio::test]
    async fn chat_round_trip() {
        let (path, shutdown, _btx) = start_server();
        let mut c = connect(&path).await;
        send_msg(
            &mut c,
            &ClientMessage::Chat {
                request_id: "r1".into(),
                session_id: None,
                message: "hello".into(),
            },
        )
        .await;
        assert!(
            matches!(read_msg(&mut c).await, DaemonMessage::Delta { content, .. } if content == "hello")
        );
        assert!(
            matches!(read_msg(&mut c).await, DaemonMessage::ChatComplete { content, .. } if content == "hello")
        );
        shutdown.cancel();
    }

    #[tokio::test]
    async fn malformed_json_gets_error_not_crash() {
        let (path, shutdown, _btx) = start_server();
        let mut c = connect(&path).await;
        c.write_all(b"{not json}\n").await.unwrap();
        assert!(matches!(
            read_msg(&mut c).await,
            DaemonMessage::Error { .. }
        ));
        // The connection is still usable afterwards.
        send_msg(
            &mut c,
            &ClientMessage::Status {
                request_id: "s1".into(),
            },
        )
        .await;
        assert!(matches!(
            read_msg(&mut c).await,
            DaemonMessage::Status { .. }
        ));
        shutdown.cancel();
    }

    #[tokio::test]
    async fn only_subscribers_get_broadcasts() {
        let (path, shutdown, btx) = start_server();
        let mut sub = connect(&path).await;
        let mut plain = connect(&path).await;

        send_msg(&mut sub, &ClientMessage::Subscribe { request_id: None }).await;
        // Give the reader a moment to flip the subscribed flag.
        tokio::time::sleep(Duration::from_millis(50)).await;

        btx.send(DaemonMessage::Toast {
            title: "t".into(),
            body: "b".into(),
            icon: None,
        })
        .unwrap();

        assert!(matches!(
            read_msg(&mut sub).await,
            DaemonMessage::Toast { .. }
        ));

        // The non-subscriber should NOT receive the toast; a subsequent status
        // request must be the first thing it reads.
        send_msg(
            &mut plain,
            &ClientMessage::Status {
                request_id: "s".into(),
            },
        )
        .await;
        assert!(matches!(
            read_msg(&mut plain).await,
            DaemonMessage::Status { .. }
        ));
        shutdown.cancel();
    }

    #[tokio::test]
    async fn cancel_interrupts_in_flight_request() {
        let (path, shutdown, _btx) = start_server();
        let mut c = connect(&path).await;
        send_msg(
            &mut c,
            &ClientMessage::SessionList {
                request_id: "blk".into(),
            },
        )
        .await;
        // Not completed yet; cancel it.
        tokio::time::sleep(Duration::from_millis(30)).await;
        send_msg(
            &mut c,
            &ClientMessage::Cancel {
                request_id: "blk".into(),
            },
        )
        .await;
        assert!(matches!(
            read_msg(&mut c).await,
            DaemonMessage::Error { message, .. } if message == "cancelled"
        ));
        shutdown.cancel();
    }

    #[tokio::test]
    async fn stale_socket_is_replaced() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("hermes-dms-stale-{}.sock", uuid::Uuid::new_v4()));
        // A leftover file with no listener behind it. (A real dropped
        // UnixListener races: tokio doesn't unlink on drop and the fd can stay
        // briefly connectable, making this flaky under parallel runs.)
        std::fs::write(&path, b"").unwrap();
        // prepare_socket_path should remove it since nothing is listening.
        prepare_socket_path(&path).await.unwrap();
        assert!(UnixListener::bind(&path).is_ok());
        let _ = std::fs::remove_file(&path);
    }
}
