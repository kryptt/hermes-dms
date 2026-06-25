//! Desktop session identity helpers.
//!
//! New conversations get a fresh client-minted `chat_id`; the gateway owns the
//! session it keys off that id, and the panel uses the id as its conversation
//! handle. Desktop sessions are recognized in the switcher by the gateway's
//! `source == "desktop"` tag, not by this id shape.

use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

/// Id prefix for chat_ids this daemon mints.
pub const DESKTOP_SESSION_PREFIX: &str = "desktop_";

/// Generate a fresh desktop chat_id: `desktop_{unix_secs}_{8 hex}`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_id_has_prefix_and_shape() {
        let id = new_desktop_session_id();
        assert!(id.starts_with(DESKTOP_SESSION_PREFIX));
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
}
