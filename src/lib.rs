//! hermes-dms: a compositor-native Hermes bridge daemon.
//!
//! Three interfaces share one process: an MCP server (Hermes calls desktop
//! tools), a REST client (the desktop talks to Hermes), and a Unix-socket IPC
//! server (local QML plugins and `hermes-dms-ctl`).

pub mod config;
pub mod desktop;
pub mod hermes;
pub mod ipc;
pub mod mcp;

pub use config::Config;
