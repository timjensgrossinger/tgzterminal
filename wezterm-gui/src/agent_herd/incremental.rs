//! Reading only what was appended to a transcript since the last read.
//!
//! A transcript is an append-only file of newline-terminated records that the
//! agent is still writing. Readers that fold every record into some running
//! state (which files were touched, how many tokens were used) keep a byte
//! offset per file and call [`read_appended`] to feed the new records to
//! their fold.
//!
//! Blocking file I/O: call it off the GUI thread.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Read size per chunk.
const CHUNK_BYTES: usize = 1024 * 1024;

/// Pass the complete lines in `bytes` to `fold`; returns how many bytes that
/// consumed. A trailing line without its newline is left for the next read:
/// the agent may still be writing it.
fn fold_complete_lines(bytes: &[u8], fold: &mut dyn FnMut(&str)) -> usize {
    let Some(last_newline) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return 0;
    };
    for line in bytes[..last_newline].split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(line) = std::str::from_utf8(line) {
            fold(line);
        }
    }
    last_newline + 1
}

/// Feed `fold` every complete line of `path` from `*offset` up to `len`, and
/// advance `*offset` past them. `*offset` always ends just past a complete
/// line.
///
/// `len` is the file's current length, which the caller has already compared
/// with `*offset` (a file that shrank was rewritten and must be folded again
/// from zero with fresh state).
///
/// At most `max_bytes` are read: with more than that outstanding, the read
/// starts that far before `len` and the older records are skipped. Returns
/// true when that happened.
pub(crate) fn read_appended(
    path: &Path,
    len: u64,
    offset: &mut u64,
    max_bytes: u64,
    fold: &mut dyn FnMut(&str),
) -> std::io::Result<bool> {
    // Jumping forward lands mid-line, so the partial line up to the next
    // newline is dropped.
    let mut skip_partial = false;
    if len.saturating_sub(*offset) > max_bytes {
        *offset = len - max_bytes;
        skip_partial = true;
    }
    let skipped = skip_partial;
    if *offset >= len {
        return Ok(skipped);
    }

    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(*offset))?;
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; CHUNK_BYTES];
    let mut remaining = len - *offset;
    while remaining > 0 {
        let want = (remaining as usize).min(CHUNK_BYTES);
        let read = file.read(&mut chunk[..want])?;
        if read == 0 {
            break;
        }
        remaining -= read as u64;
        pending.extend_from_slice(&chunk[..read]);
        if skip_partial {
            let Some(newline) = pending.iter().position(|byte| *byte == b'\n') else {
                *offset += pending.len() as u64;
                pending.clear();
                continue;
            };
            *offset += newline as u64 + 1;
            pending.drain(..=newline);
            skip_partial = false;
        }
        let consumed = fold_complete_lines(&pending, fold);
        *offset += consumed as u64;
        pending.drain(..consumed);
    }
    Ok(skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn read(path: &Path, offset: &mut u64, max_bytes: u64) -> (Vec<String>, bool) {
        let len = std::fs::metadata(path).unwrap().len();
        let mut lines = vec![];
        let skipped = read_appended(path, len, offset, max_bytes, &mut |line| {
            lines.push(line.to_string())
        })
        .unwrap();
        (lines, skipped)
    }

    #[test]
    fn later_reads_see_only_what_was_appended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "one\ntwo\n").unwrap();

        let mut offset = 0;
        assert_eq!(read(&path, &mut offset, u64::MAX).0, vec!["one", "two"]);
        assert!(read(&path, &mut offset, u64::MAX).0.is_empty());

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"three\n").unwrap();
        assert_eq!(read(&path, &mut offset, u64::MAX).0, vec!["three"]);
    }

    #[test]
    fn a_line_still_being_written_waits_for_its_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "one\ntw").unwrap();

        let mut offset = 0;
        assert_eq!(read(&path, &mut offset, u64::MAX).0, vec!["one"]);
        assert_eq!(offset, 4);

        std::fs::write(&path, "one\ntwo\n").unwrap();
        assert_eq!(read(&path, &mut offset, u64::MAX).0, vec!["two"]);
    }

    #[test]
    fn past_the_cap_the_oldest_records_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "aaaa\nbbbb\ncccc\n").unwrap();

        // 7 bytes back from the end lands inside "bbbb", which is dropped.
        let mut offset = 0;
        let (lines, skipped) = read(&path, &mut offset, 7);
        assert_eq!(lines, vec!["cccc"]);
        assert!(skipped);
        assert_eq!(offset, 15);
    }
}
