//! Protocol types for client-server IPC.
//!
//! All communication between the botty CLI (client) and the botty server
//! happens over a Unix socket using JSON-serialized Request/Response messages.

use serde::{Deserialize, Serialize};

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
        /// Environment variables to set (KEY=VALUE pairs).
        #[serde(default)]
        env: Vec<String>,
        /// Clear environment before spawning.
        #[serde(default)]
        env_clear: bool,
    },

    /// List all agents.
    List,

    /// Kill an agent by ID.
    Kill {
        /// Agent ID.
        id: String,
        /// Unix signal number (default: SIGTERM = 15).
        #[serde(default = "default_signal")]
        signal: i32,
    },

    /// Send UTF-8 text input to an agent.
    Send {
        /// Agent ID.
        id: String,
        /// Text to send.
        data: String,
        /// Whether to append a newline.
        #[serde(default)]
        newline: bool,
    },

    /// Send raw bytes to an agent.
    SendBytes {
        /// Agent ID.
        id: String,
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
    /// Terminal size (rows, cols).
    pub size: (u16, u16),
    /// Unix timestamp when the agent was spawned (millis).
    pub started_at: u64,
    /// Exit code if the agent has exited.
    pub exit_code: Option<i32>,
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

    /// Raw output bytes (for tail without follow).
    Output {
        /// Output data (base64 encoded in JSON).
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
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
/// Used with the `botty events` command for reactive orchestration.
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

/// Module for base64 encoding/decoding of byte vectors in serde.
mod base64_bytes {
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
                env: vec![],
                env_clear: false,
            },
            Request::List,
            Request::Kill {
                id: "test-agent".into(),
                signal: 9,
            },
            Request::Send {
                id: "test-agent".into(),
                data: "hello\n".into(),
                newline: false,
            },
            Request::SendBytes {
                id: "test-agent".into(),
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
        ];

        for req in requests {
            let json = serde_json::to_string(&req).expect("serialize");
            let parsed: Request = serde_json::from_str(&json).expect("deserialize");
            let json2 = serde_json::to_string(&parsed).expect("re-serialize");
            assert_eq!(json, json2, "roundtrip failed for {:?}", req);
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
                    size: (24, 80),
                    started_at: 1706140800000,
                    exit_code: None,
                }],
            },
            Response::Output {
                data: b"hello world\n".to_vec(),
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
            }),
            Response::Event(Event::AgentOutput {
                id: "test-agent".into(),
                data: b"hello".to_vec(),
            }),
            Response::Event(Event::AgentExited {
                id: "test-agent".into(),
                exit_code: Some(0),
            }),
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
            id: "test".into(),
            data: vec![0x1b, 0x5b, 0x41],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("G1tB")); // base64 of [0x1b, 0x5b, 0x41]
    }
}
