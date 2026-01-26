//! The botty server.
//!
//! Owns PTYs, agents, transcripts, and virtual screens.
//! Listens on a Unix socket for client requests.

// These casts are intentional and safe:
// - PIDs are always positive (i32 -> u32)  
// - Timestamps won't overflow u64 until year 584942417355
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_truncation)]
// This module has complex control flow that doesn't benefit from map_or_else
#![allow(clippy::option_if_let_else)]
// The handle_request function is large but logically coherent
#![allow(clippy::too_many_lines)]
// Dropping mutex guards explicitly adds noise without benefit
#![allow(clippy::significant_drop_tightening)]

mod agent;
mod manager;
mod screen;
mod transcript;

pub use agent::{Agent, AgentState as InternalAgentState};
pub use manager::AgentManager;
pub use screen::Screen;
pub use transcript::Transcript;

use crate::protocol::{
    AgentInfo, AgentState, AttachEndReason, DumpFormat, Event, Request, Response, TranscriptEntry,
};
use crate::pty;
use nix::sys::signal::Signal;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, error, info, warn};

/// Errors that can occur in the server.
#[derive(Debug, Error)]
pub enum ServerError {
    #[error("failed to bind socket: {0}")]
    Bind(#[source] std::io::Error),

    #[error("failed to accept connection: {0}")]
    Accept(#[source] std::io::Error),

    #[error("agent not found: {0}")]
    AgentNotFound(String),

    #[error("failed to spawn agent: {0}")]
    Spawn(#[source] crate::pty::PtyError),

    #[error("I/O error: {0}")]
    Io(#[source] std::io::Error),
}

/// The botty server.
pub struct Server {
    socket_path: PathBuf,
    manager: Arc<Mutex<AgentManager>>,
    shutdown_tx: broadcast::Sender<()>,
    /// Broadcast channel for events (spawned, output, exited).
    event_tx: broadcast::Sender<Event>,
}

impl Server {
    /// Create a new server that will listen on the given socket path.
    #[must_use] 
    pub fn new(socket_path: PathBuf) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        // Event channel with enough capacity for bursty output
        let (event_tx, _) = broadcast::channel(1024);
        Self {
            socket_path,
            manager: Arc::new(Mutex::new(AgentManager::new())),
            shutdown_tx,
            event_tx,
        }
    }

    /// Run the server event loop.
    pub async fn run(&mut self) -> Result<(), ServerError> {
        // Security: Check for symlink attack before removing existing socket
        if self.socket_path.exists() {
            // Don't follow symlinks - check if it's actually a symlink
            let metadata = std::fs::symlink_metadata(&self.socket_path)
                .map_err(ServerError::Io)?;
            
            if metadata.file_type().is_symlink() {
                return Err(ServerError::Bind(std::io::Error::other(
                    "socket path is a symlink - possible security attack",
                )));
            }
            
            // Only remove if it's a socket (or we can't tell)
            if metadata.file_type().is_socket() || metadata.file_type().is_file() {
                std::fs::remove_file(&self.socket_path).ok();
            }
        }

        // Ensure parent directory exists
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent).map_err(ServerError::Io)?;
        }

        let listener = UnixListener::bind(&self.socket_path).map_err(ServerError::Bind)?;
        
        // Security: Set socket permissions to owner-only (0o700)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&self.socket_path, perms).map_err(ServerError::Io)?;
        }
        
        info!("Server listening on {:?}", self.socket_path);

        // Start the PTY output reader task
        let manager = Arc::clone(&self.manager);
        let event_tx = self.event_tx.clone();
        let mut pty_shutdown = self.shutdown_tx.subscribe();
        tokio::spawn(async move {
            tokio::select! {
                () = pty_reader_task(manager, event_tx) => {}
                _ = pty_shutdown.recv() => {}
            }
        });

        let mut shutdown_rx = self.shutdown_tx.subscribe();

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _addr)) => {
                            debug!("Accepted connection");
                            let manager = Arc::clone(&self.manager);
                            let shutdown_tx = self.shutdown_tx.clone();
                            let event_tx = self.event_tx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, manager, shutdown_tx, event_tx).await {
                                    error!("Connection error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            error!("Accept error: {}", e);
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Shutdown signal received");
                    break;
                }
            }
        }

        // Clean up socket
        std::fs::remove_file(&self.socket_path).ok();
        info!("Server shut down");
        Ok(())
    }

    /// Request server shutdown.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

