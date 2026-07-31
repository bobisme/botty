//! Command-line interface for vessel.

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

/// Split the `send` / `send-bytes` positionals into (agent ID, payload).
///
/// The ID is the first positional, so with a selector -- where the ID is
/// omitted -- clap parks the payload there instead. Shift it back.
///
/// Passing both an ID and a selector is rejected rather than guessed at: the
/// two are alternative ways to name the same thing, and `kill` already treats
/// them as exclusive.
///
/// # Errors
///
/// Returns an error if a selector is combined with an explicit agent ID.
pub fn split_send_positionals(
    id: Option<String>,
    payload: Option<String>,
    selector_used: bool,
) -> Result<(Option<String>, Option<String>), String> {
    if !selector_used {
        return Ok((id, payload));
    }
    if id.is_some() && payload.is_some() {
        return Err("specify either an agent ID or --label/--proc/--all, not both".to_string());
    }
    Ok((None, payload.or(id)))
}

/// Split the `send-keys` positionals into (agent ID, key names).
///
/// Same shift as [`split_send_positionals`], but the payload is variadic: with
/// a selector every positional is a key name, so the one clap took for the ID
/// belongs at the front of the list.
#[must_use]
pub fn split_send_keys_positionals(
    id: Option<String>,
    mut keys: Vec<String>,
    selector_used: bool,
) -> (Option<String>, Vec<String>) {
    if selector_used && let Some(first) = id {
        keys.insert(0, first);
        return (None, keys);
    }
    (id, keys)
}

/// Parse a named key sequence into bytes.
///
/// Supported keys:
/// - Arrow keys: `up`, `down`, `left`, `right`
/// - Special keys: `enter`, `tab`, `escape`, `backspace`, `delete`, `space`
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
        "up" => Some(vec![0x1b, 0x5b, 0x41]),    // ESC [ A
        "down" => Some(vec![0x1b, 0x5b, 0x42]),  // ESC [ B
        "right" => Some(vec![0x1b, 0x5b, 0x43]), // ESC [ C
        "left" => Some(vec![0x1b, 0x5b, 0x44]),  // ESC [ D

        // Special keys
        "enter" | "return" => Some(vec![0x0d]), // CR ("return" is an alias)
        "tab" => Some(vec![0x09]),              // HT
        "escape" | "esc" => Some(vec![0x1b]),   // ESC
        "backspace" => Some(vec![0x7f]),        // DEL
        "delete" | "del" => Some(vec![0x1b, 0x5b, 0x33, 0x7e]), // ESC [ 3 ~
        "space" => Some(vec![0x20]),            // SP (literal space byte)

        // Navigation keys
        "home" => Some(vec![0x1b, 0x5b, 0x48]), // ESC [ H
        "end" => Some(vec![0x1b, 0x5b, 0x46]),  // ESC [ F
        "pageup" | "pgup" => Some(vec![0x1b, 0x5b, 0x35, 0x7e]), // ESC [ 5 ~
        "pagedown" | "pgdn" | "pgdown" => Some(vec![0x1b, 0x5b, 0x36, 0x7e]), // ESC [ 6 ~

        // Function keys (commonly used)
        "f1" => Some(vec![0x1b, 0x4f, 0x50]), // ESC O P
        "f2" => Some(vec![0x1b, 0x4f, 0x51]), // ESC O Q
        "f3" => Some(vec![0x1b, 0x4f, 0x52]), // ESC O R
        "f4" => Some(vec![0x1b, 0x4f, 0x53]), // ESC O S

        _ => None,
    }
}

/// PTY-based agent runtime.
#[derive(Debug, Parser)]
#[command(name = "vessel", version, about)]
pub struct Cli {
    /// Path to the Unix socket.
    #[arg(long, env = "VESSEL_SOCKET")]
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
    ///
    /// Agents start with a clean environment. A minimal set of essential
    /// variables (PATH, HOME, USER, TERM, SHELL, LANG) is inherited from
    /// the server. Use --env to add or override variables.
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

