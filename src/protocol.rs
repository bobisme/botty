//! Protocol types for client-server IPC.
//!
//! All communication between the vessel CLI (client) and the vessel server
//! happens over a Unix socket using JSON-serialized Request/Response messages.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// A recorded command sent to an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedCommand {
    /// Unix timestamp in milliseconds when the command was recorded.
    pub timestamp: u64,
    /// The type of command ("send", "`send_bytes`", or "`send_keys`").
    pub command: String,
    /// The payload of the command.
    /// For "send": the text that was sent.
    /// For "`send_bytes"`: hex-encoded bytes.
    /// For "`send_keys"`: the key name.
    pub payload: String,
}

impl RecordedCommand {
    /// Create a new recorded command with the current timestamp.
    #[must_use]
    pub fn new(command: impl Into<String>, payload: impl Into<String>) -> Self {
        // Milliseconds since the Unix epoch fit in u64 until well past the year
        // AD 580 million, so this cast cannot truncate in practice.
        #[allow(clippy::cast_possible_truncation)]
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            timestamp,
            command: command.into(),
            payload: payload.into(),
        }
    }
}

/// Format for transcript dump output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum DumpFormat {
    /// Plain text output.
    #[default]
    Text,
    /// JSON Lines with timestamps per chunk.
    Jsonl,
}

/// Requests from client to server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Spawn a new agent with the given command.
    Spawn {
        /// Command and arguments to execute.
        cmd: Vec<String>,
        /// Terminal rows (default: 24).
        #[serde(default = "default_rows")]
        rows: u16,
        /// Terminal columns (default: 80).
        #[serde(default = "default_cols")]
        cols: u16,
        /// Optional custom agent ID (must be unique).
        #[serde(default)]
        name: Option<String>,
        /// Labels for grouping agents.
        #[serde(default)]
        labels: Vec<String>,
        /// Auto-kill after this many seconds (None = no timeout).
        #[serde(default)]
        timeout: Option<u64>,
        /// Stop recording transcript after this many bytes (None = unlimited).
        #[serde(default)]
        max_output: Option<u64>,
        /// Environment variables to set (KEY=VALUE pairs).
        /// The environment is always clean; only these vars are set.
        #[serde(default)]
        env: Vec<String>,
        /// Working directory for the spawned process.
        #[serde(default)]
        cwd: Option<String>,
        /// Prevent auto-resize from view command.
        #[serde(default)]
        no_resize: bool,
        /// Enable command recording for this agent.
        #[serde(default)]
        record: bool,
        /// Memory limit for the agent (e.g., "4G", "512M").
        /// Uses systemd cgroups on Linux.
        #[serde(default)]
        memory_limit: Option<String>,
    },

    /// List all agents (optionally filtered by labels).
    List {
        /// Filter by labels (agents must have ALL specified labels).
        #[serde(default)]
        labels: Vec<String>,
    },

    /// Kill an agent by ID, by labels, by process name, or all agents.
    Kill {
        /// Agent ID (optional if using labels, `proc_filter`, or all).
        #[serde(default)]
        id: Option<String>,
        /// Kill all agents with these labels.
        #[serde(default)]
        labels: Vec<String>,
        /// Kill all running agents.
        #[serde(default)]
        all: bool,
        /// Unix signal number (default: SIGTERM = 15).
        #[serde(default = "default_signal")]
        signal: i32,
        /// Kill agents whose command matches this substring.
        #[serde(default)]
        proc_filter: Option<String>,
    },

    /// Send UTF-8 text input to an agent, or to every agent a selector matches.
    Send {
        /// Agent ID. Optional when using `labels`, `proc_filter`, or `all`.
        #[serde(default)]
        id: Option<String>,
        /// Send to all running agents with these labels.
        #[serde(default)]
        labels: Vec<String>,
        /// Send to all running agents.
        #[serde(default)]
        all: bool,
        /// Send to running agents whose command contains this substring.
        #[serde(default)]
        proc_filter: Option<String>,
        /// Text to send.
        data: String,
        /// Whether to append a newline (LF).
        #[serde(default)]
        newline: bool,
        /// Whether to append Enter key (CR).
        #[serde(default)]
        enter: bool,
        /// Milliseconds to wait between the text write and the submit key
        /// write. `None` uses [`DEFAULT_SUBMIT_DELAY_MS`]; `Some(0)` writes the
        /// key immediately after the text (still a separate `write(2)`).
        ///
        /// Only meaningful when `newline` or `enter` is set.
        #[serde(default)]
        submit_delay_ms: Option<u64>,
        /// Wrap the text in bracketed-paste markers so a TUI takes it as one
        /// paste rather than as typed keystrokes. See [`PASTE_START`].
        #[serde(default)]
        paste: bool,
    },

    /// Send raw bytes to an agent, or to every agent a selector matches.
    SendBytes {
        /// Agent ID. Optional when using `labels`, `proc_filter`, or `all`.
        #[serde(default)]
        id: Option<String>,
        /// Send to all running agents with these labels.
        #[serde(default)]
        labels: Vec<String>,
        /// Send to all running agents.
        #[serde(default)]
        all: bool,
        /// Send to running agents whose command contains this substring.
        #[serde(default)]
        proc_filter: Option<String>,
        /// Raw bytes (base64 encoded in JSON).
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },

    /// Tail the transcript buffer.
    Tail {
        /// Agent ID.
        id: String,
        /// Number of lines to return.
        #[serde(default = "default_tail_lines")]
        lines: usize,
        /// Whether to stream new output (server will send multiple responses).
        #[serde(default)]
        follow: bool,
    },

    /// Dump the transcript buffer.
    Dump {
        /// Agent ID.
        id: String,
        /// Only include output since this Unix timestamp (millis).
        #[serde(default)]
        since: Option<u64>,
        /// Output format.
        #[serde(default)]
        format: DumpFormat,
    },

    /// Get a snapshot of the virtual screen.
    Snapshot {
        /// Agent ID.
        id: String,
        /// Whether to strip ANSI color codes (default: true).
        #[serde(default = "default_true")]
        strip_colors: bool,
    },

    /// Attach to an agent (interactive mode).
    /// This switches the connection to streaming mode.
    Attach {
        /// Agent ID.
        id: String,
        /// Read-only mode (output only, no input forwarding).
        #[serde(default)]
        readonly: bool,
    },

    /// Request server shutdown.
    Shutdown,

    /// Ping the server (for health checks / auto-start detection).
    Ping,

    /// Subscribe to event stream.
    /// Server will send Event responses until the connection is closed.
    Events {
        /// Filter to specific agent IDs (empty = all agents).
        #[serde(default)]
        filter: Vec<String>,
        /// Include output events (can be noisy).
        #[serde(default)]
        include_output: bool,
    },

    /// Resize an agent's terminal.
    Resize {
        /// Agent ID.
        id: String,
        /// New terminal rows.
        rows: u16,
        /// New terminal columns.
        cols: u16,
        /// Clear transcript buffer after resize (useful when viewing to avoid
        /// displaying old output rendered at wrong size).
        #[serde(default)]
        clear_transcript: bool,
    },

    /// Get the recorded commands for an agent.
    GetRecording {
        /// Agent ID.
        id: String,
    },

    /// Get the runtime environment of an agent.
    GetEnv {
        /// Agent ID.
        id: String,
    },
}

