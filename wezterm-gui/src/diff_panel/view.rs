//! Turning a [`ChangeSet`] into the flat list of rows the panel scrolls.

use super::ChangeSet;
use std::collections::HashSet;

/// One row of the panel's body. Indices point back into the change set, so a
/// row is a few words rather than a copy of the text it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// A file's header: status, path and counts.
    File(usize),
    /// The break between two hunks of the same file.
    Gap,
    Line {
        file: usize,
        hunk: usize,
        line: usize,
    },
    /// A file's note ("binary", "showing the first N lines").
    Note(usize),
}

/// Every row of `set`, skipping the bodies of files in `collapsed`.
pub fn flatten(set: &ChangeSet, collapsed: &HashSet<String>) -> Vec<Row> {
    let mut rows = Vec::new();
    for (file_idx, file) in set.files.iter().enumerate() {
        rows.push(Row::File(file_idx));
        if collapsed.contains(&file.path) {
            continue;
        }
        for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
            if hunk_idx > 0 {
                rows.push(Row::Gap);
            }
            rows.extend((0..hunk.lines.len()).map(|line| Row::Line {
                file: file_idx,
                hunk: hunk_idx,
                line,
            }));
        }
        if file.note.is_some() {
            rows.push(Row::Note(file_idx));
        }
    }
    rows
}

/// One line summing up the set: "4 files · 2 modified · 1 added · 1 deleted".
/// Empty when nothing changed.
pub fn summary(set: &ChangeSet) -> String {
    use super::FileStatus::*;
    if set.files.is_empty() {
        return String::new();
    }
    let count = |status| set.files.iter().filter(|f| f.status == status).count();
    let total = set.files.len();
    let mut parts = vec![format!(
        "{total} {}",
        if total == 1 { "file" } else { "files" }
    )];
    for (status, word) in [
        (Modified, "modified"),
        (Added, "added"),
        (Deleted, "deleted"),
        (Renamed, "renamed"),
        (Conflicted, "in conflict"),
    ] {
        let n = count(status);
        if n > 0 {
            parts.push(format!("{n} {word}"));
        }
    }
    parts.join(" \u{00b7} ")
}

/// Where each chip of the file index goes when chips flow left to right and
/// wrap, like words in a paragraph.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChipFlow {
    /// `(row, x)` of each chip that fits, in order.
    pub placed: Vec<(usize, f32)>,
    /// Rows in use.
    pub rows: usize,
    /// Chips that did not fit in `max_rows`.
    pub hidden: usize,
    /// `(row, x)` of the "+N more" marker, when chips are hidden.
    pub more_at: Option<(usize, f32)>,
}

/// Lay out chips of the given `widths` in at most `max_rows` rows of width
/// `avail`. When some do not fit, room is made on the last row for a marker
/// `more_width` wide.
pub fn flow_chips(
    widths: &[f32],
    avail: f32,
    gap: f32,
    max_rows: usize,
    more_width: f32,
) -> ChipFlow {
    let mut flow = ChipFlow::default();
    if max_rows == 0 || avail <= 0. {
        flow.hidden = widths.len();
        return flow;
    }
    let (mut row, mut x) = (0, 0.);
    for (idx, width) in widths.iter().enumerate() {
        let width = width.min(avail);
        if x > 0. && x + width > avail {
            row += 1;
            x = 0.;
        }
        if row >= max_rows {
            flow.hidden = widths.len() - idx;
            break;
        }
        flow.placed.push((row, x));
        x += width + gap;
    }
    flow.rows = flow.placed.last().map_or(0, |(row, _)| row + 1);
    if flow.hidden > 0 {
        // Give up chips from the end of the last row until the marker fits.
        let last_row = max_rows - 1;
        loop {
            let end = match flow.placed.last() {
                Some((row, x)) if *row == last_row => {
                    x + widths[flow.placed.len() - 1].min(avail) + gap
                }
                _ => 0.,
            };
            if end + more_width <= avail || end == 0. {
                flow.more_at = Some((last_row, end));
                break;
            }
            flow.placed.pop();
            flow.hidden += 1;
        }
    }
    flow
}

/// The last component of a `/`-separated path.
pub fn file_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// The row holding file `index`'s header.
pub fn row_of_file(rows: &[Row], index: usize) -> Option<usize> {
    rows.iter().position(|row| *row == Row::File(index))
}

/// The file whose section contains row `at`, and how far below that file's
/// header the row is.
pub fn file_at_row(rows: &[Row], at: usize) -> Option<(usize, usize)> {
    rows.iter()
        .enumerate()
        .take(at + 1)
        .rev()
        .find_map(|(idx, row)| match row {
            Row::File(file) => Some((*file, at - idx)),
            _ => None,
        })
}

/// Widest line number in the set, in digits: the gutter's column width.
pub fn line_number_digits(set: &ChangeSet) -> usize {
    let max = set
        .files
        .iter()
        .flat_map(|file| &file.hunks)
        .flat_map(|hunk| &hunk.lines)
        .flat_map(|line| [line.old_no, line.new_no])
        .flatten()
        .max()
        .unwrap_or(0);
    max.to_string().len()
}

/// Source text made safe to draw in fixed cells: tabs become spaces and
/// control characters, which would otherwise move the cursor or draw nothing,
/// become spaces too.
pub fn display_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// `text` cut to `cols` characters, keeping its end: for a path, the file
/// name matters more than the directories leading to it.
pub fn elide_start(text: &str, cols: usize) -> String {
    let len = text.chars().count();
    if len <= cols {
        return text.to_string();
    }
    if cols <= 1 {
        return "\u{2026}".repeat(cols);
    }
    let tail: String = text.chars().skip(len - (cols - 1)).collect();
    format!("\u{2026}{tail}")
}

