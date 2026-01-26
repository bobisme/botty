//! Test framework for agent orchestration scenarios.
//!
//! Provides ergonomic APIs for testing multi-agent TUI interactions:
//!
//! ```ignore
//! let harness = TestHarness::new().await;
//! let agent = harness.spawn(&["bash"]).await?;
//! 
//! agent.send("echo hello").await?;
//! agent.wait_for_content("hello", Duration::from_secs(5)).await?;
//! 
//! let snapshot = agent.snapshot().await?;
//! assert!(snapshot.contains("hello"));
//! ```

use crate::{Client, Request, Response, Server};
use regex::Regex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Errors from the test framework.
#[derive(Debug, Error)]
pub enum TestError {
    #[error("timeout waiting for condition")]
    Timeout,

    #[error("agent not found: {0}")]
    AgentNotFound(String),

    #[error("spawn failed: {0}")]
    SpawnFailed(String),

    #[error("request failed: {0}")]
    RequestFailed(String),

    #[error("server error: {0}")]
    ServerError(String),
}

/// Test harness that manages server lifecycle and provides agent spawning.
pub struct TestHarness {
    socket_path: PathBuf,
    client: Arc<Mutex<Client>>,
    server_handle: JoinHandle<()>,
}

impl TestHarness {
    /// Create a new test harness with a unique socket path.
    pub async fn new() -> Self {
        let socket_path = Self::unique_socket_path();
        
        // Start server in background
        let server_socket = socket_path.clone();
        let server_handle = tokio::spawn(async move {
            let mut server = Server::new(server_socket);
            let _ = server.run().await;
        });

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        let client = Client::new(socket_path.clone());

        Self {
            socket_path,
            client: Arc::new(Mutex::new(client)),
            server_handle,
        }
    }

    /// Generate a unique socket path for this test.
    fn unique_socket_path() -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        PathBuf::from(format!("/tmp/botty-test-{pid}-{id}.sock"))
    }

    /// Spawn a new agent with the given command.
    pub async fn spawn(&self, cmd: &[&str]) -> Result<AgentHandle, TestError> {
        self.spawn_with_size(cmd, 24, 80).await
    }

    /// Spawn a new agent with custom terminal size.
    pub async fn spawn_with_size(
        &self,
        cmd: &[&str],
        rows: u16,
        cols: u16,
    ) -> Result<AgentHandle, TestError> {
        let request = Request::Spawn {
            cmd: cmd.iter().map(std::string::ToString::to_string).collect(),
            rows,
            cols,
            name: None,
            labels: vec![],
            timeout: None,
            max_output: None,
            env: vec![],
            env_clear: false,
        };

        let response = self
            .client
            .lock()
            .await
            .request(request)
            .await
            .map_err(|e| TestError::RequestFailed(e.to_string()))?;

        match response {
            Response::Spawned { id, .. } => Ok(AgentHandle {
                id,
                client: Arc::clone(&self.client),
            }),
            Response::Error { message } => Err(TestError::SpawnFailed(message)),
            _ => Err(TestError::SpawnFailed("unexpected response".into())),
        }
    }

    /// List all agents.
    pub async fn list(&self) -> Result<Vec<String>, TestError> {
        let response = self
            .client
            .lock()
            .await
            .request(Request::List { labels: vec![] })
            .await
            .map_err(|e| TestError::RequestFailed(e.to_string()))?;

        match response {
            Response::Agents { agents } => Ok(agents.into_iter().map(|a| a.id).collect()),
            Response::Error { message } => Err(TestError::RequestFailed(message)),
            _ => Err(TestError::RequestFailed("unexpected response".into())),
        }
    }

    /// Get the socket path (useful for direct connections).
    #[must_use] 
    pub const fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    /// Shutdown the server gracefully.
    pub async fn shutdown(self) {
        let _ = self.client.lock().await.request(Request::Shutdown).await;
        self.server_handle.abort();
        // Clean up socket file
        std::fs::remove_file(&self.socket_path).ok();
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        // Best effort cleanup
        std::fs::remove_file(&self.socket_path).ok();
    }
}

/// Handle for interacting with a spawned agent.
#[derive(Clone)]
pub struct AgentHandle {
    id: String,
    client: Arc<Mutex<Client>>,
}

impl AgentHandle {
    /// Get the agent ID.
    #[must_use] 
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Send text input to the agent (with newline).
    pub async fn send(&self, text: &str) -> Result<(), TestError> {
        self.send_raw(text, true).await
    }

    /// Send text input without trailing newline.
    pub async fn send_no_newline(&self, text: &str) -> Result<(), TestError> {
        self.send_raw(text, false).await
    }

