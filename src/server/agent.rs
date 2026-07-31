//! Agent representation.

use super::screen::Screen;
use super::transcript::Transcript;
use crate::protocol::{ExitReason, RecordedCommand, ResourceLimits};
use crate::pty::PtyProcess;
use crate::runtime::sync::Mutex;
use std::sync::Arc;
use std::time::Instant;

/// Internal agent state (different from `protocol::AgentState` for internal tracking).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Running,
    Exited { code: i32 },
}

/// An agent running in a PTY.
// The bool fields track independent runtime state flags (attached, sigterm_sent,
// no_resize, recording); collapsing them into an enum would not model the
// orthogonal states correctly.
#[allow(clippy::struct_excessive_bools)]
pub struct Agent {
    /// Unique agent ID (e.g., "rusty-nail").
    pub id: String,
    /// The command that was spawned.
    pub command: Vec<String>,
    /// Labels for grouping agents.
    pub labels: Vec<String>,
    /// The PTY process.
    pub pty: PtyProcess,
    /// Current state.
    pub state: AgentState,
    /// Why the agent exited (if exited).
    pub exit_reason: Option<ExitReason>,
    /// When the agent was started.
    pub started_at: Instant,
    /// Transcript buffer.
    pub transcript: Transcript,
    /// Virtual screen.
    pub screen: Screen,
    /// Whether a client is currently attached to this agent.
    /// When attached, the background `pty_reader_task` should skip this agent
    /// since the attach bridge handles I/O directly.
    pub attached: bool,
    /// Resource limits for this agent.
    pub limits: Option<ResourceLimits>,
    /// Whether SIGTERM has been sent (for timeout grace period).
    pub sigterm_sent: bool,
    /// When SIGTERM was sent (for tracking grace period).
    pub sigterm_sent_at: Option<Instant>,
    /// Last time the screen was cleared (for resize with `clear_transcript`).
    /// Used to avoid sending stale initial renders in attach.
    pub screen_cleared_at: Option<Instant>,
    /// Whether this agent is immune to auto-resize from view.
    pub no_resize: bool,
    /// Whether command recording is enabled for this agent.
    pub recording: bool,
    /// Recorded commands (populated when `recording` is true).
    pub recorded_commands: Vec<RecordedCommand>,
    /// Serializes writes to this agent's PTY.
    ///
    /// A `send` can take a while — a payload larger than the PTY input buffer
    /// needs several `write(2)` calls, and `--enter` deliberately pauses before
    /// the submit key. That work must happen with the manager lock released,
    /// or the PTY reader task cannot drain output and a child that blocks
    /// writing its echo will never read the rest of our payload. This lock
    /// keeps concurrent senders from interleaving bytes into each other's
    /// payloads once the manager lock is gone.
    pub write_lock: Arc<Mutex<()>>,
}

impl Agent {
    /// Create a new agent.
    // Constructor mirrors the agent's distinct configuration inputs; grouping
    // them into a params struct would only add indirection here.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        id: String,
        command: Vec<String>,
        labels: Vec<String>,
        limits: Option<ResourceLimits>,
        pty: PtyProcess,
        rows: u16,
        cols: u16,
        no_resize: bool,
        record: bool,
    ) -> Self {
        // Use max_output limit for transcript size, or default to 1MB
        let transcript_size = limits
            .and_then(|l| l.max_output)
            .map_or(1024 * 1024, |m| m as usize);

        Self {
            id,
            command,
            labels,
            pty,
            state: AgentState::Running,
            exit_reason: None,
            started_at: Instant::now(),
            transcript: Transcript::new(transcript_size),
            screen: Screen::new(rows, cols),
            attached: false,
            limits,
            sigterm_sent: false,
            sigterm_sent_at: None,
            screen_cleared_at: None,
            no_resize,
            recording: record,
            recorded_commands: Vec::new(),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Record a command if recording is enabled.
    pub fn record_command(&mut self, command: impl Into<String>, payload: impl Into<String>) {
        if self.recording {
            self.recorded_commands
                .push(RecordedCommand::new(command, payload));
        }
    }

    /// Check if the agent has exceeded its timeout.
    /// Uses millisecond precision to avoid early triggering.
    #[must_use]
    pub fn is_timed_out(&self) -> bool {
        if let Some(limits) = self.limits
            && let Some(timeout_secs) = limits.timeout
        {
            // Convert to millis for precision - timeout fires when elapsed >= timeout
            let timeout_millis = timeout_secs * 1000;
            return self.started_at.elapsed().as_millis() as u64 >= timeout_millis;
        }
        false
    }

    /// Check if SIGKILL should be sent (SIGTERM grace period expired).
    /// Grace period is 5 seconds after SIGTERM.
    #[must_use]
    pub fn should_sigkill(&self) -> bool {
        if let Some(sent_at) = self.sigterm_sent_at {
            // 5 second grace period in millis for precision
            return sent_at.elapsed().as_millis() >= 5000;
        }
        false
    }

    /// Check if the agent has all the specified labels.
    #[must_use]
    pub fn has_labels(&self, labels: &[String]) -> bool {
        labels.iter().all(|l| self.labels.contains(l))
    }

    /// Get the process ID.
    #[must_use]
    #[allow(clippy::cast_sign_loss)] // PIDs are always positive
    #[allow(clippy::missing_const_for_fn)] // as_raw() isn't const
    pub fn pid(&self) -> u32 {
        self.pty.pid.as_raw() as u32
    }

    /// Check if the agent is still running.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        matches!(self.state, AgentState::Running)
    }

    /// Get the exit code if the agent has exited.
    #[must_use]
    pub const fn exit_code(&self) -> Option<i32> {
        match self.state {
            AgentState::Exited { code } => Some(code),
            AgentState::Running => None,
        }
    }
}
