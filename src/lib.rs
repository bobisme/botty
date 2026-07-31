//! vessel — PTY-based Agent Runtime
//!
//! A tmux-style, user-scoped PTY server for running and coordinating
//! interactive agents as terminal programs.

// Error documentation is deferred - the errors are self-explanatory from types
#![allow(clippy::missing_errors_doc)]

pub mod attach;
pub mod cli;
pub mod client;
pub mod output;
pub mod protocol;
pub mod pty;
pub mod runtime;
pub mod server;
pub mod sys;
pub mod telemetry;
pub mod testing;
pub mod view;

/// Check whether `systemd-run --user` is available.
///
/// Result is cached after the first call (process-lifetime).
pub fn has_systemd_run() -> bool {
    use std::sync::OnceLock;
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("systemd-run")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

pub use attach::{AttachConfig, AttachError, run_attach};
pub use cli::{
    Cli, Command, parse_key_notation, parse_key_sequence, split_send_keys_positionals,
    split_send_positionals,
};
pub use client::{Client, ClientError, default_socket_path};
pub use output::{OutputFormat, json_envelope, resolve_format, text_record};
pub use protocol::{
    AgentInfo, AgentState, DumpFormat, Event, ExitReason, RecordedCommand, Request, ResourceLimits,
    Response, SendOutcome,
};
pub use server::{Server, ServerError};
pub use testing::{AgentHandle, TestError, TestHarness};
pub use view::{TmuxView, ViewError, ViewMode};
