//! The `DesktopServer` MCP tool surface: notify, launch_app, screenshot.

use base64::Engine;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ErrorData, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use tokio::sync::broadcast;

use crate::desktop::notify::Urgency;
use crate::desktop::{launch, notify, screenshot};
use crate::ipc::protocol::DaemonMessage;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NotifyRequest {
    /// Notification title / summary line.
    pub title: String,
    /// Notification body text.
    pub body: String,
    /// Urgency: "low", "normal" (default), or "critical".
    pub urgency: Option<String>,
    /// Optional freedesktop icon name (e.g. "dialog-information").
    pub icon: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LaunchRequest {
    /// Executable to run (argv[0]). This is NOT a shell line — there is no
    /// shell expansion; pass arguments via `args`.
    pub command: String,
    /// Optional arguments, each a separate argv entry.
    pub args: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScreenshotRequest {
    /// "screen" (default) for the focused screen, or "window" for the focused
    /// window (captured pixel-accurately by niri).
    pub target: Option<String>,
}

/// MCP server exposing desktop control tools. `dbus` is optional so the server
/// can be constructed (and its tool list inspected) without a session bus.
#[derive(Clone)]
pub struct DesktopServer {
    tool_router: ToolRouter<Self>,
    dbus: Option<zbus::Connection>,
    toast_tx: broadcast::Sender<DaemonMessage>,
}

impl DesktopServer {
    pub fn new(dbus: Option<zbus::Connection>, toast_tx: broadcast::Sender<DaemonMessage>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            dbus,
            toast_tx,
        }
    }
}

#[tool_router]
impl DesktopServer {
    #[tool(
        description = "Show a desktop notification on the user's screen (rh-anine). Use this to surface short messages, alerts, or the result of an action."
    )]
    async fn desktop_notify(
        &self,
        Parameters(req): Parameters<NotifyRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let conn = self
            .dbus
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("D-Bus session bus unavailable", None))?;
        let urgency = Urgency::from_opt(req.urgency.as_deref());
        let id = notify::send(conn, &req.title, &req.body, urgency, req.icon.as_deref())
            .await
            .map_err(|e| ErrorData::internal_error(format!("notification failed: {e}"), None))?;

        // Also surface it to any subscribed panel as a toast.
        let _ = self.toast_tx.send(DaemonMessage::Toast {
            title: req.title,
            body: req.body,
            icon: req.icon,
        });
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Notification sent (id: {id})"
        ))]))
    }

    #[tool(
        description = "Launch a desktop application on rh-anine. The command runs directly (no shell, no expansion); pass arguments via 'args'."
    )]
    async fn desktop_launch_app(
        &self,
        Parameters(req): Parameters<LaunchRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = req.args.unwrap_or_default();
        let pid = launch::launch_detached(&req.command, &args)
            .map_err(|e| ErrorData::internal_error(format!("launch failed: {e}"), None))?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Launched {} (pid: {pid})",
            req.command
        ))]))
    }

    #[tool(
        description = "Capture a screenshot of the desktop and return it as a PNG image. target 'screen' (default) captures the focused screen; 'window' captures the focused window."
    )]
    async fn desktop_screenshot(
        &self,
        Parameters(req): Parameters<ScreenshotRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let target = screenshot::Target::from_opt(req.target.as_deref());
        let png = tokio::task::spawn_blocking(move || screenshot::capture(target))
            .await
            .map_err(|e| ErrorData::internal_error(format!("join error: {e}"), None))?
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        Ok(CallToolResult::success(vec![Content::image(
            b64,
            "image/png",
        )]))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DesktopServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Desktop control surface for rh-anine: notifications, app launching, and screenshots.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> DesktopServer {
        let (tx, _) = broadcast::channel(4);
        DesktopServer::new(None, tx)
    }

    #[test]
    fn registers_exactly_three_tools() {
        let s = server();
        let tools = s.tool_router.list_all();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(tools.len(), 3, "tools: {names:?}");
        assert!(names.contains(&"desktop_notify"));
        assert!(names.contains(&"desktop_launch_app"));
        assert!(names.contains(&"desktop_screenshot"));
    }

    #[test]
    fn tools_have_input_schemas() {
        let s = server();
        for tool in s.tool_router.list_all() {
            // Each tool exposes a non-empty JSON input schema object.
            assert!(
                !tool.input_schema.is_empty(),
                "tool {} has no schema",
                tool.name
            );
        }
    }

    #[tokio::test]
    async fn notify_without_dbus_errors_cleanly() {
        let s = server();
        let res = s
            .desktop_notify(Parameters(NotifyRequest {
                title: "t".into(),
                body: "b".into(),
                urgency: None,
                icon: None,
            }))
            .await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn launch_tool_spawns_detached_process() {
        let s = server();
        let res = s
            .desktop_launch_app(Parameters(LaunchRequest {
                command: "/bin/true".into(),
                args: None,
            }))
            .await;
        assert!(res.is_ok());
    }
}
