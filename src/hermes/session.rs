//! Desktop session identity helpers.
//!
//! Hermes hardcodes `source: "api_server"` for sessions created over the API
//! (see D5), so desktop sessions are distinguished by a recognizable id prefix
//! plus a `[Desktop]` title tag for the future session picker.

use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

/// Id prefix for sessions this daemon creates.
pub const DESKTOP_SESSION_PREFIX: &str = "desktop_";

/// Title prefix for desktop sessions (client-side filtering aid).
pub const DESKTOP_TITLE_PREFIX: &str = "[Desktop]";

/// Generate a fresh desktop session id: `desktop_{unix_secs}_{8 hex}`.
pub fn new_desktop_session_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // First 4 bytes of a v4 UUID → 8 hex chars. Formatting the bytes directly
    // avoids slicing a String (`&s[..8]`), which would panic on a short string.
    let rand = Uuid::new_v4();
    let [b0, b1, b2, b3, ..] = rand.into_bytes();
    format!("{DESKTOP_SESSION_PREFIX}{ts}_{b0:02x}{b1:02x}{b2:02x}{b3:02x}")
}

/// Whether an id was minted by this daemon.
pub fn is_desktop_session(id: &str) -> bool {
    id.starts_with(DESKTOP_SESSION_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_id_has_prefix_and_shape() {
        let id = new_desktop_session_id();
        assert!(is_desktop_session(&id));
        // desktop_{ts}_{rand}: at least three underscore-delimited parts.
        let rest = id.strip_prefix(DESKTOP_SESSION_PREFIX).unwrap();
        let (ts, rand) = rest.split_once('_').expect("ts_rand");
        assert!(ts.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(rand.len(), 8);
        assert!(rand.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ids_are_unique() {
        let a = new_desktop_session_id();
        let b = new_desktop_session_id();
        assert_ne!(a, b);
    }

    #[test]
    fn non_desktop_ids_rejected() {
        assert!(!is_desktop_session("api_123_abc"));
        assert!(!is_desktop_session("telegram_42"));
    }
}