        /// Additional environment variables (KEY=VALUE format, can be repeated).
        /// Agents always get PATH, HOME, USER, TERM, SHELL, LANG from the
        /// server. Use --env to add more or override these defaults.
        #[arg(long, short, value_name = "KEY=VALUE")]
        env: Vec<String>,

        /// Inherit env vars from the calling shell (comma-separated names).
        /// Reads each variable from the client's environment and passes it
        /// to the spawned agent (e.g., --env-inherit `BOTBUS_AGENT,EDITOR`).
        #[arg(long, value_delimiter = ',')]
        env_inherit: Vec<String>,

        /// Set working directory for the spawned process.
        #[arg(long)]
        cwd: Option<String>,

        /// Memory limit for the agent and all its children (e.g., "4G", "512M").
        /// Uses systemd cgroups on Linux. When exceeded, only this agent is killed,
        /// not the entire system.
        #[arg(long, value_name = "SIZE")]
        memory_limit: Option<String>,

        /// Prevent auto-resize from view command (keeps stable dimensions for snapshots).
        #[arg(long)]
        no_resize: bool,

        /// Enable command recording for this agent.
        /// All send/send-keys commands will be captured with timestamps.
        /// Retrieve recordings with `vessel recording <agent-id>`.
        #[arg(long)]
        record: bool,

        /// Wait for agent(s) to exit before spawning (can be repeated).
        #[arg(long)]
        after: Vec<String>,

        /// Wait for agent to output a pattern before spawning.
        /// Format: "agent-id" or "agent-id:regex" (e.g., "setup:ready" waits for "ready" in setup's output).
        #[arg(long)]
        wait_for: Vec<String>,

        /// Output format: text (id only), json (envelope), or pretty (human-readable).
        #[arg(long)]
        format: Option<String>,

        /// Output in JSON format (alias for --format json).
        #[arg(long, hide = true)]
        json: bool,

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

        /// Output format: text, json, or pretty.
        #[arg(long, default_value = "pretty")]
        format: String,

