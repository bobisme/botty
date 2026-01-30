//! Command-line interface for botty.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Parse a key notation string into a byte value.
///
/// Supported formats:
/// - `ctrl-X` or `ctrl+X` - Control character (e.g., `ctrl-g` = 0x07)
/// - `^X` - Control character shorthand (e.g., `^G` = 0x07)
/// - Single character - Literal character (e.g., `d` = 0x64)
///
/// Returns None if the notation is invalid.
#[must_use]
pub fn parse_key_notation(s: &str) -> Option<u8> {
    let s = s.trim().to_lowercase();

    // ctrl-X or ctrl+X format
    if let Some(rest) = s.strip_prefix("ctrl-").or_else(|| s.strip_prefix("ctrl+")) {
        if rest.len() == 1 {
            let c = rest.chars().next()?;
            if c.is_ascii_alphabetic() {
                // ctrl-a = 0x01, ctrl-z = 0x1a
                return Some((c as u8) - b'a' + 1);
            }
        }
        return None;
    }

    // ^X format
    if let Some(rest) = s.strip_prefix('^') {
        if rest.len() == 1 {
            let c = rest.chars().next()?;
            if c.is_ascii_alphabetic() {
                return Some((c as u8) - b'a' + 1);
            }
        }
        return None;
    }

    // Single character
    if s.len() == 1 {
        return Some(s.as_bytes()[0]);
    }

    None
}

/// Parse a named key sequence into bytes.
///
/// Supported keys:
/// - Arrow keys: `up`, `down`, `left`, `right`
/// - Special keys: `enter`, `tab`, `escape`, `backspace`, `delete`
/// - Navigation: `home`, `end`, `pageup`, `pagedown`
/// - Control sequences: `ctrl-c`, `ctrl-d`, etc.
/// - Single characters: `a`, `b`, `x`, etc.
///
/// Returns None if the key name is not recognized.
#[must_use]
pub fn parse_key_sequence(s: &str) -> Option<Vec<u8>> {
    let s = s.trim().to_lowercase();

    // Try single-byte keys first (ctrl-X, single chars)
    if let Some(byte) = parse_key_notation(&s) {
        return Some(vec![byte]);
    }

    // Multi-byte ANSI escape sequences
    match s.as_str() {
        // Arrow keys (ESC [ X)
        "up" => Some(vec![0x1b, 0x5b, 0x41]),       // ESC [ A
        "down" => Some(vec![0x1b, 0x5b, 0x42]),     // ESC [ B
        "right" => Some(vec![0x1b, 0x5b, 0x43]),    // ESC [ C
        "left" => Some(vec![0x1b, 0x5b, 0x44]),     // ESC [ D

        // Special keys
        "enter" => Some(vec![0x0d]),                // CR
        "return" => Some(vec![0x0d]),               // Alias for enter
        "tab" => Some(vec![0x09]),                  // HT
        "escape" | "esc" => Some(vec![0x1b]),       // ESC
        "backspace" => Some(vec![0x7f]),            // DEL
        "delete" | "del" => Some(vec![0x1b, 0x5b, 0x33, 0x7e]), // ESC [ 3 ~

        // Navigation keys
        "home" => Some(vec![0x1b, 0x5b, 0x48]),     // ESC [ H
        "end" => Some(vec![0x1b, 0x5b, 0x46]),      // ESC [ F
        "pageup" | "pgup" => Some(vec![0x1b, 0x5b, 0x35, 0x7e]), // ESC [ 5 ~
        "pagedown" | "pgdn" | "pgdown" => Some(vec![0x1b, 0x5b, 0x36, 0x7e]), // ESC [ 6 ~

        // Function keys (commonly used)
        "f1" => Some(vec![0x1b, 0x4f, 0x50]),       // ESC O P
        "f2" => Some(vec![0x1b, 0x4f, 0x51]),       // ESC O Q
        "f3" => Some(vec![0x1b, 0x4f, 0x52]),       // ESC O R
        "f4" => Some(vec![0x1b, 0x4f, 0x53]),       // ESC O S

        _ => None,
    }
}

/// PTY-based agent runtime.
#[derive(Debug, Parser)]
#[command(name = "botty", version, about)]
pub struct Cli {
    /// Path to the Unix socket.
    #[arg(long, env = "BOTTY_SOCKET")]
    pub socket: Option<PathBuf>,

