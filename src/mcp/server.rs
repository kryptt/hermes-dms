//! MCP HTTP server: mounts the `DesktopServer` StreamableHTTP service on axum.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::tools::DesktopServer;
use crate::ipc::protocol::DaemonMessage;

/// Serve the MCP endpoint at `http://{addr}/mcp` until `shutdown` fires.
///
/// `allowed_hosts` is derived from `addr` (NOT left at the rmcp default of
/// localhost-only, which would reject Hermes connecting to the VLAN20 IP).
/// `allowed_origins` stays empty: Hermes is server-to-server and sends no
/// `Origin`, which always passes; network isolation (binding to the VLAN20 IP)
/// is the primary defense.
pub async fn serve(
    addr: SocketAddr,
    dbus: Option<zbus::Connection>,
    toast_tx: broadcast::Sender<DaemonMessage>,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    // StreamableHttpServerConfig is #[non_exhaustive]; build via the setters.
    let config = StreamableHttpServerConfig::default()
        .with_sse_keep_alive(Some(Duration::from_secs(30)))
        .with_allowed_hosts([
            addr.to_string(),
            addr.ip().to_string(),
            "localhost".to_string(),
            "127.0.0.1".to_string(),
        ])
        .with_cancellation_token(shutdown.child_token());

    let factory_dbus = dbus;
    let factory_tx = toast_tx;
    let service = StreamableHttpService::new(
        move || Ok(DesktopServer::new(factory_dbus.clone(), factory_tx.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "MCP server listening at /mcp");

    let shutdown_signal = async move {
        shutdown.cancelled().await;
        info!("MCP server shutting down");
    };
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server binds and shuts down cleanly on the cancellation token.
    #[tokio::test]
    async fn binds_and_shuts_down() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (tx, _) = broadcast::channel(4);
        let shutdown = CancellationToken::new();
        // Bind to an ephemeral port; we can't pre-read it from serve(), so just
        // assert serve() returns promptly once cancelled.
        let s = shutdown.clone();
        let handle = tokio::spawn(async move { serve(addr, None, tx, s).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        shutdown.cancel();
        let res = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(res.is_ok(), "serve did not shut down within timeout");
    }
}
