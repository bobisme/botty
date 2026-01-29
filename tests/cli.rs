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
    env.botty().args(["kill", &agent_id]).assert().success();
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
    env.botty().args(["kill", &agent_id]).assert().success();
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
    env.botty().args(["kill", &agent_id]).assert().success();
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
    env.botty().args(["kill", &agent_id]).assert().success();
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
    env.botty().args(["kill", &agent_id]).assert().success();
}

#[test]
fn test_spawn_with_custom_name() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn with custom name
    let output = env
        .botty()
        .args(["spawn", "--name", "my-worker", "--", "sleep", "30"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success(), "spawn should succeed");
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(agent_id, "my-worker", "should return the custom name");

    // List should show the custom name
    env.botty()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("my-worker"));

    // Clean up
    env.botty().args(["kill", "my-worker"]).assert().success();
}

#[test]
fn test_spawn_duplicate_name_fails() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn first agent with custom name
    let output = env
        .botty()
        .args(["spawn", "--name", "unique-name", "--", "sleep", "30"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success(), "first spawn should succeed");

    // Try to spawn second agent with same name - should fail
    env.botty()
        .args(["spawn", "--name", "unique-name", "--", "sleep", "30"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already in use"));

    // Clean up
    env.botty().args(["kill", "unique-name"]).assert().success();
}

#[test]
fn test_exec_command() {
    let mut env = TestEnv::new();
    env.start_server();

    // Execute a simple command
    env.botty()
        .args(["exec", "--timeout", "5", "--", "echo", "EXEC_TEST_OUTPUT"])
        .assert()
        .success()
        .stdout(predicate::str::contains("EXEC_TEST_OUTPUT"));
}

#[test]
fn test_exec_multiline_output() {
    let mut env = TestEnv::new();
    env.start_server();

    // Execute a command with multiple lines of output
    env.botty()
        .args([
            "exec",
            "--timeout",
            "5",
            "--",
            "echo first; echo second; echo third",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("first"))
        .stdout(predicate::str::contains("second"))
        .stdout(predicate::str::contains("third"));
}

#[test]
fn test_exec_exit_code_propagation() {
    let mut env = TestEnv::new();
    env.start_server();

    // Execute a failing command - should propagate exit code
    env.botty()
        .args(["exec", "--timeout", "5", "--", "false"])
        .assert()
        .failure()
        .code(1);

    // Execute a command that fails with code 2
    env.botty()
        .args(["exec", "--timeout", "5", "--", "ls /nonexistent_path_12345"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn test_kill_idempotent() {
    let mut env = TestEnv::new();
    env.start_server();

    // Killing a non-existent agent should succeed (like rm -f, pkill)
    env.botty()
        .args(["kill", "nonexistent-agent"])
        .assert()
        .success();

    // Kill --all with no agents should also succeed
    env.botty()
        .args(["kill", "--all"])
        .assert()
        .success();

    // Spawn an agent, kill it, then kill it again (should be idempotent)
    let output = env
        .botty()
        .args(["spawn", "--", "sleep", "100"])
        .output()
        .expect("failed to run spawn");
    assert!(output.status.success());
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // First kill succeeds
    env.botty().args(["kill", &agent_id]).assert().success();

    // Give it time to exit
    std::thread::sleep(Duration::from_millis(100));

    // Second kill should also succeed (idempotent)
    env.botty()
        .args(["kill", &agent_id])
        .assert()
        .success();
}

#[test]
fn test_send_key() {
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

    // Send arrow keys and special keys - should all succeed
    env.botty()
        .args(["send-key", &agent_id, "up"])
        .assert()
        .success();

    env.botty()
        .args(["send-key", &agent_id, "down"])
        .assert()
        .success();

    env.botty()
        .args(["send-key", &agent_id, "enter"])
        .assert()
        .success();

    env.botty()
        .args(["send-key", &agent_id, "tab"])
        .assert()
        .success();

    env.botty()
        .args(["send-key", &agent_id, "ctrl-c"])
        .assert()
        .success();

    // Invalid key name should fail
    env.botty()
        .args(["send-key", &agent_id, "invalid-key"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown key"));

    // Clean up
    env.botty().args(["kill", &agent_id]).assert().success();
}

#[test]
fn test_wait_combined_conditions() {
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

    std::thread::sleep(Duration::from_millis(300));

    // Test 1: Wait with --stable alone should work
    env.botty()
        .args(["wait", &agent_id, "--stable", "100", "--timeout", "5"])
        .assert()
        .success();

    // Test 2: Send some output and wait for it with --contains alone
    env.botty()
        .args(["send", &agent_id, "echo test123"])
        .assert()
        .success();

    env.botty()
        .args(["wait", &agent_id, "--contains", "test123", "--timeout", "5"])
        .assert()
        .success();

    // Test 3: Combined --stable AND --contains
    // Send a command and wait for both stable screen AND specific content
    env.botty()
        .args(["send", &agent_id, "echo hello-combined"])
        .assert()
        .success();

    env.botty()
        .args([
            "wait",
            &agent_id,
            "--stable",
            "100",
            "--contains",
            "hello-combined",
            "--timeout",
            "5",
        ])
        .assert()
        .success();

    // Clean up
    env.botty().args(["kill", &agent_id]).assert().success();
}