/// Handle a single client connection.
async fn handle_connection(
    stream: UnixStream,
    manager: Arc<Mutex<AgentManager>>,
    shutdown_tx: broadcast::Sender<()>,
    event_tx: broadcast::Sender<Event>,
) -> Result<(), ServerError> {
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = writer;
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(ServerError::Io)?;

        if n == 0 {
            // EOF - client disconnected
            debug!("Client disconnected");
            break;
        }

        let request: Request = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                let response = Response::error(format!("invalid request: {e}"));
                let mut json = serde_json::to_string(&response)
                    .expect("Response serialization should never fail");
                json.push('\n');
                writer.write_all(json.as_bytes()).await.ok();
                continue;
            }
        };

        debug!(?request, "Received request");

        // Handle attach request specially - it switches to streaming mode
        if let Request::Attach { id, readonly } = &request {
            let attach_result = handle_attach(
                id.clone(),
                *readonly,
                reader.into_inner(),
                writer,
                &manager,
            )
            .await;

            match attach_result {
                Ok(()) => {
                    debug!("Attach session ended normally");
                }
                Err(e) => {
                    warn!("Attach session error: {}", e);
                }
            }
            // After attach, the connection is done
            return Ok(());
        }

        // Handle events request specially - it switches to streaming mode
        if let Request::Events { filter, include_output } = &request {
            let events_result = handle_events(
                filter.clone(),
                *include_output,
                writer,
                &event_tx,
            )
            .await;

            match events_result {
                Ok(()) => {
                    debug!("Events stream ended normally");
                }
                Err(e) => {
                    warn!("Events stream error: {}", e);
                }
            }
            // After events, the connection is done
            return Ok(());
        }

        let is_shutdown = matches!(request, Request::Shutdown);
        let response = handle_request(request, &manager, &event_tx).await;

        let mut json = serde_json::to_string(&response)
            .expect("Response serialization should never fail");
        json.push('\n');
        writer
            .write_all(json.as_bytes())
            .await
            .map_err(ServerError::Io)?;

        // Trigger shutdown after sending response
        if is_shutdown {
            let _ = shutdown_tx.send(());
            break;
        }
    }

    Ok(())
}