        /// Output in JSON format (alias for --format json).
        #[arg(long, hide = true)]
        json: bool,
    },

    /// Kill an agent (or all agents matching labels/process name).
    Kill {
        /// Agent ID (optional if using --label, --proc, or --all).
        id: Option<String>,

        /// Kill all agents with these labels (can be repeated, matches agents with ALL labels).
        #[arg(long, short)]
        label: Vec<String>,

        /// Kill all running agents.
        #[arg(long, short)]
        all: bool,

        /// Send SIGKILL instead of SIGTERM (force kill, no cleanup).
        #[arg(long, short)]
        force: bool,

        /// Kill agents whose command contains this substring (e.g., --proc htop).
        #[arg(long, short)]
        proc: Option<String>,

        /// Output format: text, json, or pretty.
        #[arg(long)]
        format: Option<String>,

        /// Output in JSON format (alias for --format json).
        #[arg(long, hide = true)]
        json: bool,
    },

    /// Send a Unix signal to an agent.
    ///
    /// Signal can be a name (TERM, KILL, USR1, HUP, INT, STOP, CONT, etc.)
    /// or a number (15, 9, 10, etc.). Names are case-insensitive and the
    /// SIG prefix is optional (e.g., TERM, SIGTERM, and term all work).
    Signal {
        /// Agent ID (optional if using --label, --proc, or --all).
        id: Option<String>,

        /// Signal to send (name or number, e.g., USR1, HUP, 10).
        #[arg(long, short)]
        signal: String,

        /// Send to all agents with these labels (can be repeated).
        #[arg(long, short)]
        label: Vec<String>,

        /// Send to all running agents.
        #[arg(long, short)]
        all: bool,

        /// Send to agents whose command contains this substring.
        #[arg(long, short)]
        proc: Option<String>,
    },

    /// Send text to an agent (literal, no newline by default).
    /// Use --newline to append a newline character, or --enter to append
    /// a carriage return (like pressing Enter in a terminal).
    ///
    /// Pass "-" as the text to read the payload from stdin, which avoids
    /// quoting a long prompt on the command line.
    ///
    /// For a multi-line prompt to a TUI agent, use --paste: it wraps the text
    /// in bracketed-paste markers so the whole thing lands as one prompt.
    ///
    /// The submit key is written separately from the text, after a short
    /// pause, so full-screen TUIs register it as a keypress instead of
    /// absorbing it into the composer as pasted content. Tune the pause with
    /// --submit-delay-ms.
    Send {
        /// Agent ID. Omit it when using --label, --proc, or --all.
        id: Option<String>,

        /// Text to send (optional when using --enter). Use "-" to read stdin.
        text: Option<String>,

        /// Send to all running agents with these labels (can be repeated).
        #[arg(long, short)]
        label: Vec<String>,

        /// Send to all running agents.
        #[arg(long, short)]
        all: bool,

        /// Send to running agents whose command contains this substring.
        ///
        /// Long-only here: -p is --paste on this command.
        #[arg(long)]
        proc: Option<String>,

        /// Wrap the text in bracketed-paste markers (ESC[200~ .. ESC[201~).
        ///
        /// Required for multi-line prompts to full-screen TUIs: without it the
        /// first newline submits a truncated prompt and each remaining line
        /// lands as its own turn. Combine with --enter to paste then submit.
        #[arg(short = 'p', long, alias = "bracketed")]
        paste: bool,

        /// Append a newline (LF) after the text.
        #[arg(short = 'n', long)]
        newline: bool,

        /// Append Enter key (CR) after the text. Equivalent to send-keys enter.
        #[arg(short = 'e', long)]
        enter: bool,

        /// Milliseconds to wait before the --newline/--enter key (default: 50).
        ///
        /// Raise it for a TUI that still swallows the key; set 0 to write the
        /// key immediately after the text, which is safe for shells and other
        /// line-oriented programs.
        #[arg(long, value_name = "MS")]
        submit_delay_ms: Option<u64>,

        /// Output format: text, json, or pretty.
        #[arg(long)]
        format: Option<String>,

        /// Output in JSON format (alias for --format json).
        #[arg(long, hide = true)]
        json: bool,
    },

    /// Send raw bytes to an agent, or to every agent a selector matches.
    SendBytes {
        /// Agent ID. Omit it when using --label, --proc, or --all.
        id: Option<String>,

        /// Hex-encoded bytes (e.g., "1b5b41" for up arrow).
        hex: Option<String>,

        /// Send to all running agents with these labels (can be repeated).
        #[arg(long, short)]
        label: Vec<String>,

        /// Send to all running agents.
        #[arg(long, short)]
        all: bool,

        /// Send to running agents whose command contains this substring.
        ///
        /// Long-only, to match `send`, where -p is --paste.
        #[arg(long)]
        proc: Option<String>,

        /// Output format: text, json, or pretty.
        #[arg(long)]
        format: Option<String>,

        /// Output in JSON format (alias for --format json).
        #[arg(long, hide = true)]
        json: bool,
    },

    /// Send named key sequences to an agent, or to every agent a selector
    /// matches.
    ///
    /// Supports arrow keys (up/down/left/right), special keys (enter/tab/escape),
    /// control sequences (ctrl-c/ctrl-d), and more. See --help for full list.
    ///
    /// With a selector the ID is omitted, so every positional is a key name:
    /// `vessel send-keys --label worker ctrl-c`.
    SendKeys {
        /// Agent ID. Omit it when using --label, --proc, or --all.
        id: Option<String>,

        /// Send to all running agents with these labels (can be repeated).
        #[arg(long, short)]
        label: Vec<String>,

        /// Send to all running agents.
        #[arg(long, short)]
        all: bool,

        /// Send to running agents whose command contains this substring.
        ///
        /// Long-only, to match `send`, where -p is --paste.
        #[arg(long)]
        proc: Option<String>,

        /// Key names separated by spaces (e.g., "up", "down enter", "ctrl-c").
        ///
        /// Supported keys:
        /// - Arrow keys: up, down, left, right
        /// - Special: enter, tab, escape, backspace, delete, space
        /// - Navigation: home, end, pageup, pagedown
        /// - Control: ctrl-c, ctrl-d, etc.
        /// - Function: f1, f2, f3, f4
        /// - Single chars: a, b, x, etc.
        keys: Vec<String>,

        /// Output format: text, json, or pretty.
        #[arg(long)]
        format: Option<String>,

        /// Output in JSON format (alias for --format json).
        #[arg(long, hide = true)]
        json: bool,
    },

    /// Tail agent output.
    Tail {
        /// Agent ID.
        id: String,

        /// Number of lines to show.
        #[arg(short = 'n', long, default_value = "10")]
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

        /// Compare with previous snapshot file and show diff.
        #[arg(long)]
        diff: Option<String>,
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
    #[command(after_help = "\
SUBAGENT WORKFLOW:
  Spawn a child, wait for it to finish, then check its exit code:

    child=$(vessel spawn --name parent/child -- my-command --flag)
    vessel wait --exited \"$child\"
    echo \"Exit code: $?\"

  Wait for multiple agents to exit:

    vessel wait --exited worker-1 worker-2 worker-3

  Return when any listed agent exits:

    vessel wait --exited --any worker-1 worker-2 worker-3

  Combined with output conditions (single agent only):

    vessel wait --exited --contains 'done' --print my-agent")]
    Wait {
        /// Agent ID(s). Multiple IDs can be specified with --exited.
        id: Vec<String>,

        /// Wait until the agent has exited.
        #[arg(long)]
        exited: bool,

        /// Return when any listed agent exits instead of waiting for all.
        #[arg(long)]
        any: bool,

        /// Wait until output contains this string.
        #[arg(long)]
        contains: Option<String>,

        /// Wait until output matches this regex pattern.
        #[arg(long)]
        pattern: Option<String>,

        /// Wait until screen is stable (hasn't changed for this duration).
        #[arg(long, value_name = "MILLIS")]
        stable: Option<u64>,

        /// Timeout in seconds (0 = wait forever).
        #[arg(long, short, default_value = "0")]
        timeout: u64,

        /// Print the snapshot when condition is met.
        #[arg(long, short)]
        print: bool,
    },

    /// Assert that agent output matches a condition.
    ///
    /// Exits with code 0 if assertion passes, code 1 if it fails.
    /// Prints clear error message on failure showing expected vs actual.
    Assert {
        /// Agent ID.
        id: String,

        /// Assert output contains this string.
        #[arg(long)]
        contains: Option<String>,

        /// Assert output does NOT contain this string.
        #[arg(long)]
        not_contains: Option<String>,

        /// Assert output matches this regex pattern.
        #[arg(long)]
        pattern: Option<String>,

        /// Timeout in seconds (default: check immediately).
        #[arg(long, short, default_value = "0")]
        timeout: u64,
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

    /// Resize all agents in a vessel view session to match their pane sizes.
    /// This is typically called from a tmux hook, not manually.
    #[command(hide = true)]
    ResizePanes {
        /// Layout mode used by the view session.
        #[arg(long, default_value = "panes")]
        mode: String,
    },

    /// Get recorded commands for an agent.
    ///
    /// Returns a JSON array of commands that were sent to the agent,
    /// each with a timestamp, command type, and payload.
    /// Recording must be enabled at spawn time with --record.
    Recording {
        /// Agent ID.
        id: String,

        /// Output format: text (one-line records), json (envelope), or pretty (formatted JSON).
        #[arg(long)]
        format: Option<String>,

        /// Output in JSON format (alias for --format json).
        #[arg(long, hide = true)]
        json: bool,
    },

    /// Generate a test script from a recorded agent session.
    ///
    /// Reads an agent's recording and outputs an executable bash script
    /// that replays the recorded commands. Timing delays are derived from
    /// recording timestamps (capped at 2s). Redirect output to a file:
    ///   vessel gen-test <agent-id> > test.sh && chmod +x test.sh
    GenTest {
        /// Agent ID.
        id: String,
    },

    /// Show an agent's runtime environment variables.
    ///
    /// Reads /proc/<pid>/environ for a running agent and displays its
    /// actual environment. Useful for verifying that env vars like
    /// `CARGO_BUILD_JOBS` are reaching the spawned process.
    Env {
        /// Agent ID.
        id: String,

        /// Output format: text (KEY=VALUE lines), json, or pretty.
        #[arg(long)]
        format: Option<String>,

        /// Output in JSON format (alias for --format json).
        #[arg(long, hide = true)]
        json: bool,
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
        assert_eq!(
            parse_key_sequence("delete"),
            Some(vec![0x1b, 0x5b, 0x33, 0x7e])
        );
        // "space" sends a literal space byte; a bare " " arg is trimmed away,
        // so the named key is the only way to send space via send-keys.
        assert_eq!(parse_key_sequence("space"), Some(vec![0x20]));
        assert_eq!(parse_key_sequence("SPACE"), Some(vec![0x20]));
        assert_eq!(parse_key_sequence(" space "), Some(vec![0x20]));
    }

    #[test]
    fn test_parse_key_sequence_navigation() {
        assert_eq!(parse_key_sequence("home"), Some(vec![0x1b, 0x5b, 0x48]));
        assert_eq!(parse_key_sequence("end"), Some(vec![0x1b, 0x5b, 0x46]));
        assert_eq!(
            parse_key_sequence("pageup"),
            Some(vec![0x1b, 0x5b, 0x35, 0x7e])
        );
        assert_eq!(
            parse_key_sequence("pagedown"),
            Some(vec![0x1b, 0x5b, 0x36, 0x7e])
        );
        assert_eq!(
            parse_key_sequence("pgup"),
            Some(vec![0x1b, 0x5b, 0x35, 0x7e])
        );
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

    // bn-1dxu: with a selector the ID positional is omitted, so clap parks the
    // payload in the ID slot and it has to shift back.

    #[test]
    fn positionals_unchanged_without_a_selector() {
        let got = split_send_positionals(Some("agent".into()), Some("hello".into()), false);
        assert_eq!(got, Ok((Some("agent".into()), Some("hello".into()))));
    }

    #[test]
    fn id_required_form_allows_a_missing_payload() {
        // `send agent --enter` sends no text, just the submit key.
        let got = split_send_positionals(Some("agent".into()), None, false);
        assert_eq!(got, Ok((Some("agent".into()), None)));
    }

    #[test]
    fn selector_shifts_the_payload_out_of_the_id_slot() {
        // `send --label worker "hello"` -> clap sees id="hello", payload=None.
        let got = split_send_positionals(Some("hello".into()), None, true);
        assert_eq!(got, Ok((None, Some("hello".into()))));
    }

    #[test]
    fn selector_with_no_payload_stays_empty() {
        let got = split_send_positionals(None, None, true);
        assert_eq!(got, Ok((None, None)));
    }

    #[test]
    fn selector_plus_explicit_id_is_rejected() {
        // Both slots filled means an ID was passed alongside a selector; the
        // two are alternative ways to name the same thing.
        let err = split_send_positionals(Some("agent".into()), Some("hello".into()), true)
            .expect_err("id + selector must be rejected");
        assert!(err.contains("not both"), "unexpected error: {err}");
    }

    #[test]
    fn send_keys_selector_reclaims_the_first_key() {
        // `send-keys --label worker ctrl-c enter` -> clap sees id="ctrl-c".
        let (id, keys) =
            split_send_keys_positionals(Some("ctrl-c".into()), vec!["enter".into()], true);
        assert_eq!(id, None);
        assert_eq!(keys, vec!["ctrl-c".to_string(), "enter".to_string()]);
    }

    #[test]
    fn send_keys_without_a_selector_keeps_the_id() {
        let (id, keys) =
            split_send_keys_positionals(Some("agent".into()), vec!["up".into()], false);
        assert_eq!(id, Some("agent".into()));
        assert_eq!(keys, vec!["up".to_string()]);
    }
}
