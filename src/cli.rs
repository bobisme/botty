//! Command-line interface for botty.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

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

        /// Command to run (after --).
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },

    /// List all agents.
    List,

    /// Kill an agent.
    Kill {
        /// Agent ID.
        id: String,

        /// Send SIGKILL instead of SIGTERM.
        #[arg(long, short = '9')]
        force: bool,
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
    },

    /// Run the server (usually started automatically).
    Server {
        /// Run as a daemon (fork to background).
        #[arg(long)]
        daemon: bool,
    },

    /// Shut down the server.
    Shutdown,
}
