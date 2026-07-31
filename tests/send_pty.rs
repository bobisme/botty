//! PTY write semantics for `send` (bn-2eg).
//!
//! These live in their own test binary rather than `integration.rs`: they push
//! tens of KiB through a PTY and take seconds rather than milliseconds, and
//! each `async_test!` stands up a runtime sized to the machine. Sharing a
//! binary with the fast integration tests widened the window where every one
//! of them was live at once, which starved the timing-sensitive ones. Cargo
//! runs test binaries one at a time, so this keeps the peak down.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use vessel::runtime;
use vessel::{Client, Request, Response, Server};

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Generate a unique socket path for each test.
fn unique_socket_path() -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    PathBuf::from(format!("/tmp/vessel-send-test-{pid}-{id}.sock"))
}

/// Helper to clean up socket after test.
struct SocketCleanup(PathBuf);

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

// bn-2eg: a Send payload larger than the PTY input buffer must arrive whole.
// The master fd is non-blocking, so write(2) returns a short count once the
// buffer fills; the old handler matched `Ok(_)` and reported success, silently
// dropping the tail. Lines stay well under MAX_CANON (4096) so the line
// discipline keeps handing them to `cat` as we write.
vessel::async_test! {
    async fn test_send_large_payload_is_not_truncated() {
        let socket_path = unique_socket_path();
        let _cleanup = SocketCleanup(socket_path.clone());

        let server_socket = socket_path.clone();
        runtime::task::spawn(async move {
            let mut server = Server::new(server_socket);
            let _ = server.run().await;
        });
        runtime::time::sleep(Duration::from_millis(100)).await;

        let mut client = Client::new(socket_path);
        let response = client
            .request(Request::Spawn {
                cmd: vec!["cat".into()],
                name: None,
                labels: vec![],
                rows: 50,
                cols: 200,
                timeout: None,
                max_output: None,
                env: vec![],
                cwd: None,
                no_resize: false,
                record: false,
                memory_limit: None,
            })
            .await
            .expect("spawn failed");

        let agent_id = match response {
            Response::Spawned { id, .. } => id,
            other => panic!("expected Spawned, got {:?}", other),
        };

        runtime::time::sleep(Duration::from_millis(200)).await;

        // ~48 KiB across 600 lines. Past the PTY input buffer, so short writes
        // are guaranteed, and past the output buffer too: `cat` blocks writing
        // its echo until the reader task drains it. If the send held the
        // manager lock across its retries the reader could not run, the child
        // would never read the rest of the payload, and this send would stall
        // until the write timeout. (Measured: writes stall at ~26 KiB.)
        //
        // Deliberately no larger — this runs alongside timing-sensitive tests
        // in a shared runtime, and pushing megabytes through the vt100 screen
        // parser starves them.
        const LINES: usize = 600;
        let mut payload = String::new();
        for i in 0..LINES {
            payload.push_str(&format!("VESSEL_LINE_{i:04}_{}\n", "x".repeat(60)));
        }
        assert!(
            payload.len() > 32 * 1024,
            "payload must exceed the PTY buffers to exercise short writes"
        );

        let response = client
            .request(Request::Send {
                id: agent_id.clone(),
                data: payload,
                newline: false,
                enter: false,
                submit_delay_ms: None,
            })
            .await
            .expect("send failed");
        assert!(matches!(response, Response::Ok), "got {:?}", response);

        // Let `cat` echo everything back into the transcript.
        runtime::time::sleep(Duration::from_millis(1500)).await;

        let response = client
            .request(Request::Dump {
                id: agent_id.clone(),
                since: None,
                format: Default::default(),
            })
            .await
            .expect("dump failed");

        let transcript = match response {
            Response::Output { data, .. } => String::from_utf8_lossy(&data).into_owned(),
            other => panic!("expected Output, got {:?}", other),
        };

        // The tail is what a short write drops, so check the last line as well
        // as the first.
        assert!(
            transcript.contains("VESSEL_LINE_0000_"),
            "first line missing from transcript"
        );
        assert!(
            transcript.contains(&format!("VESSEL_LINE_{:04}_", LINES - 1)),
            "last line missing: payload was truncated"
        );

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
    }
}

// bn-2eg: the submit key must be written separately from the text, after a
// pause, or a TUI's paste detection absorbs it as content. The observable
// contract at this layer is that the server actually waits before writing it.
vessel::async_test! {
    async fn test_send_waits_before_writing_submit_key() {
        let socket_path = unique_socket_path();
        let _cleanup = SocketCleanup(socket_path.clone());

        let server_socket = socket_path.clone();
        runtime::task::spawn(async move {
            let mut server = Server::new(server_socket);
            let _ = server.run().await;
        });
        runtime::time::sleep(Duration::from_millis(100)).await;

        let mut client = Client::new(socket_path);
        let response = client
            .request(Request::Spawn {
                cmd: vec!["cat".into()],
                name: None,
                labels: vec![],
                rows: 24,
                cols: 80,
                timeout: None,
                max_output: None,
                env: vec![],
                cwd: None,
                no_resize: false,
                record: false,
                memory_limit: None,
            })
            .await
            .expect("spawn failed");

        let agent_id = match response {
            Response::Spawned { id, .. } => id,
            other => panic!("expected Spawned, got {:?}", other),
        };
        runtime::time::sleep(Duration::from_millis(200)).await;

        // An explicit delay is honoured...
        let start = std::time::Instant::now();
        let response = client
            .request(Request::Send {
                id: agent_id.clone(),
                data: "delayed".into(),
                newline: false,
                enter: true,
                submit_delay_ms: Some(300),
            })
            .await
            .expect("send failed");
        let elapsed = start.elapsed();
        assert!(matches!(response, Response::Ok), "got {:?}", response);
        // Timer granularity lets the sleep wake a few ms early; the point is
        // that a gap of roughly the requested size happened at all, not that
        // it is exact.
        assert!(
            elapsed >= Duration::from_millis(250),
            "send returned in {elapsed:?}, so no gap preceded the submit key"
        );

        // ...and Some(0) opts out, for line-oriented programs.
        let start = std::time::Instant::now();
        let response = client
            .request(Request::Send {
                id: agent_id.clone(),
                data: "immediate".into(),
                newline: false,
                enter: true,
                submit_delay_ms: Some(0),
            })
            .await
            .expect("send failed");
        let elapsed = start.elapsed();
        assert!(matches!(response, Response::Ok), "got {:?}", response);
        assert!(
            elapsed < Duration::from_millis(200),
            "submit_delay_ms: Some(0) should not wait, took {elapsed:?}"
        );

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
    }
}