    /// Enable verbose logging.
    #[arg(short, long)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Spawn a new agent.
    Spawn {
        /// Terminal rows.
        #[arg(long, default_value = "24")]
        rows: u16,

        /// Terminal columns.
        #[arg(long, default_value = "80")]
        cols: u16,

        /// Custom agent ID (must be unique, defaults to generated name).
        #[arg(long, short)]
        name: Option<String>,

        /// Labels for grouping agents (can be repeated, e.g., --label worker --label batch-1).
        #[arg(long, short)]
        label: Vec<String>,

        /// Auto-kill agent after this many seconds. Sends SIGTERM first, then SIGKILL after 5s grace.
        #[arg(long, short)]
        timeout: Option<u64>,

        /// Stop recording transcript after this many bytes (e.g., 1048576 for 1MB).
        #[arg(long)]
        max_output: Option<u64>,

        /// Environment variables (KEY=VALUE format, can be repeated).
        #[arg(long, short, value_name = "KEY=VALUE")]
        env: Vec<String>,

        /// Clear environment before spawning (only set explicit --env vars).
        #[arg(long)]
        env_clear: bool,

        /// Wait for agent(s) to exit before spawning (can be repeated).
        #[arg(long)]
        after: Vec<String>,

        /// Wait for agent to output a pattern before spawning.
        /// Format: "agent-id" or "agent-id:regex" (e.g., "setup:ready" waits for "ready" in setup's output).
        #[arg(long)]
        wait_for: Vec<String>,

        /// Command to run (after --).
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },

    /// List agents.
    List {
        /// Show all agents including exited ones.
        #[arg(long)]
        all: bool,

        /// Filter by label (can be repeated, agents must have ALL labels).
        #[arg(long, short)]
        label: Vec<String>,

        /// Output format: toon (default, token-optimized), json, or text.
        #[arg(long, default_value = "toon")]
        format: String,

        /// Output in JSON format (for piping to jq). Deprecated: use --format json.
        #[arg(long, hide = true)]
        json: bool,
    },

    /// Kill an agent (or all agents matching labels).
    Kill {
        /// Agent ID (optional if using --label or --all).
        id: Option<String>,

        /// Kill all agents with these labels (can be repeated, matches agents with ALL labels).
        #[arg(long, short)]
        label: Vec<String>,

        /// Kill all running agents.
        #[arg(long, short)]
        all: bool,

        /// Send SIGTERM instead of SIGKILL (for graceful shutdown).
        #[arg(long)]
        term: bool,
    },

    /// Send input to an agent.
    Send {
        /// Agent ID.
        id: String,

        /// Text to send.
        text: String,

        /// Do not append a newline.
        #[arg(long)]
        no_newline: bool,
    },

    /// Send raw bytes to an agent.
    SendBytes {
        /// Agent ID.
        id: String,

        /// Hex-encoded bytes (e.g., "1b5b41" for up arrow).
        hex: String,
    },

    /// Send a named key sequence to an agent.
    ///
    /// Supports arrow keys (up/down/left/right), special keys (enter/tab/escape),
    /// control sequences (ctrl-c/ctrl-d), and more. See --help for full list.
    SendKey {
        /// Agent ID.
        id: String,

        /// Key name (e.g., "up", "down", "enter", "ctrl-c", "tab").
        ///
        /// Supported keys:
        /// - Arrow keys: up, down, left, right
        /// - Special: enter, tab, escape, backspace, delete
        /// - Navigation: home, end, pageup, pagedown
        /// - Control: ctrl-c, ctrl-d, etc.
        /// - Function: f1, f2, f3, f4
        /// - Single chars: a, b, x, etc.
        key: String,
    },

    /// Tail agent output.
    Tail {
        /// Agent ID.
        id: String,

        /// Number of lines to show.
        #[arg(short = 'n', default_value = "10")]
        lines: usize,

        /// Follow output (like tail -f).
        #[arg(short, long)]
        follow: bool,

        /// Show raw output including ANSI escape codes.
        #[arg(long)]
        raw: bool,

        /// Show current screen state before streaming (for TUI viewing).
        /// Implies --follow and --raw.
        #[arg(long)]
        replay: bool,
    },

    /// Dump agent transcript.
    Dump {
        /// Agent ID.
        id: String,

        /// Only include output since this Unix timestamp (millis).
        #[arg(long)]
        since: Option<u64>,

        /// Output format (text or jsonl).
        #[arg(long, default_value = "text")]
        format: String,
    },

    /// Get a snapshot of the agent's screen.
    Snapshot {
        /// Agent ID.
        id: String,

        /// Include ANSI color codes.
        #[arg(long)]
        raw: bool,
    },

    /// Attach to an agent interactively.
    Attach {
        /// Agent ID.
        id: String,

        /// Read-only mode.
        #[arg(long)]
        readonly: bool,

        /// Detach key prefix (default: ctrl-g).
        /// Press this followed by 'd' to detach.
        /// Formats: ctrl-X, ^X, or single char.
        #[arg(long, default_value = "ctrl-g")]
        detach_key: String,
    },

