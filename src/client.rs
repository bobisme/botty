//! Client for communicating with the botty server.
//!
//! Handles Unix socket connection and auto-starting the server.

#![allow(unsafe_code)] // getuid() call

use crate::protocol::{Request, Response};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::{debug, info, warn};

/// Errors that can occur in the client.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("failed to connect to server: {0}")]
    Connect(#[source] std::io::Error),

    #[error("failed to send request: {0}")]
    Send(#[source] std::io::Error),

    #[error("failed to receive response: {0}")]
    Receive(#[source] std::io::Error),

    #[error("failed to serialize request: {0}")]
    Serialize(#[source] serde_json::Error),

    #[error("failed to deserialize response: {0}")]
    Deserialize(#[source] serde_json::Error),

    #[error("failed to start server: {0}")]
    ServerStart(#[source] std::io::Error),

    #[error("server did not start in time")]
    ServerTimeout,

    #[error("server returned error: {0}")]
    ServerError(String),

    #[error("connection lost")]
    ConnectionLost,
}

/// Get the default socket path for the botty server.
pub fn default_socket_path() -> PathBuf {
    if let Some(runtime_dir) = dirs::runtime_dir() {
        runtime_dir.join("botty.sock")
    } else {
        // Fallback to /tmp/botty-$UID.sock
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/botty-{uid}.sock"))
    }
}

/// Client for the botty server.
pub struct Client {
    socket_path: PathBuf,
    stream: Option<BufReader<UnixStream>>,
}

impl Client {
    /// Create a new client that will connect to the given socket path.
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            stream: None,
        }
    }

    /// Create a client with the default socket path.
    pub fn with_default_path() -> Self {
        Self::new(default_socket_path())
    }

    /// Connect to the server, starting it if necessary.
    pub async fn connect(&mut self) -> Result<(), ClientError> {
        if self.stream.is_some() {
            return Ok(());
        }

        // Try to connect directly first
        match UnixStream::connect(&self.socket_path).await {
            Ok(stream) => {
                debug!("Connected to existing server");
                self.stream = Some(BufReader::new(stream));
                return Ok(());
            }
            Err(e) => {
                debug!("Could not connect to server: {}", e);
            }
        }

        // Start the server
        self.start_server().await?;

        // Try to connect with retries
        for i in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            match UnixStream::connect(&self.socket_path).await {
                Ok(stream) => {
                    info!("Connected to server after {} attempts", i + 1);
                    self.stream = Some(BufReader::new(stream));
                    return Ok(());
                }
                Err(e) => {
                    if i % 10 == 9 {
                        debug!("Still waiting for server (attempt {}): {}", i + 1, e);
                    }
                }
            }
        }

        Err(ClientError::ServerTimeout)
    }

    /// Start the server as a background process.
    async fn start_server(&self) -> Result<(), ClientError> {
        info!("Starting server...");

        // Get the path to the current executable
        let exe = std::env::current_exe().map_err(ClientError::ServerStart)?;

        // Spawn server in background
        Command::new(&exe)
            .arg("server")
            .arg("--daemon")
            .spawn()
            .map_err(ClientError::ServerStart)?;

        Ok(())
    }

    /// Send a request to the server and wait for a response.
    pub async fn request(&mut self, request: Request) -> Result<Response, ClientError> {
        // Ensure we're connected
        self.connect().await?;

        let stream = self.stream.as_mut().ok_or(ClientError::ConnectionLost)?;

        // Serialize and send request
        let mut json = serde_json::to_string(&request).map_err(ClientError::Serialize)?;
        json.push('\n');

        stream
            .get_mut()
            .write_all(json.as_bytes())
            .await
            .map_err(ClientError::Send)?;

        // Read response
        let mut line = String::new();
        let n = stream
            .read_line(&mut line)
            .await
            .map_err(ClientError::Receive)?;

        if n == 0 {
            return Err(ClientError::ConnectionLost);
        }

        let response: Response =
            serde_json::from_str(&line).map_err(ClientError::Deserialize)?;

        // Check for server error
        if let Response::Error { message } = &response {
            warn!("Server returned error: {}", message);
        }

        Ok(response)
    }

    /// Get the socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}
