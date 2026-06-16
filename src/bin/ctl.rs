//! `hermes-dms-ctl` — bridges the daemon's Unix socket to stdin/stdout for the
//! QML plugins (`Process` + `StdioCollector`) and for shell use.
//!
//! Modes:
//!   chat "<message>"  one-shot: send, print the final response, exit
//!   stream            long-lived: subscribe + relay JSON-lines both ways
//!   status            one-shot: print daemon/Hermes status, exit

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use hermes_dms::config::Config;
use hermes_dms::ipc::client::{IpcClient, send_line};
use hermes_dms::ipc::protocol::{ClientMessage, DaemonMessage};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Parser)]
#[command(name = "hermes-dms-ctl", about = "Control client for the hermes-dms daemon")]
struct Cli {
    /// Path to the daemon's Unix socket (default: $XDG_RUNTIME_DIR/hermes-dms.sock).
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Send a one-shot chat message and print the response.
    Chat {
        /// The message to send to Roci.
        message: String,
        /// Optional existing session id (omit for a fresh ephemeral session).
        #[arg(long)]
        session: Option<String>,
    },
    /// Subscribe and relay events as JSON-lines (used by the panel plugin).
    Stream,
    /// Print daemon/Hermes status.
    Status,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let socket = cli.socket.unwrap_or_else(Config::default_socket_path);

    let result = match cli.command {
        Command::Chat { message, session } => run_chat(&socket, message, session).await,
        Command::Stream => run_stream(&socket).await,
        Command::Status => run_status(&socket).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hermes-dms-ctl: {e}");
            ExitCode::FAILURE
        }
    }
}

fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// One-shot chat: print streamed deltas as they arrive, exit on completion.
async fn run_chat(
    socket: &PathBuf,
    message: String,
    session: Option<String>,
) -> Result<(), String> {
    let mut client = IpcClient::connect(socket)
        .await
        .map_err(|e| format!("connecting to daemon socket: {e}"))?;
    let request_id = new_request_id();
    client
        .send(&ClientMessage::Chat {
            request_id: request_id.clone(),
            session_id: session,
            message,
        })
        .await
        .map_err(|e| format!("sending chat: {e}"))?;

    let mut stdout = tokio::io::stdout();
    loop {
        match client.next_message().await.map_err(|e| e.to_string())? {
            None => return Err("daemon closed the connection before completing".into()),
            Some(msg) => match msg {
                DaemonMessage::Delta { request_id: id, content } if id == request_id => {
                    stdout.write_all(content.as_bytes()).await.map_err(|e| e.to_string())?;
                    stdout.flush().await.map_err(|e| e.to_string())?;
                }
                DaemonMessage::ChatComplete { request_id: id, content, .. } if id == request_id => {
                    // If nothing was streamed, print the final content; always
                    // end with a newline.
                    if !content.is_empty() {
                        stdout.write_all(content.as_bytes()).await.map_err(|e| e.to_string())?;
                    }
                    stdout.write_all(b"\n").await.map_err(|e| e.to_string())?;
                    stdout.flush().await.map_err(|e| e.to_string())?;
                    return Ok(());
                }
                DaemonMessage::Error { request_id: id, message }
                    if id.as_deref() == Some(request_id.as_str()) || id.is_none() =>
                {
                    return Err(message);
                }
                _ => {} // tool_progress, broadcasts, other requests: ignore
            },
        }
    }
}

/// Full-duplex bridge: stdin JSON-lines → socket, socket events → stdout.
async fn run_stream(socket: &PathBuf) -> Result<(), String> {
    let mut client = IpcClient::connect(socket)
        .await
        .map_err(|e| format!("connecting to daemon socket: {e}"))?;
    client
        .send(&ClientMessage::Subscribe { request_id: None })
        .await
        .map_err(|e| format!("subscribing: {e}"))?;

    let (mut reader, mut writer) = client.into_halves();

    // stdin → socket
    let stdin_task = tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ClientMessage>(&line) {
                Ok(msg) => {
                    if send_line(&mut writer, &msg).await.is_err() {
                        break;
                    }
                }
                Err(e) => eprintln!("hermes-dms-ctl: ignoring malformed stdin line: {e}"),
            }
        }
    });

    // socket → stdout (one JSON object per line, verbatim)
    let mut stdout = tokio::io::stdout();
    while let Some(line) = reader.next_line().await.map_err(|e| e.to_string())? {
        stdout.write_all(line.as_bytes()).await.map_err(|e| e.to_string())?;
        stdout.write_all(b"\n").await.map_err(|e| e.to_string())?;
        stdout.flush().await.map_err(|e| e.to_string())?;
    }

    stdin_task.abort();
    Ok(())
}

/// One-shot status query.
async fn run_status(socket: &PathBuf) -> Result<(), String> {
    let mut client = IpcClient::connect(socket)
        .await
        .map_err(|e| format!("connecting to daemon socket: {e}"))?;
    let request_id = new_request_id();
    client
        .send(&ClientMessage::Status { request_id: request_id.clone() })
        .await
        .map_err(|e| format!("requesting status: {e}"))?;

    loop {
        match client.next_message().await.map_err(|e| e.to_string())? {
            None => return Err("daemon closed the connection".into()),
            Some(DaemonMessage::Status { hermes, daemon, .. }) => {
                println!("daemon: {daemon}");
                println!("hermes: {hermes}");
                return Ok(());
            }
            Some(DaemonMessage::Error { message, .. }) => return Err(message),
            Some(_) => {}
        }
    }
}
