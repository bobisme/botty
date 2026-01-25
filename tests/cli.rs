//! End-to-end CLI tests using assert_cmd.
//!
//! These tests run the actual botty binary and verify stdout/stderr/exit codes.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Generate a unique socket path for each test.
fn unique_socket_path() -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    PathBuf::from(format!("/tmp/botty-cli-test-{pid}-{id}.sock"))
}

/// Helper to clean up socket after test.
struct TestEnv {
    socket_path: PathBuf,
    server_process: Option<std::process::Child>,
}

impl TestEnv {
    fn new() -> Self {
        let socket_path = unique_socket_path();
        Self {
            socket_path,
            server_process: None,
        }
    }

    fn socket_arg(&self) -> String {
        format!("--socket={}", self.socket_path.display())
    }

    fn start_server(&mut self) {
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_botty"))
            .arg(&self.socket_arg())
            .arg("server")
            .spawn()
            .expect("failed to start server");
        self.server_process = Some(child);
        // Give server time to start
        std::thread::sleep(Duration::from_millis(200));
    }

    fn botty(&self) -> Command {
        let mut cmd = Command::cargo_bin("botty").unwrap();
        cmd.arg(&self.socket_arg());
        cmd
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // Try to shut down the server gracefully
        if self.server_process.is_some() {
            let _ = std::process::Command::new(env!("CARGO_BIN_EXE_botty"))
                .arg(&self.socket_arg())
                .arg("shutdown")
                .output();
        }

        // Kill server if still running
        if let Some(mut child) = self.server_process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }

        // Clean up socket
        std::fs::remove_file(&self.socket_path).ok();
    }
}

#[test]
fn test_help() {
    Command::cargo_bin("botty")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("PTY-based agent runtime"))
        .stdout(predicate::str::contains("spawn"))
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("kill"));
}

#[test]
fn test_version() {
    Command::cargo_bin("botty")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("botty"));
}

#[test]
fn test_spawn_help() {
    Command::cargo_bin("botty")
        .unwrap()
        .args(["spawn", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Spawn a new agent"))
        .stdout(predicate::str::contains("--rows"))
        .stdout(predicate::str::contains("--cols"));
}

#[test]
fn test_spawn_list_kill_workflow() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn an agent
    let output = env
        .botty()
        .args(["spawn", "--", "sleep", "30"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success(), "spawn should succeed");
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(!agent_id.is_empty(), "should return agent ID");

    // List agents
    env.botty()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains(&agent_id))
        .stdout(predicate::str::contains("sleep 30"))
        .stdout(predicate::str::contains("running"));

    // Kill the agent
    env.botty()
        .args(["kill", &agent_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Signal sent"));

    // List should show exited (need --all to see exited agents)
    std::thread::sleep(Duration::from_millis(200));
    env.botty()
        .args(["list", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("exited"));
}

#[test]
fn test_send_and_snapshot() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn bash
    let output = env
        .botty()
        .args(["spawn", "--", "bash"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success());
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    std::thread::sleep(Duration::from_millis(200));

    // Send a command
    env.botty()
        .args(["send", &agent_id, "echo UNIQUE_TEST_STRING_12345"])
        .assert()
        .success();

    std::thread::sleep(Duration::from_millis(300));

    // Snapshot should contain our output
    env.botty()
        .args(["snapshot", &agent_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("UNIQUE_TEST_STRING_12345"));

    // Clean up
    env.botty()
        .args(["kill", "-9", &agent_id])
        .assert()
        .success();
}

#[test]
fn test_tail() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn something that produces output
    let output = env
        .botty()
        .args([
            "spawn",
            "--",
            "sh",
            "-c",
            "echo FIRST_LINE; echo SECOND_LINE; sleep 30",
        ])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success());
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    std::thread::sleep(Duration::from_millis(300));

    // Tail should show the output
    env.botty()
        .args(["tail", &agent_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("FIRST_LINE"))
        .stdout(predicate::str::contains("SECOND_LINE"));

    // Clean up
    env.botty()
        .args(["kill", "-9", &agent_id])
        .assert()
        .success();
}

#[test]
fn test_agent_not_found() {
    let mut env = TestEnv::new();
    env.start_server();

    // Try to snapshot a non-existent agent
    env.botty()
        .args(["snapshot", "nonexistent-agent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn test_spawn_requires_command() {
    Command::cargo_bin("botty")
        .unwrap()
        .args(["spawn", "--"])
        .assert()
        .failure();
}

#[test]
fn test_send_bytes_hex() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn bash
    let output = env
        .botty()
        .args(["spawn", "--", "bash"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success());
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    std::thread::sleep(Duration::from_millis(200));

    // Send "hi\n" as hex (68 69 0a)
    env.botty()
        .args(["send-bytes", &agent_id, "68690a"])
        .assert()
        .success();

    // Clean up
    env.botty()
        .args(["kill", "-9", &agent_id])
        .assert()
        .success();
}

#[test]
fn test_shutdown() {
    let mut env = TestEnv::new();
    env.start_server();

    // Shutdown should succeed
    env.botty()
        .arg("shutdown")
        .assert()
        .success()
        .stdout(predicate::str::contains("shutting down"));

    // Mark server as None so Drop doesn't try to shut it down again
    env.server_process = None;
}

#[test]
fn test_wait_for_content() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn a program that outputs text after a delay
    let output = env
        .botty()
        .args([
            "spawn",
            "--",
            "sh",
            "-c",
            "sleep 0.2; echo MARKER_READY; sleep 30",
        ])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success());
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Wait should succeed when the content appears
    env.botty()
        .args([
            "wait",
            &agent_id,
            "--contains",
            "MARKER_READY",
            "--timeout",
            "5",
            "--print",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MARKER_READY"));

    // Clean up
    env.botty()
        .args(["kill", "-9", &agent_id])
        .assert()
        .success();
}

#[test]
fn test_wait_timeout() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn a program that never outputs the expected content
    let output = env
        .botty()
        .args(["spawn", "--", "sleep", "30"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success());
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Wait should fail after timeout
    env.botty()
        .args([
            "wait",
            &agent_id,
            "--contains",
            "NEVER_APPEARS",
            "--timeout",
            "1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("timeout"));

    // Clean up
    env.botty()
        .args(["kill", "-9", &agent_id])
        .assert()
        .success();
}
