//! Unified-diff parser shared by every provider.
//!
//! Understands the framing both `git diff` (`diff --git`, extended headers)
//! and `svn diff` (`Index:`) put around the common `---` / `+++` / `@@` core.

use super::{DiffLine, FileChange, FileStatus, Hunk, LineKind};

/// A hunk being filled: how many old and new lines its header still promises.
struct OpenHunk {
    hunk: Hunk,
    old_left: u32,
    new_left: u32,
    old_no: u32,
    new_no: u32,
}

fn parse_range(part: &str) -> Option<(u32, u32)> {
    let mut split = part.splitn(2, ',');
    let start = split.next()?.parse().ok()?;
    let count = match split.next() {
        Some(count) => count.parse().ok()?,
        None => 1,
    };
    Some((start, count))
}

/// `@@ -12,7 +12,9 @@ optional section heading`
fn parse_hunk_header(line: &str) -> Option<OpenHunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    Some(OpenHunk {
        hunk: Hunk {
            old_start,
            new_start,
            lines: Vec::new(),
        },
        old_left: old_count,
        new_left: new_count,
        old_no: old_start,
        new_no: new_start,
    })
}

/// A name from a `---` / `+++` line: without the trailing tab-separated
/// timestamp or revision, the `a/` / `b/` prefix, or surrounding quotes.
/// Returns the name and whatever followed the tab.
fn header_name<'a>(rest: &'a str, prefix: &str) -> (&'a str, &'a str) {
    let (name, suffix) = rest.split_once('\t').unwrap_or((rest, ""));
    let name = name.trim_matches('"');
    (name.strip_prefix(prefix).unwrap_or(name), suffix)
}

/// The path from `diff --git a/X b/Y`. Only needed when no `---` / `+++`
/// lines follow (binary files, pure renames, mode changes).
fn git_header_path(rest: &str) -> Option<String> {
    let rest = rest.trim_matches('"');
    let body = rest.strip_prefix("a/")?;
    // Not a rename: the two halves are the same path, so split in the middle.
    // That survives paths containing " b/", which a search for it would not.
    if body.len() >= 3 && (body.len() - 3) % 2 == 0 {
        let half = (body.len() - 3) / 2;
        if body.is_char_boundary(half) && body[half..].starts_with(" b/") {
            let (left, right) = (&body[..half], &body[half + 3..]);
            if left == right {
                return Some(left.to_string());
            }
        }
    }
    body.rsplit_once(" b/").map(|(_, new)| new.to_string())
}

fn finish_hunk(file: &mut Option<FileChange>, open: &mut Option<OpenHunk>) {
    if let (Some(file), Some(open)) = (file.as_mut(), open.take()) {
        if !open.hunk.lines.is_empty() {
            file.hunks.push(open.hunk);
        }
    }
}

fn finish_file(files: &mut Vec<FileChange>, file: &mut Option<FileChange>) {
    if let Some(mut file) = file.take() {
        if file.path.is_empty() {
            return;
        }
        file.cap_lines();
        files.push(file);
    }
}