/// Information about a single agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    /// Unique agent ID (e.g., "rusty-nail").
    pub id: String,
    /// Process ID of the agent.
    pub pid: u32,
    /// Current state.
    pub state: AgentState,
    /// Command that was spawned.
    pub command: Vec<String>,
    /// Labels assigned to this agent.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Terminal size (rows, cols).
    pub size: (u16, u16),
    /// Unix timestamp when the agent was spawned (millis).
    pub started_at: u64,
    /// Exit code if the agent has exited.
    pub exit_code: Option<i32>,
    /// Exit reason (normal, timeout, killed).
    #[serde(default)]
    pub exit_reason: Option<ExitReason>,
    /// Resource limits applied to this agent.
    #[serde(default)]
    pub limits: Option<ResourceLimits>,
    /// Whether this agent is immune to auto-resize.
    #[serde(default)]
    pub no_resize: bool,
    /// Resident set size in bytes (agent + child process tree).
    /// None if the process has exited or RSS couldn't be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_bytes: Option<u64>,
}

/// Why an agent exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    /// Normal exit (process exited on its own).
    Normal,
    /// Killed by timeout.
    Timeout,
    /// Killed by user request.
    Killed,
}

/// Resource limits for an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Timeout in seconds (None = no timeout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    /// Max transcript bytes (None = unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
}

/// Agent lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    /// Agent is running.
    Running,
    /// Agent has exited.
    Exited,
}

/// Transcript entry with timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    /// Unix timestamp in milliseconds.
    pub timestamp: u64,
    /// Output bytes (base64 encoded in JSON).
    #[serde(with = "base64_bytes")]
    pub data: Vec<u8>,
}

