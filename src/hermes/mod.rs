//! Hermes platform API client: session list/history + health.

pub mod client;
pub mod session;

pub use client::{HermesClient, HermesError};
pub use session::new_desktop_session_id;
