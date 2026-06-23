//! MCP HTTP server: mounts the `DesktopServer` StreamableHTTP service on axum.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::tools::DesktopServer;
use crate::ipc::protocol::DaemonMessage;

/// Serve the MCP endpoint at `http://{addr}/mcp` until `shutdown` fires.
///
/// When `auth_token` is set, a Bearer-auth layer guards `/mcp` (needed once the
/// endpoint is reachable via Traefik). Hermes dials the bind address directly
/// (config-based MCP registration), so the bind address is the only Host
/// accepted.
pub async fn serve(
    addr: SocketAddr,
    auth_token: Option<String>,
    dbus: Option<zbus::Connection>,
    toast_tx: broadcast::Sender<DaemonMessage>,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let allowed_hosts = vec![
        addr.to_string(),
        addr.ip().to_string(),
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ];

    // StreamableHttpServerConfig is #[non_exhaustive]; build via the setters.
    let config = StreamableHttpServerConfig::default()
        .with_sse_keep_alive(Some(Duration::from_secs(30)))
        .with_allowed_hosts(allowed_hosts)
        .with_cancellation_token(shutdown.child_token());

    let factory_dbus = dbus;
    let factory_tx = toast_tx;
    let service = StreamableHttpService::new(
        move || Ok(DesktopServer::new(factory_dbus.clone(), factory_tx.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    let mut router = axum::Router::new().nest_service("/mcp", service);
    if let Some(token) = auth_token {
        let expected = Arc::new(token);
        router = router.layer(middleware::from_fn(move |req, next| {
            require_bearer(expected.clone(), req, next)
        }));
        info!("MCP Bearer authentication enabled");
    } else {
        info!("MCP Bearer authentication disabled (no mcp_auth_token configured)");
    }

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

/// Bearer-auth middleware for the MCP endpoint: passes the request through only
/// when `Authorization: Bearer <expected>` matches, else returns 401.
async fn require_bearer(expected: Arc<String>, req: Request, next: Next) -> Response {
    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match provided {
        Some(token) if token == expected.as_str() => next.run(req).await,
        _ => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
    }
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
        let handle = tokio::spawn(async move { serve(addr, None, None, tx, s).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        shutdown.cancel();
        let res = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(res.is_ok(), "serve did not shut down within timeout");
    }

    /// The Bearer middleware rejects missing/incorrect tokens and passes the
    /// correct one through to the wrapped handler.
    #[tokio::test]
    async fn bearer_middleware_gates_requests() {
        use axum::body::Body;
        use axum::routing::get;
        use tower::ServiceExt;

        let expected = Arc::new("secret".to_string());
        let app = axum::Router::new()
            .route("/mcp", get(|| async { "ok" }))
            .layer(middleware::from_fn(move |req, next| {
                require_bearer(expected.clone(), req, next)
            }));

        let no_header = app
            .clone()
            .oneshot(Request::builder().uri("/mcp").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(no_header.status(), StatusCode::UNAUTHORIZED);

        let wrong = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/mcp")
                    .header("authorization", "Bearer nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        let ok = app
            .oneshot(
                Request::builder()
                    .uri("/mcp")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
    }
}
