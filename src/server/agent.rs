//! Agent representation.

use super::screen::Screen;
use super::transcript::Transcript;
use crate::protocol::{ExitReason, ResourceLimits};
use crate::pty::PtyProcess;
use std::time::Instant;

/// Internal agent state (different from `protocol::AgentState` for internal tracking).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Running,
    Exited { code: i32 },
}

/// An agent running in a PTY.
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
}

impl Agent {
    /// Create a new agent.
    #[must_use]
    pub fn new(
        id: String,
        command: Vec<String>,
        labels: Vec<String>,
        limits: Option<ResourceLimits>,
        pty: PtyProcess,
        rows: u16,
        cols: u16,
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
        }
    }

    /// Check if the agent has exceeded its timeout.
    #[must_use]
    pub fn is_timed_out(&self) -> bool {
        if let Some(limits) = self.limits {
            if let Some(timeout_secs) = limits.timeout {
                return self.started_at.elapsed().as_secs() >= timeout_secs;
            }
        }
        false
    }

    /// Check if SIGKILL should be sent (SIGTERM grace period expired).
    /// Grace period is 5 seconds after SIGTERM.
    #[must_use]
    pub fn should_sigkill(&self) -> bool {
        if let Some(sent_at) = self.sigterm_sent_at {
            return sent_at.elapsed().as_secs() >= 5;
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
