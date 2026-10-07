//! Masking of token-shaped strings ("secrets") in terminal panes.
//!
//! Two consumers share one rule set:
//!
//! * painting: [`scan_logical_lines`] finds the cell columns to hide in the
//!   visible rows and [`mask_line`] returns a copy of a row with those cells
//!   replaced by bullets. The terminal model is never altered, so selection,
//!   search and a plain copy still see the real text.
//! * copying: [`SecretRules::redact`] rewrites text on its way to the
//!   clipboard for the pane Copy actions and `CopyRedactedTo`.
//!
//! Matching is by shape and best effort. It hides what it recognises and
//! promises nothing about what it does not.
//!
//! Privacy: nothing here logs matched text, only counts.

use crate::TermWindow;
use config::keyassignment::ClipboardCopyDestination;
use config::SecretMaskingConfig;
use mux::pane::{LogicalLine, Pane, PaneId};
use regex::Regex;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use termwiz::cell::Cell;
use termwiz::surface::{Line, SequenceNo, SEQ_ZERO};
use wezterm_term::StableRowIndex;
use window::WindowOps;

/// What a redacted secret is replaced with in copied text.
pub(crate) const REDACTED: &str = "[REDACTED]";

/// What a masked cell is painted as.
const MASK_CHAR: char = '•';

/// Rows above the viewport that are scanned as well, so a private key whose
/// `BEGIN` line has just scrolled off still has its body masked.
const SCAN_MARGIN_ROWS: StableRowIndex = 48;

/// Panes whose scan is remembered; past this the cache is dropped rather than
/// tracking which panes closed.
const PANE_CACHE_CAP: usize = 128;

/// Built-in rules for strings recognisable by their own shape. Where a rule
/// has a capture group, group 1 is the secret and the rest of the match is
/// context that stays visible. See also [`assignment_patterns`].
const BUILTIN_PATTERNS: &[&str] = &[
    // AWS access key id
    r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
    // GitHub tokens
    r"\bgh[pousr]_[A-Za-z0-9]{36,}\b",
    r"\bgithub_pat_[A-Za-z0-9_]{22,}\b",
    // GitLab personal access token
    r"\bglpat-[A-Za-z0-9_-]{20,}",
    // Slack tokens
    r"\bxox[abeprs]-[A-Za-z0-9-]{10,}",
    // Stripe live keys
    r"\b[sr]k_live_[A-Za-z0-9]{16,}",
    // `sk-` style API keys
    r"\bsk-[A-Za-z0-9_-]{20,}",
    // Google API key
    r"\bAIza[0-9A-Za-z_-]{35}",
    // JSON web token
    r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
    // Authorization: Bearer <token>
    r"(?i)\bbearer\s+([A-Za-z0-9._~+/=-]{16,})",
    // scheme://user:password@host
    r"[A-Za-z][A-Za-z0-9+.-]*://[^\s/:@]+:([^\s/@]+)@",
];

/// Words that, inside a name, say its value is a password or secret, in
/// English and German. A value under such a name is hidden whatever its
/// length: a four-character password is still a password.
const PASSWORD_NAME_WORDS: &str =
    "password|passwd|passphrase|passwort|kennwort|secret|geheimnis|geheim|zugangscode";

/// Words for keys, tokens and credentials, in English and German. These also
/// turn up as ordinary labels (`tokens: 5000`), so a value under such a name
/// is hidden only from [`KEY_VALUE_MIN_LEN`] characters up.
const KEY_NAME_WORDS: &str = "token|api[_-]?key|access[_-]?key|private[_-]?key|credentials?|\
     schlüssel|schluessel|zugangsdaten";

const KEY_VALUE_MIN_LEN: usize = 8;

