//! botty — PTY-based Agent Runtime
//!
//! A tmux-style, user-scoped PTY server for running and coordinating
//! interactive agents as terminal programs.

pub mod cli;
pub mod client;
pub mod protocol;
pub mod pty;
pub mod server;

pub use cli::{Cli, Command};
pub use client::{default_socket_path, Client, ClientError};
pub use protocol::{AgentInfo, AgentState, DumpFormat, Request, Response};
pub use server::{Server, ServerError};
