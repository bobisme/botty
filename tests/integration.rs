//! Integration tests for botty server/client IPC.
//!
//! Each test uses a unique socket path to avoid conflicts.

use botty::protocol::{AgentState, AttachEndReason};
use botty::{Client, Request, Response, Server};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Generate a unique socket path for each test.
fn unique_socket_path() -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    PathBuf::from(format!("/tmp/botty-test-{pid}-{id}.sock"))
}

/// Helper to clean up socket after test.
struct SocketCleanup(PathBuf);

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

#[tokio::test]
async fn test_server_ping_pong() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());

    // Start server in background
    let server_socket = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(server_socket);
        server.run().await
    });

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Connect and ping
    let mut client = Client::new(socket_path);
    let response = timeout(Duration::from_secs(5), client.request(Request::Ping))
        .await
        .expect("timeout")
        .expect("request failed");

    assert!(matches!(response, Response::Pong));

    // Shutdown
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}

#[tokio::test]
async fn test_spawn_and_list() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());

    // Start server
    let server_socket = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(server_socket);
        server.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = Client::new(socket_path);

    // Spawn an agent
    let response = client
        .request(Request::Spawn {
            cmd: vec!["sleep".into(), "10".into()],
            rows: 24,
            cols: 80,
            name: None,
            labels: vec![],
            timeout: None,
            max_output: None,
            env: vec![],
            env_clear: false,
        })
        .await
        .expect("spawn failed");

    let agent_id = match response {
        Response::Spawned { id, pid } => {
            assert!(pid > 0);
            id
        }
        other => panic!("expected Spawned, got {:?}", other),
    };

    // List agents
    let response = client.request(Request::List { labels: vec![] }).await.expect("list failed");

    match response {
        Response::Agents { agents } => {
            assert_eq!(agents.len(), 1);
            assert_eq!(agents[0].id, agent_id);
            assert_eq!(agents[0].command, vec!["sleep", "10"]);
        }
        other => panic!("expected Agents, got {:?}", other),
    }

    // Kill the agent
    let response = client
        .request(Request::Kill {
            id: Some(agent_id),
            labels: vec![],
            all: false,
            signal: 15,
            proc_filter: None,
        })
        .await
        .expect("kill failed");

    assert!(matches!(response, Response::Ok));

    // Shutdown
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}

#[tokio::test]
async fn test_spawn_send_snapshot() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());

    // Start server
    let server_socket = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(server_socket);
        server.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = Client::new(socket_path);

    // Spawn bash
    let response = client
        .request(Request::Spawn {
            cmd: vec!["bash".into()],
            rows: 24,
            cols: 80,
            name: None,
            labels: vec![],
            timeout: None,
            max_output: None,
            env: vec![],
            env_clear: false,
        })
        .await
        .expect("spawn failed");

    let agent_id = match response {
        Response::Spawned { id, .. } => id,
        other => panic!("expected Spawned, got {:?}", other),
    };

    // Give bash time to start
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Send a command
    let response = client
        .request(Request::Send {
            id: agent_id.clone(),
            data: "echo BOTTY_TEST_OUTPUT".into(),
            newline: true,
        })
        .await
        .expect("send failed");

    assert!(matches!(response, Response::Ok));

    // Wait for command to execute
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Get snapshot
    let response = client
        .request(Request::Snapshot {
            id: agent_id.clone(),
            strip_colors: true,
        })
        .await
        .expect("snapshot failed");

    match response {
        Response::Snapshot { content, .. } => {
            assert!(
                content.contains("BOTTY_TEST_OUTPUT"),
                "snapshot should contain our output: {}",
                content
            );
        }
        other => panic!("expected Snapshot, got {:?}", other),
    }

    // Kill and shutdown
    let _ = client
        .request(Request::Kill {
            id: Some(agent_id),
            labels: vec![],
            all: false,
            signal: 9,
            proc_filter: None,
        })
        .await;
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}

#[tokio::test]
async fn test_agent_not_found() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());

    // Start server
    let server_socket = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(server_socket);
        server.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = Client::new(socket_path);

    // Try to snapshot a non-existent agent
    let response = client
        .request(Request::Snapshot {
            id: "nonexistent-agent".into(),
            strip_colors: true,
        })
        .await
        .expect("request failed");

    match response {
        Response::Error { message } => {
            assert!(message.contains("not found"));
        }
        other => panic!("expected Error, got {:?}", other),
    }

    // Shutdown
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}

