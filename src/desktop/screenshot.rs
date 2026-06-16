//! Screenshots via niri's built-in IPC screenshot actions — no external tool.
//!
//! niri captures the focused screen or window, writing a PNG to an absolute
//! path we choose; we then read and return the bytes. This removes the previous
//! `grim` dependency and, because niri captures the focused window natively,
//! gives pixel-accurate single-window shots (which `grim` could not from niri's
//! workspace-relative geometry).

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use niri_ipc::socket::Socket;
use niri_ipc::{Action, Request};
use uuid::Uuid;

/// PNG magic bytes, used to detect a fully-written file.
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// What to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The focused screen/output.
    Screen,
    /// The focused window (pixel-accurate, via niri).
    Window,
}

impl Target {
    pub fn from_opt(s: Option<&str>) -> Self {
        match s {
            Some("window") => Target::Window,
            _ => Target::Screen,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScreenshotError {
    #[error("the session is locked")]
    Locked,
    #[error("connecting to niri: {0}")]
    NiriConnect(#[source] std::io::Error),
    #[error("niri screenshot failed: {0}")]
    Niri(String),
    #[error("screenshot file never materialized at {0}")]
    Timeout(String),
}

/// A unique absolute path for the capture, under `$XDG_RUNTIME_DIR` (local
/// tmpfs) when available.
fn temp_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!("hermes-dms-shot-{}.png", Uuid::new_v4()))
}

/// Best-effort lock check via `loginctl`. Returns false if it can't be
/// determined, so a missing `loginctl` never blocks screenshots.
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

/// Capture a PNG via niri and return its bytes. Blocking — call from
/// `spawn_blocking`. Errors if the session is locked.
pub fn capture(target: Target) -> Result<Vec<u8>, ScreenshotError> {
    if session_locked() {
        return Err(ScreenshotError::Locked);
    }

    let path = temp_path();
    let path_str = path.to_string_lossy().into_owned();
    let action = match target {
        Target::Window => Action::ScreenshotWindow {
            id: None,
            write_to_disk: true,
            show_pointer: false,
            path: Some(path_str.clone()),
        },
        Target::Screen => Action::ScreenshotScreen {
            write_to_disk: true,
            show_pointer: false,
            path: Some(path_str.clone()),
        },
    };

    let mut socket = Socket::connect().map_err(ScreenshotError::NiriConnect)?;
    match socket.send(Request::Action(action)) {
        Ok(Ok(_)) => {}
        Ok(Err(msg)) => return Err(ScreenshotError::Niri(msg)),
        Err(e) => return Err(ScreenshotError::NiriConnect(e)),
    }

    // niri writes the file asynchronously after acknowledging the action; poll
    // until a fully-written PNG appears (guard against reading a partial file).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(bytes) = std::fs::read(&path)
            && bytes.starts_with(PNG_MAGIC)
        {
            let _ = std::fs::remove_file(&path);
            return Ok(bytes);
        }
        if Instant::now() >= deadline {
            let _ = std::fs::remove_file(&path);
            return Err(ScreenshotError::Timeout(path_str));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_from_opt() {
        assert_eq!(Target::from_opt(Some("window")), Target::Window);
        assert_eq!(Target::from_opt(Some("full")), Target::Screen);
        assert_eq!(Target::from_opt(Some("screen")), Target::Screen);
        assert_eq!(Target::from_opt(None), Target::Screen);
    }

    #[test]
    fn temp_path_is_absolute_png() {
        let p = temp_path();
        assert!(p.is_absolute());
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("png"));
    }

    #[test]
    fn temp_paths_are_unique() {
        assert_ne!(temp_path(), temp_path());
    }
}
