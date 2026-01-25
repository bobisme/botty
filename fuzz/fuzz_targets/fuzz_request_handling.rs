//! Fuzz target for request handling logic.
//!
//! Uses arbitrary to generate structured Request values and verify handling doesn't panic.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

/// Fuzzable subset of Request that doesn't require server state.
#[derive(Debug, Arbitrary)]
enum FuzzRequest {
    Spawn {
        cmd: Vec<String>,
        rows: u16,
        cols: u16,
    },
    Kill {
        id: String,
        signal: i32,
    },
    Send {
        id: String,
        data: String,
        newline: bool,
    },
    SendBytes {
        id: String,
        data: Vec<u8>,
    },
}

fuzz_target!(|req: FuzzRequest| {
    // Convert to real Request and serialize/deserialize
    let request = match req {
        FuzzRequest::Spawn { cmd, rows, cols } => {
            botty::protocol::Request::Spawn { cmd, rows, cols }
        }
        FuzzRequest::Kill { id, signal } => botty::protocol::Request::Kill { id, signal },
        FuzzRequest::Send { id, data, newline } => {
            botty::protocol::Request::Send { id, data, newline }
        }
        FuzzRequest::SendBytes { id, data } => botty::protocol::Request::SendBytes { id, data },
    };

    // Roundtrip through JSON - should not panic
    if let Ok(json) = serde_json::to_string(&request) {
        let _ = serde_json::from_str::<botty::protocol::Request>(&json);
    }
});