#[tokio::test]
async fn test_screen_cursor_movement() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());

    // Start server
    let server_socket = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(server_socket);
        server.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = Client::new(socket_path);

    // Spawn a shell that does cursor movement
    // \r moves cursor to beginning of line, so "ABC\rX" becomes "XBC"
    let response = client
        .request(Request::Spawn {
            cmd: vec![
                "sh".into(),
                "-c".into(),
                r#"printf "ABC\rX"; sleep 10"#.into(),
            ],
            rows: 24,
            cols: 80,
            name: None,
            labels: vec![],
            timeout: None,
            max_output: None,
            env: vec![],
            env_clear: false,
        })
        .await
        .expect("spawn failed");

    let agent_id = match response {
        Response::Spawned { id, .. } => id,
        other => panic!("expected Spawned, got {:?}", other),
    };

    // Wait for output
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Get snapshot
    let response = client
        .request(Request::Snapshot {
            id: agent_id.clone(),
            strip_colors: true,
        })
        .await
        .expect("snapshot failed");

    match response {
        Response::Snapshot { content, .. } => {
            assert!(
                content.contains("XBC"),
                "cursor movement should produce XBC: {}",
                content
            );
        }
        other => panic!("expected Snapshot, got {:?}", other),
    }

    // Cleanup
    let _ = client
        .request(Request::Kill {
            id: Some(agent_id),
            labels: vec![],
            all: false,
            signal: 9,
            proc_filter: None,
        })
        .await;
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}

#[tokio::test]
async fn test_transcript_tail() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());

    // Start server
    let server_socket = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(server_socket);
        server.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = Client::new(socket_path);

    // Spawn something that produces output
    let response = client
        .request(Request::Spawn {
            cmd: vec![
                "sh".into(),
                "-c".into(),
                "echo LINE_ONE; echo LINE_TWO; sleep 10".into(),
            ],
            rows: 24,
            cols: 80,
            name: None,
            labels: vec![],
            timeout: None,
            max_output: None,
            env: vec![],
            env_clear: false,
        })
        .await
        .expect("spawn failed");

    let agent_id = match response {
        Response::Spawned { id, .. } => id,
        other => panic!("expected Spawned, got {:?}", other),
    };

    // Wait for output
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Get tail
    let response = client
        .request(Request::Tail {
            id: agent_id.clone(),
            lines: 10,
            follow: false,
        })
        .await
        .expect("tail failed");

    match response {
        Response::Output { data } => {
            let text = String::from_utf8_lossy(&data);
            assert!(text.contains("LINE_ONE"), "should contain LINE_ONE: {}", text);
            assert!(text.contains("LINE_TWO"), "should contain LINE_TWO: {}", text);
        }
        other => panic!("expected Output, got {:?}", other),
    }

    // Cleanup
    let _ = client
        .request(Request::Kill {
            id: Some(agent_id),
            labels: vec![],
            all: false,
            signal: 9,
            proc_filter: None,
        })
        .await;
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}

// ============================================================================
// Attach mode tests
// ============================================================================

#[tokio::test]
async fn test_attach_and_detach() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());

    // Start server
    let server_socket = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(server_socket);
        server.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Spawn an agent using regular client
    let mut client = Client::new(socket_path.clone());
    let response = client
        .request(Request::Spawn {
            cmd: vec!["bash".into()],
            rows: 24,
            cols: 80,
            name: None,
            labels: vec![],
            timeout: None,
            max_output: None,
            env: vec![],
            env_clear: false,
        })
        .await
        .expect("spawn failed");

    let agent_id = match response {
        Response::Spawned { id, .. } => id,
        other => panic!("expected Spawned, got {:?}", other),
    };

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Now connect directly for attach (bypassing Client wrapper)
    let mut stream = UnixStream::connect(&socket_path)
        .await
        .expect("connect failed");

    // Send attach request
    let attach_req = Request::Attach {
        id: agent_id.clone(),
        readonly: false,
    };
    let mut json = serde_json::to_string(&attach_req).unwrap();
    json.push('\n');
    stream.write_all(json.as_bytes()).await.expect("write failed");

    // Read AttachStarted response
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read failed");

    let response: Response = serde_json::from_str(&line).expect("parse failed");
    match response {
        Response::AttachStarted { id, size } => {
            assert_eq!(id, agent_id);
            assert_eq!(size, (24, 80));
        }
        other => panic!("expected AttachStarted, got {:?}", other),
    }

    // Detach by closing the connection (simulates client disconnect)
    drop(reader);
    drop(stream);

    // Give server time to process detach
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify agent is still running (detach shouldn't kill it)
    let response = client.request(Request::List { labels: vec![] }).await.expect("list failed");
    match response {
        Response::Agents { agents } => {
            assert_eq!(agents.len(), 1);
            assert_eq!(agents[0].id, agent_id);
        }
        other => panic!("expected Agents, got {:?}", other),
    }

    // Cleanup
    let _ = client
        .request(Request::Kill {
            id: Some(agent_id),
            labels: vec![],
            all: false,
            signal: 9,
            proc_filter: None,
        })
        .await;
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}

