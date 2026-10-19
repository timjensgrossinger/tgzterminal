//! The quick preview: the start of a file, as text ready to draw in fixed
//! cells. Read on a worker thread; a large file is only read as far as the
//! preview goes.

use std::io::Read;
use std::path::Path;

/// Most bytes read for a preview.
pub const MAX_BYTES: usize = 128 * 1024;
/// Most lines kept.
pub const MAX_LINES: usize = 2000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preview {
    Text {
        lines: Vec<String>,
        /// The file goes on past what was read.
        truncated: bool,
    },
    /// Not text; the size says what there is.
    Binary { bytes: u64 },
    /// Why the file could not be read.
    Unreadable(String),
}

/// The start of the file at `path`.
pub fn read_preview(path: &Path) -> Preview {
    let meta = match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Preview::Unreadable("Not a file".to_string()),
        Err(err) => return Preview::Unreadable(err.to_string()),
    };
    let mut bytes = Vec::with_capacity((meta.len() as usize).min(MAX_BYTES));
    let read = std::fs::File::open(path)
        .and_then(|file| file.take(MAX_BYTES as u64).read_to_end(&mut bytes));
    if let Err(err) = read {
        return Preview::Unreadable(err.to_string());
    }
    preview_of(&bytes, meta.len())
}

/// [`read_preview`] for bytes already read; `len` is the whole file's size.
pub fn preview_of(bytes: &[u8], len: u64) -> Preview {
    if crate::diff_panel::looks_binary(bytes) {
        return Preview::Binary { bytes: len };
    }
    let mut truncated = (bytes.len() as u64) < len;
    let text = String::from_utf8_lossy(bytes);
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        if lines.len() == MAX_LINES {
            truncated = true;
            break;
        }
        lines.push(crate::diff_panel::view::display_text(line));
    }
    // A cut in the middle of a line leaves half a line: drop it.
    if truncated && (bytes.len() as u64) < len && !text.ends_with('\n') {
        lines.pop();
    }
    Preview::Text { lines, truncated }
}

/// A size as people read it: `812 B`, `14.2 KB`, `3.0 MB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_split_into_drawable_lines() {
        let text = b"fn main() {\n\tprintln!();\r\n}\n";
        let preview = preview_of(text, text.len() as u64);
        assert_eq!(
            preview,
            Preview::Text {
                lines: vec!["fn main() {".into(), "    println!();".into(), "}".into()],
                truncated: false,
            }
        );
    }

    #[test]
    fn a_file_read_only_in_part_says_so_and_drops_the_cut_line() {
        let preview = preview_of(b"one\ntwo\nthr", 1000);
        assert_eq!(
            preview,
            Preview::Text {
                lines: vec!["one".into(), "two".into()],
                truncated: true,
            }
        );
    }

    #[test]
    fn binary_is_not_shown() {
        assert_eq!(preview_of(b"PNG\0\x01", 5), Preview::Binary { bytes: 5 });
    }

    #[test]
    fn reads_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "hello\n").unwrap();
        assert_eq!(
            read_preview(&path),
            Preview::Text {
                lines: vec!["hello".into()],
                truncated: false
            }
        );
        assert!(matches!(read_preview(dir.path()), Preview::Unreadable(_)));
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(812), "812 B");
        assert_eq!(human_size(14_540), "14.2 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
    }
}
