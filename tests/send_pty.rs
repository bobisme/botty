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
                id: Some(agent_id.clone()),
                labels: Vec::new(),
                all: false,
                proc_filter: None,
                data: payload,
                newline: false,
                enter: false,
                submit_delay_ms: None,
                paste: false,
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
                id: Some(agent_id.clone()),
                labels: Vec::new(),
                all: false,
                proc_filter: None,
                data: "delayed".into(),
                newline: false,
                enter: true,
                submit_delay_ms: Some(300),
                paste: false,
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
                id: Some(agent_id.clone()),
                labels: Vec::new(),
                all: false,
                proc_filter: None,
                data: "immediate".into(),
                newline: false,
                enter: true,
                submit_delay_ms: Some(0),
                paste: false,
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

// bn-1a0f: --paste must wrap the payload in bracketed-paste markers so a TUI
// takes a multi-line prompt as one paste. `cat` echoes its stdin verbatim, so
// the markers are observable in the transcript.
vessel::async_test! {
    async fn test_send_paste_wraps_payload_in_markers() {
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

        let response = client
            .request(Request::Send {
                id: Some(agent_id.clone()),
                labels: Vec::new(),
                all: false,
                proc_filter: None,
                data: "first\nsecond\nthird".into(),
                newline: false,
                enter: false,
                submit_delay_ms: None,
                paste: true,
            })
            .await
            .expect("send failed");
        assert!(matches!(response, Response::Ok), "got {:?}", response);

        // The closing marker has no trailing newline, so the line discipline
        // holds it in the canonical buffer and `cat` has not read it yet.
        // Flush with a bare newline so the envelope reaches `cat` and comes
        // back as real bytes; the terminal's own echo renders ESC as "^[".
        let response = client
            .request(Request::Send {
                id: Some(agent_id.clone()),
                labels: Vec::new(),
                all: false,
                proc_filter: None,
                data: String::new(),
                newline: true,
                enter: false,
                submit_delay_ms: Some(0),
                paste: false,
            })
            .await
            .expect("flush send failed");
        assert!(matches!(response, Response::Ok), "got {:?}", response);

        runtime::time::sleep(Duration::from_millis(500)).await;

        let response = client
            .request(Request::Dump {
                id: agent_id.clone(),
                since: None,
                format: Default::default(),
            })
            .await
            .expect("dump failed");

        let transcript = match response {
            Response::Output { data, .. } => data,
            other => panic!("expected Output, got {:?}", other),
        };

        // Match real ESC bytes, which only appear in `cat`'s output -- the
        // echoed copy uses the printable "^[" rendering.
        let find = |needle: &[u8]| {
            transcript
                .windows(needle.len())
                .position(|w| w == needle)
        };

        let start = find(b"\x1b[200~").expect("paste introducer missing from transcript");
        let end = find(b"\x1b[201~").expect("paste terminator missing from transcript");
        assert!(start < end, "introducer must precede terminator");

        // All three lines land inside the envelope, newlines intact.
        let body = &transcript[start + 6..end];
        let text = String::from_utf8_lossy(body);
        assert!(text.contains("first"), "body missing 'first': {text:?}");
        assert!(text.contains("second"), "body missing 'second': {text:?}");
        assert!(text.contains("third"), "body missing 'third': {text:?}");

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

// bn-1a0f + bn-2eg together: --paste --enter is the orchestrator's workflow --
// deliver a multi-line prompt as one paste, then submit it. The CR must land
// outside the paste envelope, or it is just more pasted content.
vessel::async_test! {
    async fn test_send_paste_with_enter_puts_key_outside_the_envelope() {
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

        let response = client
            .request(Request::Send {
                id: Some(agent_id.clone()),
                labels: Vec::new(),
                all: false,
                proc_filter: None,
                data: "alpha\nbeta".into(),
                newline: false,
                enter: true,
                submit_delay_ms: Some(60),
                paste: true,
            })
            .await
            .expect("send failed");
        assert!(matches!(response, Response::Ok), "got {:?}", response);

        runtime::time::sleep(Duration::from_millis(500)).await;

        let response = client
            .request(Request::Dump {
                id: agent_id.clone(),
                since: None,
                format: Default::default(),
            })
            .await
            .expect("dump failed");

        let transcript = match response {
            Response::Output { data, .. } => data,
            other => panic!("expected Output, got {:?}", other),
        };

        let end = transcript
            .windows(6)
            .position(|w| w == b"\x1b[201~")
            .expect("paste terminator missing from transcript");

        // The CR is echoed after the closing marker, not inside it.
        let after_envelope = &transcript[end + 6..];
        assert!(
            after_envelope.contains(&b'\r') || after_envelope.contains(&b'\n'),
            "submit key did not arrive after the paste envelope: {:?}",
            String::from_utf8_lossy(after_envelope)
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

/// Spawn `count` agents running `cat`, all carrying `label`.
async fn spawn_labelled_cats(client: &mut Client, label: &str, count: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        let response = client
            .request(Request::Spawn {
                cmd: vec!["cat".into()],
                name: None,
                labels: vec![label.to_string()],
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
        match response {
            Response::Spawned { id, .. } => ids.push(id),
            other => panic!("expected Spawned, got {:?}", other),
        }
    }
    ids
}

async fn transcript_of(client: &mut Client, id: &str) -> String {
    let response = client
        .request(Request::Dump {
            id: id.to_string(),
            since: None,
            format: Default::default(),
        })
        .await
        .expect("dump failed");
    match response {
        Response::Output { data, .. } => String::from_utf8_lossy(&data).into_owned(),
        other => panic!("expected Output, got {:?}", other),
    }
}

// bn-1dxu: --label fans the same input out to every matching agent, and the
// response names each one so partial failure cannot hide behind a bare Ok.
vessel::async_test! {
    async fn test_send_label_reaches_every_matching_agent() {
        let socket_path = unique_socket_path();
        let _cleanup = SocketCleanup(socket_path.clone());

        let server_socket = socket_path.clone();
        runtime::task::spawn(async move {
            let mut server = Server::new(server_socket);
            let _ = server.run().await;
        });
        runtime::time::sleep(Duration::from_millis(100)).await;

        let mut client = Client::new(socket_path);
        let targets = spawn_labelled_cats(&mut client, "batch", 3).await;
        // An agent outside the label must not receive the input.
        let outsider = spawn_labelled_cats(&mut client, "other", 1).await;
        runtime::time::sleep(Duration::from_millis(300)).await;

        let response = client
            .request(Request::Send {
                id: None,
                labels: vec!["batch".into()],
                all: false,
                proc_filter: None,
                data: "FANOUT_MARKER".into(),
                newline: true,
                enter: false,
                submit_delay_ms: Some(0),
                paste: false,
            })
            .await
            .expect("send failed");

        match response {
            Response::SendResults { results } => {
                assert_eq!(results.len(), 3, "should report one result per match");
                assert!(
                    results.iter().all(|r| r.is_ok()),
                    "all deliveries should succeed: {results:?}"
                );
                let mut got: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
                got.sort_unstable();
                let mut want: Vec<&str> = targets.iter().map(String::as_str).collect();
                want.sort_unstable();
                assert_eq!(got, want, "results must name exactly the matched agents");
            }
            other => panic!("expected SendResults, got {:?}", other),
        }

        runtime::time::sleep(Duration::from_millis(600)).await;

        for id in &targets {
            let transcript = transcript_of(&mut client, id).await;
            assert!(
                transcript.contains("FANOUT_MARKER"),
                "agent {id} never received the input: {transcript:?}"
            );
        }

        let untouched = transcript_of(&mut client, &outsider[0]).await;
        assert!(
            !untouched.contains("FANOUT_MARKER"),
            "unlabelled agent should not have received the input: {untouched:?}"
        );

        let _ = client
            .request(Request::Kill {
                id: None,
                labels: vec![],
                all: true,
                signal: 9,
                proc_filter: None,
            })
            .await;
        let _ = client.request(Request::Shutdown).await;
    }
}

// bn-1dxu: a request naming one agent keeps the original Ok/Error contract, so
// existing callers see no change.
vessel::async_test! {
    async fn test_send_single_id_still_answers_ok() {
        let socket_path = unique_socket_path();
        let _cleanup = SocketCleanup(socket_path.clone());

        let server_socket = socket_path.clone();
        runtime::task::spawn(async move {
            let mut server = Server::new(server_socket);
            let _ = server.run().await;
        });
        runtime::time::sleep(Duration::from_millis(100)).await;

        let mut client = Client::new(socket_path);
        let ids = spawn_labelled_cats(&mut client, "solo", 1).await;
        runtime::time::sleep(Duration::from_millis(200)).await;

        let response = client
            .request(Request::Send {
                id: Some(ids[0].clone()),
                labels: vec![],
                all: false,
                proc_filter: None,
                data: "SOLO".into(),
                newline: true,
                enter: false,
                submit_delay_ms: Some(0),
                paste: false,
            })
            .await
            .expect("send failed");
        assert!(
            matches!(response, Response::Ok),
            "single-id send must still answer Ok, got {response:?}"
        );

        // And an unknown ID still errors rather than returning an empty list.
        let response = client
            .request(Request::Send {
                id: Some("no-such-agent".into()),
                labels: vec![],
                all: false,
                proc_filter: None,
                data: "x".into(),
                newline: false,
                enter: false,
                submit_delay_ms: Some(0),
                paste: false,
            })
            .await
            .expect("send failed");
        match response {
            Response::Error { message } => {
                assert!(message.contains("agent not found"), "got {message}");
            }
            other => panic!("expected Error, got {:?}", other),
        }

        let _ = client
            .request(Request::Kill {
                id: None,
                labels: vec![],
                all: true,
                signal: 9,
                proc_filter: None,
            })
            .await;
        let _ = client.request(Request::Shutdown).await;
    }
}

// bn-1dxu: a selector matching nothing is an error, not a silent no-op that
// reports success for zero agents.
vessel::async_test! {
    async fn test_send_label_matching_nothing_is_an_error() {
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
            .request(Request::Send {
                id: None,
                labels: vec!["nobody-has-this".into()],
                all: false,
                proc_filter: None,
                data: "x".into(),
                newline: true,
                enter: false,
                submit_delay_ms: Some(0),
                paste: false,
            })
            .await
            .expect("send failed");

        match response {
            Response::Error { message } => {
                assert!(
                    message.contains("no agents match the specified labels"),
                    "got {message}"
                );
            }
            other => panic!("expected Error, got {:?}", other),
        }

        let _ = client.request(Request::Shutdown).await;
    }
}

// bn-1dxu: send-bytes gets the same selectors, which is what makes send-keys
// fan out (it expands to one SendBytes per key).
vessel::async_test! {
    async fn test_send_bytes_label_reaches_every_matching_agent() {
        let socket_path = unique_socket_path();
        let _cleanup = SocketCleanup(socket_path.clone());

        let server_socket = socket_path.clone();
        runtime::task::spawn(async move {
            let mut server = Server::new(server_socket);
            let _ = server.run().await;
        });
        runtime::time::sleep(Duration::from_millis(100)).await;

        let mut client = Client::new(socket_path);
        let targets = spawn_labelled_cats(&mut client, "keys", 2).await;
        runtime::time::sleep(Duration::from_millis(300)).await;

        // "OK\n" as raw bytes.
        let response = client
            .request(Request::SendBytes {
                id: None,
                labels: vec!["keys".into()],
                all: false,
                proc_filter: None,
                data: b"BYTES_MARKER\n".to_vec(),
            })
            .await
            .expect("send_bytes failed");

        match response {
            Response::SendResults { results } => {
                assert_eq!(results.len(), 2);
                assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
            }
            other => panic!("expected SendResults, got {:?}", other),
        }

        runtime::time::sleep(Duration::from_millis(600)).await;

        for id in &targets {
            let transcript = transcript_of(&mut client, id).await;
            assert!(
                transcript.contains("BYTES_MARKER"),
                "agent {id} never received the bytes: {transcript:?}"
            );
        }

        let _ = client
            .request(Request::Kill {
                id: None,
                labels: vec![],
                all: true,
                signal: 9,
                proc_filter: None,
            })
            .await;
        let _ = client.request(Request::Shutdown).await;
    }
}
