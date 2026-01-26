//! Agent representation.

use super::screen::Screen;
use super::transcript::Transcript;
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
}

impl Agent {
    /// Create a new agent.
    #[must_use]
    pub fn new(
        id: String,
        command: Vec<String>,
        labels: Vec<String>,
        pty: PtyProcess,
        rows: u16,
        cols: u16,
    ) -> Self {
        Self {
            id,
            command,
            labels,
            pty,
            state: AgentState::Running,
            started_at: Instant::now(),
            transcript: Transcript::new(1024 * 1024), // 1MB default
            screen: Screen::new(rows, cols),
            attached: false,
        }
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
