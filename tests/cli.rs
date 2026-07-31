//! End-to-end CLI tests using assert_cmd.
//!
//! These tests run the actual vessel binary and verify stdout/stderr/exit codes.

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
    PathBuf::from(format!("/tmp/vessel-cli-test-{pid}-{id}.sock"))
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
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_vessel"))
            .arg(&self.socket_arg())
            .arg("server")
            .spawn()
            .expect("failed to start server");
        self.server_process = Some(child);
        // Give server time to start
        std::thread::sleep(Duration::from_millis(200));
    }

    fn vessel(&self) -> Command {
        let mut cmd = Command::cargo_bin("vessel").unwrap();
        cmd.arg(&self.socket_arg());
        cmd
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // Try to shut down the server gracefully
        if self.server_process.is_some() {
            let _ = std::process::Command::new(env!("CARGO_BIN_EXE_vessel"))
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
    Command::cargo_bin("vessel")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("PTY-based runtime"))
        .stdout(predicate::str::contains("spawn"))
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("kill"));
}

#[test]
fn test_version() {
    Command::cargo_bin("vessel")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("vessel"));
}

#[test]
fn test_spawn_help() {
    Command::cargo_bin("vessel")
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
        .vessel()
        .args(["spawn", "--", "sleep", "30"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success(), "spawn should succeed");
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(!agent_id.is_empty(), "should return agent ID");

    // List agents
    env.vessel()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains(&agent_id))
        .stdout(predicate::str::contains("sleep 30"))
        .stdout(predicate::str::contains("running"));

    // Kill the agent
    env.vessel()
        .args(["kill", &agent_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Signal sent"));

    // List should show exited (need --all to see exited agents)
    std::thread::sleep(Duration::from_millis(200));
    env.vessel()
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
        .vessel()
        .args(["spawn", "--", "bash"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success());
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    std::thread::sleep(Duration::from_millis(200));

    // Send a command
    env.vessel()
        .args([
            "send",
            &agent_id,
            "echo UNIQUE_TEST_STRING_12345",
            "--newline",
        ])
        .assert()
        .success();

    std::thread::sleep(Duration::from_millis(300));

    // Snapshot should contain our output
    env.vessel()
        .args(["snapshot", &agent_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("UNIQUE_TEST_STRING_12345"));

    // Clean up
    env.vessel().args(["kill", &agent_id]).assert().success();
}

// bn-1a0f: "-" reads the payload from stdin, so a long prompt does not have to
// be quoted onto the command line.
#[test]
fn test_send_reads_text_from_stdin() {
    let mut env = TestEnv::new();
    env.start_server();

    let output = env
        .vessel()
        .args(["spawn", "--", "bash"])
        .output()
        .expect("failed to run spawn");
    assert!(output.status.success());
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    std::thread::sleep(Duration::from_millis(200));

    env.vessel()
        .args(["send", &agent_id, "-", "--newline"])
        .write_stdin("echo STDIN_SOURCED_PROMPT")
        .assert()
        .success();

    std::thread::sleep(Duration::from_millis(400));

    env.vessel()
        .args(["snapshot", &agent_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("STDIN_SOURCED_PROMPT"));

    env.vessel().args(["kill", &agent_id]).assert().success();
}

// bn-1dxu: --label fans out to every matching agent, and send-keys takes the
// same selectors. With a selector the agent ID is omitted, so every positional
// is payload.
#[test]
fn test_send_label_fans_out_to_matching_agents() {
    let mut env = TestEnv::new();
    env.start_server();

    let mut ids = Vec::new();
    for _ in 0..2 {
        let output = env
            .vessel()
            .args(["spawn", "--label", "fanout", "--", "bash"])
            .output()
            .expect("failed to run spawn");
        assert!(output.status.success());
        ids.push(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    // An agent outside the label, to prove the selector actually filters.
    let output = env
        .vessel()
        .args(["spawn", "--label", "excluded", "--", "bash"])
        .output()
        .expect("failed to run spawn");
    assert!(output.status.success());
    let outsider = String::from_utf8_lossy(&output.stdout).trim().to_string();

    std::thread::sleep(Duration::from_millis(300));

    // Note: no agent ID positional, because --label was given.
    env.vessel()
        .args([
            "send",
            "echo CLI_FANOUT_MARKER",
            "--label",
            "fanout",
            "--newline",
            "--submit-delay-ms",
            "0",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("  ok"));

    std::thread::sleep(Duration::from_millis(600));

    for id in &ids {
        env.vessel()
            .args(["snapshot", id])
            .assert()
            .success()
            .stdout(predicate::str::contains("CLI_FANOUT_MARKER"));
    }

    env.vessel()
        .args(["snapshot", &outsider])
        .assert()
        .success()
        .stdout(predicate::str::contains("CLI_FANOUT_MARKER").not());

    env.vessel().args(["kill", "--all"]).assert().success();
}

// bn-1dxu: a selector that matches nothing must fail loudly.
#[test]
fn test_send_label_no_match_exits_nonzero() {
    let mut env = TestEnv::new();
    env.start_server();

    env.vessel()
        .args(["send", "hello", "--label", "nobody-has-this", "--newline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no agents match"));
}

// bn-1dxu: an explicit ID alongside a selector is ambiguous and is rejected
// rather than guessed at.
#[test]
fn test_send_rejects_id_together_with_selector() {
    let mut env = TestEnv::new();
    env.start_server();

    env.vessel()
        .args(["send", "some-agent", "hello", "--label", "batch"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not both"));
}

// bn-1dxu: send-keys with a selector -- the variadic case, where the key that
// clap parked in the ID slot has to move back to the front of the key list.
#[test]
fn test_send_keys_label_fans_out() {
    let mut env = TestEnv::new();
    env.start_server();

    let mut ids = Vec::new();
    for _ in 0..2 {
        let output = env
            .vessel()
            .args(["spawn", "--label", "keyfan", "--", "bash"])
            .output()
            .expect("failed to run spawn");
        assert!(output.status.success());
        ids.push(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    std::thread::sleep(Duration::from_millis(300));

    // Type the text, then submit it -- two keys, no agent ID positional.
    env.vessel()
        .args(["send", "echo KEYFAN_MARKER", "--label", "keyfan"])
        .assert()
        .success();

    std::thread::sleep(Duration::from_millis(200));

    env.vessel()
        .args(["send-keys", "enter", "--label", "keyfan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("  ok"));

    std::thread::sleep(Duration::from_millis(600));

    for id in &ids {
        env.vessel()
            .args(["snapshot", id])
            .assert()
            .success()
            .stdout(predicate::str::contains("KEYFAN_MARKER"));
    }

    env.vessel().args(["kill", "--all"]).assert().success();
}

#[test]
fn test_tail() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn something that produces output
    let output = env
        .vessel()
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
    env.vessel()
        .args(["tail", &agent_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("FIRST_LINE"))
        .stdout(predicate::str::contains("SECOND_LINE"));

    // Clean up
    env.vessel().args(["kill", &agent_id]).assert().success();
}

#[test]
fn test_agent_not_found() {
    let mut env = TestEnv::new();
    env.start_server();

    // Try to snapshot a non-existent agent
    env.vessel()
        .args(["snapshot", "nonexistent-agent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn test_spawn_requires_command() {
    Command::cargo_bin("vessel")
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
        .vessel()
        .args(["spawn", "--", "bash"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success());
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    std::thread::sleep(Duration::from_millis(200));

    // Send "hi\n" as hex (68 69 0a)
    env.vessel()
        .args(["send-bytes", &agent_id, "68690a"])
        .assert()
        .success();

    // Clean up
    env.vessel().args(["kill", &agent_id]).assert().success();
}

#[test]
fn test_shutdown() {
    let mut env = TestEnv::new();
    env.start_server();

    // Shutdown should succeed
    env.vessel()
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
        .vessel()
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
    env.vessel()
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
    env.vessel().args(["kill", &agent_id]).assert().success();
}

#[test]
fn test_wait_timeout() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn a program that never outputs the expected content
    let output = env
        .vessel()
        .args(["spawn", "--", "sleep", "30"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success());
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Wait should fail after timeout
    env.vessel()
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
    env.vessel().args(["kill", &agent_id]).assert().success();
}

#[test]
fn test_spawn_with_custom_name() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn with custom name
    let output = env
        .vessel()
        .args(["spawn", "--name", "my-worker", "--", "sleep", "30"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success(), "spawn should succeed");
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(agent_id, "my-worker", "should return the custom name");

    // List should show the custom name
    env.vessel()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("my-worker"));

    // Clean up
    env.vessel().args(["kill", "my-worker"]).assert().success();
}

#[test]
fn test_spawn_duplicate_name_fails() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn first agent with custom name
    let output = env
        .vessel()
        .args(["spawn", "--name", "unique-name", "--", "sleep", "30"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success(), "first spawn should succeed");

    // Try to spawn second agent with same name - should fail
    env.vessel()
        .args(["spawn", "--name", "unique-name", "--", "sleep", "30"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already in use"));

    // Clean up
    env.vessel()
        .args(["kill", "unique-name"])
        .assert()
        .success();
}

#[test]
fn test_exec_command() {
    let mut env = TestEnv::new();
    env.start_server();

    // Execute a simple command
    env.vessel()
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
    env.vessel()
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
    env.vessel()
        .args(["exec", "--timeout", "5", "--", "false"])
        .assert()
        .failure()
        .code(1);

    // Execute a command that fails with code 2
    env.vessel()
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
    env.vessel()
        .args(["kill", "nonexistent-agent"])
        .assert()
        .success();

    // Kill --all with no agents should also succeed
    env.vessel().args(["kill", "--all"]).assert().success();

    // Spawn an agent, kill it, then kill it again (should be idempotent)
    let output = env
        .vessel()
        .args(["spawn", "--", "sleep", "100"])
        .output()
        .expect("failed to run spawn");
    assert!(output.status.success());
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // First kill succeeds
    env.vessel().args(["kill", &agent_id]).assert().success();

    // Give it time to exit
    std::thread::sleep(Duration::from_millis(100));

    // Second kill should also succeed (idempotent)
    env.vessel().args(["kill", &agent_id]).assert().success();
}

#[test]
fn test_send_key() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn bash
    let output = env
        .vessel()
        .args(["spawn", "--", "bash"])
        .output()
        .expect("failed to run spawn");
    assert!(output.status.success());
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    std::thread::sleep(Duration::from_millis(200));

    // Send single keys - should all succeed
    env.vessel()
        .args(["send-keys", &agent_id, "up"])
        .assert()
        .success();

    env.vessel()
        .args(["send-keys", &agent_id, "down"])
        .assert()
        .success();

    env.vessel()
        .args(["send-keys", &agent_id, "enter"])
        .assert()
        .success();

    env.vessel()
        .args(["send-keys", &agent_id, "tab"])
        .assert()
        .success();

    env.vessel()
        .args(["send-keys", &agent_id, "ctrl-c"])
        .assert()
        .success();

    // Send multiple keys at once
    env.vessel()
        .args(["send-keys", &agent_id, "up", "down", "enter"])
        .assert()
        .success();

    // Invalid key name should fail
    env.vessel()
        .args(["send-keys", &agent_id, "invalid-key"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown key"));

    // Clean up
    env.vessel().args(["kill", &agent_id]).assert().success();
}

#[test]
fn test_wait_combined_conditions() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn bash
    let output = env
        .vessel()
        .args(["spawn", "--", "bash"])
        .output()
        .expect("failed to run spawn");
    assert!(output.status.success());
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    std::thread::sleep(Duration::from_millis(300));

    // Test 1: Wait with --stable alone should work
    env.vessel()
        .args(["wait", &agent_id, "--stable", "100", "--timeout", "5"])
        .assert()
        .success();

    // Test 2: Send some output and wait for it with --contains alone
    env.vessel()
        .args(["send", &agent_id, "echo test123", "--newline"])
        .assert()
        .success();

    env.vessel()
        .args(["wait", &agent_id, "--contains", "test123", "--timeout", "5"])
        .assert()
        .success();

    // Test 3: Combined --stable AND --contains
    // Send a command and wait for both stable screen AND specific content
    env.vessel()
        .args(["send", &agent_id, "echo hello-combined", "--newline"])
        .assert()
        .success();

    env.vessel()
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
    env.vessel().args(["kill", &agent_id]).assert().success();
}

// ============================================================================
// wait --exited tests
// ============================================================================

#[test]
fn test_wait_exited() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn a short-lived command that exits 0
    let output = env
        .vessel()
        .args([
            "spawn",
            "--name",
            "exit-ok",
            "--",
            "sh",
            "-c",
            "echo hello; exit 0",
        ])
        .output()
        .expect("failed to run spawn");
    assert!(output.status.success());

    // Wait for it to exit
    env.vessel()
        .args(["wait", "--exited", "exit-ok", "--timeout", "10"])
        .assert()
        .success()
        .code(0);
}

#[test]
fn test_wait_exited_nonzero() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn a command that exits with code 42
    let output = env
        .vessel()
        .args(["spawn", "--name", "exit-42", "--", "sh", "-c", "exit 42"])
        .output()
        .expect("failed to run spawn");
    assert!(output.status.success());

    // Wait for it to exit - should propagate exit code 42
    env.vessel()
        .args(["wait", "--exited", "exit-42", "--timeout", "10"])
        .assert()
        .failure()
        .code(42);
}

#[test]
fn test_wait_exited_already_exited() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn a very short-lived command
    let output = env
        .vessel()
        .args(["spawn", "--name", "already-done", "--", "true"])
        .output()
        .expect("failed to run spawn");
    assert!(output.status.success());

    // Wait a bit so it definitely exits
    std::thread::sleep(Duration::from_millis(500));

    // wait --exited should return immediately since it already exited
    env.vessel()
        .args(["wait", "--exited", "already-done", "--timeout", "5"])
        .assert()
        .success()
        .code(0);
}

#[test]
fn test_wait_exited_timeout() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn a long-running command
    let output = env
        .vessel()
        .args(["spawn", "--name", "long-run", "--", "sleep", "999"])
        .output()
        .expect("failed to run spawn");
    assert!(output.status.success());

    // Wait with a very short timeout - should fail
    env.vessel()
        .args(["wait", "--exited", "long-run", "--timeout", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("timeout"));

    // Clean up
    env.vessel().args(["kill", "long-run"]).assert().success();
}

#[test]
fn test_wait_exited_any_returns_first_exit_and_prints_id() {
    let mut env = TestEnv::new();
    env.start_server();

    env.vessel()
        .args([
            "spawn",
            "--name",
            "fast-exit",
            "--",
            "sh",
            "-c",
            "sleep 0.2; exit 0",
        ])
        .assert()
        .success();

    env.vessel()
        .args(["spawn", "--name", "slow-exit", "--", "sleep", "999"])
        .assert()
        .success();

    env.vessel()
        .args([
            "wait",
            "--exited",
            "--any",
            "fast-exit",
            "slow-exit",
            "--timeout",
            "10",
        ])
        .assert()
        .success()
        .stdout(predicate::str::diff("fast-exit\n"));

    env.vessel().args(["kill", "slow-exit"]).assert().success();
}

#[test]
fn test_wait_exited_any_reports_all_already_exited_agents() {
    let mut env = TestEnv::new();
    env.start_server();

    env.vessel()
        .args(["spawn", "--name", "done-ok", "--", "true"])
        .assert()
        .success();

    env.vessel()
        .args(["spawn", "--name", "done-bad", "--", "sh", "-c", "exit 7"])
        .assert()
        .success();

    std::thread::sleep(Duration::from_millis(500));

    env.vessel()
        .args([
            "wait",
            "--exited",
            "--any",
            "done-ok",
            "done-bad",
            "--timeout",
            "5",
        ])
        .assert()
        .failure()
        .code(7)
        .stdout(predicate::str::diff("done-ok\ndone-bad\n"));
}

#[test]
fn test_wait_any_requires_exited() {
    let mut env = TestEnv::new();
    env.start_server();

    env.vessel()
        .args([
            "spawn",
            "--name",
            "wait-any-no-exited",
            "--",
            "sleep",
            "999",
        ])
        .assert()
        .success();

    env.vessel()
        .args(["wait", "--any", "wait-any-no-exited", "--timeout", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--any requires --exited"));

    env.vessel()
        .args(["kill", "wait-any-no-exited"])
        .assert()
        .success();
}

// ============================================================================
// Slash in agent name tests
// ============================================================================

#[test]
fn test_spawn_slash_name() {
    let mut env = TestEnv::new();
    env.start_server();

    // Spawn with a slash in the name
    let output = env
        .vessel()
        .args(["spawn", "--name", "parent/child", "--", "sleep", "30"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success(), "spawn should succeed");
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(agent_id, "parent/child", "should return the slash name");

    // List should show the slash name
    env.vessel()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("parent/child"));

    // Kill by slash name should work
    env.vessel()
        .args(["kill", "parent/child"])
        .assert()
        .success();
}

#[test]
fn test_spawn_slash_name_invalid() {
    let mut env = TestEnv::new();
    env.start_server();

    // Leading slash should be rejected
    env.vessel()
        .args(["spawn", "--name", "/leading", "--", "sleep", "30"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must not start/end with '/'"));

    // Trailing slash should be rejected
    env.vessel()
        .args(["spawn", "--name", "trailing/", "--", "sleep", "30"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must not start/end with '/'"));

    // Double slash should be rejected
    env.vessel()
        .args(["spawn", "--name", "a//b", "--", "sleep", "30"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "must not start/end with '/' or contain '//'",
        ));
}

#[test]
fn test_spawn_multi_level_slash() {
    let mut env = TestEnv::new();
    env.start_server();

    // Multi-level slash names should work
    let output = env
        .vessel()
        .args(["spawn", "--name", "a/b/c", "--", "sleep", "30"])
        .output()
        .expect("failed to run spawn");

    assert!(output.status.success(), "spawn should succeed");
    let agent_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(agent_id, "a/b/c");

    // Snapshot should work with slash names
    std::thread::sleep(Duration::from_millis(200));
    env.vessel().args(["snapshot", "a/b/c"]).assert().success();

    // Clean up
    env.vessel().args(["kill", "a/b/c"]).assert().success();
}