/// Handle a single request.
async fn handle_request(
    request: Request,
    manager: &Arc<Mutex<AgentManager>>,
    event_tx: &broadcast::Sender<Event>,
) -> Response {
    match request {
        Request::Ping => Response::Pong,

        Request::Spawn { cmd, rows, cols, name, labels, env, env_clear } => {
            if cmd.is_empty() {
                return Response::error("command is empty");
            }

            // Parse environment variables
            let env_vars: Vec<(String, String)> = env
                .iter()
                .filter_map(|s| {
                    let mut parts = s.splitn(2, '=');
                    match (parts.next(), parts.next()) {
                        (Some(key), Some(value)) if !key.is_empty() => {
                            Some((key.to_string(), value.to_string()))
                        }
                        _ => None, // Skip malformed entries
                    }
                })
                .collect();

            // Validate and resolve agent ID
            let mut mgr = manager.lock().await;
            let id = if let Some(custom_name) = name {
                // Validate custom name
                if custom_name.is_empty() {
                    return Response::error("agent name cannot be empty");
                }
                // Check for uniqueness - allow reusing names of exited agents
                if let Some(existing) = mgr.get(&custom_name) {
                    if existing.is_running() {
                        return Response::error(format!("agent name already in use: {custom_name}"));
                    }
                    // Remove the exited agent to reuse the name
                    mgr.remove(&custom_name);
                }
                custom_name
            } else {
                mgr.generate_id()
            };
            drop(mgr); // Release lock before spawning

            let spawn_env = pty::SpawnEnv {
                vars: env_vars,
                clear: env_clear,
            };
            match pty::spawn_with_env(&cmd, rows, cols, &spawn_env) {
                Ok(pty_process) => {
                    let mut mgr = manager.lock().await;
                    // Double-check uniqueness (in case of race) - only block if running
                    if let Some(existing) = mgr.get(&id) {
                        if existing.is_running() {
                            return Response::error(format!("agent name already in use: {id}"));
                        }
                        mgr.remove(&id);
                    }
                    let pid = pty_process.pid.as_raw() as u32;
                    let agent = Agent::new(id.clone(), cmd.clone(), labels.clone(), pty_process, rows, cols);
                    mgr.add(agent);
                    info!(%id, %pid, ?labels, "Spawned agent");
                    
                    // Publish spawn event
                    let _ = event_tx.send(Event::AgentSpawned {
                        id: id.clone(),
                        pid,
                        command: cmd,
                        labels,
                    });
                    
                    Response::Spawned { id, pid }
                }
                Err(e) => Response::error(format!("spawn failed: {e}")),
            }
        }

        Request::List { labels } => {
            let mgr = manager.lock().await;
            let agents: Vec<AgentInfo> = mgr
                .list()
                .filter(|agent| labels.is_empty() || agent.has_labels(&labels))
                .map(|agent| {
                    let elapsed = agent.started_at.elapsed();
                    let now_millis = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let started_at = now_millis.saturating_sub(elapsed.as_millis() as u64);

                    AgentInfo {
                        id: agent.id.clone(),
                        pid: agent.pid(),
                        state: match agent.state {
                            InternalAgentState::Running => AgentState::Running,
                            InternalAgentState::Exited { .. } => AgentState::Exited,
                        },
                        command: agent.command.clone(),
                        labels: agent.labels.clone(),
                        size: agent.screen.size(),
                        started_at,
                        exit_code: agent.exit_code(),
                    }
                })
                .collect();
            Response::Agents { agents }
        }

        Request::Kill { id, labels, signal } => {
            // Validate signal number - only allow standard signals (1-31)
            // Real-time signals (32-64) and invalid numbers are rejected
            if !(1..=31).contains(&signal) {
                return Response::error(format!("invalid signal number: {signal} (must be 1-31)"));
            }
            
            let mgr = manager.lock().await;
            
            // Determine which agents to kill
            let targets: Vec<String> = if let Some(ref agent_id) = id {
                // Kill by specific ID
                vec![agent_id.clone()]
            } else if !labels.is_empty() {
                // Kill by labels
                mgr.list()
                    .filter(|a| a.is_running() && a.has_labels(&labels))
                    .map(|a| a.id.clone())
                    .collect()
            } else {
                return Response::error("must specify either agent ID or --label");
            };
            
            if targets.is_empty() {
                if id.is_some() {
                    return Response::error(format!("agent not found: {}", id.unwrap()));
                }
                return Response::error("no agents match the specified labels");
            }
            
            let sig = Signal::try_from(signal).unwrap_or(Signal::SIGTERM);
            let mut errors = Vec::new();
            let mut killed = 0;
            
            for target_id in targets {
                if let Some(agent) = mgr.get(&target_id) {
                    // Check if agent already exited
                    if !agent.is_running() {
                        info!(%target_id, "Agent already exited, nothing to kill");
                        continue;
                    }
                    match agent.pty.signal(sig) {
                        Ok(()) => {
                            info!(%target_id, ?sig, "Sent signal to agent");
                            killed += 1;
                        }
                        Err(e) => {
                            errors.push(format!("{target_id}: {e}"));
                        }
                    }
                }
            }
            
            if !errors.is_empty() {
                Response::error(format!("failed to kill some agents: {}", errors.join(", ")))
            } else if killed == 0 && id.is_some() {
                Response::error(format!("agent not found: {}", id.unwrap()))
            } else {
                Response::Ok
            }
        }

        Request::Send { id, data, newline } => {
            let mgr = manager.lock().await;
            if let Some(agent) = mgr.get(&id) {
                let mut bytes = data.into_bytes();
                if newline {
                    bytes.push(b'\n');
                }

                // Write to PTY master
                let fd = agent.pty.master_fd();
                // SAFETY: The fd is valid for the lifetime of the agent
                #[allow(unsafe_code)]
                let borrowed_fd = unsafe { BorrowedFd::borrow_raw(fd) };
                match nix::unistd::write(borrowed_fd, &bytes)
                {
                    Ok(_) => Response::Ok,
                    Err(e) => Response::error(format!("write failed: {e}")),
                }
            } else {
                Response::error(format!("agent not found: {id}"))
            }
        }

        Request::SendBytes { id, data } => {
            let mgr = manager.lock().await;
            if let Some(agent) = mgr.get(&id) {
                let fd = agent.pty.master_fd();
                // SAFETY: The fd is valid for the lifetime of the agent
                #[allow(unsafe_code)]
                let borrowed_fd = unsafe { BorrowedFd::borrow_raw(fd) };
                match nix::unistd::write(borrowed_fd, &data)
                {
                    Ok(_) => Response::Ok,
                    Err(e) => Response::error(format!("write failed: {e}")),
                }
            } else {
                Response::error(format!("agent not found: {id}"))
            }
        }

        Request::Tail {
            id,
            lines: _,
            follow: _,
        } => {
            let mgr = manager.lock().await;
            if let Some(agent) = mgr.get(&id) {
                // Return full transcript - client handles offset tracking
                let data = agent.transcript.all_bytes();
                Response::Output { data }
            } else {
                Response::error(format!("agent not found: {id}"))
            }
        }

        Request::Dump { id, since, format } => {
            let mgr = manager.lock().await;
            if let Some(agent) = mgr.get(&id) {
                let entries: Vec<TranscriptEntry> = if let Some(ts) = since {
                    agent
                        .transcript
                        .since(ts)
                        .into_iter()
                        .map(|e| TranscriptEntry {
                            timestamp: e.timestamp,
                            data: e.data.clone(),
                        })
                        .collect()
                } else {
                    agent
                        .transcript
                        .all()
                        .map(|e| TranscriptEntry {
                            timestamp: e.timestamp,
                            data: e.data.clone(),
                        })
                        .collect()
                };

                match format {
                    DumpFormat::Jsonl => Response::Transcript { entries },
                    DumpFormat::Text => {
                        let data: Vec<u8> = entries.iter().flat_map(|e| e.data.clone()).collect();
                        Response::Output { data }
                    }
                }
            } else {
                Response::error(format!("agent not found: {id}"))
            }
        }

        Request::Snapshot { id, strip_colors } => {
            let mgr = manager.lock().await;
            if let Some(agent) = mgr.get(&id) {
                let content = if strip_colors {
                    agent.screen.snapshot()
                } else {
                    agent.screen.contents_formatted()
                };
                let cursor = agent.screen.cursor_position();
                let size = agent.screen.size();
                Response::Snapshot {
                    content,
                    cursor,
                    size,
                }
            } else {
                Response::error(format!("agent not found: {id}"))
            }
        }

        Request::Attach { id, readonly: _ } => {
            // Attach is handled specially in handle_connection
            // If we get here, something went wrong
            let mgr = manager.lock().await;
            if mgr.get(&id).is_some() {
                Response::error("attach request should not reach handle_request")
            } else {
                Response::error(format!("agent not found: {id}"))
            }
        }

        Request::Events { .. } => {
            // Events is handled specially in handle_connection
            // If we get here, something went wrong
            Response::error("events request should not reach handle_request")
        }

        Request::Resize { id, rows, cols } => {
            let mut mgr = manager.lock().await;
            if let Some(agent) = mgr.get_mut(&id) {
                // Resize the PTY
                if let Err(e) = agent.pty.resize(rows, cols) {
                    return Response::error(format!("resize failed: {e}"));
                }
                // Update the screen model
                agent.screen.resize(rows, cols);
                info!(%id, %rows, %cols, "Resized agent");
                Response::Ok
            } else {
                Response::error(format!("agent not found: {id}"))
            }
        }

        Request::Shutdown => {
            info!("Shutdown requested");
            // TODO: Actually trigger shutdown
            Response::Ok
        }
    }
}

