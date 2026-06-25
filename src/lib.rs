//! hermes-dms: a compositor-native Hermes bridge daemon.
//!
//! Three interfaces share one process: an MCP server (Hermes calls desktop
//! tools), a REST client (the desktop talks to Hermes), and a Unix-socket IPC
//! server (local QML plugins and `hermes-dms-ctl`).

// No `.unwrap()`/`.expect()` in production code; tests are exempt via not(test).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod bridge;
pub mod config;
pub mod daemon;
pub mod desktop;
pub mod hermes;
pub mod ipc;
pub mod mcp;
pub mod ollama;

pub use config::Config;
