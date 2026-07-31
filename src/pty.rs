//! PTY creation and management.
//!
//! Provides helpers for spawning processes in pseudo-terminals.
//!
//! # Safety
//!
//! This module uses unsafe code for PTY operations (fork, ioctl, dup2).
//! These are fundamental operations that cannot be done safely.

#![allow(unsafe_code)]

use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::pty::{OpenptyResult, Winsize, openpty};
use nix::sys::signal::{self, Signal};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{ForkResult, Pid, fork, setsid};
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

    #[error("failed to change directory: {0}")]
    Chdir(#[source] std::io::Error),

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

unsafe extern "C" {
    /// The process environment, as `execvp` and friends read it.
    ///
    /// Declared here because the `libc` crate does not expose it on every
    /// target. Written only in a forked child between `fork` and `exec`, where
    /// a single pointer store is one of the few things that is safe to do.
    static mut environ: *mut *mut libc::c_char;
}

/// Minimal set of environment variables always provided to spawned agents.
///
/// These are captured from the server's environment at spawn time.
/// Explicit `--env` values override these.
const ESSENTIAL_ENV_VARS: &[&str] = &[
    "PATH",                     // command resolution
    "HOME",                     // home directory
    "USER",                     // current user
    "TERM",                     // terminal type (critical for PTY)
    "SHELL",                    // default shell
    "LANG",                     // locale / character encoding
    "XDG_RUNTIME_DIR",          // systemd, D-Bus, Wayland sockets
    "DBUS_SESSION_BUS_ADDRESS", // systemd-run --user needs session bus
];

/// Environment configuration for spawning.
///
/// The environment is always cleared before setting vars.
/// Essential vars (PATH, HOME, USER, TERM, SHELL, LANG) are set from
/// the server's environment, then explicit vars are applied on top.
#[derive(Debug, Default)]
pub struct SpawnEnv {
    /// Environment variables to set (key, value pairs).
    /// These override essential vars if they share a key.
    pub vars: Vec<(String, String)>,
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
    spawn_with_env(cmd, rows, cols, &SpawnEnv::default(), None)
}

/// Spawn a command in a new PTY with custom environment.
///
/// # Arguments
///
/// * `cmd` - Command and arguments to execute
/// * `rows` - Terminal height in rows
/// * `cols` - Terminal width in columns
/// * `env` - Environment configuration
/// * `cwd` - Optional working directory for the child process
///
/// # Returns
///
/// A `PtyProcess` containing the master FD and child PID.
pub fn spawn_with_env(
    cmd: &[String],
    rows: u16,
    cols: u16,
    env: &SpawnEnv,
    cwd: Option<&str>,
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

    // Everything the child needs is built HERE, before the fork.
    //
    // After fork(2) in a multi-threaded process the child gets only the
    // calling thread, but it inherits every lock in whatever state it was in
    // at that instant. A lock another thread held is now held forever, so the
    // child may call nothing that takes one -- POSIX limits it to
    // async-signal-safe functions until exec. That rules out malloc and Rust's
    // `std::env` accessors, which take a global lock.
    //
    // This is not theoretical: the child used to build CStrings and rewrite
    // its environment with `env::set_var` after forking, and would
    // occasionally wedge in `futex_do_wait` before reaching exec while the
    // parent blocked in waitpid. It reproduced in ~40% of threaded test runs
    // and never once with `--test-threads=1`. The server forks agents from a
    // thread pool, so the same deadlock was reachable from `vessel spawn`.
    let explicit_keys: std::collections::HashSet<&str> =
        env.vars.iter().map(|(k, _)| k.as_str()).collect();
    let essential: Vec<(String, String)> = ESSENTIAL_ENV_VARS
        .iter()
        .filter(|k| !explicit_keys.contains(**k))
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();

    // The child's environment as KEY=VALUE, replacing the old clear-then-set
    // dance: execve takes the environment directly, so the child never has to
    // touch `std::env` at all.
    let envp: Vec<CString> = essential
        .iter()
        .chain(env.vars.iter())
        .map(|(k, v)| CString::new(format!("{k}={v}")))
        .collect::<Result<_, _>>()
        .map_err(|_| PtyError::EmptyCommand)?;

    let prog = CString::new(cmd[0].as_str()).map_err(|_| PtyError::EmptyCommand)?;
    let args: Vec<CString> = cmd
        .iter()
        .map(|s| CString::new(s.as_str()))
        .collect::<Result<_, _>>()
        .map_err(|_| PtyError::EmptyCommand)?;
    let cwd_c: Option<CString> = match cwd {
        Some(dir) => Some(CString::new(dir).map_err(|_| PtyError::EmptyCommand)?),
        None => None,
    };

    // NULL-terminated pointer arrays for execve. Built here so the child only
    // has to read them; `nix::unistd::execve` would allocate these itself.
    let argv_ptrs: Vec<*const libc::c_char> = args
        .iter()
        .map(|c| c.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let envp_ptrs: Vec<*const libc::c_char> = envp
        .iter()
        .map(|c| c.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

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
            // Child: set up the terminal and exec.
            //
            // CRITICAL: After fork(), the child must NEVER return from this
            // function. If any step fails, it must _exit() immediately.
            // Returning would let the child continue executing the parent's
            // code (e.g., test runner logic), causing hangs and zombies.
            //
            // EQUALLY CRITICAL: every call below must be async-signal-safe.
            // No allocation, no `std::env`, no formatting, no Rust std call
            // that might take a lock -- see the note above the fork. The
            // payload was all built in the parent; this branch only reads it.

            // Close master in child. OwnedFd's Drop is a bare close(2).
            drop(master);

            // Create a new session
            if setsid().is_err() {
                unsafe { libc::_exit(1) };
            }

            // Set the slave as the controlling terminal
            unsafe {
                // Cast the request constant to the type ioctl() expects. On
                // macOS/BSD that param is c_ulong while TIOCSCTTY is c_uint, so
                // an explicit `as _` keeps this portable (no-op on Linux).
                if libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY as _, 0) < 0 {
                    libc::_exit(1);
                }
            }

            // Redirect stdin/stdout/stderr to the slave
            let slave_fd = slave.as_raw_fd();
            unsafe {
                if libc::dup2(slave_fd, libc::STDIN_FILENO) < 0
                    || libc::dup2(slave_fd, libc::STDOUT_FILENO) < 0
                    || libc::dup2(slave_fd, libc::STDERR_FILENO) < 0
                {
                    libc::_exit(1);
                }
            }

            // Close the original slave fd if it's not one of 0, 1, 2
            if slave_fd > 2 {
                drop(slave);
            }

            // Change working directory if requested. chdir(2) on a CString
            // built before the fork; `env::set_current_dir` would allocate.
            if let Some(dir) = &cwd_c {
                unsafe {
                    if libc::chdir(dir.as_ptr()) < 0 {
                        libc::_exit(1);
                    }
                }
            }

            // Install the child's environment by repointing `environ`, then
            // exec. A single pointer store, so it is async-signal-safe, where
            // the old `env::set_var` loop was not.
            //
            // `execvp` rather than `execve` keeps PATH resolution, and it
            // resolves against the PATH we just installed -- same as before,
            // when the environment was rewritten ahead of the call. `execvpe`
            // would do both in one step but is a glibc extension.
            unsafe {
                environ = envp_ptrs.as_ptr().cast_mut().cast();
                libc::execvp(prog.as_ptr(), argv_ptrs.as_ptr());
                libc::_exit(127);
            }
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

    /// Read everything the child writes to the PTY until it exits.
    fn drain(pty: &PtyProcess) -> String {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        // The master is non-blocking, so poll until the child exits and the
        // buffer drains.
        for _ in 0..400 {
            match nix::unistd::read(&pty.master, &mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(nix::errno::Errno::EAGAIN) => {
                    if pty.try_wait().ok().flatten().is_some() && !out.is_empty() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    // The child's environment is assembled before the fork and installed by
    // repointing `environ`, because the old post-fork `env::set_var` loop could
    // deadlock (see the note in spawn_with_env). These pin the behaviour that
    // rewrite has to preserve.

    #[test]
    fn explicit_env_vars_reach_the_child() {
        let env = SpawnEnv {
            vars: vec![("VESSEL_PROBE".into(), "probe-value".into())],
        };
        let pty = spawn_with_env(
            &["sh".into(), "-c".into(), "echo $VESSEL_PROBE".into()],
            24,
            80,
            &env,
            None,
        )
        .unwrap();
        let out = drain(&pty);
        assert!(
            out.contains("probe-value"),
            "child did not see the explicit var: {out:?}"
        );
    }

    #[test]
    fn child_environment_is_otherwise_clean() {
        // A variable in the parent that is neither essential nor explicit must
        // not survive into the child.
        // SAFETY: single-threaded test process at this point; the value is
        // read back only through the spawned child.
        unsafe { std::env::set_var("VESSEL_MUST_NOT_LEAK", "leaked") };

        let pty = spawn(
            &[
                "sh".into(),
                "-c".into(),
                "echo [${VESSEL_MUST_NOT_LEAK:-absent}]".into(),
            ],
            24,
            80,
        )
        .unwrap();
        let out = drain(&pty);

        unsafe { std::env::remove_var("VESSEL_MUST_NOT_LEAK") };

        assert!(
            out.contains("[absent]"),
            "parent variable leaked into the child: {out:?}"
        );
    }

    #[test]
    fn essential_vars_are_inherited() {
        let pty = spawn(
            &["sh".into(), "-c".into(), "echo [${HOME:-unset}]".into()],
            24,
            80,
        )
        .unwrap();
        let out = drain(&pty);
        assert!(
            out.contains("/") && !out.contains("[unset]"),
            "HOME should be inherited from the server: {out:?}"
        );
    }

    #[test]
    fn explicit_var_overrides_an_essential_one() {
        let env = SpawnEnv {
            vars: vec![("TERM".into(), "vessel-test-term".into())],
        };
        let pty = spawn_with_env(
            &["sh".into(), "-c".into(), "echo $TERM".into()],
            24,
            80,
            &env,
            None,
        )
        .unwrap();
        let out = drain(&pty);
        assert!(
            out.contains("vessel-test-term"),
            "explicit var should win over the essential default: {out:?}"
        );
    }

    #[test]
    fn cwd_is_applied_in_the_child() {
        let pty = spawn_with_env(
            &["sh".into(), "-c".into(), "pwd".into()],
            24,
            80,
            &SpawnEnv::default(),
            Some("/tmp"),
        )
        .unwrap();
        let out = drain(&pty);
        assert!(out.contains("/tmp"), "cwd was not applied: {out:?}");
    }

    #[test]
    fn command_still_resolves_through_path() {
        // execvp resolves against the PATH we install, so a bare command name
        // must still work.
        let pty = spawn(&["true".into()], 24, 80).unwrap();
        assert_eq!(pty.wait().unwrap(), 0);
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