/// Handle attach mode - streaming I/O between client and agent PTY.
async fn handle_attach(
    agent_id: String,
    readonly: bool,
    mut reader: OwnedReadHalf,
    mut writer: OwnedWriteHalf,
    manager: &Arc<Mutex<AgentManager>>,
) -> Result<(), ServerError> {
    // Check if agent exists, get initial info, and mark as attached
    let size = {
        let mut mgr = manager.lock().await;
        if let Some(agent) = mgr.get_mut(&agent_id) {
            if !agent.is_running() {
                let response = Response::error(format!("agent {agent_id} has exited"));
                let mut json = serde_json::to_string(&response)
                    .expect("Response serialization should never fail");
                json.push('\n');
                writer.write_all(json.as_bytes()).await.ok();
                return Ok(());
            }
            // Mark agent as attached so pty_reader_task skips it
            agent.attached = true;
            agent.screen.size()
        } else {
            let response = Response::error(format!("agent not found: {agent_id}"));
            let mut json = serde_json::to_string(&response)
                .expect("Response serialization should never fail");
            json.push('\n');
            writer.write_all(json.as_bytes()).await.ok();
            return Ok(());
        }
    };

    // Send AttachStarted response
    let response = Response::AttachStarted {
        id: agent_id.clone(),
        size,
    };
    let mut json = serde_json::to_string(&response)
        .expect("Response serialization should never fail");
    json.push('\n');
    writer
        .write_all(json.as_bytes())
        .await
        .map_err(ServerError::Io)?;

    info!("Attach started for agent {agent_id}");

    // Run the I/O bridge
    let result = run_attach_bridge(
        &agent_id,
        readonly,
        &mut reader,
        &mut writer,
        manager,
    )
    .await;

    // Clear attached flag
    {
        let mut mgr = manager.lock().await;
        if let Some(agent) = mgr.get_mut(&agent_id) {
            agent.attached = false;
        }
    }

    // Send AttachEnded response
    let end_reason = match &result {
        Ok(reason) => reason.clone(),
        Err(e) => AttachEndReason::Error {
            message: e.to_string(),
        },
    };

    let response = Response::AttachEnded { reason: end_reason };
    let mut json = serde_json::to_string(&response)
        .expect("Response serialization should never fail");
    json.push('\n');
    writer.write_all(json.as_bytes()).await.ok();

    info!("Attach ended for agent {}", agent_id);

    result.map(|_| ())
}

