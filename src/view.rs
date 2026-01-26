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

    #[error("unsupported view mode: {0} (use 'panes' or 'windows')")]
    UnsupportedMode(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// View layout mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// All agents in split panes within one window (default).
    #[default]
    Panes,
    /// Each agent gets its own tmux window (tab-style navigation).
    Windows,
}

impl ViewMode {
    /// Parse mode from string.
    pub fn from_str(s: &str) -> Result<Self, ViewError> {
        match s.to_lowercase().as_str() {
            "panes" | "pane" => Ok(Self::Panes),
            "windows" | "window" | "tabs" | "tab" => Ok(Self::Windows),
            _ => Err(ViewError::UnsupportedMode(s.to_string())),
        }
    }
}

/// tmux session manager for botty view.
pub struct TmuxView {
    session_name: String,
    /// Set of agent IDs with active panes/windows
    active_panes: HashSet<String>,
    /// Path to botty binary (for spawning tail commands)
    botty_path: String,
    /// Layout mode (panes vs windows)
    mode: ViewMode,
}

impl TmuxView {
    /// Create a new tmux view manager.
    #[must_use]
    pub fn new(botty_path: String) -> Self {
        Self::with_mode(botty_path, ViewMode::default())
    }

    /// Create a new tmux view manager with specified mode.
    #[must_use]
    pub fn with_mode(botty_path: String, mode: ViewMode) -> Self {
        Self {
            session_name: "botty".to_string(),
            active_panes: HashSet::new(),
            botty_path,
            mode,
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

    /// Create a pane/window for an agent.
    /// In panes mode: splits the window.
    /// In windows mode: creates a new tmux window.
    pub fn add_pane(&mut self, agent_id: &str) -> Result<(), ViewError> {
        if self.active_panes.contains(agent_id) {
            // Already have a pane/window for this agent
            return Ok(());
        }

        // Use attach --readonly instead of tail for proper TUI display.
        // attach --readonly:
        // 1. Sends initial screen render (full screen with colors/positioning)
        // 2. Streams live PTY output in raw mode
        // 3. Properly handles cursor positioning and scroll regions
        let tail_cmd = format!("{} attach --readonly {}", self.botty_path, agent_id);

        match self.mode {
            ViewMode::Panes => self.add_pane_split(agent_id, &tail_cmd)?,
            ViewMode::Windows => self.add_window(agent_id, &tail_cmd)?,
        }

        self.active_panes.insert(agent_id.to_string());
        Ok(())
    }

    /// Add agent as a pane (split mode).
    fn add_pane_split(&self, agent_id: &str, tail_cmd: &str) -> Result<(), ViewError> {
        if self.active_panes.is_empty() {
            // First pane - respawn it with our command (replaces the shell)
            let status = Command::new("tmux")
                .args([
                    "respawn-pane",
                    "-t",
                    &format!("{}:agents", self.session_name),
                    "-k", // kill existing process
                    tail_cmd,
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
            // Also set @agent_id pane option for auto-resize (immune to title overwrites)
            let _ = Command::new("tmux")
                .args([
                    "set-option",
                    "-p",
                    "-t",
                    &format!("{}:agents", self.session_name),
                    "@agent_id",
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
                    tail_cmd,
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
            // Also set @agent_id pane option for auto-resize (immune to title overwrites)
            let _ = Command::new("tmux")
                .args([
                    "set-option",
                    "-p",
                    "-t",
                    &format!("{}:agents", self.session_name),
                    "@agent_id",
                    agent_id,
                ])
                .status();

            // Re-tile the layout
            self.retile()?;
        }
        Ok(())
    }

    /// Add agent as a new window (windows/tabs mode).
    fn add_window(&self, agent_id: &str, tail_cmd: &str) -> Result<(), ViewError> {
        if self.active_panes.is_empty() {
            // First window - respawn the initial window
            let status = Command::new("tmux")
                .args([
                    "respawn-window",
                    "-t",
                    &format!("{}:agents", self.session_name),
                    "-k",
                    tail_cmd,
                ])
                .status()?;

            if !status.success() {
                return Err(ViewError::TmuxFailed(
                    "failed to respawn first window".into(),
                ));
            }

            // Rename the window
            let _ = Command::new("tmux")
                .args([
                    "rename-window",
                    "-t",
                    &format!("{}:agents", self.session_name),
                    agent_id,
                ])
                .status();
            // Set @agent_id pane option for auto-resize
            let _ = Command::new("tmux")
                .args([
                    "set-option",
                    "-p",
                    "-t",
                    &format!("{}:agents", self.session_name),
                    "@agent_id",
                    agent_id,
                ])
                .status();
        } else {
            // Create a new window
            let status = Command::new("tmux")
                .args([
                    "new-window",
                    "-t",
                    &self.session_name,
                    "-n",
                    agent_id,
                    tail_cmd,
                ])
                .status()?;

            if !status.success() {
                return Err(ViewError::TmuxFailed("failed to create window".into()));
            }
            // Set @agent_id pane option for auto-resize
            let _ = Command::new("tmux")
                .args([
                    "set-option",
                    "-p",
                    "-t",
                    &format!("{}:{}", self.session_name, agent_id),
                    "@agent_id",
                    agent_id,
                ])
                .status();
        }
        Ok(())
    }

    /// Remove a pane/window for an agent.
    pub fn remove_pane(&mut self, agent_id: &str) -> Result<(), ViewError> {
        if !self.active_panes.contains(agent_id) {
            return Ok(());
        }

        match self.mode {
            ViewMode::Panes => self.remove_pane_split(agent_id)?,
            ViewMode::Windows => self.remove_window(agent_id)?,
        }

        self.active_panes.remove(agent_id);
        Ok(())
    }

    /// Remove a pane in split mode.
    fn remove_pane_split(&self, agent_id: &str) -> Result<(), ViewError> {
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

        // Re-tile if we still have panes
        if self.active_panes.len() > 1 {
            self.retile()?;
        }

        Ok(())
    }

    /// Remove a window in windows mode.
    fn remove_window(&self, agent_id: &str) -> Result<(), ViewError> {
        // In windows mode, window name is the agent ID
        let _ = Command::new("tmux")
            .args([
                "kill-window",
                "-t",
                &format!("{}:{}", self.session_name, agent_id),
            ])
            .status();
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

    /// Mark a pane as existing (for initializing from known state).
    /// This doesn't create a pane, just tracks that one exists.
    pub fn mark_pane_exists(&mut self, agent_id: &str) {
        self.active_panes.insert(agent_id.to_string());
    }

    /// Get the sizes of all panes/windows, keyed by agent ID.
    /// Uses @agent_id pane option which is immune to programs overwriting titles.
    /// Returns a map of agent_id -> (rows, cols).
    pub fn get_pane_sizes(&self) -> Result<std::collections::HashMap<String, (u16, u16)>, ViewError> {
        let mut sizes = std::collections::HashMap::new();

        // Use @agent_id pane option - immune to title overwrites by programs
        #[allow(clippy::literal_string_with_formatting_args)]
        let format_str = "#{@agent_id}:#{pane_height}:#{pane_width}";

        let session_window = format!("{}:agents", self.session_name);
        let output = match self.mode {
            ViewMode::Panes => Command::new("tmux")
                .args(["list-panes", "-t", &session_window, "-F", format_str])
                .output()?,
            ViewMode::Windows => Command::new("tmux")
                .args(["list-panes", "-s", "-t", &self.session_name, "-F", format_str])
                .output()?,
        };

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() >= 3 {
                    let agent_id = parts[0];
                    // Skip if @agent_id is empty (pane not managed by us)
                    if agent_id.is_empty() {
                        continue;
                    }
                    if let (Ok(rows), Ok(cols)) = (parts[1].parse::<u16>(), parts[2].parse::<u16>()) {
                        if self.active_panes.contains(agent_id) {
                            sizes.insert(agent_id.to_string(), (rows, cols));
                        }
                    }
                }
            }
        }

        Ok(sizes)
    }

    /// Set up a tmux hook to call botty resize when panes are resized.
    /// The hook runs a script that resizes all agents to match their pane sizes.
    pub fn setup_resize_hook(&self) -> Result<(), ViewError> {
        // Create a resize command that will be called on pane resize
        // This iterates through panes and calls botty resize for each
        let resize_cmd = format!(
            r#"run-shell '{} resize-all-panes'"#,
            self.botty_path
        );

        // Note: tmux hooks are tricky. For now, we'll use a simpler approach
        // and just resize on attach and when panes are added.
        // A proper hook would be:
        // tmux set-hook -t botty after-resize-pane "run-shell '...'"
        
        // For now, this is a no-op placeholder. The resize-all-panes command
        // doesn't exist yet, and implementing proper hooks requires more work.
        let _ = resize_cmd;
        
        Ok(())
    }

    /// Get the botty path (for external use in resize commands).
    #[must_use]
    pub fn botty_path(&self) -> &str {
        &self.botty_path
    }
}