/// The scroll offset after a wheel movement of `delta` notches (positive is
/// up), kept within what there is to scroll.
pub fn scrolled(offset: usize, delta: isize, total: usize, visible: usize) -> usize {
    let max = total.saturating_sub(visible);
    let step = delta.unsigned_abs().max(1) * 3;
    if delta > 0 {
        offset.saturating_sub(step)
    } else {
        (offset + step).min(max)
    }
    .min(max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_panel::{ChangeSource, FileChange, FileStatus};
    use std::path::PathBuf;

    fn set() -> ChangeSet {
        let mut two_hunks = FileChange::all_added("a.txt", "1\n2\n");
        two_hunks.hunks.push(two_hunks.hunks[0].clone());
        let mut binary = FileChange::new("b.bin", FileStatus::Modified);
        binary.note = Some("binary".into());
        ChangeSet {
            source: ChangeSource::Snapshot,
            root: PathBuf::from("/x"),
            files: vec![two_hunks, binary],
            note: None,
        }
    }

    #[test]
    fn rows_follow_files_hunks_and_notes() {
        let rows = flatten(&set(), &HashSet::new());
        assert_eq!(
            rows,
            vec![
                Row::File(0),
                Row::Line {
                    file: 0,
                    hunk: 0,
                    line: 0
                },
                Row::Line {
                    file: 0,
                    hunk: 0,
                    line: 1
                },
                Row::Gap,
                Row::Line {
                    file: 0,
                    hunk: 1,
                    line: 0
                },
                Row::Line {
                    file: 0,
                    hunk: 1,
                    line: 1
                },
                Row::File(1),
                Row::Note(1),
            ]
        );
    }

    #[test]
    fn a_collapsed_file_keeps_only_its_header() {
        let collapsed: HashSet<String> = ["a.txt".to_string()].into();
        assert_eq!(
            flatten(&set(), &collapsed),
            vec![Row::File(0), Row::File(1), Row::Note(1)]
        );
    }

    #[test]
    fn the_summary_counts_files_by_what_happened_to_them() {
        assert_eq!(
            summary(&set()),
            "2 files \u{00b7} 1 modified \u{00b7} 1 added"
        );
        let one = ChangeSet {
            files: vec![FileChange::new("gone", FileStatus::Deleted)],
            ..set()
        };
        assert_eq!(summary(&one), "1 file \u{00b7} 1 deleted");
        let none = ChangeSet {
            files: Vec::new(),
            ..set()
        };
        assert_eq!(summary(&none), "");
    }

    #[test]
    fn chips_wrap_like_words() {
        let flow = flow_chips(&[40., 40., 40.], 100., 10., 3, 30.);
        assert_eq!(flow.placed, vec![(0, 0.), (0, 50.), (1, 0.)]);
        assert_eq!((flow.rows, flow.hidden, flow.more_at), (2, 0, None));
    }

    #[test]
    fn chips_past_the_last_row_are_counted_and_the_marker_gets_room() {
        // Two fit per row; the marker needs a chip's place on the last row.
        let flow = flow_chips(&[40.; 6], 100., 10., 2, 30.);
        assert_eq!(flow.placed, vec![(0, 0.), (0, 50.), (1, 0.)]);
        assert_eq!(flow.hidden, 3);
        assert_eq!(flow.more_at, Some((1, 50.)));
    }

    #[test]
    fn a_chip_wider_than_the_panel_takes_a_row_of_its_own() {
        let flow = flow_chips(&[500., 40.], 100., 10., 3, 30.);
        assert_eq!(flow.placed, vec![(0, 0.), (1, 0.)]);
        assert_eq!(flow_chips(&[40.], 100., 10., 0, 30.).hidden, 1);
    }

    #[test]
    fn rows_and_files_map_both_ways() {
        let rows = flatten(&set(), &HashSet::new());
        assert_eq!(row_of_file(&rows, 1), Some(6));
        assert_eq!(file_at_row(&rows, 0), Some((0, 0)));
        assert_eq!(file_at_row(&rows, 4), Some((0, 4)));
        assert_eq!(file_at_row(&rows, 7), Some((1, 1)));
        assert_eq!(file_name("src/deep/a.rs"), "a.rs");
        assert_eq!(file_name("vendor/dir/"), "dir");
        assert_eq!(file_name("top"), "top");
    }

    #[test]
    fn the_gutter_fits_the_largest_line_number() {
        assert_eq!(line_number_digits(&set()), 1);
        let big = ChangeSet {
            files: vec![FileChange::all_added("x", &"l\n".repeat(120))],
            ..set()
        };
        assert_eq!(line_number_digits(&big), 3);
    }

    #[test]
    fn control_characters_never_reach_the_cells() {
        assert_eq!(display_text("\tif x {\r"), "    if x { ");
        assert_eq!(display_text("a\u{1b}[31mb"), "a [31mb");
    }

    #[test]
    fn long_paths_keep_their_file_name() {
        assert_eq!(elide_start("src/a.rs", 20), "src/a.rs");
        assert_eq!(elide_start("very/deep/dir/file.rs", 8), "\u{2026}file.rs");
        assert_eq!(elide_start("abc", 1), "\u{2026}");
        assert_eq!(elide_start("abc", 0), "");
    }

    #[test]
    fn scrolling_stops_at_both_ends() {
        assert_eq!(scrolled(0, 1, 100, 10), 0);
        assert_eq!(scrolled(0, -1, 100, 10), 3);
        assert_eq!(scrolled(89, -1, 100, 10), 90);
        assert_eq!(scrolled(10, 2, 100, 10), 4);
        // Nothing to scroll: stay at the top whatever the offset was.
        assert_eq!(scrolled(5, -1, 8, 10), 0);
    }
}
