//! Hermes platform API client: session management + streaming chat.

pub mod client;
pub mod session;
pub mod sse;

pub use client::{HermesClient, HermesError};
pub use session::{DESKTOP_TITLE_PREFIX, is_desktop_session, new_desktop_session_id};
pub use sse::ChatEvent;
