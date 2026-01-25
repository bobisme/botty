//! Fuzz target for protocol parsing.
//!
//! Tests that arbitrary bytes don't cause panics when parsed as Request/Response.

#![no_main]

use botty::protocol::{Request, Response};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try to parse as Request - should not panic
    let _ = serde_json::from_slice::<Request>(data);

    // Try to parse as Response - should not panic
    let _ = serde_json::from_slice::<Response>(data);

    // Try as string (like we read from socket)
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = serde_json::from_str::<Request>(s);
        let _ = serde_json::from_str::<Response>(s);
    }
});
