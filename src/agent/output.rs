//! Bounded tool output. Retain the beginning and end, including final errors.

use std::collections::VecDeque;

pub const TOOL_OUTPUT_BYTES: usize = 32_000;

pub fn truncate_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let marker = "\n… output omitted …\n";
    if max_bytes < marker.len() {
        return "[truncated]".chars().take(max_bytes).collect();
    }
    let available = max_bytes - marker.len();
    let mut head = available / 2;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - (available - available / 2);
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}{marker}{}", &text[..head], &text[tail..])
}

#[derive(Debug)]
pub struct BoundedOutput {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    limit: usize,
    total: u64,
}

impl BoundedOutput {
    pub fn new(limit: usize) -> Self {
        Self {
            head: Vec::new(),
            tail: VecDeque::new(),
            limit,
            total: 0,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len() as u64);
        let take = (self.limit / 2)
            .saturating_sub(self.head.len())
            .min(bytes.len());
        self.head.extend_from_slice(&bytes[..take]);
        let bytes = &bytes[take..];
        let capacity = self.limit - self.limit / 2;
        if bytes.len() >= capacity {
            self.tail.clear();
            self.tail.extend(&bytes[bytes.len() - capacity..]);
        } else {
            let excess = (self.tail.len() + bytes.len()).saturating_sub(capacity);
            self.tail.drain(..excess);
            self.tail.extend(bytes);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    pub fn render(&self) -> String {
        let mut bytes = self.head.clone();
        if self.total > self.limit as u64 {
            bytes.extend_from_slice(
                format!("\n… {} bytes omitted …\n", self.total - self.limit as u64).as_bytes(),
            );
        }
        bytes.extend(self.tail.iter());
        truncate_text(&String::from_utf8_lossy(&bytes), self.limit)
    }

    pub fn take(&mut self) -> String {
        let text = self.render();
        self.head.clear();
        self.tail.clear();
        self.total = 0;
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_memory_while_retaining_initial_and_final_output() {
        let mut out = BoundedOutput::new(1024);
        out.push(b"START\n");
        for _ in 0..10_000 {
            out.push(&[b'x'; 4096]);
        }
        out.push(b"\nFINAL ERROR");
        assert!(out.head.len() + out.tail.len() <= 1024);
        let text = out.take();
        assert!(text.starts_with("START\n"));
        assert!(text.ends_with("FINAL ERROR"));
        assert!(text.len() <= 1024);
        assert!(out.is_empty());
    }

    #[test]
    fn small_output_is_exact_and_unicode_truncation_is_valid() {
        let mut out = BoundedOutput::new(1024);
        out.push("你好\n".as_bytes());
        out.push(b"end");
        assert_eq!(out.render(), "你好\nend");
        for limit in 0..200 {
            let text = truncate_text(&"你好🙂".repeat(100), limit);
            assert!(text.len() <= limit);
        }
    }
}