    /// Send text with explicit newline control.
    async fn send_raw(&self, text: &str, newline: bool) -> Result<(), TestError> {
        let request = Request::Send {
            id: self.id.clone(),
            data: text.to_string(),
            newline,
        };

        let response = self
            .client
            .lock()
            .await
            .request(request)
            .await
            .map_err(|e| TestError::RequestFailed(e.to_string()))?;

        match response {
            Response::Ok => Ok(()),
            Response::Error { message } => {
                if message.contains("not found") {
                    Err(TestError::AgentNotFound(self.id.clone()))
                } else {
                    Err(TestError::RequestFailed(message))
                }
            }
            _ => Err(TestError::RequestFailed("unexpected response".into())),
        }
    }

    /// Send raw bytes to the agent.
    pub async fn send_bytes(&self, data: &[u8]) -> Result<(), TestError> {
        let request = Request::SendBytes {
            id: self.id.clone(),
            data: data.to_vec(),
        };

        let response = self
            .client
            .lock()
            .await
            .request(request)
            .await
            .map_err(|e| TestError::RequestFailed(e.to_string()))?;

        match response {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(TestError::RequestFailed(message)),
            _ => Err(TestError::RequestFailed("unexpected response".into())),
        }
    }

    /// Get a snapshot of the agent's screen.
    pub async fn snapshot(&self) -> Result<String, TestError> {
        let request = Request::Snapshot {
            id: self.id.clone(),
            strip_colors: true,
        };

        let response = self
            .client
            .lock()
            .await
            .request(request)
            .await
            .map_err(|e| TestError::RequestFailed(e.to_string()))?;

        match response {
            Response::Snapshot { content, .. } => Ok(content),
            Response::Error { message } => {
                if message.contains("not found") {
                    Err(TestError::AgentNotFound(self.id.clone()))
                } else {
                    Err(TestError::RequestFailed(message))
                }
            }
            _ => Err(TestError::RequestFailed("unexpected response".into())),
        }
    }

    /// Wait until the screen contains the given substring.
    pub async fn wait_for_content(
        &self,
        needle: &str,
        timeout_duration: Duration,
    ) -> Result<String, TestError> {
        let deadline = Instant::now() + timeout_duration;
        let poll_interval = Duration::from_millis(50);

        while Instant::now() < deadline {
            let snapshot = self.snapshot().await?;
            if snapshot.contains(needle) {
                return Ok(snapshot);
            }
            tokio::time::sleep(poll_interval).await;
        }

        Err(TestError::Timeout)
    }

    /// Wait until the screen matches the given regex pattern.
    pub async fn wait_for_pattern(
        &self,
        pattern: &str,
        timeout_duration: Duration,
    ) -> Result<String, TestError> {
        let re = Regex::new(pattern).map_err(|e| TestError::RequestFailed(e.to_string()))?;
        let deadline = Instant::now() + timeout_duration;
        let poll_interval = Duration::from_millis(50);

        while Instant::now() < deadline {
            let snapshot = self.snapshot().await?;
            if re.is_match(&snapshot) {
                return Ok(snapshot);
            }
            tokio::time::sleep(poll_interval).await;
        }

        Err(TestError::Timeout)
    }

    /// Wait until the screen hasn't changed for the given duration.
    ///
    /// Useful for waiting for TUI animations/rendering to complete.
    pub async fn wait_for_stable(
        &self,
        stable_duration: Duration,
        timeout_duration: Duration,
    ) -> Result<String, TestError> {
        let deadline = Instant::now() + timeout_duration;
        let poll_interval = Duration::from_millis(50);

        let mut last_snapshot = self.snapshot().await?;
        let mut stable_since = Instant::now();

        while Instant::now() < deadline {
            tokio::time::sleep(poll_interval).await;

            let current = self.snapshot().await?;
            if current == last_snapshot {
                if stable_since.elapsed() >= stable_duration {
                    return Ok(current);
                }
            } else {
                last_snapshot = current;
                stable_since = Instant::now();
            }
        }

        Err(TestError::Timeout)
    }

    /// Wait for a shell prompt (common patterns like $, >, #).
    ///
    /// This uses a heuristic pattern that matches common shell prompts.
    /// For custom prompts, use `wait_for_pattern` or `wait_for_prompt_custom`.
    pub async fn wait_for_prompt(&self, timeout_duration: Duration) -> Result<String, TestError> {
        // Common shell prompts: ends with $, #, >, or % followed by optional whitespace
        // Also matches things like "user@host:~$ " or "(venv) $ "
        self.wait_for_pattern(r"[$#>%]\s*$", timeout_duration).await
    }

