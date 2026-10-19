//! Token usage from Claude Code transcripts (JSONL), read incrementally.
//!
//! Each assistant message can span several lines that repeat the same
//! `message.id` and `usage`, so usage is counted once per message id.
//! Transcripts are undocumented internals: every field is optional and a bad
//! line is skipped, never fatal.

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use tower_proto::Usage;

/// Read at most this much new transcript per call.
const MAX_CHUNK: u64 = 8 << 20;

#[derive(Default)]
pub struct TranscriptCursor {
    pub path: String,
    pub offset: u64,
    seen: HashSet<String>,
}

/// Usage added by complete lines in `chunk`; `seen` dedupes message ids.
pub fn fold_lines(chunk: &str, seen: &mut HashSet<String>) -> Usage {
    let mut u = Usage::default();
    for line in chunk.lines() {
        if !line.contains("\"usage\"") {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let msg = match v.get("message") {
            Some(m) => m,
            None => continue,
        };
        let usage = match msg.get("usage") {
            Some(u) => u,
            None => continue,
        };
        if let Some(id) = msg.get("id").and_then(|i| i.as_str()) {
            if !seen.insert(id.to_string()) {
                continue;
            }
        }
        let n = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        u.input_tokens +=
            n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
        u.output_tokens += n("output_tokens");
    }
    u
}

impl TranscriptCursor {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            ..Default::default()
        }
    }

    /// Usage appended since the last call. Only whole lines are consumed;
    /// a partial last line is read again next time.
    pub fn advance(&mut self) -> Usage {
        let mut f = match std::fs::File::open(Path::new(&self.path)) {
            Ok(f) => f,
            Err(_) => return Usage::default(),
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            // Rewritten or truncated: start over without double counting.
            self.offset = 0;
        }
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return Usage::default();
        }
        let mut buf = Vec::new();
        if f.take(MAX_CHUNK).read_to_end(&mut buf).is_err() {
            return Usage::default();
        }
        let end = match buf.iter().rposition(|b| *b == b'\n') {
            Some(i) => i + 1,
            None => return Usage::default(),
        };
        self.offset += end as u64;
        if self.seen.len() > 50_000 {
            self.seen.clear();
        }
        fold_lines(&String::from_utf8_lossy(&buf[..end]), &mut self.seen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str, inp: u64, out: u64) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"id":"{id}","usage":{{"input_tokens":{inp},"cache_read_input_tokens":10,"output_tokens":{out}}}}}}}"#
        )
    }

    #[test]
    fn repeated_message_ids_count_once() {
        let mut seen = HashSet::new();
        let chunk = [
            line("m1", 100, 5),
            line("m1", 100, 5),
            line("m2", 50, 7),
            "garbage".into(),
        ]
        .join("\n");
        let u = fold_lines(&chunk, &mut seen);
        assert_eq!(u.input_tokens, 100 + 10 + 50 + 10);
        assert_eq!(u.output_tokens, 12);
    }

    #[test]
    fn cursor_reads_only_new_complete_lines() {
        let p =
            std::env::temp_dir().join(format!("tower-usage-{}.jsonl", crate::paths::random_u64()));
        std::fs::write(
            &p,
            format!("{}\n{}", line("a", 1, 1), &line("b", 2, 2)[..20]),
        )
        .unwrap();
        let mut c = TranscriptCursor::new(&p.display().to_string());
        assert_eq!(c.advance().output_tokens, 1, "the partial line waits");
        std::fs::write(&p, format!("{}\n{}\n", line("a", 1, 1), line("b", 2, 2))).unwrap();
        assert_eq!(c.advance().output_tokens, 2);
        assert_eq!(c.advance().total(), 0);
        std::fs::write(&p, format!("{}\n", line("c", 3, 3))).unwrap();
        assert_eq!(
            c.advance().output_tokens,
            3,
            "a shorter file restarts from the top"
        );
    }
}
