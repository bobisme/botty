//! botty — PTY-based Agent Runtime
//!
//! A tmux-style, user-scoped PTY server for running and coordinating
//! interactive agents as terminal programs.

// Error documentation is deferred - the errors are self-explanatory from types
#![allow(clippy::missing_errors_doc)]

pub mod attach;
pub mod cli;
pub mod client;
pub mod protocol;
pub mod pty;
pub mod server;
pub mod testing;

pub use attach::{run_attach, AttachConfig, AttachError};
pub use cli::{parse_key_notation, Cli, Command};
pub use client::{default_socket_path, Client, ClientError};
pub use protocol::{AgentInfo, AgentState, DumpFormat, Event, Request, Response};
pub use server::{Server, ServerError};
pub use testing::{AgentHandle, TestError, TestHarness};