/// Rules for a value assigned to a name containing one of `words`. Group 1 is
/// the value. Four shapes, each narrow enough that source code scrolling past
/// is not blanked out:
///
/// * `PASSWORD=value`, `--token=value`: no spaces, and the value ends the word;
/// * `api_key: "value"`, `passwort = 'value'`: a quoted value;
/// * `password: value` at the end of a line (YAML, and prompts that echo),
///   also when a closing quote follows, as in `echo "Passwort: geheim"`;
/// * `--password value`: a command-line flag and its argument.
fn assignment_patterns(words: &str, min_len: usize) -> Vec<String> {
    // `\w` is Unicode-aware, so names such as `Geheimschlüssel` match whole.
    let name = format!(r"[\w.-]*(?:{words})[\w-]*");
    let bare = format!(r#"[^\s"',;(){{}}<>]{{{min_len},}}"#);
    vec![
        format!(r#"(?im)\b{name}["']?=["']?({bare})(?:["',;\s]|$)"#),
        format!(r#"(?i)\b{name}["']?\s*[=:]\s*["']([^"'\s]{{{min_len},}})["']"#),
        format!(r#"(?im)\b{name}:\s+({bare})["']?\s*$"#),
        format!(
            r#"(?im)(?:^|\s)--?{name}\s+["']?([^\s"'-][^\s"']{{{rest},}})["']?(?:\s|$)"#,
            rest = min_len.saturating_sub(1)
        ),
    ]
}

/// Every built-in rule: the fixed token shapes plus the name-based ones.
fn builtin_patterns() -> Vec<String> {
    let mut patterns: Vec<String> = BUILTIN_PATTERNS.iter().map(|p| p.to_string()).collect();
    patterns.extend(assignment_patterns(PASSWORD_NAME_WORDS, 1));
    patterns.extend(assignment_patterns(KEY_NAME_WORDS, KEY_VALUE_MIN_LEN));
    patterns
}

/// The body of a PEM private key. Spans lines, so it only ever matches copied
/// text; on screen the body is found line by line (see [`scan_logical_lines`]).
const PEM_BLOCK_PATTERN: &str =
    r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----(.+?)-----END [A-Z0-9 ]*PRIVATE KEY-----";

fn is_pem_begin(text: &str) -> bool {
    text.contains("-----BEGIN ") && text.contains("PRIVATE KEY-----")
}

fn is_pem_end(text: &str) -> bool {
    text.contains("-----END ") && text.contains("PRIVATE KEY-----")
}

/// The compiled rule set for one configuration.
pub(crate) struct SecretRules {
    rules: Vec<Regex>,
    pem_block: Option<Regex>,
}

impl SecretRules {
    /// A user pattern that does not compile is skipped and reported once per
    /// compile; it must not take the built-in rules down with it.
    pub(crate) fn compile(config: &SecretMaskingConfig) -> Self {
        let mut rules = vec![];
        let mut pem_block = None;
        if config.builtin_patterns {
            for pattern in builtin_patterns() {
                match Regex::new(&pattern) {
                    Ok(regex) => rules.push(regex),
                    Err(err) => log::error!("secret masking: built-in rule is invalid: {err:#}"),
                }
            }
            pem_block = Regex::new(PEM_BLOCK_PATTERN).ok();
        }
        for (idx, pattern) in config.patterns.iter().enumerate() {
            match Regex::new(pattern) {
                Ok(regex) => rules.push(regex),
                Err(err) => log::warn!(
                    "secret_masking.patterns[{}] is not a valid regex and is ignored: {err:#}",
                    idx + 1
                ),
            }
        }
        Self { rules, pem_block }
    }

    fn collect(&self, text: &str, multiline: bool) -> Vec<Range<usize>> {
        let mut found = vec![];
        let pem = if multiline {
            self.pem_block.as_ref()
        } else {
            None
        };
        for regex in self.rules.iter().chain(pem) {
            for caps in regex.captures_iter(text) {
                let Some(m) = caps.get(1).or_else(|| caps.get(0)) else {
                    continue;
                };
                if !m.is_empty() {
                    found.push(m.range());
                }
            }
        }
        merge_ranges(found)
    }

    /// Byte ranges of the secrets in one line of text: sorted, non-overlapping.
    pub(crate) fn find(&self, text: &str) -> Vec<Range<usize>> {
        self.collect(text, false)
    }

    /// `text` with every secret replaced by [`REDACTED`], and how many were.
    /// `text` may span lines.
    pub(crate) fn redact(&self, text: &str) -> (String, usize) {
        let ranges = self.collect(text, true);
        if ranges.is_empty() {
            return (text.to_string(), 0);
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for range in &ranges {
            out.push_str(&text[last..range.start]);
            out.push_str(REDACTED);
            last = range.end;
        }
        out.push_str(&text[last..]);
        (out, ranges.len())
    }
}

fn merge_ranges(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// Suffix for a copy toast, empty when nothing was redacted.
pub(crate) fn redaction_note(count: usize) -> String {
    match count {
        0 => String::new(),
        1 => " · 1 secret redacted".to_string(),
        n => format!(" · {n} secrets redacted"),
    }
}

/// Suffix for the toast of a "(no secrets)" copy. Unlike [`redaction_note`]
/// it also speaks up when nothing matched: the user asked for a redaction,
/// and silence would not say whether it ran.
pub(crate) fn forced_redaction_note(count: usize) -> String {
    match count {
        0 => " · no secrets found".to_string(),
        count => redaction_note(count),
    }
}

/// Identifies one secret on screen: the first row of its logical line and its
/// starting column within that logical line. Stable across the rows a wrapped
/// secret occupies, so hovering any of them reveals all of them.
pub(crate) type SecretId = (StableRowIndex, usize);

/// The part of one secret that lies on one physical row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MaskSpan {
    pub cols: Range<usize>,
    pub id: SecretId,
}

/// The text of `line` plus, for each cell, `(byte offset, column, width)`.
fn line_text(line: &Line) -> (String, Vec<(usize, usize, usize)>) {
    let mut text = String::new();
    let mut cells = vec![];
    for cell in line.visible_cells() {
        cells.push((text.len(), cell.cell_index(), cell.width().max(1)));
        text.push_str(cell.str());
    }
    (text, cells)
}

/// The columns covered by `bytes`, given the cell table from [`line_text`].
fn byte_range_to_cols(cells: &[(usize, usize, usize)], bytes: &Range<usize>) -> Range<usize> {
    let first = cells
        .partition_point(|(byte, _, _)| *byte <= bytes.start)
        .saturating_sub(1);
    let last = cells
        .partition_point(|(byte, _, _)| *byte < bytes.end)
        .saturating_sub(1);
    match (cells.get(first), cells.get(last)) {
        (Some((_, start, _)), Some((_, col, width))) => *start..col + width,
        _ => 0..0,
    }
}

/// Split a column range of a logical line across its physical rows.
fn split_across_rows(
    first_row: StableRowIndex,
    row_lens: &[usize],
    cols: &Range<usize>,
) -> Vec<(StableRowIndex, Range<usize>)> {
    let mut out = vec![];
    let mut offset = 0;
    for (idx, len) in row_lens.iter().enumerate() {
        let start = cols.start.max(offset);
        let end = cols.end.min(offset + len);
        if start < end {
            out.push((
                first_row + idx as StableRowIndex,
                start - offset..end - offset,
            ));
        }
        offset += len;
    }
    out
}

/// Where to mask in `lines`, keyed by physical row.
///
/// Works on logical lines so a token wrapped across rows is matched once.
/// `lines` must be in row order: a private key is recognised by its `BEGIN`
/// line and everything up to the `END` line is hidden.
pub(crate) fn scan_logical_lines(
    lines: &[LogicalLine],
    rules: &SecretRules,
) -> HashMap<StableRowIndex, Vec<MaskSpan>> {
    let mut rows: HashMap<StableRowIndex, Vec<MaskSpan>> = HashMap::new();
    let mut in_pem = false;
    for logical in lines {
        let (text, cells) = line_text(&logical.logical);
        let mut cols: Vec<Range<usize>> = vec![];

        if rules.pem_block.is_some() {
            if is_pem_end(&text) {
                in_pem = false;
            } else if is_pem_begin(&text) {
                in_pem = true;
            } else if in_pem {
                let body = text.trim();
                if !body.is_empty() {
                    let start = text.len() - text.trim_start().len();
                    cols.push(byte_range_to_cols(&cells, &(start..start + body.len())));
                }
            }
        }

        for bytes in rules.find(&text) {
            cols.push(byte_range_to_cols(&cells, &bytes));
        }
        if cols.is_empty() {
            continue;
        }

        let row_lens: Vec<usize> = logical.physical_lines.iter().map(|l| l.len()).collect();
        for cols in merge_ranges(cols) {
            let id = (logical.first_row, cols.start);
            for (row, cols) in split_across_rows(logical.first_row, &row_lens, &cols) {
                rows.entry(row).or_default().push(MaskSpan { cols, id });
            }
        }
    }
    rows
}

/// A copy of `line` with the cells in `spans` painted as bullets. Attributes
/// are kept so the mask sits in the colours the secret had.
pub(crate) fn mask_line<'a>(line: &Line, spans: impl Iterator<Item = &'a Range<usize>>) -> Line {
    let mut masked = line.clone();
    let len = masked.len();
    for cols in spans {
        for idx in cols.start..cols.end.min(len) {
            let attrs = masked
                .get_cell(idx)
                .map(|cell| cell.attrs().clone())
                .unwrap_or_default();
            masked.set_cell(idx, Cell::new(MASK_CHAR, attrs), SEQ_ZERO);
        }
    }
    // The clone inherited the original's cached shape hash; it no longer
    // describes these cells.
    masked.clear_appdata();
    masked
}

/// The scan of one pane, valid while nothing it was computed from changed.
struct PaneMasks {
    seqno: SequenceNo,
    range: Range<StableRowIndex>,
    generation: usize,
    rows: HashMap<StableRowIndex, Vec<MaskSpan>>,
}

/// Per-window masking state.
#[derive(Default)]
pub struct SecretMaskState {
    /// `ToggleSecretMasking` for this window; `None` follows the config.
    enabled_override: Option<bool>,
    rules: Option<(usize, Arc<SecretRules>)>,
    panes: HashMap<PaneId, PaneMasks>,
    /// The secret under the pointer, shown unmasked.
    reveal: Option<(PaneId, SecretId)>,
}

impl TermWindow {
    fn secret_masking_enabled(&self) -> bool {
        self.secret_mask
            .enabled_override
            .unwrap_or(self.config.secret_masking.enabled)
    }

    fn secret_masking_on_screen(&self) -> bool {
        self.secret_masking_enabled() && self.config.secret_masking.mask_on_screen
    }

    fn secret_rules(&mut self) -> Arc<SecretRules> {
        let generation = self.config.generation();
        match &self.secret_mask.rules {
            Some((cached, rules)) if *cached == generation => Arc::clone(rules),
            _ => {
                let rules = Arc::new(SecretRules::compile(&self.config.secret_masking));
                self.secret_mask.rules = Some((generation, Arc::clone(&rules)));
                rules
            }
        }
    }

    pub(crate) fn toggle_secret_masking(&mut self) {
        let enabled = !self.secret_masking_enabled();
        self.secret_mask.enabled_override = Some(enabled);
        self.secret_mask.panes.clear();
        self.secret_mask.reveal = None;
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Bring the mask table for `pane` up to date for the rows about to be
    /// painted. A no-op while the pane's content and viewport are unchanged.
    pub(crate) fn refresh_secret_masks(
        &mut self,
        pane: &Arc<dyn Pane>,
        visible: Range<StableRowIndex>,
    ) {
        let pane_id = pane.pane_id();
        if !self.secret_masking_on_screen() {
            self.secret_mask.panes.remove(&pane_id);
            return;
        }
        let seqno = pane.get_current_seqno();
        let generation = self.config.generation();
        if let Some(cached) = self.secret_mask.panes.get(&pane_id) {
            if cached.seqno == seqno && cached.range == visible && cached.generation == generation {
                return;
            }
        }
        let rules = self.secret_rules();
        let scan_range = visible.start.saturating_sub(SCAN_MARGIN_ROWS).max(0)..visible.end;
        let rows = scan_logical_lines(&pane.get_logical_lines(scan_range), &rules);
        if self.secret_mask.panes.len() >= PANE_CACHE_CAP
            && !self.secret_mask.panes.contains_key(&pane_id)
        {
            self.secret_mask.panes.clear();
        }
        self.secret_mask.panes.insert(
            pane_id,
            PaneMasks {
                seqno,
                range: visible,
                generation,
                rows,
            },
        );
    }

    /// The row to paint in place of `line`, if any of it is masked.
    pub(crate) fn secret_masked_line(
        &self,
        pane_id: PaneId,
        row: StableRowIndex,
        line: &Line,
    ) -> Option<Line> {
        let spans = self.secret_mask.panes.get(&pane_id)?.rows.get(&row)?;
        let revealed = match self.secret_mask.reveal {
            Some((pane, id)) if pane == pane_id => Some(id),
            _ => None,
        };
        let mut hidden = spans
            .iter()
            .filter(|span| Some(span.id) != revealed)
            .map(|span| &span.cols)
            .peekable();
        hidden.peek()?;
        Some(mask_line(line, hidden))
    }

    /// The pointer left the window: nothing is hovered any more.
    pub(crate) fn clear_secret_reveal(&mut self) {
        self.secret_mask.reveal = None;
    }

    /// Track the secret under the pointer. Returns true when it changed and
    /// the pane needs repainting.
    pub(crate) fn update_secret_reveal(
        &mut self,
        pane_id: PaneId,
        row: StableRowIndex,
        column: usize,
    ) -> bool {
        let reveal =
            if self.secret_masking_on_screen() && self.config.secret_masking.reveal_on_hover {
                self.secret_mask
                    .panes
                    .get(&pane_id)
                    .and_then(|masks| masks.rows.get(&row))
                    .and_then(|spans| spans.iter().find(|span| span.cols.contains(&column)))
                    .map(|span| (pane_id, span.id))
            } else {
                None
            };
        if reveal == self.secret_mask.reveal {
            return false;
        }
        self.secret_mask.reveal = reveal;
        true
    }

    /// Whether the pane Copy actions redact on their own, which makes the
    /// copy menu's "(no secrets)" rows redundant.
    pub(crate) fn copy_actions_redact(&self) -> bool {
        self.secret_masking_enabled() && self.config.secret_masking.redact_copy_actions
    }

    /// Redact `text` regardless of `secret_masking.enabled`: the caller was
    /// asked for a copy without secrets.
    pub(crate) fn redact_always(&mut self, text: &str) -> (String, usize) {
        self.secret_rules().redact(text)
    }

    /// Redact `text` for a pane Copy action when masking covers those.
    /// Returns the text to copy and how many secrets were removed.
    pub(crate) fn redact_for_copy_action(&mut self, text: String) -> (String, usize) {
        if !self.copy_actions_redact() {
            return (text, 0);
        }
        self.secret_rules().redact(&text)
    }

    /// `CopyRedactedTo`: an explicit request, so it works whether or not
    /// masking is switched on.
    pub(crate) fn copy_selection_redacted(
        &mut self,
        pane: &Arc<dyn Pane>,
        destination: ClipboardCopyDestination,
    ) {
        let text = self.selection_text(pane);
        if text.is_empty() {
            return;
        }
        let (text, count) = self.secret_rules().redact(&text);
        self.copy_to_clipboard(destination, text);
        if count > 0 {
            wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                title: "Copy".to_string(),
                message: format!("Copied selection{}", redaction_note(count)),
                url: None,
                timeout: Some(std::time::Duration::from_millis(1800)),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termwiz::cell::CellAttributes;

    fn rules() -> SecretRules {
        SecretRules::compile(&SecretMaskingConfig::default())
    }

    fn rules_with(patterns: &[&str], builtin: bool) -> SecretRules {
        SecretRules::compile(&SecretMaskingConfig {
            builtin_patterns: builtin,
            patterns: patterns.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        })
    }

    fn found<'a>(rules: &SecretRules, text: &'a str) -> Vec<&'a str> {
        rules.find(text).into_iter().map(|r| &text[r]).collect()
    }

    fn plain(text: &str) -> Line {
        Line::from_text(text, &CellAttributes::default(), SEQ_ZERO, None)
    }

    /// One logical line made of `rows`, starting at `first_row`.
    fn logical(first_row: StableRowIndex, rows: &[&str]) -> LogicalLine {
        LogicalLine {
            physical_lines: rows.iter().map(|row| plain(row)).collect(),
            logical: plain(&rows.concat()),
            first_row,
        }
    }

    const GITHUB: &str = "ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ";

    #[test]
    fn builtin_rules_find_known_token_shapes() {
        let rules = rules();
        for token in [
            "AKIAIOSFODNN7EXAMPLE",
            GITHUB,
            "github_pat_11ABCDEFG0abcdefghijkl_mnopqrstuvwxyz",
            "glpat-abcdefghij0123456789",
            "xoxb-123456789012-abcdefghijkl",
            "sk_live_abcdefghijklmnop1234",
            "sk-proj-abcdefghijklmnopqrstuvwx",
            "AIzaSyA-abcdefghijklmnopqrstuvwxyz01234",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N",
        ] {
            let text = format!("export KEY {token} done");
            assert_eq!(found(&rules, &text), vec![token], "{token}");
        }
    }

    #[test]
    fn capture_group_keeps_the_context_visible() {
        let rules = rules();
        assert_eq!(
            found(&rules, "Authorization: Bearer abcdefghijklmnop1234"),
            vec!["abcdefghijklmnop1234"]
        );
        assert_eq!(
            found(
                &rules,
                "git clone https://tim:hunter2pw@example.com/repo.git"
            ),
            vec!["hunter2pw"]
        );
        assert_eq!(
            found(&rules, "DB_PASSWORD=correcthorse"),
            vec!["correcthorse"]
        );
        assert_eq!(
            found(&rules, r#"  api_key: "abcd1234efgh""#),
            vec!["abcd1234efgh"]
        );
        assert_eq!(
            found(&rules, "curl --token=abcdef123456 host"),
            vec!["abcdef123456"]
        );
    }

    #[test]
    fn every_builtin_rule_compiles() {
        for pattern in builtin_patterns() {
            assert!(Regex::new(&pattern).is_ok(), "{pattern}");
        }
    }

    #[test]
    fn a_value_named_like_a_password_is_hidden_at_any_length() {
        let rules = rules();
        for (text, secret) in [
            ("password=abc", "abc"),
            ("PASSWD=x", "x"),
            ("db.password: pw1", "pw1"),
            (r#"passphrase = "a b""#, ""),
            (r#"client_secret: "s3""#, "s3"),
            ("mysql --password hunter2 -h db", "hunter2"),
            ("login -password pw", "pw"),
            // German
            ("Passwort: geheim", "geheim"),
            // The command that printed it: the value runs up to a quote.
            (r#"echo "Passwort: geheim""#, "geheim"),
            ("echo 'password: abc'", "abc"),
            (r#"anmelden --passwort "abc""#, "abc"),
            ("KENNWORT=abc", "abc"),
            (r#"db_passwort = "x1""#, "x1"),
            ("anmelden --passwort abc", "abc"),
            ("Geheimnis: 42", "42"),
            ("Zugangscode: 1234", "1234"),
            ("Geheimschlüssel: k9", "k9"),
            ("GEHEIMSCHLÜSSEL=k9", "k9"),
        ] {
            let expected: Vec<&str> = if secret.is_empty() {
                vec![]
            } else {
                vec![secret]
            };
            assert_eq!(found(&rules, text), expected, "{text}");
        }
    }

    #[test]
    fn a_value_named_like_a_key_needs_some_length() {
        let rules = rules();
        // Labels that merely contain "token" or "key" are everyday output.
        for text in [
            "tokens: 5000",
            "token=abc",
            "total tokens: 12k",
            "Schlüssel: A1",
            "credentials: none",
        ] {
            assert!(found(&rules, text).is_empty(), "{text}");
        }
        for (text, secret) in [
            ("API-Schlüssel: abcd1234efgh", "abcd1234efgh"),
            ("api_schluessel=abcd1234efgh", "abcd1234efgh"),
            ("Zugangsdaten: benutzer:geheim99", "benutzer:geheim99"),
            ("private_key: 0123456789abcdef", "0123456789abcdef"),
            ("deploy --token abcdef123456 now", "abcdef123456"),
        ] {
            assert_eq!(found(&rules, text), vec![secret], "{text}");
        }
    }

    #[test]
    fn ordinary_output_is_left_alone() {
        let rules = rules();
        for text in [
            "commit 484fc9b32a1be4b2b6ebc4cee38fc45c38165e05",
            "id 123e4567-e89b-12d3-a456-426614174000",
            "/Users/tim/Documents/tgzterminal/wezterm-gui/src/main.rs",
            "https://example.com/path?query=1",
            "ssh tim@example.com",
            "let token = fetch_token(&client);",
            "password: ",
            "Passwort:",
            "skipped 12 tests in 0.42s",
            "connect(user=user, password=password)",
            "let passwort = eingabe.trim();",
            "cd $PWD && echo $OLDPWD",
            "PWD=/Users/tim/Documents",
            "the password is not stored anywhere",
            "ls --color auto --all",
        ] {
            assert!(found(&rules, text).is_empty(), "{text}");
        }
    }

    #[test]
    fn user_patterns_add_to_or_replace_the_builtins() {
        let both = rules_with(&[r"\bTKT-\d{6}\b"], true);
        assert_eq!(
            found(&both, &format!("TKT-123456 {GITHUB}")),
            vec!["TKT-123456", GITHUB]
        );

        let only = rules_with(&[r"corp_id=(\w+)"], false);
        assert_eq!(found(&only, &format!("corp_id=abc {GITHUB}")), vec!["abc"]);
    }

    #[test]
    fn an_invalid_user_pattern_does_not_disable_the_rest() {
        let rules = rules_with(&["(unclosed"], true);
        assert_eq!(found(&rules, GITHUB), vec![GITHUB]);
    }

    #[test]
    fn overlapping_matches_merge_into_one() {
        // The assignment rule and the GitHub rule both hit the same token.
        let rules = rules();
        let text = format!("GITHUB_TOKEN={GITHUB}");
        assert_eq!(found(&rules, &text), vec![GITHUB]);
        assert_eq!(merge_ranges(vec![5..9, 0..3, 2..6]), vec![0..9]);
        assert_eq!(merge_ranges(vec![0..2, 4..6]), vec![0..2, 4..6]);
    }

    #[test]
    fn redact_replaces_and_counts() {
        let rules = rules();
        let (text, count) = rules.redact(&format!("a {GITHUB}\nPASSWORD=correcthorse\nb"));
        assert_eq!(text, "a [REDACTED]\nPASSWORD=[REDACTED]\nb");
        assert_eq!(count, 2);

        let (text, count) = rules.redact("nothing here");
        assert_eq!(text, "nothing here");
        assert_eq!(count, 0);
    }

    #[test]
    fn redact_hides_a_private_key_body() {
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\nAAAAB3NzaC1\n-----END OPENSSH PRIVATE KEY-----";
        let (text, count) = rules().redact(&format!("before\n{key}\nafter"));
        assert_eq!(
            text,
            "before\n-----BEGIN OPENSSH PRIVATE KEY-----[REDACTED]-----END OPENSSH PRIVATE KEY-----\nafter"
        );
        assert_eq!(count, 1);
    }

    #[test]
    fn redaction_note_wording() {
        assert_eq!(redaction_note(0), "");
        assert_eq!(redaction_note(1), " · 1 secret redacted");
        assert_eq!(redaction_note(3), " · 3 secrets redacted");
        assert_eq!(forced_redaction_note(0), " · no secrets found");
        assert_eq!(forced_redaction_note(2), " · 2 secrets redacted");
    }

    #[test]
    fn scan_maps_a_match_to_its_columns() {
        let lines = [logical(10, &[&format!("key {GITHUB} end")])];
        let rows = scan_logical_lines(&lines, &rules());
        assert_eq!(
            rows.get(&10),
            Some(&vec![MaskSpan {
                cols: 4..4 + GITHUB.len(),
                id: (10, 4),
            }])
        );
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn scan_splits_a_wrapped_secret_across_its_rows() {
        // 10 columns wide: the token starts at column 4 of the first row and
        // runs through the next four rows.
        let text = format!("key {GITHUB}");
        let chars: Vec<char> = text.chars().collect();
        let rows_text: Vec<String> = chars.chunks(10).map(|c| c.iter().collect()).collect();
        let row_refs: Vec<&str> = rows_text.iter().map(|s| s.as_str()).collect();
        let lines = [logical(0, &row_refs)];

        let rows = scan_logical_lines(&lines, &rules());
        assert_eq!(rows.len(), row_refs.len());
        assert_eq!(rows[&0][0].cols, 4..10);
        assert_eq!(rows[&1][0].cols, 0..10);
        assert_eq!(rows[&4][0].cols, 0..4);
        // Every row carries the same id, so hovering one reveals all.
        assert!(rows.values().all(|spans| spans[0].id == (0, 4)));
    }

    #[test]
    fn scan_accounts_for_wide_characters() {
        // Each CJK character is two columns wide.
        let lines = [logical(0, &["日本 PASSWORD=correcthorse"])];
        let rows = scan_logical_lines(&lines, &rules());
        assert_eq!(rows[&0][0].cols, 14..26);
    }

    #[test]
    fn scan_masks_private_key_body_lines() {
        let lines = [
            logical(0, &["-----BEGIN RSA PRIVATE KEY-----"]),
            logical(1, &["MIIEowIBAAKCAQEA"]),
            logical(2, &["  q8Zx"]),
            logical(3, &["-----END RSA PRIVATE KEY-----"]),
            logical(4, &["plain text"]),
        ];
        let rows = scan_logical_lines(&lines, &rules());
        assert_eq!(rows[&1][0].cols, 0..16);
        assert_eq!(rows[&2][0].cols, 2..6);
        assert!(!rows.contains_key(&0));
        assert!(!rows.contains_key(&3));
        assert!(!rows.contains_key(&4));
    }

    #[test]
    fn mask_line_replaces_only_the_given_columns() {
        let line = plain("key secret end");
        let masked = mask_line(&line, [4..10].iter());
        assert_eq!(masked.as_str(), "key •••••• end");
        // The original is untouched.
        assert_eq!(line.as_str(), "key secret end");
    }

    #[test]
    fn mask_line_ignores_columns_past_the_end() {
        let line = plain("abc");
        let masked = mask_line(&line, [1..40].iter());
        assert_eq!(masked.as_str(), "a••");
    }
}
