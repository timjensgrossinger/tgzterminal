//! The sidebar's search field as a search over everything: tabs, commands,
//! pane layouts, ssh hosts, past agent sessions, files and scrollback.
//!
//! Pure: candidates in, ranked rows out. The window gathers candidates from
//! its caches (never from disk on the paint path) and acts on the row picked.
//!
//! A query may start with a prefix that narrows it to one kind, as in Warp:
//! `>` commands (and layouts, ssh hosts), `@` sessions, `/` files and `#`
//! scrollback. Without one every kind is searched and each shows its best
//! few.

use crate::overlay::selector::{matcher_pattern, matcher_score};
use config::keyassignment::{KeyAssignment, PaneLayout};
use mux::pane::PaneId;
use std::path::PathBuf;
use wezterm_term::StableRowIndex;

/// What kind of thing a result is. The order is the order of the sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Source {
    Tab,
    Command,
    Layout,
    Ssh,
    Session,
    File,
    Scrollback,
}

impl Source {
    pub(crate) fn heading(self) -> &'static str {
        match self {
            Source::Tab => "Tabs",
            Source::Command => "Commands",
            Source::Layout => "Layouts",
            Source::Ssh => "SSH hosts",
            Source::Session => "Sessions",
            Source::File => "Files",
            Source::Scrollback => "In scrollback",
        }
    }
}

/// The narrowing a prefix asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    All,
    /// `>`: commands, layouts and ssh hosts, the things one runs.
    Actions,
    /// `@`
    Sessions,
    /// `/`
    Files,
    /// `#`
    Scrollback,
}

impl Scope {
    pub(crate) fn includes(self, source: Source) -> bool {
        match self {
            Scope::All => true,
            Scope::Actions => matches!(source, Source::Command | Source::Layout | Source::Ssh),
            Scope::Sessions => source == Source::Session,
            Scope::Files => source == Source::File,
            Scope::Scrollback => source == Source::Scrollback,
        }
    }
}

/// The query's scope and what is left to match once the prefix is gone.
pub(crate) fn parse_query(query: &str) -> (Scope, &str) {
    let trimmed = query.trim_start();
    let scope = match trimmed.chars().next() {
        Some('>') => Scope::Actions,
        Some('@') => Scope::Sessions,
        Some('/') => Scope::Files,
        Some('#') => Scope::Scrollback,
        _ => return (Scope::All, query.trim()),
    };
    (scope, trimmed[1..].trim())
}

/// What picking a result does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Target {
    /// Activate the tab at this index.
    Tab(usize),
    /// Run a key assignment, as the command palette does.
    Command(KeyAssignment),
    /// Rearrange the active tab.
    Preset(PaneLayout),
    /// Open a `tgz_layouts` entry by name.
    UserLayout(String),
    /// Connect to an ssh domain by name.
    Ssh(String),
    /// Resume a past agent session.
    Session {
        adapter_id: String,
        session_id: String,
    },
    /// Open `rel` under `root` in the editor.
    File { root: PathBuf, rel: String },
    /// Show `row` of a pane's scrollback.
    Scrollback {
        pane_id: PaneId,
        row: StableRowIndex,
    },
}

/// Something that may match: what is shown, and the text matched against.
#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub source: Source,
    pub label: String,
    /// Right-aligned beside the label: where it is, its key, its kind.
    pub detail: String,
    /// Matched against; may say more than the label (a session's first
    /// prompt and folder, a command's description).
    pub haystack: String,
    pub target: Target,
}

/// One row of the results list.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Row {
    /// A section heading; not selectable.
    Heading(Source),
    Hit {
        source: Source,
        label: String,
        detail: String,
        target: Target,
    },
}

impl Row {
    pub(crate) fn is_hit(&self) -> bool {
        matches!(self, Row::Hit { .. })
    }
}