#[tokio::test]
async fn test_attach_readonly_mode() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());

    // Start server
    let server_socket = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(server_socket);
        server.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Spawn an agent that produces output
    let mut client = Client::new(socket_path.clone());
    let response = client
        .request(Request::Spawn {
            cmd: vec!["sh".into(), "-c".into(), "echo HELLO; sleep 10".into()],
            rows: 24,
            cols: 80,
            name: None,
            labels: vec![],
            timeout: None,
            max_output: None,
            env: vec![],
            env_clear: false,
        })
        .await
        .expect("spawn failed");

    let agent_id = match response {
        Response::Spawned { id, .. } => id,
        other => panic!("expected Spawned, got {:?}", other),
    };

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Connect for readonly attach
    let mut stream = UnixStream::connect(&socket_path)
        .await
        .expect("connect failed");

    let attach_req = Request::Attach {
        id: agent_id.clone(),
        readonly: true,
    };
    let mut json = serde_json::to_string(&attach_req).unwrap();
    json.push('\n');
    stream.write_all(json.as_bytes()).await.expect("write failed");

    // Read AttachStarted (may include initial screen data after the JSON)
    let mut buf = vec![0u8; 65536];
    let n = stream.read(&mut buf).await.expect("read failed");
    // Find the newline that terminates the JSON response
    let newline_pos = buf[..n].iter().position(|&b| b == b'\n').expect("no newline");
    let response: Response = serde_json::from_slice(&buf[..newline_pos]).expect("parse failed");
    
    assert!(matches!(response, Response::AttachStarted { .. }));

    // In readonly mode, we should still receive PTY output
    // The agent already printed "HELLO", so we may or may not see it depending on timing
    // Just verify we can read without error
    
    // Close connection
    drop(stream);

    // Cleanup
    let _ = client
        .request(Request::Kill {
            id: Some(agent_id),
            labels: vec![],
            all: false,
            signal: 9,
            proc_filter: None,
        })
        .await;
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}

#[tokio::test]
async fn test_attach_nonexistent_agent() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());

    // Start server
    let server_socket = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(server_socket);
        server.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Connect and try to attach to non-existent agent
    let mut stream = UnixStream::connect(&socket_path)
        .await
        .expect("connect failed");

    let attach_req = Request::Attach {
        id: "nonexistent-agent".into(),
        readonly: false,
    };
    let mut json = serde_json::to_string(&attach_req).unwrap();
    json.push('\n');
    stream.write_all(json.as_bytes()).await.expect("write failed");

    // Should get error response
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read failed");

    let response: Response = serde_json::from_str(&line).expect("parse failed");
    match response {
        Response::Error { message } => {
            assert!(message.contains("not found"));
        }
        other => panic!("expected Error, got {:?}", other),
    }

    // Cleanup
    let mut client = Client::new(socket_path);
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}

#[tokio::test]
async fn test_attach_receives_output() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());

    // Start server
    let server_socket = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(server_socket);
        server.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Spawn agent
    let mut client = Client::new(socket_path.clone());
    let response = client
        .request(Request::Spawn {
            cmd: vec!["bash".into()],
            rows: 24,
            cols: 80,
            name: None,
            labels: vec![],
            timeout: None,
            max_output: None,
            env: vec![],
            env_clear: false,
        })
        .await
        .expect("spawn failed");

    let agent_id = match response {
        Response::Spawned { id, .. } => id,
        other => panic!("expected Spawned, got {:?}", other),
    };

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Connect for attach
    let mut stream = UnixStream::connect(&socket_path)
        .await
        .expect("connect failed");

    let attach_req = Request::Attach {
        id: agent_id.clone(),
        readonly: false,
    };
    let mut json = serde_json::to_string(&attach_req).unwrap();
    json.push('\n');
    stream.write_all(json.as_bytes()).await.expect("write failed");

    // Read AttachStarted
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await.expect("read failed");
    let line_end = buf[..n].iter().position(|&b| b == b'\n').unwrap_or(n);
    let response: Response = serde_json::from_slice(&buf[..line_end]).expect("parse failed");
    assert!(matches!(response, Response::AttachStarted { .. }));

    // Send a command through the attach connection
    let cmd = b"echo ATTACH_TEST_OUTPUT\n";
    stream.write_all(cmd).await.expect("write failed");

    // Read output - may come in multiple chunks
    let mut output = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    
    while tokio::time::Instant::now() < deadline {
        match timeout(Duration::from_millis(100), stream.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => {
                output.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&output);
                if text.contains("ATTACH_TEST_OUTPUT") {
                    break;
                }
            }
            _ => {}
        }
    }

    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains("ATTACH_TEST_OUTPUT"),
        "should receive command output through attach: {}",
        text
    );

    // Cleanup
    drop(stream);
    let _ = client
        .request(Request::Kill {
            id: Some(agent_id),
            labels: vec![],
            all: false,
            signal: 9,
            proc_filter: None,
        })
        .await;
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}

