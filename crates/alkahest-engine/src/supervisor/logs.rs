//! Engine output capture: a bounded in-memory ring of lines, mirrored to tracing and,
//! optionally, to a per-model log file.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::Mutex;
use serde::Serialize;

/// Which pipe a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error (llama.cpp and vLLM log here).
    Stderr,
}

impl Stream {
    /// `stdout` / `stderr`.
    pub fn as_str(self) -> &'static str {
        match self {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        }
    }
}

/// One captured line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogLine {
    /// Source pipe.
    pub stream: Stream,
    /// The line without its terminator, lossily decoded.
    pub text: String,
    /// When it was read.
    pub at: SystemTime,
}

/// Longest line kept; engines occasionally print megabyte-long JSON blobs.
const MAX_LINE: usize = 8 * 1024;

#[derive(Debug)]
struct Inner {
    lines: VecDeque<LogLine>,
    capacity: usize,
    file: Option<File>,
}

/// A shared ring buffer of the last `capacity` lines of an engine's output.
#[derive(Debug, Clone)]
pub struct LogBuffer {
    inner: Arc<Mutex<Inner>>,
}

impl LogBuffer {
    /// An empty buffer keeping `capacity` lines (at least one).
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                lines: VecDeque::with_capacity(capacity.min(4096)),
                capacity: capacity.max(1),
                file: None,
            })),
        }
    }

    /// Also appends every line to `path` (created if missing). The file is the per-model log
    /// the admin API and `alkahest logs` will show.
    pub fn tee_to_file(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        self.inner.lock().file = Some(f);
        Ok(())
    }

    /// Records one line.
    pub fn push(&self, stream: Stream, text: &str) {
        let mut text = text.trim_end_matches(['\r', '\n']).to_string();
        if text.len() > MAX_LINE {
            let mut cut = MAX_LINE;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str(" [truncated]");
        }
        let mut g = self.inner.lock();
        if let Some(f) = g.file.as_mut() {
            // A full disk must not take the engine down; the in-memory ring still has it.
            let _ = writeln!(f, "[{}] {text}", stream.as_str());
        }
        if g.lines.len() == g.capacity {
            g.lines.pop_front();
        }
        g.lines.push_back(LogLine {
            stream,
            text,
            at: SystemTime::now(),
        });
    }

    /// The last `n` lines, oldest first.
    pub fn tail(&self, n: usize) -> Vec<LogLine> {
        let g = self.inner.lock();
        let skip = g.lines.len().saturating_sub(n);
        g.lines.iter().skip(skip).cloned().collect()
    }

    /// The last `n` lines of `stream` joined with newlines.
    pub fn tail_text(&self, stream: Stream, n: usize) -> String {
        let g = self.inner.lock();
        let mut picked: Vec<&str> = g
            .lines
            .iter()
            .rev()
            .filter(|l| l.stream == stream)
            .take(n)
            .map(|l| l.text.as_str())
            .collect();
        picked.reverse();
        picked.join("\n")
    }

    /// Number of lines held.
    pub fn len(&self) -> usize {
        self.inner.lock().lines.len()
    }

    /// Whether nothing was captured yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_the_last_lines() {
        let b = LogBuffer::new(3);
        for i in 0..5 {
            b.push(Stream::Stderr, &format!("line {i}\n"));
        }
        b.push(Stream::Stdout, "out");
        let tail: Vec<String> = b.tail(10).into_iter().map(|l| l.text).collect();
        assert_eq!(tail, vec!["line 3", "line 4", "out"]);
        assert_eq!(b.tail_text(Stream::Stderr, 5), "line 3\nline 4");
    }

    #[test]
    fn long_lines_are_truncated() {
        let b = LogBuffer::new(2);
        b.push(Stream::Stdout, &"é".repeat(MAX_LINE));
        assert!(b.tail(1)[0].text.ends_with("[truncated]"));
    }
}