/// Handle event streaming - subscribe to agent lifecycle events.
async fn handle_events(
    filter: Vec<String>,
    include_output: bool,
    mut writer: OwnedWriteHalf,
    event_tx: &broadcast::Sender<Event>,
) -> Result<(), ServerError> {
    let mut event_rx = event_tx.subscribe();
    
    info!(?filter, %include_output, "Events subscription started");

    loop {
        match event_rx.recv().await {
            Ok(event) => {
                // Filter by agent ID if specified
                let agent_id = match &event {
                    Event::AgentSpawned { id, .. }
                    | Event::AgentOutput { id, .. }
                    | Event::AgentExited { id, .. } => id,
                };

                // Skip if not in filter (unless filter is empty = all)
                if !filter.is_empty() && !filter.contains(agent_id) {
                    continue;
                }

                // Skip output events if not requested
                if !include_output && matches!(event, Event::AgentOutput { .. }) {
                    continue;
                }

                // Send event to client
                let response = Response::Event(event);
                let mut json = serde_json::to_string(&response)
                    .expect("Response serialization should never fail");
                json.push('\n');
                
                if writer.write_all(json.as_bytes()).await.is_err() {
                    // Client disconnected
                    debug!("Events client disconnected");
                    break;
                }
            }
            Err(broadcast::error::RecvError::Closed) => {
                // Channel closed (server shutting down)
                debug!("Events channel closed");
                break;
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                // We missed some events - log but continue
                warn!("Events subscriber lagged, missed {n} events");
            }
        }
    }

    info!("Events subscription ended");
    Ok(())
}

