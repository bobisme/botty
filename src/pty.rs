//! PTY creation and management.
//!
//! Provides helpers for spawning processes in pseudo-terminals.
//!
//! # Safety
//!
//! This module uses unsafe code for PTY operations (fork, ioctl, dup2).
//! These are fundamental operations that cannot be done safely.

#![allow(unsafe_code)]

use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::pty::{openpty, OpenptyResult, Winsize};
use nix::sys::signal::{self, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{execvp, fork, setsid, ForkResult, Pid};
use std::ffi::CString;
use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};
use thiserror::Error;

/// Errors that can occur during PTY operations.
#[derive(Debug, Error)]
pub enum PtyError {
    #[error("failed to open PTY: {0}")]
    OpenPty(#[source] nix::Error),

    #[error("failed to fork: {0}")]
    Fork(#[source] nix::Error),

    #[error("failed to create session: {0}")]
    Setsid(#[source] nix::Error),

    #[error("failed to set controlling terminal: {0}")]
    SetControllingTerminal(#[source] nix::Error),

    #[error("failed to exec: {0}")]
    Exec(#[source] nix::Error),

    #[error("command is empty")]
    EmptyCommand,

    #[error("invalid command string: {0}")]
    InvalidCommand(#[source] std::ffi::NulError),

    #[error("failed to send signal: {0}")]
    Signal(#[source] nix::Error),

    #[error("failed to wait: {0}")]
    Wait(#[source] nix::Error),
}

/// Result of spawning a process in a PTY.
pub struct PtyProcess {
    /// The master side of the PTY.
    pub master: OwnedFd,
    /// The child process ID.
    pub pid: Pid,
    /// Terminal size.
    pub size: Winsize,
}

impl PtyProcess {
    /// Get the raw file descriptor of the master PTY.
    #[must_use]
    pub fn master_fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    /// Send a signal to the child process.
    pub fn signal(&self, sig: Signal) -> Result<(), PtyError> {
        signal::kill(self.pid, sig).map_err(PtyError::Signal)
    }

    /// Check if the child process has exited without blocking.
    /// Returns `Some(exit_code)` if exited, None if still running.
    pub fn try_wait(&self) -> Result<Option<i32>, PtyError> {
        match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)).map_err(PtyError::Wait)? {
            WaitStatus::Exited(_, code) => Ok(Some(code)),
            WaitStatus::Signaled(_, sig, _) => Ok(Some(128 + sig as i32)),
            // All other states (StillAlive, Stopped, Continued, etc.) mean not exited yet
            _ => Ok(None),
        }
    }

    /// Wait for the child process to exit (blocking).
    pub fn wait(&self) -> Result<i32, PtyError> {
        match waitpid(self.pid, None).map_err(PtyError::Wait)? {
            WaitStatus::Exited(_, code) => Ok(code),
            WaitStatus::Signaled(_, sig, _) => Ok(128 + sig as i32),
            status => {
                tracing::warn!(?status, "unexpected wait status");
                Ok(-1)
            }
        }
    }

    /// Resize the PTY.
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), PtyError> {
        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // TIOCSWINSZ ioctl
        unsafe {
            let ret = libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &winsize);
            if ret < 0 {
                return Err(PtyError::SetControllingTerminal(nix::Error::last()));
            }
        }
        Ok(())
    }
}

/// Environment configuration for spawning.
#[derive(Debug, Default)]
pub struct SpawnEnv {
    /// Environment variables to set (key, value pairs).
    pub vars: Vec<(String, String)>,
    /// If true, clear the environment before setting vars.
    pub clear: bool,
}

/// Spawn a command in a new PTY.
///
/// # Arguments
///
/// * `cmd` - Command and arguments to execute
/// * `rows` - Terminal height in rows
/// * `cols` - Terminal width in columns
///
/// # Returns
///
/// A `PtyProcess` containing the master FD and child PID.
pub fn spawn(cmd: &[String], rows: u16, cols: u16) -> Result<PtyProcess, PtyError> {
    spawn_with_env(cmd, rows, cols, &SpawnEnv::default())
}

/// Spawn a command in a new PTY with custom environment.
///
/// # Arguments
///
/// * `cmd` - Command and arguments to execute
/// * `rows` - Terminal height in rows
/// * `cols` - Terminal width in columns
/// * `env` - Environment configuration
///
/// # Returns
///
/// A `PtyProcess` containing the master FD and child PID.
pub fn spawn_with_env(
    cmd: &[String],
    rows: u16,
    cols: u16,
    env: &SpawnEnv,
) -> Result<PtyProcess, PtyError> {
    if cmd.is_empty() {
        return Err(PtyError::EmptyCommand);
    }

    let winsize = Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    // Open a new PTY pair
    let OpenptyResult { master, slave } = openpty(&winsize, None).map_err(PtyError::OpenPty)?;

    // Fork the process
    match unsafe { fork() }.map_err(PtyError::Fork)? {
        ForkResult::Parent { child } => {
            // Parent: close slave, keep master
            drop(slave);

            // Set master to non-blocking mode for async I/O
            let flags = fcntl(&master, FcntlArg::F_GETFL).map_err(PtyError::OpenPty)?;
            let mut flags = OFlag::from_bits_retain(flags);
            flags.insert(OFlag::O_NONBLOCK);
            fcntl(&master, FcntlArg::F_SETFL(flags)).map_err(PtyError::OpenPty)?;

            Ok(PtyProcess {
                master,
                pid: child,
                size: winsize,
            })
        }
        ForkResult::Child => {
            // Child: set up the terminal and exec

            // Close master in child
            drop(master);

            // Create a new session
            setsid().map_err(PtyError::Setsid)?;

            // Set the slave as the controlling terminal
            unsafe {
                let ret = libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY, 0);
                if ret < 0 {
                    std::process::exit(1);
                }
            }

            // Redirect stdin/stdout/stderr to the slave using libc directly
            // (nix's dup2 API is awkward for this use case)
            let slave_fd = slave.as_raw_fd();
            unsafe {
                if libc::dup2(slave_fd, libc::STDIN_FILENO) < 0 {
                    std::process::exit(1);
                }
                if libc::dup2(slave_fd, libc::STDOUT_FILENO) < 0 {
                    std::process::exit(1);
                }
                if libc::dup2(slave_fd, libc::STDERR_FILENO) < 0 {
                    std::process::exit(1);
                }
            }

            // Close the original slave fd if it's not one of 0, 1, 2
            if slave_fd > 2 {
                drop(slave);
            }

            // Set up environment
            // SAFETY: We're in a forked child process before exec, so modifying
            // environment is safe (no other threads exist in this process).
            unsafe {
                if env.clear {
                    // Clear all environment variables
                    for (key, _) in std::env::vars() {
                        std::env::remove_var(&key);
                    }
                }
                // Set requested environment variables
                for (key, value) in &env.vars {
                    std::env::set_var(key, value);
                }
            }

            // Convert command to CStrings
            let prog = CString::new(cmd[0].as_str()).map_err(PtyError::InvalidCommand)?;
            let args: Vec<CString> = cmd
                .iter()
                .map(|s| CString::new(s.as_str()))
                .collect::<Result<_, _>>()
                .map_err(PtyError::InvalidCommand)?;

            // Exec the command
            execvp(&prog, &args).map_err(PtyError::Exec)?;

            // execvp only returns on error
            unreachable!()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_spawn_echo() {
        let pty = spawn(&["sh".into(), "-c".into(), "echo hello".into()], 24, 80).unwrap();

        // Wait for child to exit
        let exit_code = pty.wait().unwrap();
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn test_spawn_exit_code() {
        let pty = spawn(&["sh".into(), "-c".into(), "exit 42".into()], 24, 80).unwrap();
        let exit_code = pty.wait().unwrap();
        assert_eq!(exit_code, 42);
    }

    #[test]
    fn test_spawn_empty_command() {
        let result = spawn(&[], 24, 80);
        assert!(matches!(result, Err(PtyError::EmptyCommand)));
    }

    #[test]
    fn test_try_wait() {
        let pty = spawn(&["sleep".into(), "0.1".into()], 24, 80).unwrap();

        // Should still be running
        let result = pty.try_wait().unwrap();
        assert!(result.is_none());

        // Wait for it to finish
        std::thread::sleep(Duration::from_millis(200));

        // Now it should be done
        let result = pty.try_wait().unwrap();
        assert_eq!(result, Some(0));
    }
}