pub fn parse(text: &str) -> Vec<FileChange> {
    let mut files = Vec::new();
    let mut file: Option<FileChange> = None;
    let mut open: Option<OpenHunk> = None;

    for line in text.lines() {
        // Inside a hunk the header's line counts decide what a line is, so a
        // removed line that happens to read `--- x` is not taken for a header.
        if let Some(hunk) = open.as_mut() {
            if hunk.old_left > 0 || hunk.new_left > 0 {
                let (kind, body) = match line.as_bytes().first() {
                    Some(b'+') => (LineKind::Added, &line[1..]),
                    Some(b'-') => (LineKind::Removed, &line[1..]),
                    Some(b' ') => (LineKind::Context, &line[1..]),
                    // "\ No newline at end of file" describes the line above.
                    Some(b'\\') => continue,
                    // Some tools strip the lone space of an empty context line.
                    None => (LineKind::Context, ""),
                    Some(_) => (LineKind::Context, line),
                };
                let (old_no, new_no) = match kind {
                    LineKind::Added => {
                        hunk.new_left = hunk.new_left.saturating_sub(1);
                        hunk.new_no += 1;
                        (None, Some(hunk.new_no - 1))
                    }
                    LineKind::Removed => {
                        hunk.old_left = hunk.old_left.saturating_sub(1);
                        hunk.old_no += 1;
                        (Some(hunk.old_no - 1), None)
                    }
                    LineKind::Context => {
                        hunk.old_left = hunk.old_left.saturating_sub(1);
                        hunk.new_left = hunk.new_left.saturating_sub(1);
                        hunk.old_no += 1;
                        hunk.new_no += 1;
                        (Some(hunk.old_no - 1), Some(hunk.new_no - 1))
                    }
                };
                if let Some(file) = file.as_mut() {
                    match kind {
                        LineKind::Added => file.added += 1,
                        LineKind::Removed => file.removed += 1,
                        LineKind::Context => {}
                    }
                }
                hunk.hunk.lines.push(DiffLine {
                    kind,
                    old_no,
                    new_no,
                    text: body.to_string(),
                });
                continue;
            }
        }

        if let Some(next) = parse_hunk_header(line) {
            finish_hunk(&mut file, &mut open);
            if file.is_some() {
                open = Some(next);
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("diff --git ") {
            finish_hunk(&mut file, &mut open);
            finish_file(&mut files, &mut file);
            file = Some(FileChange::new(
                git_header_path(rest).unwrap_or_default(),
                FileStatus::Modified,
            ));
            continue;
        }
        if let Some(rest) = line.strip_prefix("Index: ") {
            finish_hunk(&mut file, &mut open);
            finish_file(&mut files, &mut file);
            file = Some(FileChange::new(rest.trim(), FileStatus::Modified));
            continue;
        }

        if let Some(rest) = line.strip_prefix("--- ") {
            // A bare unified diff has no framing line: `---` after a finished
            // hunk, or with no file open, starts the next file.
            let starts_file = match file.as_ref() {
                None => true,
                Some(file) => !file.hunks.is_empty() || open.is_some(),
            };
            if starts_file {
                finish_hunk(&mut file, &mut open);
                finish_file(&mut files, &mut file);
                file = Some(FileChange::new("", FileStatus::Modified));
            }
            let (name, suffix) = header_name(rest, "a/");
            let current = file.as_mut().expect("a file was just opened");
            if name == "/dev/null" || suffix.contains("(nonexistent)") {
                current.status = FileStatus::Added;
            } else if current.path.is_empty() {
                current.path = name.to_string();
            }
            continue;
        }

        let Some(current) = file.as_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("+++ ") {
            let (name, suffix) = header_name(rest, "b/");
            if name == "/dev/null" || suffix.contains("(nonexistent)") {
                current.status = FileStatus::Deleted;
            } else if current.status != FileStatus::Renamed {
                current.path = name.to_string();
            }
        } else if line.starts_with("new file mode") {
            current.status = FileStatus::Added;
        } else if line.starts_with("deleted file mode") {
            current.status = FileStatus::Deleted;
        } else if let Some(from) = line.strip_prefix("rename from ") {
            current.status = FileStatus::Renamed;
            current.old_path = Some(from.trim_matches('"').to_string());
        } else if let Some(to) = line.strip_prefix("rename to ") {
            current.status = FileStatus::Renamed;
            current.path = to.trim_matches('"').to_string();
        } else if line.starts_with("Binary files ")
            || line.starts_with("GIT binary patch")
            || line.starts_with("Cannot display: file marked as a binary type")
        {
            current.note = Some("binary".to_string());
        }
    }

    finish_hunk(&mut file, &mut open);
    finish_file(&mut files, &mut file);
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIT: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,5 @@ fn main() {
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
+    extra();
 }

@@ -10 +11 @@
---- a removed line that looks like a header
+++++ an added line that looks like a header
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
\\ No newline at end of file
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 4444444..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/old name.txt b/new name.txt
similarity index 100%
rename from old name.txt
rename to new name.txt
diff --git a/logo.png b/logo.png
index 5555555..6666666 100644
Binary files a/logo.png and b/logo.png differ
";

    #[test]
    fn git_output_is_split_into_files_with_statuses() {
        let files = parse(GIT);
        let summary: Vec<(&str, FileStatus, usize, usize)> = files
            .iter()
            .map(|f| (f.path.as_str(), f.status, f.added, f.removed))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("src/main.rs", FileStatus::Modified, 3, 2),
                ("new.txt", FileStatus::Added, 2, 0),
                ("gone.txt", FileStatus::Deleted, 0, 1),
                ("new name.txt", FileStatus::Renamed, 0, 0),
                ("logo.png", FileStatus::Modified, 0, 0),
            ]
        );
        assert_eq!(files[3].old_path.as_deref(), Some("old name.txt"));
        assert_eq!(files[4].note.as_deref(), Some("binary"));
    }

    #[test]
    fn line_numbers_follow_each_side() {
        let files = parse(GIT);
        let hunk = &files[0].hunks[0];
        assert_eq!((hunk.old_start, hunk.new_start), (1, 1));
        let numbers: Vec<(LineKind, Option<u32>, Option<u32>)> = hunk
            .lines
            .iter()
            .map(|l| (l.kind, l.old_no, l.new_no))
            .collect();
        assert_eq!(
            numbers,
            vec![
                (LineKind::Context, Some(1), Some(1)),
                (LineKind::Removed, Some(2), None),
                (LineKind::Added, None, Some(2)),
                (LineKind::Added, None, Some(3)),
                (LineKind::Context, Some(3), Some(4)),
                (LineKind::Context, Some(4), Some(5)),
            ]
        );
        assert_eq!(hunk.lines[2].text, "    println!(\"new\");");
    }

    #[test]
    fn hunk_lines_that_look_like_headers_stay_hunk_lines() {
        let files = parse(GIT);
        let hunk = &files[0].hunks[1];
        assert_eq!(hunk.lines.len(), 2);
        assert_eq!(hunk.lines[0].kind, LineKind::Removed);
        assert_eq!(
            hunk.lines[0].text,
            "--- a removed line that looks like a header"
        );
        assert_eq!(hunk.lines[1].kind, LineKind::Added);
        assert_eq!(hunk.lines[1].new_no, Some(11));
    }

    #[test]
    fn svn_output_is_understood() {
        let svn = "\
Index: trunk/a.c
===================================================================
--- trunk/a.c\t(revision 7)
+++ trunk/a.c\t(working copy)
@@ -1,2 +1,2 @@
 int a;
-int b;
+int c;
Index: trunk/added.c
===================================================================
--- trunk/added.c\t(nonexistent)
+++ trunk/added.c\t(working copy)
@@ -0,0 +1 @@
+int z;
Index: trunk/removed.c
===================================================================
--- trunk/removed.c\t(revision 7)
+++ trunk/removed.c\t(nonexistent)
@@ -1 +0,0 @@
-int y;
Index: trunk/pic.png
===================================================================
Cannot display: file marked as a binary type.
svn:mime-type = application/octet-stream
";
        let files = parse(svn);
        let summary: Vec<(&str, FileStatus, usize, usize)> = files
            .iter()
            .map(|f| (f.path.as_str(), f.status, f.added, f.removed))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("trunk/a.c", FileStatus::Modified, 1, 1),
                ("trunk/added.c", FileStatus::Added, 1, 0),
                ("trunk/removed.c", FileStatus::Deleted, 0, 1),
                ("trunk/pic.png", FileStatus::Modified, 0, 0),
            ]
        );
        assert_eq!(files[3].note.as_deref(), Some("binary"));
    }

    #[test]
    fn a_bare_unified_diff_needs_no_framing_line() {
        let bare = "\
--- one.txt\t2026-01-01
+++ one.txt\t2026-01-02
@@ -1 +1 @@
-a
+b
--- two.txt
+++ two.txt
@@ -1 +1,2 @@
 x
+y
";
        let files = parse(bare);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "one.txt");
        assert_eq!((files[1].path.as_str(), files[1].added), ("two.txt", 1));
    }

    #[test]
    fn a_path_containing_the_separator_is_not_split_on_it() {
        assert_eq!(
            git_header_path("a/x b/y.txt b/x b/y.txt").as_deref(),
            Some("x b/y.txt")
        );
        assert_eq!(git_header_path("a/old b/new").as_deref(), Some("new"));
    }

    #[test]
    fn garbage_yields_no_files() {
        assert!(parse("").is_empty());
        assert!(parse("fatal: not a git repository\n").is_empty());
    }
}
