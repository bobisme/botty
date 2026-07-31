//! The vessel server.
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
    AgentInfo, AgentState, AttachEndReason, DumpFormat, Event, ExitReason, Request, Response,
    TranscriptEntry,
};
use crate::pty;
use crate::runtime::io::{AsyncReadExt, AsyncWriteExt};
use crate::runtime::net::{OwnedReadHalf, OwnedWriteHalf, UnixStream};
use crate::runtime::sync::{Mutex, broadcast};
use nix::sys::signal::Signal;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tracing::{debug, error, info, instrument, warn};

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

    #[error("another server is already running on this socket")]
    AlreadyRunning,
}

/// The vessel server.
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
    #[instrument(skip(self), fields(socket = %self.socket_path.display()))]
    pub async fn run(&mut self) -> Result<(), ServerError> {
        // Security: Check for symlink attack before removing existing socket
        if self.socket_path.exists() {
            // Don't follow symlinks - check if it's actually a symlink
            let metadata = std::fs::symlink_metadata(&self.socket_path).map_err(ServerError::Io)?;

            if metadata.file_type().is_symlink() {
                return Err(ServerError::Bind(std::io::Error::other(
                    "socket path is a symlink - possible security attack",
                )));
            }

            // If it's a socket, check if another server is already running
            if metadata.file_type().is_socket() {
                if UnixStream::connect(&self.socket_path).await.is_ok() {
                    return Err(ServerError::AlreadyRunning);
                }
                // Socket exists but no server responding - stale, safe to remove
                std::fs::remove_file(&self.socket_path).ok();
            } else if metadata.file_type().is_file() {
                std::fs::remove_file(&self.socket_path).ok();
            }
        }

        // Ensure parent directory exists
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent).map_err(ServerError::Io)?;
        }

        // Security: bind under a restrictive umask so the socket inode is
        // created owner-only atomically. Without this, the file exists with
        // the process's default umask-derived mode (often 0o644/0o664) for a
        // brief window before the set_permissions call below — enough time
        // for a local user on a multi-user parent dir (e.g. the
        // /tmp/vessel-$UID.sock fallback) to connect() and drive the server.
        let listener = {
            #[cfg(unix)]
            let _umask_guard = UmaskGuard::new(0o177);
            crate::runtime::net::bind_unix_listener(&self.socket_path)
                .await
                .map_err(ServerError::Bind)?
        };

        // Belt-and-suspenders: ensure mode is 0o600 regardless of how the
        // runtime created the inode. The umask above should already make
        // this a no-op.
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
        crate::runtime::task::spawn(async move {
            crate::runtime::select! {
                () = pty_reader_task(manager, event_tx) => {}
                _ = pty_shutdown.recv() => {}
            }
        });

        let mut shutdown_rx = self.shutdown_tx.subscribe();

        // Set up OS signal handlers so the server shuts down gracefully
        // instead of dying instantly (which orphans/kills all agents).
        let mut sigterm =
            crate::runtime::signal::signal(crate::runtime::signal::SignalKind::terminate())
                .map_err(ServerError::Io)?;
        let mut sigint =
            crate::runtime::signal::signal(crate::runtime::signal::SignalKind::interrupt())
                .map_err(ServerError::Io)?;
        let mut sighup =
            crate::runtime::signal::signal(crate::runtime::signal::SignalKind::hangup())
                .map_err(ServerError::Io)?;

        loop {
            crate::runtime::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _addr)) => {
                            debug!("Accepted connection");
                            let manager = Arc::clone(&self.manager);
                            let shutdown_tx = self.shutdown_tx.clone();
                            let event_tx = self.event_tx.clone();
                            crate::runtime::task::spawn(async move {
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
                    info!("Shutdown signal received (internal)");
                    break;
                }
                _ = sigterm.recv() => {
                    // SIGTERM: only shut down if no agents are running.
                    // Exiting would close master PTY fds and kill all agents.
                    let mgr = self.manager.lock().await;
                    let running = mgr.list().filter(|a| a.is_running()).count();
                    drop(mgr);
                    if running > 0 {
                        warn!("SIGTERM received but {} agents still running — ignoring \
                               (use `vessel shutdown` to force)", running);
                    } else {
                        info!("SIGTERM received with no running agents, shutting down");
                        break;
                    }
                }
                _ = sigint.recv() => {
                    let mgr = self.manager.lock().await;
                    let running = mgr.list().filter(|a| a.is_running()).count();
                    drop(mgr);
                    if running > 0 {
                        warn!("SIGINT received but {} agents still running — ignoring \
                               (use `vessel shutdown` to force)", running);
                    } else {
                        info!("SIGINT received with no running agents, shutting down");
                        break;
                    }
                }
                _ = sighup.recv() => {
                    // SIGHUP: parent terminal closed. Keep running if agents are alive.
                    let mgr = self.manager.lock().await;
                    let running = mgr.list().filter(|a| a.is_running()).count();
                    drop(mgr);
                    if running > 0 {
                        info!("SIGHUP received but {} agents still running, ignoring", running);
                    } else {
                        info!("SIGHUP received with no running agents, shutting down");
                        break;
                    }
                }
            }
        }

        // Gracefully shut down running agents: SIGTERM → wait → SIGKILL
        {
            let mgr = self.manager.lock().await;
            let running: Vec<String> = mgr
                .list()
                .filter(|a| a.is_running())
                .map(|a| a.id.clone())
                .collect();

            if !running.is_empty() {
                info!("Sending SIGTERM to {} running agent(s)", running.len());
                for id in &running {
                    if let Some(agent) = mgr.get(id) {
                        let _ = agent.pty.signal(Signal::SIGTERM);
                    }
                }
                drop(mgr);

                // Wait up to 5 seconds for agents to exit
                let deadline = crate::runtime::time::Instant::now() + Duration::from_secs(5);
                loop {
                    crate::runtime::time::sleep(Duration::from_millis(100)).await;
                    let mgr = self.manager.lock().await;
                    let still_running = running
                        .iter()
                        .filter(|id| mgr.get(id).is_some_and(agent::Agent::is_running))
                        .count();
                    drop(mgr);

                    if still_running == 0 {
                        info!("All agents exited gracefully");
                        break;
                    }
                    if crate::runtime::time::Instant::now() >= deadline {
                        warn!(
                            "{} agent(s) did not exit in time, sending SIGKILL",
                            still_running
                        );
                        let mgr = self.manager.lock().await;
                        for id in &running {
                            if let Some(agent) = mgr.get(id)
                                && agent.is_running()
                            {
                                let _ = agent.pty.signal(Signal::SIGKILL);
                            }
                        }
                        break;
                    }
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

/// Maximum size, in bytes, of a single newline-delimited IPC request frame.
///
/// The control socket is owner-only, but a less-trusted same-user process or a
/// spawned agent can still connect and stream bytes. Capping the frame size
/// bounds server memory so such a client cannot exhaust it by sending an
/// endless line or an oversized `SendBytes` payload (CWE-400). 1 MiB matches
/// the default transcript cap and is ~1000x the 1 KiB chunks `attach` forwards,
/// so legitimate requests are unaffected.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Maximum time spent flushing one PTY write before reporting failure.
///
/// Only reached when the child has stopped draining its stdin entirely; a
/// child that is merely slow makes progress on each retry and finishes well
/// inside this budget.
const PTY_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Backoff between retries while the PTY input buffer is full.
const PTY_WRITE_RETRY: Duration = Duration::from_millis(1);

/// Wrap `text` in bracketed-paste markers.
///
/// Any `ESC [ 201 ~` already inside `text` is dropped rather than forwarded.
/// It would otherwise close the bracket early, and everything after it would
/// arrive as ordinary keystrokes — so a prompt that merely quotes the sequence
/// (a transcript of a terminal session, say) could submit itself partway
/// through, or run whatever followed as commands. Terminals filter the
/// terminator out of pastes for the same reason.
fn wrap_bracketed_paste(text: &str) -> Vec<u8> {
    use crate::protocol::{PASTE_END, PASTE_START};

    let mut out = Vec::with_capacity(text.len() + PASTE_START.len() + PASTE_END.len());
    out.extend_from_slice(PASTE_START);

    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(PASTE_END) {
            i += PASTE_END.len();
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }

    out.extend_from_slice(PASTE_END);
    out
}

/// Duplicate an agent's PTY master fd for use after the manager lock is gone.
///
/// The `dup(2)` matters for lifetime, not just convenience: once the lock is
/// released the agent can be killed and reaped, closing the original fd.
/// Holding an independent descriptor means a late write fails with `EBADF` or
/// `EIO` instead of landing on whatever unrelated file inherited the recycled
/// descriptor number.
fn dup_pty_fd(agent: &Agent) -> Result<std::os::fd::OwnedFd, String> {
    nix::unistd::dup(crate::sys::borrow_fd(agent.pty.master_fd()))
        .map_err(|e| format!("failed to duplicate PTY descriptor: {e}"))
}

/// Write every byte of `buf` to a PTY master.
///
/// The master fd is non-blocking (see [`pty::spawn`]), so one `write(2)` may
/// accept fewer bytes than requested — a PTY's input buffer is only a few KiB,
/// smaller than a pasted prompt — or fail with `EAGAIN` when the child has not
/// drained it yet. Treating either as success silently truncates the payload,
/// so retry until the whole buffer is accepted.
///
/// Must be called with the manager lock released: draining the child's output
/// requires that lock, and a child blocked writing its echo will not read the
/// rest of our payload until it drains.
async fn write_all_pty(fd: &std::os::fd::OwnedFd, buf: &[u8]) -> Result<(), String> {
    let deadline = Instant::now() + PTY_WRITE_TIMEOUT;
    let mut written = 0;

    while written < buf.len() {
        match nix::unistd::write(fd, &buf[written..]) {
            Ok(0) => {
                return Err(format!(
                    "write failed: PTY accepted 0 of {} remaining bytes",
                    buf.len() - written
                ));
            }
            Ok(n) => written += n,
            Err(nix::errno::Errno::EAGAIN | nix::errno::Errno::EINTR) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "write timed out after {}s: PTY accepted {written} of {} bytes",
                        PTY_WRITE_TIMEOUT.as_secs(),
                        buf.len()
                    ));
                }
                crate::runtime::time::sleep(PTY_WRITE_RETRY).await;
            }
            Err(e) => return Err(format!("write failed: {e}")),
        }
    }

    Ok(())
}

