//! Local IPC: JSON-lines protocol over a Unix domain socket.

pub mod client;
pub mod protocol;
pub mod server;

pub use client::IpcClient;
pub use protocol::{ClientMessage, DaemonMessage, SessionInfo};
pub use server::{Conn, IpcServer, MessageHandler};
