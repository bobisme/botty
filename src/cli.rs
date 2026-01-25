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

        /// Environment variables (KEY=VALUE format, can be repeated).
        #[arg(long, short, value_name = "KEY=VALUE")]
        env: Vec<String>,

        /// Clear environment before spawning (only set explicit --env vars).
        #[arg(long)]
        env_clear: bool,

        /// Command to run (after --).
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },

    /// List agents.
    List {
        /// Show all agents including exited ones.
        #[arg(long)]
        all: bool,

        /// Output in JSON format (for piping to jq).
        #[arg(long)]
        json: bool,
    },

    /// Kill an agent.
    Kill {
        /// Agent ID.
        id: String,

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
    Wait {
        /// Agent ID.
        id: String,

        /// Wait until output contains this string.
        #[arg(long, group = "condition")]
        contains: Option<String>,

        /// Wait until output matches this regex pattern.
        #[arg(long, group = "condition")]
        pattern: Option<String>,

        /// Wait until screen is stable (hasn't changed for this duration).
        #[arg(long, group = "condition", value_name = "MILLIS")]
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
}