/// Error from [`FrameReader::next_frame`].
enum FrameError {
    Io(std::io::Error),
    /// A frame exceeded [`MAX_FRAME_BYTES`] without a terminating newline.
    TooLarge,
}

/// Reads newline-delimited request frames from a client with a hard per-frame
/// size cap.
///
/// Unlike `read_line`, this never accumulates an unbounded buffer: once the
/// in-progress frame grows past [`MAX_FRAME_BYTES`] without a newline, the
/// caller is told to reject the request and close the connection, so untrusted
/// socket input is bounded before it is parsed.
struct FrameReader {
    reader: OwnedReadHalf,
    /// Bytes read from the socket but not yet returned as a complete frame.
    buf: Vec<u8>,
}

impl FrameReader {
    const fn new(reader: OwnedReadHalf) -> Self {
        Self {
            reader,
            buf: Vec::new(),
        }
    }

    /// Returns the next frame (newline stripped), `Ok(None)` at EOF, or
    /// [`FrameError::TooLarge`] if a frame exceeds the size cap.
    async fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        /// Bytes read from the socket per iteration. Kept on the heap (inside
        /// `buf`) rather than on the async stack to keep the future small.
        const CHUNK: usize = 16 * 1024;
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let mut frame: Vec<u8> = self.buf.drain(..=pos).collect();
                frame.pop(); // strip the trailing '\n'
                return Ok(Some(frame));
            }

            // No complete frame yet. Reject before reading further if the
            // partial frame is already over the cap.
            if self.buf.len() > MAX_FRAME_BYTES {
                return Err(FrameError::TooLarge);
            }

            // Read straight into the heap buffer: reserve a chunk, fill it, and
            // truncate to what was actually read. Avoids a large stack array.
            let start = self.buf.len();
            self.buf.resize(start + CHUNK, 0);
            let n = self
                .reader
                .read(&mut self.buf[start..])
                .await
                .map_err(FrameError::Io)?;
            self.buf.truncate(start + n);
            if n == 0 {
                // EOF: surface any trailing newline-less bytes as a final frame.
                return if self.buf.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(std::mem::take(&mut self.buf)))
                };
            }
        }
    }

    /// Consume the reader, returning the underlying read half for the attach
    /// streaming handoff. Any buffered-but-unconsumed bytes are discarded,
    /// matching the previous `BufReader::into_inner` behaviour (the attach
    /// handshake does not pipeline input ahead of the `Attach` frame).
    fn into_inner(self) -> OwnedReadHalf {
        self.reader
    }
}