/// Responses from server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Generic success with no data.
    Ok,

    /// Pong response to Ping.
    Pong,

    /// Agent was successfully spawned.
    Spawned {
        /// The new agent's ID.
        id: String,
        /// The new agent's PID.
        pid: u32,
    },

    /// List of agents.
    Agents {
        /// The list of agents.
        agents: Vec<AgentInfo>,
    },

    /// Per-agent outcomes of a selector-based `Send`/`SendBytes`.
    ///
    /// Sent instead of [`Response::Ok`] whenever the request used `all`,
    /// `labels`, or `proc_filter`. A fan-out can succeed for some agents and
    /// fail for others, and a bare `Ok` would report that as total success --
    /// the same "looks delivered, wasn't" failure the separate-write submit
    /// key exists to avoid. Requests naming a single `id` still answer with
    /// `Ok`/`Error`, so existing callers are unaffected.
    SendResults {
        /// One entry per matched agent, in match order.
        results: Vec<SendOutcome>,
    },

    /// Raw output bytes (for tail without follow).
    Output {
        /// Output data (base64 encoded in JSON).
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
        /// Whether the agent has exited (used by tail --follow to know when to stop).
        #[serde(default)]
        exited: bool,
    },

    /// Transcript dump (for dump command).
    Transcript {
        /// Transcript entries.
        entries: Vec<TranscriptEntry>,
    },

    /// Screen snapshot.
    Snapshot {
        /// Normalized screen content.
        content: String,
        /// Cursor position (row, col), 0-indexed.
        cursor: (u16, u16),
        /// Screen size (rows, cols).
        size: (u16, u16),
    },

    /// Error response.
    Error {
        /// Error message.
        message: String,
    },

    /// Agent exited (sent during attach or tail --follow).
    AgentExited {
        /// Agent ID.
        id: String,
        /// Exit code.
        exit_code: Option<i32>,
    },

    /// Attach mode started - connection switches to streaming.
    /// After this response, the protocol changes:
    /// - Client sends raw bytes (prefixed with length) which go to agent PTY
    /// - Server sends raw bytes (prefixed with length) from agent PTY output
    /// - A zero-length message from client signals detach
    /// - `AgentExited` is sent if agent exits during attach
    AttachStarted {
        /// Agent ID.
        id: String,
        /// Current terminal size.
        size: (u16, u16),
    },

    /// Attach mode ended (sent after detach or agent exit).
    AttachEnded {
        /// Reason for ending.
        reason: AttachEndReason,
    },

    /// Server event (sent during event subscription).
    Event(Event),

    /// Recorded commands for an agent.
    Recording {
        /// The agent ID.
        agent_id: String,
        /// The recorded commands.
        commands: Vec<RecordedCommand>,
    },

    /// Agent runtime environment.
    AgentEnv {
        /// The agent ID.
        id: String,
        /// Environment variables (key=value pairs).
        env: Vec<(String, String)>,
    },
}

/// Reason attach mode ended.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachEndReason {
    /// User requested detach.
    Detached,
    /// Agent process exited.
    AgentExited { exit_code: Option<i32> },
    /// An error occurred.
    Error { message: String },
}

/// Events streamed from the server.
///
/// Used with the `vessel events` command for reactive orchestration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// An agent was spawned.
    AgentSpawned {
        /// Agent ID.
        id: String,
        /// Process ID.
        pid: u32,
        /// Command that was spawned.
        command: Vec<String>,
        /// Labels assigned to this agent.
        #[serde(default)]
        labels: Vec<String>,
    },
    /// An agent produced output.
    AgentOutput {
        /// Agent ID.
        id: String,
        /// Output data (base64 encoded in JSON).
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },
    /// An agent exited.
    AgentExited {
        /// Agent ID.
        id: String,
        /// Exit code (None if killed by signal).
        exit_code: Option<i32>,
    },
}

impl Response {
    /// Create an error response.
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }
}

// Default value helpers
const fn default_rows() -> u16 {
    24
}
const fn default_cols() -> u16 {
    80
}
const fn default_signal() -> i32 {
    15 // SIGTERM
}
const fn default_tail_lines() -> usize {
    10
}
const fn default_true() -> bool {
    true
}

/// Default gap, in milliseconds, between writing a `Send` payload and writing
/// its submit key (`--newline` / `--enter`).
///
/// Full-screen TUIs (codex, claude, and anything else built on a composer
/// widget) classify input by arrival timing: bytes that land in a single burst
/// are treated as a paste and inserted as literal content, not as keypresses.
/// A trailing CR/LF in the same `write(2)` as the text is therefore swallowed
/// into the composer instead of submitting it, and the prompt sits there
/// looking delivered. Splitting the key into its own write after a pause puts
/// it outside the burst, so it reads as a real keypress.
///
/// 50ms clears the paste-detection window of the TUIs tested while staying
/// below human-perceptible latency. Callers that know their target is a
/// line-oriented program (a shell) can pass `Some(0)` to skip the wait.
pub const DEFAULT_SUBMIT_DELAY_MS: u64 = 50;

