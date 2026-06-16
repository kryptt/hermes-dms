//! MCP server (StreamableHTTP) exposing desktop tools to Hermes.

pub mod server;
pub mod tools;

pub use server::serve;
pub use tools::DesktopServer;