/// Results per section when every kind is searched.
pub(crate) const MIXED_CAP: usize = 5;
/// Results when a prefix narrows the search to one kind.
pub(crate) const NARROW_CAP: usize = 50;

/// The candidates that match `needle`, best first, grouped by section and
/// capped per section, with a heading over each section. Candidates whose
/// source the scope leaves out are skipped. Already-ranked candidates (files
/// and scrollback, matched elsewhere) pass through with `pre_ranked`.
pub(crate) fn rank(scope: Scope, needle: &str, candidates: &[Candidate]) -> Vec<Row> {
    if needle.is_empty() {
        return vec![];
    }
    let pattern = matcher_pattern(needle);
    let cap = if scope == Scope::All {
        MIXED_CAP
    } else {
        NARROW_CAP
    };
    let mut scored: Vec<(Source, u32, usize)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| scope.includes(c.source))
        .filter_map(|(i, c)| {
            // A label hit outranks the same hit buried in the haystack.
            let on_label = matcher_score(&pattern, &c.label).map(|s| s.saturating_mul(2));
            let anywhere = matcher_score(&pattern, &c.haystack);
            on_label.max(anywhere).map(|score| (c.source, score, i))
        })
        .collect();
    // Section order, then best first, then the order given (stable).
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));

    let mut rows = vec![];
    let mut current: Option<(Source, usize)> = None;
    for (source, _, i) in scored {
        let shown = match current {
            Some((s, n)) if s == source => n,
            _ => {
                rows.push(Row::Heading(source));
                0
            }
        };
        if shown >= cap {
            continue;
        }
        current = Some((source, shown + 1));
        let c = &candidates[i];
        rows.push(Row::Hit {
            source,
            label: c.label.clone(),
            detail: c.detail.clone(),
            target: c.target.clone(),
        });
    }
    rows
}

/// Insert hits that were matched elsewhere (files on a worker, scrollback by
/// the panes) into `rows` as their own section, best first as given.
pub(crate) fn with_section(
    mut rows: Vec<Row>,
    source: Source,
    hits: &[Candidate],
    cap: usize,
) -> Vec<Row> {
    rows.retain(|row| match row {
        Row::Heading(s) => *s != source,
        Row::Hit { source: s, .. } => *s != source,
    });
    if hits.is_empty() {
        return rows;
    }
    let mut section = vec![Row::Heading(source)];
    section.extend(hits.iter().take(cap).map(|c| Row::Hit {
        source,
        label: c.label.clone(),
        detail: c.detail.clone(),
        target: c.target.clone(),
    }));
    // After every section that sorts before it.
    let at = rows
        .iter()
        .position(|row| matches!(row, Row::Heading(s) if *s > source))
        .unwrap_or(rows.len());
    rows.splice(at..at, section);
    rows
}