#[tokio::test]
async fn test_attach_agent_exit() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());

    // Start server
    let server_socket = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(server_socket);
        server.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Spawn agent that will exit quickly
    let mut client = Client::new(socket_path.clone());
    let response = client
        .request(Request::Spawn {
            cmd: vec!["sh".into(), "-c".into(), "sleep 0.5; exit 42".into()],
            rows: 24,
            cols: 80,
            name: None,
            labels: vec![],
            timeout: None,
            max_output: None,
            env: vec![],
            env_clear: false,
        })
        .await
        .expect("spawn failed");

    let agent_id = match response {
        Response::Spawned { id, .. } => id,
        other => panic!("expected Spawned, got {:?}", other),
    };

    // Connect for attach before agent exits
    let mut stream = UnixStream::connect(&socket_path)
        .await
        .expect("connect failed");

    let attach_req = Request::Attach {
        id: agent_id.clone(),
        readonly: false,
    };
    let mut json = serde_json::to_string(&attach_req).unwrap();
    json.push('\n');
    stream.write_all(json.as_bytes()).await.expect("write failed");

    // Read AttachStarted
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await.expect("read failed");
    let line_end = buf[..n].iter().position(|&b| b == b'\n').unwrap_or(n);
    let response: Response = serde_json::from_slice(&buf[..line_end]).expect("parse failed");
    assert!(matches!(response, Response::AttachStarted { .. }));

    // Wait for agent to exit and receive AttachEnded
    let mut received_end = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    
    while tokio::time::Instant::now() < deadline && !received_end {
        match timeout(Duration::from_millis(100), stream.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => {
                // Try to parse as JSON (AttachEnded message)
                if buf[0] == b'{' {
                    if let Ok(response) = serde_json::from_slice::<Response>(&buf[..n]) {
                        if let Response::AttachEnded { reason } = response {
                            match reason {
                                AttachEndReason::AgentExited { exit_code } => {
                                    assert_eq!(exit_code, Some(42));
                                    received_end = true;
                                }
                                other => panic!("expected AgentExited, got {:?}", other),
                            }
                        }
                    }
                }
            }
            Ok(Ok(0)) => break, // Connection closed
            _ => {}
        }
    }

    assert!(received_end, "should receive AttachEnded when agent exits");

    // Cleanup
    drop(stream);
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}

#[tokio::test]
async fn test_kill_all() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());

    // Start server
    let server_socket = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(server_socket);
        server.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = Client::new(socket_path);

    // Spawn multiple agents
    for i in 0..3 {
        let response = client
            .request(Request::Spawn {
                cmd: vec!["sleep".into(), "10".into()],
                rows: 24,
                cols: 80,
                name: Some(format!("agent-{i}")),
                labels: vec![],
                timeout: None,
                max_output: None,
                env: vec![],
                env_clear: false,
            })
            .await
            .expect("spawn failed");

        assert!(matches!(response, Response::Spawned { .. }));
    }

    // Verify we have 3 agents
    let response = client.request(Request::List { labels: vec![] }).await.expect("list failed");
    match &response {
        Response::Agents { agents } => {
            assert_eq!(agents.len(), 3, "should have 3 agents");
        }
        other => panic!("expected Agents, got {:?}", other),
    }

    // Kill all agents
    let response = client
        .request(Request::Kill {
            id: None,
            labels: vec![],
            all: true,
            signal: 9,
            proc_filter: None,
        })
        .await
        .expect("kill --all failed");

    assert!(matches!(response, Response::Ok));

    // Give agents time to exit
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify all agents are gone (or exited)
    let response = client.request(Request::List { labels: vec![] }).await.expect("list failed");
    match response {
        Response::Agents { agents } => {
            let running: Vec<_> = agents.iter().filter(|a| a.state == AgentState::Running).collect();
            assert!(running.is_empty(), "no agents should be running after kill --all, got: {:?}", running);
        }
        other => panic!("expected Agents, got {:?}", other),
    }

    // Shutdown
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}

#[tokio::test]
async fn test_kill_all_no_agents() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());

    // Start server
    let server_socket = socket_path.clone();
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(server_socket);
        server.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = Client::new(socket_path);

    // Kill all when there are no agents - should return error
    let response = client
        .request(Request::Kill {
            id: None,
            labels: vec![],
            all: true,
            signal: 9,
            proc_filter: None,
        })
        .await
        .expect("request failed");

    match response {
        Response::Error { message } => {
            assert!(message.contains("no running agents"), "should say no running agents: {}", message);
        }
        other => panic!("expected Error, got {:?}", other),
    }

    // Shutdown
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}