    /// Wait for a custom prompt pattern.
    pub async fn wait_for_prompt_custom(
        &self,
        prompt_pattern: &str,
        timeout_duration: Duration,
    ) -> Result<String, TestError> {
        self.wait_for_pattern(prompt_pattern, timeout_duration).await
    }

    /// Wait for content to NOT be present (useful for waiting for spinners/loading to finish).
    pub async fn wait_for_absence(
        &self,
        needle: &str,
        timeout_duration: Duration,
    ) -> Result<String, TestError> {
        let deadline = Instant::now() + timeout_duration;
        let poll_interval = Duration::from_millis(50);

        while Instant::now() < deadline {
            let snapshot = self.snapshot().await?;
            if !snapshot.contains(needle) {
                return Ok(snapshot);
            }
            tokio::time::sleep(poll_interval).await;
        }

        Err(TestError::Timeout)
    }

    /// Check if the screen currently contains the given text.
    pub async fn contains(&self, needle: &str) -> Result<bool, TestError> {
        let snapshot = self.snapshot().await?;
        Ok(snapshot.contains(needle))
    }

    /// Kill the agent.
    pub async fn kill(&self) -> Result<(), TestError> {
        self.signal(9).await
    }

    /// Send a signal to the agent.
    pub async fn signal(&self, signal: i32) -> Result<(), TestError> {
        let request = Request::Kill {
            id: Some(self.id.clone()),
            labels: vec![],
            signal,
        };

        let response = self
            .client
            .lock()
            .await
            .request(request)
            .await
            .map_err(|e| TestError::RequestFailed(e.to_string()))?;

        match response {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(TestError::RequestFailed(message)),
            _ => Err(TestError::RequestFailed("unexpected response".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_harness_spawn_and_snapshot() {
        let harness = TestHarness::new().await;

        let agent = harness
            .spawn(&["sh", "-c", "echo HARNESS_TEST; sleep 10"])
            .await
            .expect("spawn failed");

        // Wait for output
        let snapshot = agent
            .wait_for_content("HARNESS_TEST", Duration::from_secs(5))
            .await
            .expect("wait failed");

        assert!(snapshot.contains("HARNESS_TEST"));

        agent.kill().await.expect("kill failed");
        harness.shutdown().await;
    }

    #[tokio::test]
    async fn test_harness_wait_for_stable() {
        let harness = TestHarness::new().await;

        // Spawn something that produces output then stops
        let agent = harness
            .spawn(&["sh", "-c", "echo LINE1; echo LINE2; sleep 10"])
            .await
            .expect("spawn failed");

        // Wait for screen to stabilize
        let snapshot = agent
            .wait_for_stable(Duration::from_millis(200), Duration::from_secs(5))
            .await
            .expect("wait failed");

        assert!(snapshot.contains("LINE1"));
        assert!(snapshot.contains("LINE2"));

        agent.kill().await.expect("kill failed");
        harness.shutdown().await;
    }

    #[tokio::test]
    async fn test_harness_multiple_agents() {
        let harness = TestHarness::new().await;

        // Spawn two agents
        let agent1 = harness
            .spawn(&["sh", "-c", "echo AGENT_ONE; sleep 10"])
            .await
            .expect("spawn 1 failed");

        let agent2 = harness
            .spawn(&["sh", "-c", "echo AGENT_TWO; sleep 10"])
            .await
            .expect("spawn 2 failed");

        // Wait for both
        agent1
            .wait_for_content("AGENT_ONE", Duration::from_secs(5))
            .await
            .expect("wait 1 failed");

        agent2
            .wait_for_content("AGENT_TWO", Duration::from_secs(5))
            .await
            .expect("wait 2 failed");

        // Verify list shows both
        let agents = harness.list().await.expect("list failed");
        assert_eq!(agents.len(), 2);

        agent1.kill().await.ok();
        agent2.kill().await.ok();
        harness.shutdown().await;
    }

    #[tokio::test]
    async fn test_harness_send_and_receive() {
        let harness = TestHarness::new().await;

        let agent = harness.spawn(&["bash"]).await.expect("spawn failed");

        // Wait for prompt
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Send command
        agent.send("echo INTERACTIVE_TEST").await.expect("send failed");

        // Wait for output
        let snapshot = agent
            .wait_for_content("INTERACTIVE_TEST", Duration::from_secs(5))
            .await
            .expect("wait failed");

        assert!(snapshot.contains("INTERACTIVE_TEST"));

        agent.kill().await.ok();
        harness.shutdown().await;
    }
}
