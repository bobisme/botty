//! Integration tests for botty server/client IPC.
//!
//! Each test uses a unique socket path to avoid conflicts.

use botty::{Client, Request, Response, Server};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
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
    let response = client.request(Request::List).await.expect("list failed");

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
            id: agent_id,
            signal: 15,
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
            id: agent_id,
            signal: 9,
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
            id: agent_id,
            signal: 9,
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
            id: agent_id,
            signal: 9,
        })
        .await;
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}