/// Outcome of delivering input to one agent in a fan-out send.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendOutcome {
    /// The agent the input was addressed to.
    pub id: String,
    /// Why delivery failed, or `None` if the bytes were written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl SendOutcome {
    /// Record a successful delivery.
    #[must_use]
    pub const fn delivered(id: String) -> Self {
        Self { id, error: None }
    }

    /// Record a failed delivery.
    #[must_use]
    pub const fn failed(id: String, error: String) -> Self {
        Self {
            id,
            error: Some(error),
        }
    }

    /// Whether the input reached this agent.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

/// Bracketed-paste introducer, `ESC [ 200 ~`.
///
/// A TUI that has enabled bracketed paste (DECSET 2004) treats everything up
/// to [`PASTE_END`] as a single paste: newlines inside it become literal lines
/// in the composer rather than submissions. Without it, sending a multi-line
/// prompt submits a truncated first line and turns each remaining line into
/// its own turn.
pub const PASTE_START: &[u8] = b"\x1b[200~";

/// Bracketed-paste terminator, `ESC [ 201 ~`. See [`PASTE_START`].
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// Maximum length, in bytes, of a base64-encoded `SendBytes` payload accepted
/// during deserialization.
///
/// This bounds the decoded `Vec<u8>` allocation independently of the server's
/// IPC frame cap (defense in depth against CWE-400): even if the protocol
/// types are deserialized outside the framed socket reader, an oversized
/// payload is rejected before the decode buffer is allocated. 1 MiB of base64
/// decodes to ~768 KiB, far above any legitimate single PTY write.
const MAX_SEND_BYTES_BASE64_LEN: usize = 1024 * 1024;

/// Module for base64 encoding/decoding of byte vectors in serde.
mod base64_bytes {
    use super::MAX_SEND_BYTES_BASE64_LEN;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        encoded.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        use base64::Engine;
        let s = String::deserialize(deserializer)?;
        // Reject oversized payloads before allocating the decoded buffer.
        if s.len() > MAX_SEND_BYTES_BASE64_LEN {
            return Err(serde::de::Error::custom(format!(
                "send_bytes payload too large: {} encoded bytes exceeds limit of {MAX_SEND_BYTES_BASE64_LEN}",
                s.len()
            )));
        }
        base64::engine::general_purpose::STANDARD
            .decode(&s)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization_roundtrip() {
        let requests = vec![
            Request::Spawn {
                cmd: vec!["bash".into(), "-c".into(), "echo hello".into()],
                rows: 24,
                cols: 80,
                name: None,
                labels: vec!["worker".into()],
                timeout: Some(60),
                max_output: Some(1024 * 1024),
                env: vec![],
                cwd: None,
                no_resize: false,
                record: false,
                memory_limit: Some("4G".into()),
            },
            Request::List { labels: vec![] },
            Request::Kill {
                id: Some("test-agent".into()),
                labels: vec![],
                all: false,
                signal: 9,
                proc_filter: None,
            },
            Request::Send {
                id: Some("test-agent".into()),
                labels: Vec::new(),
                all: false,
                proc_filter: None,
                data: "hello\n".into(),
                newline: false,
                enter: false,
                submit_delay_ms: None,
                paste: false,
            },
            Request::SendBytes {
                id: Some("test-agent".into()),
                labels: Vec::new(),
                all: false,
                proc_filter: None,
                data: vec![0x1b, 0x5b, 0x41], // ESC [ A (up arrow)
            },
            Request::Tail {
                id: "test-agent".into(),
                lines: 20,
                follow: true,
            },
            Request::Snapshot {
                id: "test-agent".into(),
                strip_colors: true,
            },
            Request::Ping,
            Request::Shutdown,
            Request::Events {
                filter: vec!["agent-1".into()],
                include_output: true,
            },
            Request::Resize {
                id: "test-agent".into(),
                rows: 40,
                cols: 120,
                clear_transcript: false,
            },
            Request::GetRecording {
                id: "test-agent".into(),
            },
            Request::GetEnv {
                id: "test-agent".into(),
            },
        ];

        for req in requests {
            let json = serde_json::to_string(&req).expect("serialize");
            let parsed: Request = serde_json::from_str(&json).expect("deserialize");
            let json2 = serde_json::to_string(&parsed).expect("re-serialize");
            assert_eq!(json, json2, "roundtrip failed for {:?}", req);
        }
    }