/// The selectable row `step` places from `from` (wrapping), skipping
/// headings. `None` when nothing is selectable.
pub(crate) fn step_selection(rows: &[Row], from: Option<usize>, step: isize) -> Option<usize> {
    let hits: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].is_hit()).collect();
    if hits.is_empty() {
        return None;
    }
    let at = from.and_then(|from| hits.iter().position(|&i| i == from));
    let next = match at {
        None if step < 0 => hits.len() - 1,
        None => 0,
        Some(at) => (at as isize + step).rem_euclid(hits.len() as isize) as usize,
    };
    Some(hits[next])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(source: Source, label: &str, haystack: &str) -> Candidate {
        Candidate {
            source,
            label: label.to_string(),
            detail: String::new(),
            haystack: haystack.to_string(),
            target: Target::Command(KeyAssignment::Nop),
        }
    }

    fn labels(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Heading(s) => format!("# {}", s.heading()),
                Row::Hit { label, .. } => label.clone(),
            })
            .collect()
    }

    #[test]
    fn a_prefix_narrows_and_is_dropped_from_the_needle() {
        assert_eq!(parse_query(">split"), (Scope::Actions, "split"));
        assert_eq!(parse_query("  @ fix login"), (Scope::Sessions, "fix login"));
        assert_eq!(parse_query("/main.rs"), (Scope::Files, "main.rs"));
        assert_eq!(parse_query("#error"), (Scope::Scrollback, "error"));
        assert_eq!(parse_query(" grid "), (Scope::All, "grid"));
    }

    #[test]
    fn results_come_in_sections_in_a_fixed_order() {
        let candidates = vec![
            candidate(
                Source::Session,
                "fix the grid layout",
                "fix the grid layout ~/repo",
            ),
            candidate(Source::Layout, "Grid", "Grid"),
            candidate(Source::Command, "Pane layout: Grid", "Pane layout: Grid"),
            candidate(Source::Command, "New tab", "New tab"),
        ];
        let rows = rank(Scope::All, "grid", &candidates);
        assert_eq!(
            labels(&rows),
            vec![
                "# Commands",
                "Pane layout: Grid",
                "# Layouts",
                "Grid",
                "# Sessions",
                "fix the grid layout",
            ]
        );
    }

    #[test]
    fn a_session_is_found_by_its_first_prompt_and_its_folder() {
        let session = candidate(
            Source::Session,
            "Refactor auth",
            "Refactor auth · please fix the token refresh · ~/work/billing-api · main",
        );
        let candidates = vec![session];
        assert_eq!(rank(Scope::Sessions, "token refresh", &candidates).len(), 2);
        assert_eq!(rank(Scope::Sessions, "billing-api", &candidates).len(), 2);
        assert!(rank(Scope::Sessions, "kubernetes", &candidates).is_empty());
        // A prefix keeps other kinds out.
        assert!(rank(Scope::Actions, "auth", &candidates).is_empty());
    }

    #[test]
    fn the_mixed_view_shows_a_few_per_section_and_a_prefix_more() {
        let candidates: Vec<Candidate> = (0..20)
            .map(|i| candidate(Source::Command, &format!("split {i}"), "split"))
            .collect();
        assert_eq!(rank(Scope::All, "split", &candidates).len(), 1 + MIXED_CAP);
        assert_eq!(rank(Scope::Actions, "split", &candidates).len(), 1 + 20);
        assert!(rank(Scope::All, "", &candidates).is_empty());
    }

    #[test]
    fn a_section_matched_elsewhere_slots_in_by_order() {
        let rows = rank(
            Scope::All,
            "x",
            &[
                candidate(Source::Command, "x cmd", "x"),
                candidate(Source::Scrollback, "x line", "x"),
            ],
        );
        let files = vec![candidate(Source::File, "src/x.rs", "src/x.rs")];
        let rows = with_section(rows, Source::File, &files, 5);
        assert_eq!(
            labels(&rows),
            vec![
                "# Commands",
                "x cmd",
                "# Files",
                "src/x.rs",
                "# In scrollback",
                "x line"
            ]
        );
        // Replaced, not doubled, when it arrives again.
        let rows = with_section(rows, Source::File, &files, 5);
        assert_eq!(
            rows.iter()
                .filter(|r| **r == Row::Heading(Source::File))
                .count(),
            1
        );
    }

    #[test]
    fn the_selection_skips_headings_and_wraps() {
        let rows = rank(
            Scope::All,
            "a",
            &[
                candidate(Source::Command, "a1", "a"),
                candidate(Source::Session, "a2", "a"),
            ],
        );
        // [# Commands, a1, # Sessions, a2]
        assert_eq!(step_selection(&rows, None, 1), Some(1));
        assert_eq!(step_selection(&rows, Some(1), 1), Some(3));
        assert_eq!(step_selection(&rows, Some(3), 1), Some(1));
        assert_eq!(step_selection(&rows, None, -1), Some(3));
        assert_eq!(step_selection(&[], None, 1), None);
    }
}
