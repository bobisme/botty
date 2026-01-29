//! Attach mode implementation.
//!
//! Handles interactive terminal bridging between user TTY and agent PTY.

#![allow(unsafe_code)] // Terminal manipulation requires unsafe

use crate::protocol::{AttachEndReason, Request, Response};
use std::os::fd::{AsFd, OwnedFd};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tracing::{debug, info, warn};

/// Errors during attach mode.
#[derive(Debug, Error)]
pub enum AttachError {
    #[error("failed to get terminal attributes: {0}")]
    GetTermios(#[source] nix::Error),

    #[error("failed to set terminal attributes: {0}")]
    SetTermios(#[source] nix::Error),

    #[error("stdin is not a terminal")]
    NotATty,

    #[error("I/O error: {0}")]
    Io(#[source] std::io::Error),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("agent not found: {0}")]
    AgentNotFound(String),

    #[error("connection lost")]
    ConnectionLost,
}

/// Detach key sequence state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetachState {
    /// Normal state - no prefix seen.
    Normal,
    /// Saw Ctrl+G (0x07), waiting for 'd'.
    SawPrefix,
}

/// Configuration for attach mode.
pub struct AttachConfig {
    /// Agent ID (needed for resize requests).
    pub agent_id: String,
    /// Prefix key for detach (default: Ctrl+G = 0x07).
    pub detach_prefix: u8,
    /// Key after prefix to detach (default: 'd' = 0x64).
    pub detach_key: u8,
    /// Read-only mode (no input forwarding).
    pub readonly: bool,
}

impl AttachConfig {
    /// Create a new config with the given agent ID.
    pub fn new(agent_id: String) -> Self {
        Self {
            agent_id,
            detach_prefix: 0x07, // Ctrl+G
            detach_key: b'd',
            readonly: false,
        }
    }
}

/// Saved terminal state for restoration.
struct TerminalState {
    original_termios: nix::sys::termios::Termios,
    stdin_fd: OwnedFd,
}

impl TerminalState {
    /// Save current terminal state and switch to raw mode.
    fn enter_raw_mode() -> Result<Self, AttachError> {
        use nix::sys::termios::{self, LocalFlags, InputFlags, OutputFlags, SetArg};

        // Get stdin as a borrowed fd
        let stdin = std::io::stdin();
        let stdin_borrowed = stdin.as_fd();

        // Check if stdin is a TTY
        if !nix::unistd::isatty(stdin_borrowed).unwrap_or(false) {
            return Err(AttachError::NotATty);
        }

        // Get current terminal attributes
        let original_termios = termios::tcgetattr(stdin_borrowed).map_err(AttachError::GetTermios)?;

        // Create raw mode settings
        let mut raw = original_termios.clone();

        // Input flags: disable special handling
        raw.input_flags.remove(InputFlags::IGNBRK);
        raw.input_flags.remove(InputFlags::BRKINT);
        raw.input_flags.remove(InputFlags::PARMRK);
        raw.input_flags.remove(InputFlags::ISTRIP);
        raw.input_flags.remove(InputFlags::INLCR);
        raw.input_flags.remove(InputFlags::IGNCR);
        raw.input_flags.remove(InputFlags::ICRNL);
        raw.input_flags.remove(InputFlags::IXON);

        // Output flags: disable post-processing
        raw.output_flags.remove(OutputFlags::OPOST);

        // Local flags: disable echo, canonical mode, signals
        raw.local_flags.remove(LocalFlags::ECHO);
        raw.local_flags.remove(LocalFlags::ECHONL);
        raw.local_flags.remove(LocalFlags::ICANON);
        raw.local_flags.remove(LocalFlags::ISIG);
        raw.local_flags.remove(LocalFlags::IEXTEN);

        // Control chars: read returns after 1 byte, no timeout
        raw.control_chars[nix::sys::termios::SpecialCharacterIndices::VMIN as usize] = 1;
        raw.control_chars[nix::sys::termios::SpecialCharacterIndices::VTIME as usize] = 0;

        // Apply raw mode
        termios::tcsetattr(stdin_borrowed, SetArg::TCSAFLUSH, &raw)
            .map_err(AttachError::SetTermios)?;

        // Duplicate the fd so we can hold onto it for restoration
        let stdin_fd = stdin_borrowed.try_clone_to_owned()
            .map_err(AttachError::Io)?;

        Ok(Self {
            original_termios,
            stdin_fd,
        })
    }

    /// Restore original terminal state.
    fn restore(&self) -> Result<(), AttachError> {
        use nix::sys::termios::{self, SetArg};
        termios::tcsetattr(&self.stdin_fd, SetArg::TCSAFLUSH, &self.original_termios)
            .map_err(AttachError::SetTermios)
    }
}

impl Drop for TerminalState {
    fn drop(&mut self) {
        if let Err(e) = self.restore() {
            eprintln!("Warning: failed to restore terminal: {e}");
        }
    }
}

/// Run attach mode.
///
/// This takes over stdin/stdout and bridges them to the agent PTY via the server.
///
/// # Errors
///
/// Returns an error if:
/// - stdin is not a TTY
/// - Failed to set terminal to raw mode
/// - Connection lost during attach
/// - Protocol error with server
#[allow(clippy::missing_panics_doc)] // serde_json::to_string on valid types won't panic
pub async fn run_attach(
    stream: &mut UnixStream,
    agent_id: &str,
    config: AttachConfig,
) -> Result<AttachEndReason, AttachError> {
    // Send attach request
    let request = Request::Attach {
        id: agent_id.to_string(),
        readonly: config.readonly,
    };
    let mut json = serde_json::to_string(&request).expect("Request serialization should never fail");
    json.push('\n');
    stream
        .write_all(json.as_bytes())
        .await
        .map_err(AttachError::Io)?;

    // Read response
    let mut buf = vec![0u8; 4096];
    let mut response_buf = Vec::new();
    loop {
        let n = stream.read(&mut buf).await.map_err(AttachError::Io)?;
        if n == 0 {
            return Err(AttachError::ConnectionLost);
        }
        response_buf.extend_from_slice(&buf[..n]);
        if response_buf.contains(&b'\n') {
            break;
        }
    }

    // Find the newline that terminates the JSON response
    let newline_pos = response_buf.iter().position(|&b| b == b'\n')
        .ok_or_else(|| AttachError::Protocol("no newline in response".to_string()))?;
    
    // Parse just the JSON part (up to and including newline)
    let response: Response = serde_json::from_slice(&response_buf[..newline_pos])
        .map_err(|e| AttachError::Protocol(format!("invalid response: {e}")))?;

    // Any bytes after the newline are initial screen data from the server
    let mut initial_screen_data = if newline_pos + 1 < response_buf.len() {
        response_buf[newline_pos + 1..].to_vec()
    } else {
        Vec::new()
    };

    match response {
        Response::AttachStarted { id, size } => {
            info!("Attached to {} ({}x{})", id, size.1, size.0);
        }
        Response::Error { message } => {
            if message.contains("not found") {
                return Err(AttachError::AgentNotFound(agent_id.to_string()));
            }
            return Err(AttachError::Protocol(message));
        }
        _ => {
            return Err(AttachError::Protocol(format!(
                "unexpected response: {response:?}",
            )));
        }
    }

    // Enter raw mode first so the initial screen renders correctly
    let _terminal_state = TerminalState::enter_raw_mode()?;
    info!("Entered raw mode. Press Ctrl+G then 'd' to detach.");

    // Hide cursor in readonly mode - we're just viewing, not interacting
    // This prevents cursor flashing in TUI apps that continuously redraw
    if config.readonly {
        use std::io::Write;
        print!("\x1b[?25l"); // DECTCEM - hide cursor
        std::io::stdout().flush().map_err(AttachError::Io)?;
    }

    // Read any additional initial screen data that may arrive
    // The server sends the screen render after the JSON response
    // Keep reading until we get no more data (with short timeouts)
    // Limit buffer size to prevent memory exhaustion (16MB should be enough for any screen)
    const MAX_INITIAL_SCREEN_SIZE: usize = 16 * 1024 * 1024;
    {
        use std::time::Duration;
        use tokio::time::timeout;
        
        let mut extra_buf = vec![0u8; 65536]; // Large buffer for screen data
        
        // Read in a loop until no more data arrives
        // Use short timeouts to detect end of initial data
        loop {
            // Check size limit before reading more
            if initial_screen_data.len() >= MAX_INITIAL_SCREEN_SIZE {
                warn!("Initial screen data exceeded {} bytes, truncating", MAX_INITIAL_SCREEN_SIZE);
                break;
            }
            
            match timeout(Duration::from_millis(100), stream.read(&mut extra_buf)).await {
                Ok(Ok(n)) if n > 0 => {
                    initial_screen_data.extend_from_slice(&extra_buf[..n]);
                    // If we got a full buffer, there might be more
                    if n < extra_buf.len() {
                        // Partial read - probably done, but try once more
                        if let Ok(Ok(n2)) = timeout(Duration::from_millis(20), stream.read(&mut extra_buf)).await {
                            if n2 > 0 {
                                initial_screen_data.extend_from_slice(&extra_buf[..n2]);
                            }
                        }
                        break;
                    }
                }
                _ => break, // Timeout or error - done reading initial data
            }
        }
    }

    // Output any initial screen data
    if !initial_screen_data.is_empty() {
        info!("Outputting {} bytes of initial screen data", initial_screen_data.len());
        use std::io::Write;
        std::io::stdout().write_all(&initial_screen_data).map_err(AttachError::Io)?;
        std::io::stdout().flush().map_err(AttachError::Io)?;
    } else {
        info!("No initial screen data received");
    }

    // Run the I/O bridge
    let result = run_io_bridge(stream, &config).await;

    // Restore cursor visibility in readonly mode
    if config.readonly {
        use std::io::Write;
        print!("\x1b[?25h"); // DECTCEM - show cursor
        let _ = std::io::stdout().flush();
    }

    // Terminal state is restored on drop
    info!("Exited attach mode");

    result
}

/// Get terminal size from stdout
fn get_terminal_size() -> Option<(u16, u16)> {
    use nix::libc::{TIOCGWINSZ, winsize};
    use std::os::unix::io::AsRawFd;
    
    let fd = std::io::stdout().as_raw_fd();
    let mut ws: winsize = unsafe { std::mem::zeroed() };
    
    let result = unsafe { nix::libc::ioctl(fd, TIOCGWINSZ, &mut ws) };
    if result == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
        Some((ws.ws_row, ws.ws_col))
    } else {
        None
    }
}

/// Run the bidirectional I/O bridge.
async fn run_io_bridge(
    stream: &mut UnixStream,
    config: &AttachConfig,
) -> Result<AttachEndReason, AttachError> {
    use tokio::io::{stdin, stdout};
    use tokio::signal::unix::{signal, SignalKind};

    let mut stdin = stdin();
    let mut stdout = stdout();

    let mut detach_state = DetachState::Normal;
    let mut input_buf = [0u8; 1024];
    let mut output_buf = [0u8; 4096];
    
    // Set up SIGWINCH handler for terminal resize
    let mut sigwinch = signal(SignalKind::window_change())
        .map_err(|e| AttachError::Io(e))?;
    
    // Track current size to detect changes
    let mut current_size = get_terminal_size();

    loop {
        tokio::select! {
            // Handle SIGWINCH (terminal resize)
            _ = sigwinch.recv() => {
                if let Some((rows, cols)) = get_terminal_size() {
                    // Only send resize if size actually changed
                    if current_size != Some((rows, cols)) {
                        current_size = Some((rows, cols));
                        debug!("Terminal resized to {}x{}, sending resize request", rows, cols);
                        
                        // Send resize request to server
                        let request = Request::Resize {
                            id: config.agent_id.clone(),
                            rows,
                            cols,
                            clear_transcript: false,
                        };
                        let mut json = serde_json::to_string(&request)
                            .expect("Request serialization should never fail");
                        json.push('\n');
                        if let Err(e) = stream.write_all(json.as_bytes()).await {
                            warn!("Failed to send resize request: {e}");
                        }
                    }
                }
            }
            
            // Read from user's stdin
            result = stdin.read(&mut input_buf), if !config.readonly => {
                let n = result.map_err(AttachError::Io)?;
                if n == 0 {
                    // EOF on stdin - treat as detach
                    debug!("EOF on stdin, detaching");
                    send_detach(stream).await?;
                    return Ok(AttachEndReason::Detached);
                }

                // Process input for detach sequence
                let mut to_send = Vec::with_capacity(n);
                for &byte in &input_buf[..n] {
                    match detach_state {
                        DetachState::Normal => {
                            if byte == config.detach_prefix {
                                detach_state = DetachState::SawPrefix;
                            } else {
                                to_send.push(byte);
                            }
                        }
                        DetachState::SawPrefix => {
                            if byte == config.detach_key {
                                // Detach!
                                debug!("Detach sequence received");
                                send_detach(stream).await?;
                                return Ok(AttachEndReason::Detached);
                            } else if byte == config.detach_prefix {
                                // Double prefix = send one prefix
                                to_send.push(config.detach_prefix);
                                // Stay in SawPrefix state in case this is start of new sequence
                            } else {
                                // Not a detach - send the prefix and this byte
                                to_send.push(config.detach_prefix);
                                to_send.push(byte);
                                detach_state = DetachState::Normal;
                            }
                        }
                    }
                }

                // Send to server
                if !to_send.is_empty() {
                    send_data(stream, &to_send).await?;
                }
            }

            // Read from server (agent output)
            result = stream.read(&mut output_buf) => {
                let n = result.map_err(AttachError::Io)?;
                if n == 0 {
                    return Err(AttachError::ConnectionLost);
                }

                // Check if this is a JSON message (protocol message)
                if output_buf[0] == b'{' {
                    // Try to parse as a response
                    if let Ok(response) = serde_json::from_slice::<Response>(&output_buf[..n]) {
                        match response {
                            Response::AttachEnded { reason } => {
                                return Ok(reason);
                            }
                            Response::AgentExited { exit_code, .. } => {
                                return Ok(AttachEndReason::AgentExited { exit_code });
                            }
                            _ => {
                                warn!("Unexpected response during attach: {:?}", response);
                            }
                        }
                        continue;
                    }
                }

                // Regular data - write to stdout
                stdout.write_all(&output_buf[..n]).await.map_err(AttachError::Io)?;
                stdout.flush().await.map_err(AttachError::Io)?;
            }
        }
    }
}

/// Send data to the agent via the server.
async fn send_data(stream: &mut UnixStream, data: &[u8]) -> Result<(), AttachError> {
    // Simple protocol: just send raw bytes
    // The server knows we're in attach mode
    stream.write_all(data).await.map_err(AttachError::Io)?;
    Ok(())
}

/// Send detach signal to server.
async fn send_detach(stream: &mut UnixStream) -> Result<(), AttachError> {
    // Send an empty write or a special marker
    // For now, we'll just close our write side, which the server will detect
    stream.shutdown().await.map_err(AttachError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attach_config_new() {
        let config = AttachConfig::new("test-agent".to_string());
        assert_eq!(config.agent_id, "test-agent");
        assert_eq!(config.detach_prefix, 0x07); // Ctrl+G
        assert_eq!(config.detach_key, b'd');
        assert!(!config.readonly);
    }
}