    #[test]
    fn test_send_without_submit_delay_defaults_to_none() {
        // A client built before submit_delay_ms existed omits the field; it
        // must still deserialize and fall back to DEFAULT_SUBMIT_DELAY_MS.
        let json = r#"{"type":"send","id":"a","data":"hi","newline":true}"#;
        let parsed: Request = serde_json::from_str(json).expect("deserialize");
        match parsed {
            Request::Send {
                submit_delay_ms,
                newline,
                enter,
                ..
            } => {
                assert_eq!(submit_delay_ms, None);
                assert!(newline);
                assert!(!enter);
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }

    #[test]
    fn test_send_bytes_oversized_payload_rejected() {
        // A `SendBytes.data` base64 field longer than the cap must fail to
        // deserialize, before the decoded `Vec<u8>` is allocated (CWE-400).
        let oversized = "A".repeat(MAX_SEND_BYTES_BASE64_LEN + 4);
        let json = format!(r#"{{"type":"send_bytes","id":"a","data":"{oversized}"}}"#);
        let err = serde_json::from_str::<Request>(&json)
            .expect_err("oversized send_bytes payload should be rejected");
        assert!(
            err.to_string().contains("send_bytes payload too large"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_send_bytes_at_limit_accepted() {
        // A payload at exactly the cap is still accepted and decodes correctly.
        // Base64 length must be a multiple of 4; the cap (1 MiB) already is.
        let at_limit = "A".repeat(MAX_SEND_BYTES_BASE64_LEN);
        let json = format!(r#"{{"type":"send_bytes","id":"a","data":"{at_limit}"}}"#);
        let parsed: Request =
            serde_json::from_str(&json).expect("payload at the limit should be accepted");
        match parsed {
            Request::SendBytes { data, .. } => {
                // "AAAA..." decodes to all-zero bytes; 3 bytes per 4 base64 chars.
                assert_eq!(data.len(), MAX_SEND_BYTES_BASE64_LEN / 4 * 3);
            }
            other => panic!("expected SendBytes, got {other:?}"),
        }
    }

    #[test]
    fn test_response_serialization_roundtrip() {
        let responses = vec![
            Response::Ok,
            Response::Pong,
            Response::Spawned {
                id: "rusty-nail".into(),
                pid: 12345,
            },
            Response::Agents {
                agents: vec![AgentInfo {
                    id: "rusty-nail".into(),
                    pid: 12345,
                    state: AgentState::Running,
                    command: vec!["bash".into()],
                    labels: vec!["worker".into()],
                    size: (24, 80),
                    started_at: 1706140800000,
                    exit_code: None,
                    exit_reason: None,
                    limits: Some(ResourceLimits {
                        timeout: Some(60),
                        max_output: None,
                    }),
                    no_resize: false,
                    rss_bytes: Some(142_000_000),
                }],
            },
            Response::Output {
                data: b"hello world\n".to_vec(),
                exited: false,
            },
            Response::Snapshot {
                content: "$ echo hello\nhello\n$ ".into(),
                cursor: (2, 2),
                size: (24, 80),
            },
            Response::error("agent not found"),
            Response::Event(Event::AgentSpawned {
                id: "test-agent".into(),
                pid: 12345,
                command: vec!["bash".into()],
                labels: vec![],
            }),
            Response::Event(Event::AgentOutput {
                id: "test-agent".into(),
                data: b"hello".to_vec(),
            }),
            Response::Event(Event::AgentExited {
                id: "test-agent".into(),
                exit_code: Some(0),
            }),
            Response::Recording {
                agent_id: "test-agent".into(),
                commands: vec![RecordedCommand {
                    timestamp: 1706140800000,
                    command: "send".into(),
                    payload: "hello\n".into(),
                }],
            },
            Response::AgentEnv {
                id: "test-agent".into(),
                env: vec![
                    ("CARGO_BUILD_JOBS".into(), "2".into()),
                    ("PATH".into(), "/usr/bin".into()),
                ],
            },
        ];

        for resp in responses {
            let json = serde_json::to_string(&resp).expect("serialize");
            let parsed: Response = serde_json::from_str(&json).expect("deserialize");
            let json2 = serde_json::to_string(&parsed).expect("re-serialize");
            assert_eq!(json, json2, "roundtrip failed for {:?}", resp);
        }
    }

    #[test]
    fn test_base64_bytes_encoding() {
        let req = Request::SendBytes {
            id: Some("test".into()),
            labels: Vec::new(),
            all: false,
            proc_filter: None,
            data: vec![0x1b, 0x5b, 0x41],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("G1tB")); // base64 of [0x1b, 0x5b, 0x41]
    }
}