/// Run the attach mode I/O bridge.
///
/// Note on FD safety: We don't pass `pty_fd` as a parameter anymore. Instead, we
/// always get the fd from the agent while holding the manager lock. This ensures
/// the fd is valid because the Agent (and its `PtyProcess`) cannot be dropped while
/// we hold the lock.
async fn run_attach_bridge(
    agent_id: &str,
    readonly: bool,
    reader: &mut OwnedReadHalf,
    writer: &mut OwnedWriteHalf,
    manager: &Arc<Mutex<AgentManager>>,
) -> Result<AttachEndReason, ServerError> {
    let mut input_buf = [0u8; 4096];
    let mut output_buf = [0u8; 4096];

    // Create a ticker for polling the PTY
    let mut poll_interval = tokio::time::interval(Duration::from_millis(10));

    loop {
        tokio::select! {
            // Read input from client
            result = reader.read(&mut input_buf), if !readonly => {
                match result {
                    Ok(0) => {
                        // Client disconnected - treat as detach
                        debug!("Client disconnected during attach");
                        return Ok(AttachEndReason::Detached);
                    }
                    Ok(n) => {
                        // Get fd while holding lock to ensure it's valid
                        let mgr = manager.lock().await;
                        if let Some(agent) = mgr.get(agent_id) {
                            let pty_fd = agent.pty.master_fd();
                            // SAFETY: fd is valid because we hold the lock and agent exists
                            #[allow(unsafe_code)]
                            let borrowed_fd = unsafe { BorrowedFd::borrow_raw(pty_fd) };
                            if let Err(e) = nix::unistd::write(borrowed_fd, &input_buf[..n]) {
                                warn!("Failed to write to PTY: {e}");
                                return Ok(AttachEndReason::Error {
                                    message: format!("PTY write error: {e}"),
                                });
                            }
                        } else {
                            return Ok(AttachEndReason::Error {
                                message: "agent no longer exists".to_string(),
                            });
                        }
                    }
                    Err(e) => {
                        return Err(ServerError::Io(e));
                    }
                }
            }

            // Poll PTY for output
            _ = poll_interval.tick() => {
                // Hold lock while accessing agent and its fd
                let mut mgr = manager.lock().await;
                if let Some(agent) = mgr.get_mut(agent_id) {
                    // Check for exit
                    if let Ok(Some(code)) = agent.pty.try_wait() {
                        agent.state = InternalAgentState::Exited { code };
                        return Ok(AttachEndReason::AgentExited { exit_code: Some(code) });
                    }

                    if !agent.is_running() {
                        return Ok(AttachEndReason::AgentExited {
                            exit_code: agent.exit_code(),
                        });
                    }

                    // Read from PTY - fd is valid because we hold lock
                    let pty_fd = agent.pty.master_fd();
                    // SAFETY: fd is valid because we hold the lock and agent exists
                    #[allow(unsafe_code)]
                    let borrowed_fd = unsafe { BorrowedFd::borrow_raw(pty_fd) };
                    match nix::unistd::read(borrowed_fd, &mut output_buf) {
                        Ok(n) if n > 0 => {
                            let data = &output_buf[..n];
                            // Update transcript and screen
                            agent.transcript.append(data);
                            agent.screen.process(data);
                            // Send to client
                            drop(mgr); // Release lock before async write
                            writer.write_all(data).await.map_err(ServerError::Io)?;
                        }
                        // No data available (empty read or EAGAIN)
                        Ok(_) | Err(nix::Error::EAGAIN) => {}
                        Err(nix::Error::EIO) => {
                            // PTY closed - agent probably exited
                            if let Ok(Some(code)) = agent.pty.try_wait() {
                                agent.state = InternalAgentState::Exited { code };
                                return Ok(AttachEndReason::AgentExited { exit_code: Some(code) });
                            }
                        }
                        Err(e) => {
                            warn!("PTY read error: {e}");
                        }
                    }
                } else {
                    // Agent was removed
                    return Ok(AttachEndReason::Error {
                        message: "agent no longer exists".to_string(),
                    });
                }
            }
        }
    }
}

