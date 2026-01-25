//! Transcript ring buffer.

// Timestamp won't overflow u64 until year 584942417355
#![allow(clippy::cast_possible_truncation)]

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

/// A single transcript entry.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    /// Unix timestamp in milliseconds.
    pub timestamp: u64,
    /// Output bytes.
    pub data: Vec<u8>,
}

/// Ring buffer for transcript data.
pub struct Transcript {
    /// Maximum size in bytes.
    max_size: usize,
    /// Current total size in bytes.
    current_size: usize,
    /// Entries in the buffer.
    entries: VecDeque<TranscriptEntry>,
}

impl Transcript {
    /// Create a new transcript buffer with the given maximum size.
    #[must_use]
    pub const fn new(max_size: usize) -> Self {
        Self {
            max_size,
            current_size: 0,
            entries: VecDeque::new(),
        }
    }

    /// Get the current Unix timestamp in milliseconds.
    fn now_millis() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// Append data to the transcript.
    pub fn append(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        let entry = TranscriptEntry {
            timestamp: Self::now_millis(),
            data: data.to_vec(),
        };

        let entry_size = entry.data.len();

        // Remove old entries if we exceed max size
        while self.current_size + entry_size > self.max_size && !self.entries.is_empty() {
            if let Some(old) = self.entries.pop_front() {
                self.current_size -= old.data.len();
            }
        }

        self.current_size += entry_size;
        self.entries.push_back(entry);
    }

    /// Get all entries since a given timestamp.
    #[must_use]
    pub fn since(&self, timestamp: u64) -> Vec<&TranscriptEntry> {
        self.entries
            .iter()
            .filter(|e| e.timestamp >= timestamp)
            .collect()
    }

    /// Get the last N bytes of output.
    #[must_use]
    pub fn tail_bytes(&self, n: usize) -> Vec<u8> {
        let mut result = Vec::new();
        for entry in self.entries.iter().rev() {
            if result.len() >= n {
                break;
            }
            let remaining = n - result.len();
            let take = entry.data.len().min(remaining);
            result.splice(0..0, entry.data[entry.data.len() - take..].iter().copied());
        }
        result
    }

    /// Get all entries.
    pub fn all(&self) -> impl Iterator<Item = &TranscriptEntry> {
        self.entries.iter()
    }

    /// Get the total size of data in the buffer.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.current_size
    }

    /// Clear the transcript.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.current_size = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_append_and_size() {
        let mut t = Transcript::new(1024);
        t.append(b"hello");
        t.append(b"world");
        assert_eq!(t.size(), 10);
    }

    #[test]
    fn test_ring_buffer_eviction() {
        let mut t = Transcript::new(10);
        t.append(b"hello"); // 5 bytes
        t.append(b"world"); // 5 bytes, total 10
        t.append(b"!"); // 1 byte, should evict "hello"

        let all: Vec<_> = t.all().collect();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].data, b"world");
        assert_eq!(all[1].data, b"!");
    }

    #[test]
    fn test_tail_bytes() {
        let mut t = Transcript::new(1024);
        t.append(b"hello");
        t.append(b"world");

        let tail = t.tail_bytes(5);
        assert_eq!(tail, b"world");

        let tail = t.tail_bytes(7);
        assert_eq!(tail, b"loworld");
    }
}
