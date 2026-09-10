//! MCP over stdio: one JSON-RPC message per line, which is how a desktop client launches a
//! server it owns.
//!
//! Nothing but protocol may be written to stdout on this transport — every diagnostic Charon
//! emits goes to stderr through `tracing`, because a stray `println!` here corrupts the stream
//! and the client's only symptom is a server that "does not work".

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::Server;

/// Serves MCP over stdin/stdout until the client closes the stream.
pub async fn serve_stdio(server: Arc<Server>) -> std::io::Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    tracing::info!("serving MCP over stdio");

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                tracing::error!(%error, "could not read from stdin");
                return Err(error);
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str(&line) {
            Ok(request) => super::handle(&server, &request).await,
            Err(error) => {
                tracing::warn!(%error, "received a line that is not JSON");
                Some(super::parse_error())
            }
        };

        let Some(response) = response else { continue };
        let mut encoded = response.to_string();
        encoded.push('\n');
        stdout.write_all(encoded.as_bytes()).await?;
        stdout.flush().await?;
    }

    tracing::info!("stdin closed; stopping");
    Ok(())
}