/// Background task that reads from PTY masters and updates transcripts/screens.
async fn pty_reader_task(manager: Arc<Mutex<AgentManager>>, event_tx: broadcast::Sender<Event>) {
    use tokio::time::{interval, Duration};

    let mut poll_interval = interval(Duration::from_millis(10));

    loop {
        poll_interval.tick().await;

        let mut mgr = manager.lock().await;
        let ids: Vec<String> = mgr.list().map(|a| a.id.clone()).collect();

        for id in ids {
            if let Some(agent) = mgr.get_mut(&id) {
                // Skip agents that aren't running or are currently attached
                // (attached agents have their I/O handled by run_attach_bridge)
                if !agent.is_running() || agent.attached {
                    continue;
                }

                // Try to read from the PTY master
                let fd = agent.pty.master_fd();
                let mut buf = [0u8; 4096];

                // SAFETY: The fd is valid for the lifetime of the agent
                #[allow(unsafe_code)]
                let borrowed_fd = unsafe { BorrowedFd::borrow_raw(fd) };
                
                // Non-blocking read
                match nix::unistd::read(borrowed_fd, &mut buf) {
                    Ok(n) if n > 0 => {
                        let data = &buf[..n];
                        agent.transcript.append(data);
                        agent.screen.process(data);
                        
                        // Publish output event
                        let _ = event_tx.send(Event::AgentOutput {
                            id: id.clone(),
                            data: data.to_vec(),
                        });
                    }
                    // No data available (empty read or EAGAIN/EWOULDBLOCK)
                    Ok(_) | Err(nix::Error::EAGAIN) => {}
                    Err(nix::Error::EIO) => {
                        // PTY closed - child probably exited
                        if let Ok(Some(code)) = agent.pty.try_wait() {
                            agent.state = InternalAgentState::Exited { code };
                            info!(%id, %code, "Agent exited");
                            
                            // Publish exit event
                            let _ = event_tx.send(Event::AgentExited {
                                id: id.clone(),
                                exit_code: Some(code),
                            });
                        }
                    }
                    Err(e) => {
                        warn!(%id, %e, "PTY read error");
                    }
                }

                // Check if child exited
                if agent.is_running()
                    && let Ok(Some(code)) = agent.pty.try_wait() {
                        agent.state = InternalAgentState::Exited { code };
                        info!(%id, %code, "Agent exited");
                        
                        // Publish exit event
                        let _ = event_tx.send(Event::AgentExited {
                            id: id.clone(),
                            exit_code: Some(code),
                        });
                    }
            }
        }
    }
}

/// Check if a server is running by trying to connect.
pub async fn is_server_running(socket_path: &Path) -> bool {
    UnixStream::connect(socket_path).await.is_ok()
}
