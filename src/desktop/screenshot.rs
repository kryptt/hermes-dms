//! Screenshots via `grim`, with optional focused-monitor targeting via Niri.
//!
//! Pixel-accurate single-window cropping is intentionally deferred (P1): niri's
//! per-window geometry is workspace-view-relative, not the global logical
//! coordinates `grim -g` expects, and is explicitly not API-stable. `window`
//! targeting therefore captures the focused *output* (monitor), which always
//! contains the active window.

use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum ScreenshotError {
    #[error("the session is locked")]
    Locked,
    #[error("querying niri: {0}")]
    Niri(#[source] std::io::Error),
    #[error("spawning grim (is gui-apps/grim installed?): {0}")]
    Spawn(#[source] std::io::Error),
    #[error("grim failed: {0}")]
    Grim(String),
}

/// Build the `grim` argument vector. `output` restricts capture to one monitor.
pub fn build_grim_args(output: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(name) = output {
        args.push("-o".to_string());
        args.push(name.to_string());
    }
    // PNG to stdout.
    args.push("-t".to_string());
    args.push("png".to_string());
    args.push("-".to_string());
    args
}

/// Resolve the focused output's name via the niri IPC socket. A fresh
/// connection per call avoids sharing niri's single-connection socket.
pub fn focused_output_name() -> std::io::Result<Option<String>> {
    use niri_ipc::socket::Socket;
    use niri_ipc::{Request, Response};

    let mut socket = Socket::connect()?;
    match socket.send(Request::FocusedOutput) {
        Ok(Ok(Response::FocusedOutput(Some(o)))) => Ok(Some(o.name)),
        Ok(Ok(_)) => Ok(None),
        Ok(Err(msg)) => Err(std::io::Error::other(msg)),
        Err(e) => Err(e),
    }
}

/// Best-effort lock check via `loginctl`. Returns false if it can't be
/// determined (so a missing `loginctl` never blocks screenshots).
pub fn session_locked() -> bool {
    let Ok(session_id) = std::env::var("XDG_SESSION_ID") else {
        return false;
    };
    match Command::new("loginctl")
        .args(["show-session", &session_id, "-p", "LockedHint", "--value"])
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim() == "yes",
        _ => false,
    }
}

/// Capture a PNG. `output` selects a specific monitor; `None` captures the
/// whole layout. Errors if the session is locked.
pub fn capture(output: Option<&str>) -> Result<Vec<u8>, ScreenshotError> {
    if session_locked() {
        return Err(ScreenshotError::Locked);
    }
    let args = build_grim_args(output);
    let out = Command::new("grim")
        .args(&args)
        .output()
        .map_err(ScreenshotError::Spawn)?;
    if !out.status.success() {
        return Err(ScreenshotError::Grim(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grim_args_full() {
        assert_eq!(build_grim_args(None), vec!["-t", "png", "-"]);
    }

    #[test]
    fn grim_args_with_output() {
        assert_eq!(
            build_grim_args(Some("DP-1")),
            vec!["-o", "DP-1", "-t", "png", "-"]
        );
    }
}