/// Handle a single client connection.
#[instrument(skip_all)]
async fn handle_connection(
    stream: UnixStream,
    manager: Arc<Mutex<AgentManager>>,
    shutdown_tx: broadcast::Sender<()>,
    event_tx: broadcast::Sender<Event>,
) -> Result<(), ServerError> {
    let (reader, writer) = stream.into_split();
    let mut reader = FrameReader::new(reader);
    let mut writer = writer;

    loop {
        let frame = match reader.next_frame().await {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                // EOF - client disconnected
                debug!("Client disconnected");
                break;
            }
            Err(FrameError::Io(e)) => return Err(ServerError::Io(e)),
            Err(FrameError::TooLarge) => {
                // Bounded reject: tell the client and drop the connection
                // rather than retain an oversized buffer.
                let response = Response::error(format!(
                    "request frame exceeds maximum size of {MAX_FRAME_BYTES} bytes"
                ));
                let mut json = serde_json::to_string(&response)
                    .expect("Response serialization should never fail");
                json.push('\n');
                writer.write_all(json.as_bytes()).await.ok();
                debug!("Closing connection: request frame exceeded size limit");
                break;
            }
        };

        let request: Request = match serde_json::from_slice(&frame) {
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
                &event_tx,
            )
            .await;

            match attach_result {
                Ok(()) => {
                    debug!("Attach session ended normally");
                }
                Err(e) => {
                    // Broken pipe is expected when tmux session is killed (e.g., view --new-session)
                    // Don't warn about it - just log at debug level
                    if let ServerError::Io(ref io_err) = e {
                        if io_err.kind() == std::io::ErrorKind::BrokenPipe {
                            debug!(
                                "Attach session ended: broken pipe (expected when tmux kills pane)"
                            );
                        } else {
                            warn!("Attach session error: {}", e);
                        }
                    } else {
                        warn!("Attach session error: {}", e);
                    }
                }
            }
            // After attach, the connection is done
            return Ok(());
        }

        // Handle events request specially - it switches to streaming mode
        if let Request::Events {
            filter,
            include_output,
        } = &request
        {
            let events_result =
                handle_events(filter.clone(), *include_output, writer, &event_tx).await;

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

        let mut json =
            serde_json::to_string(&response).expect("Response serialization should never fail");
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
#[instrument(skip_all)]
async fn handle_request(
    request: Request,
    manager: &Arc<Mutex<AgentManager>>,
    event_tx: &broadcast::Sender<Event>,
) -> Response {
    match request {
        Request::Ping => Response::Pong,

        Request::Spawn {
            cmd,
            rows,
            cols,
            name,
            labels,
            timeout,
            max_output,
            env,
            cwd,
            no_resize,
            record,
            memory_limit,
        } => {
            if cmd.is_empty() {
                return Response::error("command is empty");
            }

            // Parse environment variables
            let mut env_vars: Vec<(String, String)> = env
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

            // Auto-inject TRACEPARENT for distributed tracing if not already set.
            // This propagates the current trace context to spawned agent processes.
            if !env_vars.iter().any(|(k, _)| k == "TRACEPARENT")
                && let Some(tp) = crate::telemetry::current_traceparent()
            {
                env_vars.push(("TRACEPARENT".to_string(), tp));
            }

            // Build resource limits if any are specified
            let limits = if timeout.is_some() || max_output.is_some() {
                Some(crate::protocol::ResourceLimits {
                    timeout,
                    max_output,
                })
            } else {
                None
            };

            // Wrap command in systemd-run to isolate each agent in its own
            // cgroup scope. This prevents pane/terminal death from killing
            // agents and provides per-agent resource isolation.
            // The agent ID is used below to derive the unit name, so we need
            // to resolve it first — but we also need the lock for that. We'll
            // build effective_cmd after resolving the ID (see below).
            let wrap_memory_limit = memory_limit.clone();

            // Validate and resolve agent ID
            // Hold the lock across the entire check+spawn+add to prevent races.
            // PTY spawn (fork+exec) is fast so this won't block other requests long.
            let mut mgr = manager.lock().await;
            let id = if let Some(custom_name) = name {
                // Validate custom name - must be non-empty and shell-safe
                // Only allow alphanumeric, hyphen, and underscore to prevent command injection
                if custom_name.is_empty() {
                    return Response::error("agent name cannot be empty");
                }
                if !custom_name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '/')
                {
                    return Response::error(
                        "agent name must contain only alphanumeric characters, hyphens, underscores, and slashes",
                    );
                }
                if custom_name.starts_with('/')
                    || custom_name.ends_with('/')
                    || custom_name.contains("//")
                {
                    return Response::error(
                        "agent name must not start/end with '/' or contain '//'",
                    );
                }
                if custom_name.len() > 64 {
                    return Response::error("agent name must be 64 characters or fewer");
                }
                // Check for uniqueness - only allow reusing names of exited agents
                if let Some(existing) = mgr.get(&custom_name) {
                    if existing.is_running() {
                        return Response::error(format!(
                            "agent name already in use: {custom_name}"
                        ));
                    }
                    // Remove the exited agent to reuse the name
                    mgr.remove(&custom_name);
                }
                custom_name
            } else {
                mgr.generate_id()
            };

            // Build the effective command — wrap in systemd-run for cgroup isolation
            let effective_cmd = if crate::has_systemd_run() {
                // Sanitize agent ID for systemd unit name: replace / with -
                let unit_id = id.replace('/', "-");
                let mut wrapped = vec![
                    "systemd-run".to_string(),
                    "--user".to_string(),
                    "--scope".to_string(),
                    "--collect".to_string(),
                    format!("--unit=vessel-agent-{unit_id}"),
                ];
                if let Some(ref limit) = wrap_memory_limit {
                    wrapped.extend([
                        "-p".to_string(),
                        format!("MemoryMax={limit}"),
                        "-p".to_string(),
                        "MemorySwapMax=0".to_string(),
                    ]);
                    info!(%limit, %id, "Wrapping spawn with systemd-run scope + memory limit");
                } else {
                    info!(%id, "Wrapping spawn with systemd-run scope");
                }
                wrapped.push("--".to_string());
                wrapped.extend(cmd.iter().cloned());
                wrapped
            } else {
                if wrap_memory_limit.is_some() {
                    warn!(
                        "--memory-limit requested but systemd-run not available; spawning without cgroup limits"
                    );
                }
                cmd.clone()
            };

            let spawn_env = pty::SpawnEnv { vars: env_vars };
            match pty::spawn_with_env(&effective_cmd, rows, cols, &spawn_env, cwd.as_deref()) {
                Ok(pty_process) => {
                    let pid = pty_process.pid.as_raw() as u32;
                    let agent = Agent::new(
                        id.clone(),
                        cmd.clone(),
                        labels.clone(),
                        limits,
                        pty_process,
                        rows,
                        cols,
                        no_resize,
                        record,
                    );
                    mgr.add(agent);
                    info!(%id, %pid, ?labels, ?limits, "Spawned agent");

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

                    let rss_bytes = if agent.is_running() {
                        get_process_tree_rss(agent.pid())
                    } else {
                        None
                    };

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
                        exit_reason: agent.exit_reason,
                        limits: agent.limits,
                        no_resize: agent.no_resize,
                        rss_bytes,
                    }
                })
                .collect();
            Response::Agents { agents }
        }

        Request::Kill {
            id,
            labels,
            all,
            signal,
            proc_filter,
        } => {
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
            } else if all {
                // Kill all running agents
                mgr.list()
                    .filter(|a| a.is_running())
                    .map(|a| a.id.clone())
                    .collect()
            } else if proc_filter.is_some() || !labels.is_empty() {
                // Kill by proc filter and/or labels (AND logic when both specified)
                mgr.list()
                    .filter(|a| {
                        if !a.is_running() {
                            return false;
                        }
                        if !labels.is_empty() && !a.has_labels(&labels) {
                            return false;
                        }
                        if let Some(ref pf) = proc_filter
                            && !a.command.join(" ").contains(pf.as_str())
                        {
                            return false;
                        }
                        true
                    })
                    .map(|a| a.id.clone())
                    .collect()
            } else {
                return Response::error("must specify agent ID, --label, --proc, or --all");
            };

            if targets.is_empty() {
                if let Some(id) = &id {
                    return Response::error(format!("agent not found: {id}"));
                }
                if all {
                    return Response::error("no running agents to kill");
                }
                if proc_filter.is_some() && !labels.is_empty() {
                    return Response::error(
                        "no agents match the specified process filter and labels",
                    );
                }
                if proc_filter.is_some() {
                    return Response::error("no agents match the specified process filter");
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
            } else if let (0, Some(id)) = (killed, &id) {
                Response::error(format!("agent not found: {id}"))
            } else {
                Response::Ok
            }
        }

        Request::Send {
            id,
            data,
            newline,
            enter,
            submit_delay_ms,
            paste,
        } => {
            // The submit key goes out in its own write(2), after a pause, so
            // the TUI sees a keypress rather than a newline buried in a pasted
            // burst. See DEFAULT_SUBMIT_DELAY_MS.
            let mut submit_key = Vec::new();
            if newline {
                submit_key.push(b'\n');
            }
            if enter {
                submit_key.push(b'\r');
            }

            let body = if paste {
                wrap_bracketed_paste(&data)
            } else {
                data.clone().into_bytes()
            };

            // Take the fd and the per-agent write lock, then release the
            // manager lock before writing: the writes below can block on a
            // full PTY buffer, and draining that buffer needs this same lock.
            let (fd, write_lock) = {
                let mut mgr = manager.lock().await;
                let Some(agent) = mgr.get_mut(&id) else {
                    return Response::error(format!("agent not found: {id}"));
                };

                let payload = if submit_key.is_empty() {
                    data.clone()
                } else {
                    format!("{data}\n")
                };
                agent.record_command("send", &payload);

                match dup_pty_fd(agent) {
                    Ok(fd) => (fd, Arc::clone(&agent.write_lock)),
                    Err(e) => return Response::error(e),
                }
            };

            let _write_guard = write_lock.lock().await;

            if let Err(e) = write_all_pty(&fd, &body).await {
                return Response::error(e);
            }

            if !submit_key.is_empty() {
                let delay = submit_delay_ms.unwrap_or(crate::protocol::DEFAULT_SUBMIT_DELAY_MS);
                if delay > 0 {
                    crate::runtime::time::sleep(Duration::from_millis(delay)).await;
                }
                if let Err(e) = write_all_pty(&fd, &submit_key).await {
                    return Response::error(e);
                }
            }

            Response::Ok
        }

        Request::SendBytes { id, data } => {
            let (fd, write_lock) = {
                let mut mgr = manager.lock().await;
                let Some(agent) = mgr.get_mut(&id) else {
                    return Response::error(format!("agent not found: {id}"));
                };
                agent.record_command("send_bytes", hex::encode(&data));

                match dup_pty_fd(agent) {
                    Ok(fd) => (fd, Arc::clone(&agent.write_lock)),
                    Err(e) => return Response::error(e),
                }
            };

            let _write_guard = write_lock.lock().await;
            match write_all_pty(&fd, &data).await {
                Ok(()) => Response::Ok,
                Err(e) => Response::error(e),
            }
        }

        Request::Tail {
            id,
            lines,
            follow: _,
        } => {
            let mgr = manager.lock().await;
            if let Some(agent) = mgr.get(&id) {
                let data = agent.transcript.tail_lines(lines);
                let exited = !agent.is_running();
                Response::Output { data, exited }
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
                        let exited = !agent.is_running();
                        Response::Output { data, exited }
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

        Request::Resize {
            id,
            rows,
            cols,
            clear_transcript,
        } => {
            // Validate dimensions to prevent crashes or resource exhaustion
            const MIN_SIZE: u16 = 1;
            const MAX_SIZE: u16 = 500;
            if !(MIN_SIZE..=MAX_SIZE).contains(&rows) || !(MIN_SIZE..=MAX_SIZE).contains(&cols) {
                return Response::error(format!(
                    "invalid dimensions: {cols}x{rows} (must be {MIN_SIZE}-{MAX_SIZE})"
                ));
            }

            let mut mgr = manager.lock().await;
            if let Some(agent) = mgr.get_mut(&id) {
                // Resize the PTY
                if let Err(e) = agent.pty.resize(rows, cols) {
                    return Response::error(format!("resize failed: {e}"));
                }
                // Update the screen model
                agent.screen.resize(rows, cols);
                // Optionally clear transcript (useful for view mode to avoid
                // displaying output rendered at old size)
                if clear_transcript {
                    agent.transcript.clear();
                    // Mark screen as recently cleared to avoid sending stale initial render in attach
                    agent.screen_cleared_at = Some(std::time::Instant::now());
                    // Send SIGWINCH to force child process to redraw its UI
                    // This is critical for TUI programs like htop that need to redraw after transcript clear
                    if let Err(e) = agent.pty.signal(nix::sys::signal::Signal::SIGWINCH) {
                        warn!(%id, "Failed to send SIGWINCH after transcript clear: {e}");
                    }
                    info!(%id, %rows, %cols, "Resized agent and cleared transcript");
                } else {
                    info!(%id, %rows, %cols, "Resized agent");
                }
                Response::Ok
            } else {
                Response::error(format!("agent not found: {id}"))
            }
        }

        Request::GetRecording { id } => {
            let mgr = manager.lock().await;
            if let Some(agent) = mgr.get(&id) {
                if agent.recording {
                    Response::Recording {
                        agent_id: id,
                        commands: agent.recorded_commands.clone(),
                    }
                } else {
                    Response::error(format!("recording not enabled for agent: {id}"))
                }
            } else {
                Response::error(format!("agent not found: {id}"))
            }
        }

        Request::GetEnv { id } => {
            let mgr = manager.lock().await;
            if let Some(agent) = mgr.get(&id) {
                if agent.is_running() {
                    let pid = agent.pid();
                    drop(mgr); // Release lock before I/O
                    match read_proc_environ(pid) {
                        Ok(env) => Response::AgentEnv { id, env },
                        Err(e) => {
                            Response::error(format!("failed to read environment for {id}: {e}"))
                        }
                    }
                } else {
                    Response::error(format!(
                        "agent {id} has exited — environment no longer available"
                    ))
                }
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
#[instrument(skip(reader, writer, manager, event_tx))]
async fn handle_attach(
    agent_id: String,
    readonly: bool,
    mut reader: OwnedReadHalf,
    mut writer: OwnedWriteHalf,
    manager: &Arc<Mutex<AgentManager>>,
    event_tx: &broadcast::Sender<Event>,
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
            let mut json =
                serde_json::to_string(&response).expect("Response serialization should never fail");
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
    let mut json =
        serde_json::to_string(&response).expect("Response serialization should never fail");
    json.push('\n');
    writer
        .write_all(json.as_bytes())
        .await
        .map_err(ServerError::Io)?;

    info!("Attach started for agent {agent_id}");

    // Send initial screen render so the client starts with correct display state
    // This is critical for TUI programs that use incremental updates.
    // However, skip sending if the screen was recently cleared (within 1s) to avoid
    // showing stale data while the child process redraws after SIGWINCH.
    {
        let mgr = manager.lock().await;
        if let Some(agent) = mgr.get(&agent_id) {
            let recently_cleared = agent
                .screen_cleared_at
                .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1));

            if recently_cleared {
                // Screen was just cleared, send a simple clear instead of stale content
                info!("Screen recently cleared, sending clear screen instead of stale render");
                drop(mgr);
                writer
                    .write_all(b"\x1b[2J\x1b[H") // Clear screen + cursor home
                    .await
                    .map_err(ServerError::Io)?;
                writer.flush().await.map_err(ServerError::Io)?;
            } else {
                // Normal case: send full screen render
                let initial_screen = agent.screen.render_full_screen();
                info!(
                    "Sending initial screen render: {} bytes",
                    initial_screen.len()
                );
                drop(mgr); // Release lock before async write
                writer
                    .write_all(&initial_screen)
                    .await
                    .map_err(ServerError::Io)?;
                writer.flush().await.map_err(ServerError::Io)?;
                info!("Initial screen render sent");
            }
        }
    }

    // Run the I/O bridge
    let result = run_attach_bridge(&agent_id, readonly, &mut reader, &mut writer, manager).await;

    // Clear attached flag and determine end reason
    let end_reason = {
        let mut mgr = manager.lock().await;
        if let Some(agent) = mgr.get_mut(&agent_id) {
            agent.attached = false;
        }

        match &result {
            Ok(reason) => reason.clone(),
            Err(e) => AttachEndReason::Error {
                message: e.to_string(),
            },
        }
    };
    // Lock released here before event broadcast

    // Publish exit event outside the lock to avoid holding it during broadcast
    // (pty_reader_task skips attached agents, so we must publish here)
    if let AttachEndReason::AgentExited { exit_code } = &end_reason {
        let _ = event_tx.send(Event::AgentExited {
            id: agent_id.clone(),
            exit_code: *exit_code,
        });
    }

    let response = Response::AttachEnded { reason: end_reason };
    let mut json =
        serde_json::to_string(&response).expect("Response serialization should never fail");
    json.push('\n');
    writer.write_all(json.as_bytes()).await.ok();

    info!("Attach ended for agent {}", agent_id);

    result.map(|_| ())
}

/// Handle event streaming - subscribe to agent lifecycle events.
#[instrument(skip(writer, event_tx))]
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
            #[cfg(feature = "runtime-asupersync")]
            Err(broadcast::error::RecvError::Cancelled) => {
                debug!("Events recv cancelled");
                break;
            }
            #[cfg(feature = "runtime-asupersync")]
            Err(broadcast::error::RecvError::PolledAfterCompletion) => {
                debug!("Events recv polled after completion");
                break;
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
    let mut poll_interval = crate::runtime::time::interval(Duration::from_millis(10));

    loop {
        crate::runtime::select! {
            // Read input from client (always read to prevent buffer deadlock,
            // but only forward to PTY in read-write mode)
            result = reader.read(&mut input_buf) => {
                match result {
                    Ok(0) => {
                        // Client disconnected - treat as detach
                        debug!("Client disconnected during attach");
                        return Ok(AttachEndReason::Detached);
                    }
                    Ok(n) => {
                        if !readonly {
                            // Get fd while holding lock to ensure it's valid
                            let mgr = manager.lock().await;
                            if let Some(agent) = mgr.get(agent_id) {
                                let pty_fd = agent.pty.master_fd();
                                let borrowed_fd = crate::sys::borrow_fd(pty_fd);
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
                        // In readonly mode, discard input (drain buffer to prevent deadlock)
                    }
                    Err(e) => {
                        return Err(ServerError::Io(e));
                    }
                }
            }

            // Poll PTY for output
            () = poll_interval.tick() => {
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
                    let borrowed_fd = crate::sys::borrow_fd(pty_fd);
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
    use crate::runtime::time::{Duration, interval};

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

                // Check for timeout
                if agent.is_timed_out() {
                    if !agent.sigterm_sent {
                        // First, send SIGTERM for graceful shutdown
                        info!(%id, "Agent timeout - sending SIGTERM");
                        let _ = agent.pty.signal(Signal::SIGTERM);
                        agent.sigterm_sent = true;
                        agent.sigterm_sent_at = Some(std::time::Instant::now());
                    } else if agent.should_sigkill() {
                        // Grace period expired, send SIGKILL
                        info!(%id, "Agent timeout grace period expired - sending SIGKILL");
                        let _ = agent.pty.signal(Signal::SIGKILL);
                    }
                }

                // Try to read from the PTY master
                let fd = agent.pty.master_fd();
                let mut buf = [0u8; 4096];

                let borrowed_fd = crate::sys::borrow_fd(fd);

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
                            // Determine exit reason based on exit code:
                            // - 128 + signal_num indicates killed by signal
                            // - SIGTERM (15) -> 143, SIGKILL (9) -> 137
                            agent.exit_reason =
                                Some(if agent.sigterm_sent && (code == 143 || code == 137) {
                                    // Process was killed by our timeout signals
                                    ExitReason::Timeout
                                } else {
                                    ExitReason::Normal
                                });
                            info!(%id, %code, exit_reason = ?agent.exit_reason, "Agent exited");

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
                    && let Ok(Some(code)) = agent.pty.try_wait()
                {
                    agent.state = InternalAgentState::Exited { code };
                    // Determine exit reason based on exit code:
                    // - 128 + signal_num indicates killed by signal
                    // - SIGTERM (15) -> 143, SIGKILL (9) -> 137
                    agent.exit_reason =
                        Some(if agent.sigterm_sent && (code == 143 || code == 137) {
                            // Process was killed by our timeout signals
                            ExitReason::Timeout
                        } else {
                            ExitReason::Normal
                        });
                    info!(%id, %code, exit_reason = ?agent.exit_reason, "Agent exited");

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

/// Read /proc/<pid>/environ and return parsed key-value pairs.
fn read_proc_environ(pid: u32) -> Result<Vec<(String, String)>, std::io::Error> {
    let path = format!("/proc/{pid}/environ");
    let data = std::fs::read(&path)?;
    let mut env = Vec::new();
    for entry in data.split(|&b| b == 0) {
        if entry.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(entry);
        if let Some((key, value)) = s.split_once('=') {
            env.push((key.to_string(), value.to_string()));
        }
    }
    env.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(env)
}

/// Get the total RSS (resident set size) in bytes for a process and all its descendants.
/// Walks /proc/<pid>/task/*/children recursively.
fn get_process_tree_rss(pid: u32) -> Option<u64> {
    let mut total_rss: u64 = 0;
    let mut stack = vec![pid];
    let page_size = crate::sys::page_size();

    while let Some(p) = stack.pop() {
        // Read RSS from /proc/<pid>/stat (field 24, 0-indexed 23)
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{p}/stat")) {
            // Fields after comm (which may contain spaces/parens) start after the last ')'
            if let Some(after_comm) = stat.rfind(')') {
                let fields: Vec<&str> = stat[after_comm + 2..].split_whitespace().collect();
                // RSS is field index 21 after the comm section (field 24 overall, minus pid/comm/state = index 21)
                if let Some(rss_pages) = fields.get(21).and_then(|s| s.parse::<u64>().ok()) {
                    total_rss += rss_pages * page_size;
                }
            }
        }
        // Find children via /proc/<pid>/task/*/children
        let task_path = format!("/proc/{p}/task");
        if let Ok(tasks) = std::fs::read_dir(&task_path) {
            for task in tasks.flatten() {
                let children_path = task.path().join("children");
                if let Ok(children) = std::fs::read_to_string(&children_path) {
                    for child_pid in children.split_whitespace() {
                        if let Ok(cpid) = child_pid.parse::<u32>() {
                            stack.push(cpid);
                        }
                    }
                }
            }
        }
    }

    if total_rss > 0 { Some(total_rss) } else { None }
}

/// Check if a server is running by trying to connect.
pub async fn is_server_running(socket_path: &Path) -> bool {
    UnixStream::connect(socket_path).await.is_ok()
}

/// RAII guard that sets a process-wide umask and restores the previous value
/// on drop. Used to bracket a single syscall that creates an inode (bind,
/// open, mkdir) so its mode is not subject to the ambient umask.
///
/// Note: `umask(2)` is process-global, not thread-local. Callers must ensure
/// no other thread in the process is creating files inside the guard's
/// lifetime. In this server it is used once during startup before any PTY
/// task is spawned.
#[cfg(unix)]
struct UmaskGuard(nix::sys::stat::Mode);

#[cfg(unix)]
impl UmaskGuard {
    fn new(mask: libc::mode_t) -> Self {
        let mode = nix::sys::stat::Mode::from_bits_truncate(mask);
        Self(nix::sys::stat::umask(mode))
    }
}

#[cfg(unix)]
impl Drop for UmaskGuard {
    fn drop(&mut self) {
        nix::sys::stat::umask(self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{PASTE_END, PASTE_START};

    #[test]
    fn wraps_text_in_paste_markers() {
        let out = wrap_bracketed_paste("line one\nline two");
        assert!(out.starts_with(PASTE_START));
        assert!(out.ends_with(PASTE_END));

        // The newline survives inside the envelope: that is the whole point,
        // it becomes a line in the composer rather than a submission.
        let inner = &out[PASTE_START.len()..out.len() - PASTE_END.len()];
        assert_eq!(inner, b"line one\nline two");
    }

    #[test]
    fn empty_text_still_produces_a_well_formed_envelope() {
        let out = wrap_bracketed_paste("");
        assert_eq!(out, [PASTE_START, PASTE_END].concat());
    }

    #[test]
    fn strips_embedded_paste_terminator() {
        // A prompt that quotes ESC[201~ must not be able to close the bracket
        // early and have its remainder delivered as live keystrokes.
        let text = "before\x1b[201~after";
        let out = wrap_bracketed_paste(text);

        let inner = &out[PASTE_START.len()..out.len() - PASTE_END.len()];
        assert_eq!(inner, b"beforeafter");

        // Exactly one terminator in the whole payload: the one we appended.
        let count = out
            .windows(PASTE_END.len())
            .filter(|w| *w == PASTE_END)
            .count();
        assert_eq!(count, 1, "payload must contain exactly one terminator");
    }

    #[test]
    fn strips_repeated_and_adjacent_terminators() {
        let out = wrap_bracketed_paste("a\x1b[201~\x1b[201~b");
        let inner = &out[PASTE_START.len()..out.len() - PASTE_END.len()];
        assert_eq!(inner, b"ab");
    }

    #[test]
    fn leaves_the_paste_introducer_alone() {
        // Only the terminator can break out of the envelope; a quoted
        // introducer is inert, so it is passed through unchanged.
        let out = wrap_bracketed_paste("a\x1b[200~b");
        let inner = &out[PASTE_START.len()..out.len() - PASTE_END.len()];
        assert_eq!(inner, b"a\x1b[200~b");
    }
}
