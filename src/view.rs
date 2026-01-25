//! Viewer integration for botty.
//!
//! Provides tmux-based viewing of agent output.

use std::collections::HashSet;
use std::process::Command;

/// Errors that can occur in the viewer.
#[derive(Debug, thiserror::Error)]
pub enum ViewError {
    #[error("tmux not found in PATH")]
    TmuxNotFound,

    #[error("tmux command failed: {0}")]
    TmuxFailed(String),

    #[error("unsupported multiplexer: {0}")]
    UnsupportedMux(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// tmux session manager for botty view.
pub struct TmuxView {
    session_name: String,
    /// Set of agent IDs with active panes
    active_panes: HashSet<String>,
    /// Path to botty binary (for spawning tail commands)
    botty_path: String,
}

impl TmuxView {
    /// Create a new tmux view manager.
    #[must_use]
    pub fn new(botty_path: String) -> Self {
        Self {
            session_name: "botty".to_string(),
            active_panes: HashSet::new(),
            botty_path,
        }
    }

    /// Check if tmux is available.
    pub fn check_tmux() -> Result<(), ViewError> {
        let output = Command::new("which").arg("tmux").output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(ViewError::TmuxNotFound)
        }
    }

    /// Check if our session already exists.
    #[must_use]
    pub fn session_exists(&self) -> bool {
        Command::new("tmux")
            .args(["has-session", "-t", &self.session_name])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Create a new tmux session (detached).
    /// Returns the window ID of the first window.
    pub fn create_session(&self) -> Result<(), ViewError> {
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &self.session_name,
                "-n",
                "agents",
            ])
            .status()?;

        if status.success() {
            Ok(())
        } else {
            Err(ViewError::TmuxFailed("failed to create session".into()))
        }
    }

    /// Create a pane for an agent.
    /// If this is the first pane, it reuses the existing window.
    /// Otherwise, it splits the window.
    pub fn add_pane(&mut self, agent_id: &str) -> Result<(), ViewError> {
        if self.active_panes.contains(agent_id) {
            // Already have a pane for this agent
            return Ok(());
        }

        let tail_cmd = format!("{} tail --replay {}", self.botty_path, agent_id);

        if self.active_panes.is_empty() {
            // First pane - respawn it with our command (replaces the shell)
            let status = Command::new("tmux")
                .args([
                    "respawn-pane",
                    "-t",
                    &format!("{}:agents", self.session_name),
                    "-k", // kill existing process
                    &tail_cmd,
                ])
                .status()?;

            if !status.success() {
                return Err(ViewError::TmuxFailed(
                    "failed to respawn first pane".into(),
                ));
            }

            // Rename the pane (set pane title)
            let _ = Command::new("tmux")
                .args([
                    "select-pane",
                    "-t",
                    &format!("{}:agents", self.session_name),
                    "-T",
                    agent_id,
                ])
                .status();
        } else {
            // Split window and run tail command
            let status = Command::new("tmux")
                .args([
                    "split-window",
                    "-t",
                    &format!("{}:agents", self.session_name),
                    "-h", // horizontal split
                    &tail_cmd,
                ])
                .status()?;

            if !status.success() {
                return Err(ViewError::TmuxFailed("failed to split window".into()));
            }

            // Set pane title
            let _ = Command::new("tmux")
                .args([
                    "select-pane",
                    "-t",
                    &format!("{}:agents", self.session_name),
                    "-T",
                    agent_id,
                ])
                .status();

            // Re-tile the layout
            self.retile()?;
        }

        self.active_panes.insert(agent_id.to_string());
        Ok(())
    }

    /// Remove a pane for an agent.
    pub fn remove_pane(&mut self, agent_id: &str) -> Result<(), ViewError> {
        if !self.active_panes.contains(agent_id) {
            return Ok(());
        }

        // Find and kill the pane with this agent ID
        // We use list-panes to find panes by title
        // Note: The format string uses tmux's #{var} syntax, not Rust's
        #[allow(clippy::literal_string_with_formatting_args)]
        let format_str = "#{pane_id}:#{pane_title}";
        
        let output = Command::new("tmux")
            .args([
                "list-panes",
                "-t",
                &format!("{}:agents", self.session_name),
                "-F",
                format_str,
            ])
            .output()?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if let Some((pane_id, title)) = line.split_once(':')
                    && title == agent_id
                {
                    // Kill this pane
                    let _ = Command::new("tmux")
                        .args(["kill-pane", "-t", pane_id])
                        .status();
                    break;
                }
            }
        }

        self.active_panes.remove(agent_id);

        // Re-tile if we still have panes
        if !self.active_panes.is_empty() {
            self.retile()?;
        }

        Ok(())
    }

    /// Re-tile all panes in the window.
    pub fn retile(&self) -> Result<(), ViewError> {
        let status = Command::new("tmux")
            .args([
                "select-layout",
                "-t",
                &format!("{}:agents", self.session_name),
                "tiled",
            ])
            .status()?;

        if status.success() {
            Ok(())
        } else {
            Err(ViewError::TmuxFailed("failed to retile".into()))
        }
    }

    /// Attach to the tmux session (blocking).
    pub fn attach(&self) -> Result<(), ViewError> {
        let status = Command::new("tmux")
            .args(["attach-session", "-t", &self.session_name])
            .status()?;

        if status.success() {
            Ok(())
        } else {
            Err(ViewError::TmuxFailed("failed to attach".into()))
        }
    }

    /// Kill the entire session.
    pub fn kill_session(&self) -> Result<(), ViewError> {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &self.session_name])
            .status();
        Ok(())
    }

    /// Get the number of active panes.
    #[must_use]
    pub fn pane_count(&self) -> usize {
        self.active_panes.len()
    }

    /// Check if we have any active panes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active_panes.is_empty()
    }
}
