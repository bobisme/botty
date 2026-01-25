//! Agent representation.

use super::screen::Screen;
use super::transcript::Transcript;
use crate::pty::PtyProcess;
use std::time::Instant;

/// Internal agent state (different from protocol::AgentState for internal tracking).
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
}

impl Agent {
    /// Create a new agent.
    pub fn new(id: String, command: Vec<String>, pty: PtyProcess, rows: u16, cols: u16) -> Self {
        Self {
            id,
            command,
            pty,
            state: AgentState::Running,
            started_at: Instant::now(),
            transcript: Transcript::new(1024 * 1024), // 1MB default
            screen: Screen::new(rows, cols),
        }
    }

    /// Get the process ID.
    pub fn pid(&self) -> u32 {
        self.pty.pid.as_raw() as u32
    }

    /// Check if the agent is still running.
    pub fn is_running(&self) -> bool {
        matches!(self.state, AgentState::Running)
    }

    /// Get the exit code if the agent has exited.
    pub fn exit_code(&self) -> Option<i32> {
        match self.state {
            AgentState::Exited { code } => Some(code),
            _ => None,
        }
    }
}