    /// Run the server (usually started automatically).
    Server {
        /// Run as a daemon (fork to background).
        #[arg(long)]
        daemon: bool,
    },

    /// Shut down the server.
    Shutdown,

    /// Wait for agent output to match a condition.
    ///
    /// Conditions can be combined with AND logic. For example:
    /// `--stable 200 --contains "$ "` waits for the screen to be stable
    /// for 200ms AND contain the prompt.
    Wait {
        /// Agent ID.
        id: String,

        /// Wait until output contains this string.
        #[arg(long)]
        contains: Option<String>,

        /// Wait until output matches this regex pattern.
        #[arg(long)]
        pattern: Option<String>,

        /// Wait until screen is stable (hasn't changed for this duration).
        #[arg(long, value_name = "MILLIS")]
        stable: Option<u64>,

        /// Timeout in seconds.
        #[arg(long, short, default_value = "30")]
        timeout: u64,

        /// Print the snapshot when condition is met.
        #[arg(long, short)]
        print: bool,
    },

    /// Execute a command and return its output.
    ///
    /// Spawns a shell, runs the command, waits for completion, and returns
    /// the output. The agent is automatically killed after completion.
    Exec {
        /// Terminal rows.
        #[arg(long, default_value = "24")]
        rows: u16,

        /// Terminal columns.
        #[arg(long, default_value = "80")]
        cols: u16,

        /// Timeout in seconds.
        #[arg(long, short, default_value = "30")]
        timeout: u64,

        /// Shell to use.
        #[arg(long, default_value = "sh")]
        shell: String,

        /// Command to execute.
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },

    /// Check system health and configuration.
    Doctor,

    /// Stream agent lifecycle events (JSON).
    Events {
        /// Filter to specific agent IDs (comma-separated, or pass multiple times).
        #[arg(long, short, value_delimiter = ',')]
        filter: Vec<String>,

        /// Include output events (can be noisy).
        #[arg(long)]
        output: bool,
    },

    /// Subscribe to agent output streams.
    ///
    /// Streams raw output from one or more agents. Useful for watching workers
    /// from an orchestrating agent. Use --prefix for multiplexed viewing.
    Subscribe {
        /// Agent IDs to subscribe to (can be repeated).
        #[arg(long, short)]
        id: Vec<String>,

        /// Subscribe to agents with these labels (can be repeated).
        #[arg(long, short)]
        label: Vec<String>,

        /// Prefix each output chunk with [agent-id] for multiplexed viewing.
        #[arg(long, short)]
        prefix: bool,

        /// Output format: raw (default) or jsonl.
        #[arg(long, default_value = "raw")]
        format: String,
    },

    /// Launch a tmux viewer showing all agents.
    View {
        /// Multiplexer to use (currently only tmux is supported).
        #[arg(long, default_value = "tmux")]
        mux: String,

        /// Layout mode: "panes" (default) shows all agents in split panes,
        /// "windows" creates a separate tmux window per agent for tab-style navigation.
        #[arg(long, default_value = "panes")]
        mode: String,

        /// Disable automatic resizing of agent PTYs to match tmux pane dimensions.
        /// By default, agents are resized when panes resize.
        #[arg(long)]
        no_resize: bool,

        /// Filter to agents with these labels (can be repeated, matches agents with ALL labels).
        #[arg(long, short)]
        label: Vec<String>,

        /// Destroy and recreate the tmux session instead of reattaching.
        #[arg(long)]
        new_session: bool,
    },

    /// Resize an agent's terminal.
    Resize {
        /// Agent ID.
        id: String,

        /// New number of rows.
        #[arg(long)]
        rows: u16,

        /// New number of columns.
        #[arg(long)]
        cols: u16,

        /// Clear transcript buffer after resize (avoids display issues from old-size output).
        #[arg(long)]
        clear: bool,
    },

