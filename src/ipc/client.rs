//! Thin client for the daemon's Unix socket, used by `hermes-dms-ctl`.

use std::path::Path;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use super::protocol::{ClientMessage, DaemonMessage};

/// A connected IPC client. Read and write halves are independent so callers can
/// relay both directions concurrently (the `stream` bridge does this).
pub struct IpcClient {
    reader: Lines<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
}

impl IpcClient {
    pub async fn connect(path: &Path) -> std::io::Result<Self> {
        let stream = UnixStream::connect(path).await?;
        let (read_half, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(read_half).lines(),
            writer,
        })
    }

    /// Split into independent halves for full-duplex relaying.
    pub fn into_halves(self) -> (Lines<BufReader<OwnedReadHalf>>, OwnedWriteHalf) {
        (self.reader, self.writer)
    }

    pub async fn send(&mut self, msg: &ClientMessage) -> std::io::Result<()> {
        send_line(&mut self.writer, msg).await
    }

    /// Read the next daemon message, or `None` at end of stream.
    pub async fn next_message(&mut self) -> std::io::Result<Option<DaemonMessage>> {
        match self.reader.next_line().await? {
            Some(line) if line.trim().is_empty() => Box::pin(self.next_message()).await,
            Some(line) => {
                let msg = serde_json::from_str(&line).map_err(std::io::Error::other)?;
                Ok(Some(msg))
            }
            None => Ok(None),
        }
    }
}

/// Write a single client message as a JSON line.
pub async fn send_line(writer: &mut OwnedWriteHalf, msg: &ClientMessage) -> std::io::Result<()> {
    let mut buf = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    buf.push(b'\n');
    writer.write_all(&buf).await?;
    writer.flush().await
}