    /// Resize all agents in a botty view session to match their pane sizes.
    /// This is typically called from a tmux hook, not manually.
    #[command(hide = true)]
    ResizePanes {
        /// Layout mode used by the view session.
        #[arg(long, default_value = "panes")]
        mode: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_key_notation_ctrl_format() {
        assert_eq!(parse_key_notation("ctrl-a"), Some(0x01));
        assert_eq!(parse_key_notation("ctrl-g"), Some(0x07));
        assert_eq!(parse_key_notation("ctrl-z"), Some(0x1a));
        assert_eq!(parse_key_notation("ctrl+a"), Some(0x01));
        assert_eq!(parse_key_notation("CTRL-A"), Some(0x01));
        assert_eq!(parse_key_notation("Ctrl-G"), Some(0x07));
    }

    #[test]
    fn test_parse_key_notation_caret_format() {
        assert_eq!(parse_key_notation("^a"), Some(0x01));
        assert_eq!(parse_key_notation("^g"), Some(0x07));
        assert_eq!(parse_key_notation("^G"), Some(0x07));
        assert_eq!(parse_key_notation("^Z"), Some(0x1a));
    }

    #[test]
    fn test_parse_key_notation_single_char() {
        assert_eq!(parse_key_notation("d"), Some(b'd'));
        assert_eq!(parse_key_notation("x"), Some(b'x'));
        // Note: single chars are lowercased for consistency
        assert_eq!(parse_key_notation("D"), Some(b'd'));
    }

    #[test]
    fn test_parse_key_notation_invalid() {
        assert_eq!(parse_key_notation("ctrl-"), None);
        assert_eq!(parse_key_notation("ctrl-ab"), None);
        assert_eq!(parse_key_notation("^"), None);
        assert_eq!(parse_key_notation("^ab"), None);
        assert_eq!(parse_key_notation("ab"), None);
        assert_eq!(parse_key_notation(""), None);
    }

    #[test]
    fn test_parse_key_sequence_arrow_keys() {
        assert_eq!(parse_key_sequence("up"), Some(vec![0x1b, 0x5b, 0x41]));
        assert_eq!(parse_key_sequence("down"), Some(vec![0x1b, 0x5b, 0x42]));
        assert_eq!(parse_key_sequence("right"), Some(vec![0x1b, 0x5b, 0x43]));
        assert_eq!(parse_key_sequence("left"), Some(vec![0x1b, 0x5b, 0x44]));
        assert_eq!(parse_key_sequence("UP"), Some(vec![0x1b, 0x5b, 0x41])); // Case insensitive
    }

    #[test]
    fn test_parse_key_sequence_special_keys() {
        assert_eq!(parse_key_sequence("enter"), Some(vec![0x0d]));
        assert_eq!(parse_key_sequence("return"), Some(vec![0x0d]));
        assert_eq!(parse_key_sequence("tab"), Some(vec![0x09]));
        assert_eq!(parse_key_sequence("escape"), Some(vec![0x1b]));
        assert_eq!(parse_key_sequence("esc"), Some(vec![0x1b]));
        assert_eq!(parse_key_sequence("backspace"), Some(vec![0x7f]));
        assert_eq!(parse_key_sequence("delete"), Some(vec![0x1b, 0x5b, 0x33, 0x7e]));
    }

    #[test]
    fn test_parse_key_sequence_navigation() {
        assert_eq!(parse_key_sequence("home"), Some(vec![0x1b, 0x5b, 0x48]));
        assert_eq!(parse_key_sequence("end"), Some(vec![0x1b, 0x5b, 0x46]));
        assert_eq!(parse_key_sequence("pageup"), Some(vec![0x1b, 0x5b, 0x35, 0x7e]));
        assert_eq!(parse_key_sequence("pagedown"), Some(vec![0x1b, 0x5b, 0x36, 0x7e]));
        assert_eq!(parse_key_sequence("pgup"), Some(vec![0x1b, 0x5b, 0x35, 0x7e]));
    }

    #[test]
    fn test_parse_key_sequence_function_keys() {
        assert_eq!(parse_key_sequence("f1"), Some(vec![0x1b, 0x4f, 0x50]));
        assert_eq!(parse_key_sequence("f2"), Some(vec![0x1b, 0x4f, 0x51]));
        assert_eq!(parse_key_sequence("f3"), Some(vec![0x1b, 0x4f, 0x52]));
        assert_eq!(parse_key_sequence("f4"), Some(vec![0x1b, 0x4f, 0x53]));
    }

    #[test]
    fn test_parse_key_sequence_control_chars() {
        assert_eq!(parse_key_sequence("ctrl-c"), Some(vec![0x03]));
        assert_eq!(parse_key_sequence("ctrl-d"), Some(vec![0x04]));
        assert_eq!(parse_key_sequence("^c"), Some(vec![0x03]));
    }

    #[test]
    fn test_parse_key_sequence_single_chars() {
        assert_eq!(parse_key_sequence("a"), Some(vec![b'a']));
        assert_eq!(parse_key_sequence("x"), Some(vec![b'x']));
        assert_eq!(parse_key_sequence("5"), Some(vec![b'5']));
    }

    #[test]
    fn test_parse_key_sequence_invalid() {
        assert_eq!(parse_key_sequence("invalid-key"), None);
        assert_eq!(parse_key_sequence("arrow-up"), None);
        assert_eq!(parse_key_sequence(""), None);
    }
}
