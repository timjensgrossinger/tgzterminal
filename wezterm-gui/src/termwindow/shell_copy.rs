//! Extracting a command, its output, or the whole scrollback from a plain
//! shell / ssh pane.
//!
//! This is the non-agent sibling of the agent "Copy conversation" path in
//! `render/sidebar.rs`: it produces the payload and the toast wording for a
//! Copy button, and touches no clipboard and no UI state itself.
//!
//! # Why this classifies rows itself instead of asking for semantic zones
//!
//! [`mux::pane::Pane`] exposes `get_semantic_zones`, and not using it is
//! deliberate:
//!
//! * `TerminalState::get_semantic_zones` merges consecutive runs of the same
//!   semantic type across rows with no contiguity requirement, which destroys
//!   the per-row, per-column `Input` runs this module needs in order to drop a
//!   zsh RPROMPT from the copied command line.
//! * Reading the semantic bits straight off the [`Line`]s makes ssh-domain
//!   (`ClientPane`) panes work for free: the bits live in
//!   `CellAttributes::attributes` and already ride the mux wire.
//! * `Line::semantic_zone_ranges` needs `&mut`, memoizes into a `Line` that is
//!   about to be dropped, and mis-reports blank rows.
//!
//! Everything below the one impure method at the bottom is a free function
//! over plain data, so the interesting cases are unit-testable with no
//! `TermWindow`, no `Pane` and no pty.

use crate::termwindow::render::sidebar::{
    agent_transcript_chunks, agent_transcript_start, AGENT_TRANSCRIPT_CHUNK_ROWS,
    AGENT_TRANSCRIPT_CLIPPED_MARKER, AGENT_TRANSCRIPT_MAX_ROWS,
};
use mux::pane::Pane;
use std::ops::Range;
use std::sync::Arc;
use termwiz::cell::SemanticType;
use termwiz::surface::Line;
use wezterm_term::StableRowIndex;

/// What one physical row is, from the per-cell OSC 133 bits on it.
///
/// Precedence: any `Prompt` cell wins; else any `Input` cell; else any
/// non-blank cell; else `Blank`. Blank cells never make a row `Output`, so a
/// run of untouched rows below a command reads as `Blank` and can be trimmed
/// off the edges of a copy without losing an interior blank line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    Prompt,
    Input,
    Output,
    Blank,
}

/// One prompt-to-prompt region of the pane, in `StableRowIndex` space. All
/// ranges are half-open: the command occupies `prompt_start..input_end` and
/// its output `input_end..output_end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CommandBlock {
    prompt_start: StableRowIndex,
    /// End of the run of `Prompt`/`Input` rows.
    input_end: StableRowIndex,
    /// `== input_end` when the command printed nothing.
    output_end: StableRowIndex,
    /// The scan window began inside this block, so its head is missing.
    head_clipped: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LastCommandPick {
    block: CommandBlock,
    /// A newer block exists below the picked one.
    pending_below: bool,
}

/// The rows a copy action read, plus what the read could not deliver.
pub(crate) struct PaneScan {
    rows: Vec<Line>,
    kinds: Vec<RowKind>,
    first_row: StableRowIndex,
    /// Older rows are still in the pane but fell outside the copy window.
    clipped: bool,
    end_row: StableRowIndex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellCopyAction {
    LastCommandOutput,
    LastCommandWithOutput,
    WholePane,
}

/// Which rung of the ladder produced the payload. Reported in the toast so a
/// guessed boundary is never presented as a known one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellCopySource {
    /// OSC 133 marks from shell integration.
    SemanticMarks,
    /// No usable marks; the boundary came from a prompt-shaped row.
    GuessedPromptLine,
    WholePane,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellCopyPayload {
    pub text: String,
    pub source: ShellCopySource,
    /// Older rows exist in the pane but fell outside the copy window.
    pub clipped: bool,
    /// The copied command started above the copy window.
    pub head_clipped: bool,
    /// A newer command is still running (or was typed and not run), so the
    /// copied one is the command before it.
    pub pending_below: bool,
}

impl Default for ShellCopySource {
    fn default() -> Self {
        Self::SemanticMarks
    }
}

/// Sigils a prompt may end in. `'>'` is deliberately absent: it collides with
/// markdown blockquotes, git and mail quoting, bash's PS2, printed shell
/// redirections and a Python REPL's `>>>`. A false positive here silently
/// copies the wrong thing, and no macOS shell ships `'>'` as its PS1.
const SHELL_PROMPT_SIGILS: &[char] = &['$', '#', '%', '❯', '➜', '»'];

/// A prompt shape learned from one unambiguous bare prompt row, used to lock
/// every later match to the same prompt family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PromptShape {
    row: StableRowIndex,
    sigil: char,
}

// ---------------------------------------------------------------------------
// Row classification
// ---------------------------------------------------------------------------

fn classify_row(line: &Line) -> RowKind {
    let mut saw_input = false;
    let mut saw_text = false;
    for cell in line.visible_cells() {
        match cell.attrs().semantic_type() {
            SemanticType::Prompt => return RowKind::Prompt,
            SemanticType::Input => saw_input = true,
            SemanticType::Output => {}
        }
        if !cell.str().trim().is_empty() {
            saw_text = true;
        }
    }
    if saw_input {
        RowKind::Input
    } else if saw_text {
        RowKind::Output
    } else {
        RowKind::Blank
    }
}

/// Columns of the *first* maximal `Input` run on the row.
///
/// First, deliberately: `wezterm.sh` leaves the pen on `Input` from the end of
/// PS1 until preexec, so a zsh RPROMPT printed after PS1 is *also* `Input`
/// penned, past a blank gap on the same row. Taking the first run yields the
/// typed command and drops the RPROMPT.
fn first_input_run(line: &Line) -> Option<Range<usize>> {
    let mut start: Option<usize> = None;
    let mut end = 0usize;
    for cell in line.visible_cells() {
        if cell.attrs().semantic_type() == SemanticType::Input {
            if start.is_none() {
                start = Some(cell.cell_index());
            }
            end = cell.cell_index() + cell.width().max(1);
        } else if start.is_some() {
            break;
        }
    }
    start.map(|start| start..end)
}

/// Text of the first maximal `Prompt` run, trailing blanks removed. Used only
/// to compare one prompt row against another, never to copy.
fn prompt_run_text(line: &Line) -> String {
    let mut start: Option<usize> = None;
    let mut end = 0usize;
    for cell in line.visible_cells() {
        if cell.attrs().semantic_type() == SemanticType::Prompt {
            if start.is_none() {
                start = Some(cell.cell_index());
            }
            end = cell.cell_index() + cell.width().max(1);
        } else if start.is_some() {
            break;
        }
    }
    match start {
        Some(start) => line.columns_as_str(start..end).trim_end().to_string(),
        None => String::new(),
    }
}

fn row_text(line: &Line) -> String {
    line.columns_as_str(0..line.len())
}

// ---------------------------------------------------------------------------
// Block structure
// ---------------------------------------------------------------------------

/// Split `kinds` into prompt-to-prompt blocks.
///
/// Consecutive `Prompt`/`Input` rows are **coalesced** into one command rather
/// than treated as a boundary each, because `wezterm.sh` marks PS2 exactly as
/// it marks PS1 and the performer discards the `k=` kind: a three line `for`
/// loop and three zero-output commands produce byte-identical attributes.
/// Coalescing fails safe — it can only add a row to the *command* text, never
/// drop output or attribute it to the wrong command.
/// [`split_repeated_prompt_rows`] splits the cases it can tell apart.
fn command_blocks(
    kinds: &[RowKind],
    first_row: StableRowIndex,
    scan_is_clipped: bool,
) -> Vec<CommandBlock> {
    let mut blocks = Vec::new();
    if kinds.is_empty() {
        return blocks;
    }

    let is_after_input = |kind: RowKind| matches!(kind, RowKind::Output | RowKind::Blank);
    let is_command = |kind: RowKind| matches!(kind, RowKind::Prompt | RowKind::Input);

    let mut preamble_end = 0usize;
    while preamble_end < kinds.len() && is_after_input(kinds[preamble_end]) {
        preamble_end += 1;
    }

    if preamble_end == kinds.len() {
        // Not one marked row anywhere: this pane has no shell integration, so
        // the caller falls back to the prompt-shape heuristic.
        return blocks;
    }

    if preamble_end > 0 {
        // Output ahead of the first prompt. It is either the first command of
        // the session (a login banner, `head_clipped` false) or the tail of a
        // command whose head fell outside the scan window (`head_clipped`
        // true). Both are worth copying; only the second is a partial copy.
        blocks.push(CommandBlock {
            prompt_start: first_row,
            input_end: first_row,
            output_end: first_row + preamble_end as StableRowIndex,
            head_clipped: scan_is_clipped,
        });
    }

    let mut i = preamble_end;
    while i < kinds.len() {
        let start = i;
        let mut j = i;
        while j < kinds.len() && is_command(kinds[j]) {
            j += 1;
        }
        let mut k = j;
        while k < kinds.len() && is_after_input(kinds[k]) {
            k += 1;
        }
        blocks.push(CommandBlock {
            prompt_start: first_row + start as StableRowIndex,
            input_end: first_row + j as StableRowIndex,
            output_end: first_row + k as StableRowIndex,
            head_clipped: false,
        });
        if k == start {
            // Loop guard: cannot happen while `kinds[start]` is a command row,
            // but never spin if that invariant is ever broken.
            break;
        }
        i = k;
    }

    blocks
}

/// The newest block that actually produced output.
///
/// This deliberately skips an idle prompt at the bottom, a command typed but
/// not yet run, and a command running with nothing printed yet — none of those
/// have output to copy. A command running *with* partial output is chosen:
/// clicking Copy in the middle of a `cargo build` should hand over the bytes
/// so far.
fn pick_last_command(blocks: &[CommandBlock]) -> Option<LastCommandPick> {
    let idx = blocks
        .iter()
        .rposition(|block| block.output_end > block.input_end)?;
    Some(LastCommandPick {
        block: blocks[idx],
        pending_below: idx + 1 < blocks.len(),
    })
}

/// Split a coalesced command run wherever a row repeats the run's own first
/// prompt string.
///
/// A second PS1 renders identically to the first; a PS2 essentially never
/// matches its PS1. The failure mode of a cwd-embedding PS1 is that `cd /tmp`
/// gets merged into the next command's text — never that output is lost or
/// attributed to the wrong command.
fn split_repeated_prompt_rows(
    rows: &[Line],
    first_row: StableRowIndex,
    block: CommandBlock,
) -> Vec<CommandBlock> {
    let start_idx = index_of(block.prompt_start, first_row, rows.len());
    let input_end_idx = index_of(block.input_end, first_row, rows.len());
    if input_end_idx <= start_idx + 1 {
        return vec![block];
    }

    let first_text = prompt_run_text(&rows[start_idx]);
    if first_text.trim().is_empty() {
        return vec![block];
    }

    let mut starts = vec![start_idx];
    for idx in (start_idx + 1)..input_end_idx {
        if classify_row(&rows[idx]) == RowKind::Prompt && prompt_run_text(&rows[idx]) == first_text
        {
            starts.push(idx);
        }
    }
    if starts.len() == 1 {
        return vec![block];
    }

    let mut out = Vec::with_capacity(starts.len());
    for (n, &start) in starts.iter().enumerate() {
        let is_last = n + 1 == starts.len();
        let next = starts.get(n + 1).copied().unwrap_or(input_end_idx);
        let next_row = first_row + next as StableRowIndex;
        out.push(CommandBlock {
            prompt_start: first_row + start as StableRowIndex,
            input_end: next_row,
            // Only the final segment owns the run's output; every earlier one
            // is a command that printed nothing.
            output_end: if is_last { block.output_end } else { next_row },
            head_clipped: n == 0 && block.head_clipped,
        });
    }
    out
}

/// Does any block below the picked one hold text the user actually typed?
///
/// [`LastCommandPick::pending_below`] is true for *any* later block, and the
/// overwhelmingly common later block is the idle prompt sitting at the bottom
/// of the pane. Reporting that as "a newer command is still running" would be
/// a lie in almost every copy, so the claim is narrowed here, where the row
/// text is available: a later block counts only if something was typed into
/// it.
fn typed_input_below(
    blocks: &[CommandBlock],
    picked: &CommandBlock,
    rows: &[Line],
    first_row: StableRowIndex,
) -> bool {
    let Some(pos) = blocks
        .iter()
        .position(|block| block.prompt_start == picked.prompt_start)
    else {
        return false;
    };
    blocks[pos + 1..].iter().any(|block| {
        let start = index_of(block.prompt_start, first_row, rows.len());
        let end = index_of(block.input_end, first_row, rows.len());
        (start..end).any(|idx| match first_input_run(&rows[idx]) {
            Some(run) => !rows[idx].columns_as_str(run).trim().is_empty(),
            None => false,
        })
    })
}

// ---------------------------------------------------------------------------
// Text assembly
// ---------------------------------------------------------------------------

/// Concatenate `rows[i]` restricted to `spans[i]`.
///
/// A newline is inserted only where the previous row was *not* soft-wrapped,
/// and trailing blanks are pruned only at the end of a wrapped run — the same
/// contract as `TermWindow::selection_text`, so a copy from this module and a
/// mouse selection of the same region produce the same string.
fn join_rows(rows: &[Line], spans: &[Range<usize>]) -> String {
    let mut s = String::new();
    let mut last_was_wrapped = false;
    for (line, span) in rows.iter().zip(spans.iter()) {
        if !s.is_empty() && !last_was_wrapped {
            s.push('\n');
        }
        let last_phys_idx = line.len().saturating_sub(1);
        let last_col_idx = span.end.saturating_sub(1).min(last_phys_idx);
        let wrapped = last_col_idx == last_phys_idx
            && line
                .get_cell(last_col_idx)
                .map(|cell| cell.attrs().wrapped())
                .unwrap_or(false);
        let text = line.columns_as_str(span.clone());
        if wrapped {
            s.push_str(&text);
        } else {
            s.push_str(text.trim_end());
        }
        last_was_wrapped = wrapped;
    }
    s
}

fn output_text(rows: &[Line]) -> String {
    let spans: Vec<Range<usize>> = rows.iter().map(|line| 0..line.len()).collect();
    join_rows(rows, &spans)
}

fn command_text(rows: &[Line]) -> String {
    let spans: Vec<Range<usize>> = rows
        .iter()
        .map(|line| first_input_run(line).unwrap_or(0..0))
        .collect();
    // A prompt row with no typed input contributes an empty segment; trimming
    // the joined result keeps it from showing up as a trailing blank line.
    join_rows(rows, &spans).trim_end().to_string()
}

fn trim_blank_rows(kinds: &[RowKind], rows: Range<usize>) -> Range<usize> {
    // Clamped both ways: the result indexes a row slice from a mouse handler,
    // so an inverted or out-of-range input must not become a panic.
    let mut start = rows.start.min(kinds.len());
    let mut end = rows.end.min(kinds.len()).max(start);
    while start < end && kinds[start] == RowKind::Blank {
        start += 1;
    }
    while end > start && kinds[end - 1] == RowKind::Blank {
        end -= 1;
    }
    start..end
}

// ---------------------------------------------------------------------------
// Prompt-shape heuristic (no shell integration)
// ---------------------------------------------------------------------------
//
// There is no ANSI to strip here: a `Line` already holds decoded graphemes.

/// High precision: this is what *learns* a prompt family, so one false
/// positive poisons every later match.
///
/// The rule that does most of the work is "the first character must be
/// non-blank": real prompts start at column 0, which rules out indented
/// output, continuation lines, `ls -l` columns, diff bodies and most code
/// listings in one test.
fn is_bare_prompt_row(text: &str) -> bool {
    let text = text.trim_end();
    let Some(first) = text.chars().next() else {
        return false;
    };
    if first.is_whitespace() {
        return false;
    }
    let Some(last) = text.chars().last() else {
        return false;
    };
    if !SHELL_PROMPT_SIGILS.contains(&last) {
        return false;
    }
    // A row of nothing but `#` is a markdown rule, not a root prompt.
    !text.chars().all(|c| c == '#')
}

/// Loose, but locked to the family [`is_bare_prompt_row`] learned.
///
/// The sigil lock is the whole trick: in a `❯`-prompted pane, `100% done` and
/// `50$ per unit` can never match, however prompt-shaped they look.
fn is_command_prompt_row(text: &str, sigil: char) -> bool {
    let text = text.trim_end();
    let Some(first) = text.chars().next() else {
        return false;
    };
    if first.is_whitespace() {
        return false;
    }
    let mut needle = String::with_capacity(sigil.len_utf8() + 1);
    needle.push(sigil);
    needle.push(' ');
    // Last occurrence: a prompt that embeds the sigil in its cwd still ends
    // with the real one.
    let Some(idx) = text.rfind(&needle) else {
        return false;
    };
    let tail = &text[idx + needle.len()..];
    if tail.trim().is_empty() {
        // Nothing typed after the prompt: that is a bare prompt, i.e. a bare
        // Enter press, and there is no command there to copy.
        return false;
    }
    if sigil == '#' {
        // Kills `# Heading` and `## Notes` without killing `root@box:~# ls`.
        let head = &text[..idx];
        if head
            .trim_matches(|c: char| c == '#' || c.is_whitespace())
            .is_empty()
        {
            return false;
        }
    }
    true
}

/// The bottom-most unambiguous bare prompt in `search`: in a live pane that is
/// the prompt the user is sitting at, which is the one boundary that is not a
/// guess about a guess.
fn learn_prompt_shape(
    kinds: &[RowKind],
    rows: &[Line],
    first_row: StableRowIndex,
    search: Range<usize>,
) -> Option<PromptShape> {
    for idx in search.rev() {
        // A row the shell already marked is not something to learn a shape
        // from; the heuristic only runs where the marks are missing.
        if !matches!(kinds.get(idx), Some(RowKind::Output)) {
            continue;
        }
        let text = row_text(rows.get(idx)?);
        let trimmed = text.trim_end();
        if is_bare_prompt_row(trimmed) {
            return Some(PromptShape {
                row: first_row + idx as StableRowIndex,
                sigil: trimmed.chars().last()?,
            });
        }
    }
    None
}

/// The bottom-most command row above `shape.row` that actually printed
/// something. Rows below it are commands that produced no output yet, which is
/// what makes [`ShellCopyPayload::pending_below`] true for a guessed boundary.
fn heuristic_command_row(
    rows: &[Line],
    first_row: StableRowIndex,
    shape: &PromptShape,
    search: Range<usize>,
) -> Option<StableRowIndex> {
    let shape_idx = index_of(shape.row, first_row, rows.len());
    let end = shape_idx.min(search.end);
    let mut next_boundary = end;
    for idx in (search.start..end).rev() {
        let text = row_text(&rows[idx]);
        if is_command_prompt_row(&text, shape.sigil) {
            let has_output =
                (idx + 1..next_boundary).any(|row| !row_text(&rows[row]).trim().is_empty());
            if has_output {
                return Some(first_row + idx as StableRowIndex);
            }
            next_boundary = idx;
        } else if is_bare_prompt_row(&text) {
            next_boundary = idx;
        }
    }
    None
}

/// Where the guessed command's output stops: the next prompt-shaped row below
/// it, or the learned prompt itself.
fn heuristic_output_end(
    rows: &[Line],
    shape: &PromptShape,
    cmd_idx: usize,
    shape_idx: usize,
) -> usize {
    for idx in (cmd_idx + 1)..shape_idx {
        let text = row_text(&rows[idx]);
        if is_command_prompt_row(&text, shape.sigil) || is_bare_prompt_row(&text) {
            return idx;
        }
    }
    shape_idx
}

fn command_after_sigil(text: &str, sigil: char) -> String {
    let text = text.trim_end();
    let mut needle = String::with_capacity(sigil.len_utf8() + 1);
    needle.push(sigil);
    needle.push(' ');
    match text.rfind(&needle) {
        Some(idx) => text[idx + needle.len()..].trim().to_string(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Payload
// ---------------------------------------------------------------------------

/// A resolved command boundary, whatever rung of the ladder found it.
struct Resolved {
    /// Set only for a guessed boundary, where the "command" is the tail of a
    /// prompt row rather than a run of `Input` cells.
    command: Option<String>,
    input: Range<usize>,
    output: Range<usize>,
    source: ShellCopySource,
    head_clipped: bool,
    pending_below: bool,
}

fn index_of(row: StableRowIndex, first_row: StableRowIndex, len: usize) -> usize {
    ((row - first_row).max(0) as usize).min(len)
}

fn resolve_guessed(
    kinds: &[RowKind],
    rows: &[Line],
    first_row: StableRowIndex,
    search: Range<usize>,
) -> Option<Resolved> {
    if search.start >= search.end {
        return None;
    }
    let shape = learn_prompt_shape(kinds, rows, first_row, search.clone())?;
    let cmd_row = heuristic_command_row(rows, first_row, &shape, search)?;
    let cmd_idx = index_of(cmd_row, first_row, rows.len());
    let shape_idx = index_of(shape.row, first_row, rows.len());
    let output_end = heuristic_output_end(rows, &shape, cmd_idx, shape_idx);
    let pending_below = (output_end..shape_idx)
        .any(|idx| is_command_prompt_row(&row_text(&rows[idx]), shape.sigil));
    Some(Resolved {
        command: Some(command_after_sigil(&row_text(&rows[cmd_idx]), shape.sigil)),
        input: cmd_idx..(cmd_idx + 1),
        output: (cmd_idx + 1)..output_end,
        source: ShellCopySource::GuessedPromptLine,
        head_clipped: false,
        pending_below,
    })
}

fn resolve_semantic(
    kinds: &[RowKind],
    rows: &[Line],
    first_row: StableRowIndex,
    blocks: &[CommandBlock],
    pick: LastCommandPick,
) -> Resolved {
    let len = rows.len();
    let input = index_of(pick.block.prompt_start, first_row, len)
        ..index_of(pick.block.input_end, first_row, len);
    let output = index_of(pick.block.input_end, first_row, len)
        ..index_of(pick.block.output_end, first_row, len);
    let pending_below =
        pick.pending_below && typed_input_below(blocks, &pick.block, rows, first_row);

    // Nested-session guard. `ssh host` into a box without shell integration
    // makes the whole remote session one block's "output". A genuine command's
    // output does not *end* at a prompt, so that — and only that — is the
    // signal to re-derive the boundary inside the region.
    //
    // Deliberately not "run the heuristic inside every output region":
    // `git log` with a `$ ` in a commit message, `cat README.md` and `history`
    // all contain prompt-shaped rows mid-output.
    let trimmed = trim_blank_rows(kinds, output.clone());
    let last_non_blank = trimmed
        .clone()
        .rev()
        .find(|&idx| !row_text(&rows[idx]).trim().is_empty());
    if let Some(last) = last_non_blank {
        if is_bare_prompt_row(&row_text(&rows[last])) {
            if let Some(mut guessed) =
                resolve_guessed(kinds, rows, first_row, trimmed.start..(last + 1))
            {
                // The nested boundary was found, so nothing about *this*
                // command's head is missing, whatever the outer block lost.
                guessed.pending_below = pending_below;
                return guessed;
            }
        }
    }

    Resolved {
        command: None,
        input,
        output,
        source: ShellCopySource::SemanticMarks,
        head_clipped: pick.block.head_clipped,
        pending_below,
    }
}

fn finish_payload(
    text: String,
    source: ShellCopySource,
    clipped: bool,
    head_clipped: bool,
    pending_below: bool,
) -> ShellCopyPayload {
    // The marker is prepended to the copied text, so it may only describe that
    // text: the whole pane really is cut off at the top, or the copied
    // command's own first rows fell outside the scan. `clipped` on its own
    // means the scan did not reach the top of the scrollback, which is worth
    // saying in the toast but says nothing about a command block that was
    // found complete.
    let text_head_missing = match source {
        ShellCopySource::WholePane => clipped,
        _ => head_clipped,
    };
    let text = if !text.is_empty() && text_head_missing {
        format!("{AGENT_TRANSCRIPT_CLIPPED_MARKER}\n{text}")
    } else {
        text
    };
    ShellCopyPayload {
        text,
        source,
        clipped,
        head_clipped,
        pending_below,
    }
}

pub(crate) fn shell_copy_payload(action: ShellCopyAction, scan: &PaneScan) -> ShellCopyPayload {
    let len = ((scan.end_row - scan.first_row).max(0) as usize)
        .min(scan.rows.len())
        .min(scan.kinds.len());
    let rows = &scan.rows[..len];
    let kinds = &scan.kinds[..len];

    if matches!(action, ShellCopyAction::WholePane) {
        let range = trim_blank_rows(kinds, 0..len);
        let text = output_text(&rows[range]);
        return finish_payload(text, ShellCopySource::WholePane, scan.clipped, false, false);
    }

    let mut blocks: Vec<CommandBlock> = Vec::new();
    for block in command_blocks(kinds, scan.first_row, scan.clipped) {
        blocks.extend(split_repeated_prompt_rows(rows, scan.first_row, block));
    }

    let resolved = match pick_last_command(&blocks) {
        Some(pick) => Some(resolve_semantic(kinds, rows, scan.first_row, &blocks, pick)),
        None => resolve_guessed(kinds, rows, scan.first_row, 0..len),
    };

    let Some(resolved) = resolved else {
        // Nothing found. The empty payload makes the toast refuse; the caller
        // must not touch the clipboard. `source` is meaningless here.
        return finish_payload(
            String::new(),
            ShellCopySource::SemanticMarks,
            scan.clipped,
            false,
            false,
        );
    };

    let output = trim_blank_rows(kinds, resolved.output.clone());
    let output_text = output_text(&rows[output]);

    let text = match action {
        ShellCopyAction::LastCommandOutput => output_text,
        ShellCopyAction::LastCommandWithOutput => {
            let command = match resolved.command.clone() {
                Some(command) => command,
                None => command_text(&rows[resolved.input.clone()]),
            };
            match (command.is_empty(), output_text.is_empty()) {
                (true, _) => output_text,
                (false, true) => command,
                (false, false) => format!("{command}\n{output_text}"),
            }
        }
        ShellCopyAction::WholePane => unreachable!("handled above"),
    };

    finish_payload(
        text,
        resolved.source,
        scan.clipped,
        resolved.head_clipped,
        resolved.pending_below,
    )
}

/// Pure, so the toast can never claim precision the extraction did not have.
pub(crate) fn shell_copy_toast_message(
    action: ShellCopyAction,
    payload: &ShellCopyPayload,
) -> String {
    if payload.text.trim().is_empty() {
        return match action {
            ShellCopyAction::WholePane => "Nothing to copy from this pane",
            _ => "No command output found in this pane",
        }
        .to_string();
    }

    let mut message = match payload.source {
        ShellCopySource::SemanticMarks => match action {
            ShellCopyAction::LastCommandOutput => "Copied the last command output",
            ShellCopyAction::LastCommandWithOutput => "Copied the last command and its output",
            ShellCopyAction::WholePane => "Copied the pane",
        },
        ShellCopySource::GuessedPromptLine => match action {
            ShellCopyAction::LastCommandOutput => {
                "Copied what looks like the last command output \
                 (no shell integration; the boundary was guessed)"
            }
            ShellCopyAction::LastCommandWithOutput => {
                "Copied what looks like the last command and its output \
                 (no shell integration; the boundary was guessed)"
            }
            ShellCopyAction::WholePane => "Copied the pane",
        },
        ShellCopySource::WholePane => "Copied the pane",
    }
    .to_string();

    if payload.pending_below {
        message.push_str("; the newest command is still running, so this is the one before it");
    }
    if payload.clipped || payload.head_clipped {
        message.push_str(" (older scrollback was not available)");
    }
    message
}

// ---------------------------------------------------------------------------
// The one impure function
// ---------------------------------------------------------------------------

impl super::TermWindow {
    /// Read the pane bottom-up and classify every row as it arrives.
    ///
    /// **This uses `get_lines`, not `get_logical_lines`** — deliberately the
    /// opposite of the agent transcript path. `get_logical_lines` widens the
    /// requested range *upward* to complete a soft-wrapped line, walking above
    /// `physical_top` and above the clamped start of the copy window; that is
    /// a real past bug in this repo (`visible_agent_text` leaked scrollback
    /// into the status scan that way). With `get_lines` there is no widening,
    /// no overlapping chunks and no dedup, and the soft wraps are
    /// reconstructed from the wrapped bit by [`join_rows`].
    fn shell_copy_scan(&self, pane: &Arc<dyn Pane>, action: ShellCopyAction) -> PaneScan {
        let dims = pane.get_dimensions();
        // Bottom of the *buffer*, not of the scrolled viewport: deriving this
        // from the scroll position would make scrolling up silently truncate
        // the copy.
        let end = dims.physical_top + dims.viewport_rows as StableRowIndex;
        let wants_command = !matches!(action, ShellCopyAction::WholePane);

        // Alt screen: vim/less/htop have no prompts, and the scrollback behind
        // them is the *pre-app* session, so without this the heuristic would
        // happily find a `$ ` inside a `less` buffer and copy it as a command.
        // Copying the whole pane is still meaningful there.
        if wants_command && pane.is_alt_screen_active() {
            return PaneScan {
                rows: Vec::new(),
                kinds: Vec::new(),
                first_row: end,
                clipped: false,
                end_row: end,
            };
        }

        let max_rows = self
            .config
            .agent_ui
            .copy_scrollback_lines
            .clamp(1, AGENT_TRANSCRIPT_MAX_ROWS) as StableRowIndex;
        let start = agent_transcript_start(dims.scrollback_top, end, max_rows);

        let mut rows: Vec<Line> = Vec::new();
        let mut kinds: Vec<RowKind> = Vec::new();
        let mut first_row = end;
        // Set when reading stopped because the whole command had been read, as
        // opposed to because the window ran out: in that case the rows above
        // `first_row` are irrelevant and the copy is not a partial one.
        let mut found_whole_command = false;

        // Chunked so one click never clones a multi-thousand-row range while
        // holding the pane's terminal mutex, and bottom-up so the common case
        // (the last command is on screen) stops after one chunk.
        for chunk in agent_transcript_chunks(start, end, AGENT_TRANSCRIPT_CHUNK_ROWS)
            .into_iter()
            .rev()
        {
            let (chunk_first, lines) = pane.get_lines(chunk);
            if lines.is_empty() || chunk_first >= first_row {
                break;
            }
            // Trust the returned first row and length, never the request.
            let take = ((first_row - chunk_first) as usize).min(lines.len());
            if take == 0 || chunk_first + take as StableRowIndex != first_row {
                // A hole between what came back and what we already hold; stop
                // rather than splice a discontiguous buffer together.
                break;
            }

            let mut head: Vec<Line> = lines.into_iter().take(take).collect();
            let mut head_kinds: Vec<RowKind> = head.iter().map(classify_row).collect();
            head.append(&mut rows);
            head_kinds.append(&mut kinds);
            rows = head;
            kinds = head_kinds;
            first_row = chunk_first;

            if wants_command {
                let blocks = command_blocks(&kinds, first_row, first_row > dims.scrollback_top);
                if let Some(pick) = pick_last_command(&blocks) {
                    // Strictly greater: a block starting on the very first row
                    // we hold may well continue above it.
                    if pick.block.prompt_start > first_row {
                        found_whole_command = true;
                        break;
                    }
                }
            }
        }

        let end_row = first_row + rows.len() as StableRowIndex;
        PaneScan {
            rows,
            kinds,
            first_row,
            clipped: !found_whole_command && first_row > dims.scrollback_top,
            end_row,
        }
    }

    pub(crate) fn shell_pane_copy_payload(
        &self,
        pane: &Arc<dyn Pane>,
        action: ShellCopyAction,
    ) -> ShellCopyPayload {
        let scan = self.shell_copy_scan(pane, action);
        shell_copy_payload(action, &scan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termwiz::cell::CellAttributes;
    use termwiz::surface::SEQ_ZERO;

    /// `mask` marks cells: `'p'` Prompt, `'i'` Input, `'o'` Output. Shorter
    /// than the text, it is padded with `'o'`.
    fn row(text: &str, mask: &str) -> Line {
        let mut line = Line::from_text(text, &CellAttributes::default(), SEQ_ZERO, None);
        let mask: Vec<char> = mask.chars().collect();
        for (idx, cell) in line.cells_mut().iter_mut().enumerate() {
            let semantic = match mask.get(idx).copied().unwrap_or('o') {
                'p' => SemanticType::Prompt,
                'i' => SemanticType::Input,
                _ => SemanticType::Output,
            };
            cell.attrs_mut().set_semantic_type(semantic);
        }
        line
    }

    /// A prompt row with the prompt string marked Prompt and the typed command
    /// marked Input, exactly as `wezterm.sh` marks them.
    fn prompt_row(prompt: &str, input: &str) -> Line {
        let mask = format!(
            "{}{}",
            "p".repeat(prompt.chars().count()),
            "i".repeat(input.chars().count())
        );
        row(&format!("{prompt}{input}"), &mask)
    }

    /// A row with no semantic marks at all, i.e. a pane with no shell
    /// integration.
    fn plain(text: &str) -> Line {
        Line::from_text(text, &CellAttributes::default(), SEQ_ZERO, None)
    }

    fn wrapped(mut line: Line) -> Line {
        line.set_last_cell_was_wrapped(true, SEQ_ZERO);
        line
    }

    fn make_scan(rows: Vec<Line>, first_row: StableRowIndex, clipped: bool) -> PaneScan {
        let kinds: Vec<RowKind> = rows.iter().map(classify_row).collect();
        let end_row = first_row + rows.len() as StableRowIndex;
        PaneScan {
            rows,
            kinds,
            first_row,
            clipped,
            end_row,
        }
    }

    fn scan(rows: Vec<Line>) -> PaneScan {
        make_scan(rows, 0, false)
    }

    fn clipped_scan(rows: Vec<Line>, first_row: StableRowIndex) -> PaneScan {
        make_scan(rows, first_row, true)
    }

    #[test]
    fn last_output_is_the_rows_after_the_last_input() {
        let scan = scan(vec![
            prompt_row("❯ ", "echo one"),
            plain("one"),
            prompt_row("❯ ", "echo two"),
            plain("two"),
            prompt_row("❯ ", ""),
        ]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandOutput, &scan);
        assert_eq!(payload.source, ShellCopySource::SemanticMarks);
        assert_eq!(payload.text, "two");
        assert!(!payload.clipped);
        assert!(!payload.head_clipped);
    }

    /// The copied command is the typed text only: never the prompt string in
    /// front of it, and never a zsh RPROMPT, which `wezterm.sh` leaves marked
    /// as Input on the same row past a blank gap.
    #[test]
    fn last_command_excludes_the_prompt_string_and_an_rprompt() {
        let with_rprompt = row("❯ ls -l        11:02", "ppiiiii");
        assert_eq!(first_input_run(&with_rprompt), Some(2..7));

        let scan = scan(vec![with_rprompt, plain("total 0"), prompt_row("❯ ", "")]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandWithOutput, &scan);
        assert_eq!(payload.source, ShellCopySource::SemanticMarks);
        assert_eq!(payload.text, "ls -l\ntotal 0");
    }

    /// `wezterm.sh` marks PS2 exactly like PS1, so a multi-row command is one
    /// coalesced run; every typed row belongs to the command text.
    #[test]
    fn multiline_input_joins_ps2_continuation_rows() {
        let scan = scan(vec![
            prompt_row("❯ ", "for i in 1 2; do"),
            prompt_row("> ", "echo $i"),
            prompt_row("> ", "done"),
            plain("1"),
            plain("2"),
            prompt_row("❯ ", ""),
        ]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandWithOutput, &scan);
        assert_eq!(payload.text, "for i in 1 2; do\necho $i\ndone\n1\n2");
    }

    /// The prompt the user is sitting at has no output, so it is not a command
    /// anyone can copy — and it must not be reported as a pending one either,
    /// or nearly every copy would claim a command is still running.
    #[test]
    fn an_idle_prompt_at_the_bottom_is_not_the_last_command() {
        let scan = scan(vec![
            prompt_row("❯ ", "ls"),
            plain("a.txt"),
            plain("b.txt"),
            prompt_row("❯ ", ""),
        ]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandOutput, &scan);
        assert_eq!(payload.text, "a.txt\nb.txt");
        assert!(!payload.pending_below);
    }

    /// A command typed but not yet run has no output; the copy falls back to
    /// the command before it and says so.
    #[test]
    fn a_typed_but_unexecuted_command_is_skipped_and_reported() {
        let scan = scan(vec![
            prompt_row("❯ ", "ls"),
            plain("a.txt"),
            prompt_row("❯ ", "vim notes.md"),
        ]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandOutput, &scan);
        assert_eq!(payload.text, "a.txt");
        assert!(payload.pending_below);
        assert!(
            shell_copy_toast_message(ShellCopyAction::LastCommandOutput, &payload)
                .contains("still running")
        );
    }

    /// Clicking Copy in the middle of a build should hand over the bytes so
    /// far, not the command before it.
    #[test]
    fn a_running_command_with_partial_output_is_the_last_command() {
        let scan = scan(vec![
            prompt_row("❯ ", "ls"),
            plain("a.txt"),
            prompt_row("❯ ", "cargo build"),
            plain("   Compiling wezterm-gui"),
        ]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandWithOutput, &scan);
        assert_eq!(payload.text, "cargo build\n   Compiling wezterm-gui");
        assert!(!payload.pending_below);
    }

    /// The scan window opened inside a command's output. The rows are still
    /// worth copying, but the copy is partial and must say so rather than
    /// present itself as the whole output.
    #[test]
    fn the_head_of_a_clipped_command_is_reported_not_hidden() {
        let scan = clipped_scan(
            vec![
                plain("   Compiling foo"),
                plain("   Compiling bar"),
                prompt_row("❯ ", ""),
            ],
            400,
        );
        let payload = shell_copy_payload(ShellCopyAction::LastCommandOutput, &scan);
        assert_eq!(payload.source, ShellCopySource::SemanticMarks);
        assert!(payload.head_clipped);
        assert_eq!(
            payload.text,
            format!("{AGENT_TRANSCRIPT_CLIPPED_MARKER}\n   Compiling foo\n   Compiling bar")
        );
        assert!(!payload.pending_below);
    }

    /// The window stopped short of the top of the scrollback, but the command
    /// that was picked is whole. The clipped marker is prepended to the copied
    /// text, so prepending it here would be a false claim *about that text* —
    /// the toast still mentions the window.
    #[test]
    fn a_complete_command_in_a_clipped_window_carries_no_marker() {
        let scan = clipped_scan(
            vec![
                plain("   Compiling foo"),
                prompt_row("❯ ", "ls"),
                plain("a.txt"),
            ],
            400,
        );
        let payload = shell_copy_payload(ShellCopyAction::LastCommandOutput, &scan);
        assert!(payload.clipped);
        assert!(!payload.head_clipped);
        assert_eq!(payload.text, "a.txt");
        assert!(
            shell_copy_toast_message(ShellCopyAction::LastCommandOutput, &payload)
                .ends_with(" (older scrollback was not available)")
        );
    }

    /// Output above the first prompt is not always a clipped command: at the
    /// top of a session it is the login banner, and the command below it is
    /// complete.
    #[test]
    fn a_login_banner_above_the_first_prompt_is_not_reported_as_clipped() {
        let scan = scan(vec![
            plain("Welcome to macOS"),
            plain("Last login: Tue Sep  9"),
            prompt_row("❯ ", "ls"),
            plain("a.txt"),
            prompt_row("❯ ", ""),
        ]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandOutput, &scan);
        assert_eq!(payload.text, "a.txt");
        assert!(!payload.head_clipped);
        assert!(!payload.clipped);
    }

    /// A soft wrap is a rendering artefact, not a line break: the same
    /// contract `TermWindow::selection_text` honours.
    #[test]
    fn soft_wrapped_output_is_joined_into_one_line() {
        let scan = scan(vec![
            prompt_row("❯ ", "cat long"),
            wrapped(plain("abcdef")),
            plain("ghi"),
            prompt_row("❯ ", ""),
        ]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandOutput, &scan);
        assert_eq!(payload.text, "abcdefghi");
    }

    /// Blank rows inside output are content (`ls` between sections, a diff
    /// hunk gap); blank rows at the edges are just the terminal's padding.
    #[test]
    fn interior_blank_output_rows_survive_and_edge_ones_do_not() {
        let scan = scan(vec![
            prompt_row("❯ ", "report"),
            plain(""),
            plain("a"),
            plain(""),
            plain("b"),
            plain(""),
        ]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandOutput, &scan);
        assert_eq!(payload.text, "a\n\nb");
    }

    /// Two commands in a row, the first printing nothing, are marked exactly
    /// like one PS2-continued command. A repeated PS1 string is the signal
    /// that tells them apart — without it, `true` would be glued onto the
    /// front of `echo hi`.
    #[test]
    fn a_zero_output_command_is_not_merged_into_the_next_command_line() {
        let scan = scan(vec![
            prompt_row("❯ ", "true"),
            prompt_row("❯ ", "echo hi"),
            plain("hi"),
            prompt_row("❯ ", ""),
        ]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandWithOutput, &scan);
        assert_eq!(payload.text, "echo hi\nhi");
    }

    /// PS2 essentially never renders the same string as PS1, so the split rule
    /// above leaves a genuine continuation alone.
    #[test]
    fn a_ps2_continuation_is_not_split() {
        let rows = vec![
            prompt_row("❯ ", "for i in 1 2; do"),
            prompt_row("> ", "echo $i"),
            prompt_row("> ", "done"),
            plain("1"),
        ];
        let kinds: Vec<RowKind> = rows.iter().map(classify_row).collect();
        let blocks = command_blocks(&kinds, 0, false);
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            split_repeated_prompt_rows(&rows, 0, blocks[0]),
            vec![blocks[0]]
        );
    }

    /// No OSC 133 anywhere: the boundary comes from the prompt shape learned
    /// from the bare prompt at the bottom of the pane.
    #[test]
    fn no_semantic_marks_falls_back_to_the_row_above_the_current_prompt() {
        let scan = scan(vec![
            plain("$ ls"),
            plain("a.txt"),
            plain("b.txt"),
            plain("$ "),
        ]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandOutput, &scan);
        assert_eq!(payload.source, ShellCopySource::GuessedPromptLine);
        assert_eq!(payload.text, "a.txt\nb.txt");
        assert!(!payload.pending_below);

        let payload = shell_copy_payload(ShellCopyAction::LastCommandWithOutput, &scan);
        assert_eq!(payload.text, "ls\na.txt\nb.txt");
    }

    /// Output is full of prompt-shaped noise. Locking the sigil to the family
    /// learned from the real prompt is what keeps `100% done` and
    /// `50$ per unit` out of the boundary search.
    #[test]
    fn a_percentage_in_output_is_not_mistaken_for_a_prompt() {
        assert!(!is_bare_prompt_row("100% done"));
        assert!(!is_command_prompt_row("100% done", '❯'));
        assert!(!is_command_prompt_row("50$ per unit", '❯'));

        let scan = scan(vec![
            plain("❯ make"),
            plain("100% done"),
            plain("50$ per unit"),
            plain("❯ "),
        ]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandOutput, &scan);
        assert_eq!(payload.source, ShellCopySource::GuessedPromptLine);
        assert_eq!(payload.text, "100% done\n50$ per unit");
    }

    /// The prose that a terminal shows most often is markdown and code. None
    /// of it may look like a prompt.
    #[test]
    fn blockquotes_headings_and_indented_lines_are_never_prompts() {
        assert!(!is_bare_prompt_row("> quoted text"));
        assert!(!is_command_prompt_row("> quoted text", '$'));
        assert!(!is_bare_prompt_row("  $ indented"));
        assert!(!is_command_prompt_row("  $ indented", '$'));
        assert!(!is_command_prompt_row("# Heading", '#'));
        assert!(!is_command_prompt_row("## Notes", '#'));
        assert!(!is_bare_prompt_row("#"));
        assert!(!is_bare_prompt_row("###"));
        assert!(!is_bare_prompt_row(""));

        // ... while a real root prompt still works.
        assert!(is_bare_prompt_row("root@box:~#"));
        assert!(is_command_prompt_row("root@box:~# ls", '#'));
    }

    /// `ssh host` into a box with no shell integration turns the whole remote
    /// session into one local block's "output". A genuine command's output
    /// does not *end* at a prompt, so that is the one case where the boundary
    /// is re-derived inside an output region.
    #[test]
    fn a_nested_ssh_session_prefers_the_guessed_boundary() {
        let scan = scan(vec![
            prompt_row("❯ ", "ssh host"),
            plain("Last login: Tue Sep  9"),
            plain("remote$ ls"),
            plain("a.txt"),
            plain("b.txt"),
            plain("remote$ "),
        ]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandOutput, &scan);
        assert_eq!(payload.source, ShellCopySource::GuessedPromptLine);
        assert_eq!(payload.text, "a.txt\nb.txt");

        let payload = shell_copy_payload(ShellCopyAction::LastCommandWithOutput, &scan);
        assert_eq!(payload.text, "ls\na.txt\nb.txt");
    }

    /// Copying the pane is the one action with nothing to infer: it takes
    /// every row as rendered, prompts included, and only reports whether the
    /// window reached the top of the scrollback.
    #[test]
    fn copy_pane_never_guesses_and_marks_a_clipped_window() {
        let scan = clipped_scan(
            vec![plain("a.txt"), prompt_row("❯ ", "ls"), plain("b.txt")],
            77,
        );
        let payload = shell_copy_payload(ShellCopyAction::WholePane, &scan);
        assert_eq!(payload.source, ShellCopySource::WholePane);
        assert!(payload.clipped);
        assert_eq!(
            payload.text,
            format!("{AGENT_TRANSCRIPT_CLIPPED_MARKER}\na.txt\n❯ ls\nb.txt")
        );
        assert_eq!(
            shell_copy_toast_message(ShellCopyAction::WholePane, &payload),
            "Copied the pane (older scrollback was not available)"
        );
    }

    /// Neither rung of the ladder found a command. An empty payload is the
    /// refusal: silently copying the whole pane instead would hand over
    /// something the user did not ask for.
    #[test]
    fn nothing_found_refuses_instead_of_copying_the_pane() {
        let scan = scan(vec![plain("hello"), plain("world")]);
        let payload = shell_copy_payload(ShellCopyAction::LastCommandOutput, &scan);
        assert!(payload.text.is_empty());
        assert_eq!(
            shell_copy_toast_message(ShellCopyAction::LastCommandOutput, &payload),
            "No command output found in this pane"
        );
        assert_eq!(
            shell_copy_toast_message(ShellCopyAction::WholePane, &ShellCopyPayload::default()),
            "Nothing to copy from this pane"
        );
    }

    /// The toast is pure so that it cannot overstate what the extraction did:
    /// a guessed boundary says guessed, a pending command says so, and a
    /// clipped window says so.
    #[test]
    fn toast_message_reports_the_guess_and_the_pending_command() {
        let guessed = ShellCopyPayload {
            text: "a.txt".to_string(),
            source: ShellCopySource::GuessedPromptLine,
            clipped: true,
            head_clipped: false,
            pending_below: true,
        };
        let message = shell_copy_toast_message(ShellCopyAction::LastCommandOutput, &guessed);
        assert!(message.contains("guessed"), "{message}");
        assert!(message.contains("still running"), "{message}");
        assert!(
            message.ends_with(" (older scrollback was not available)"),
            "{message}"
        );

        let known = ShellCopyPayload {
            text: "a.txt".to_string(),
            source: ShellCopySource::SemanticMarks,
            clipped: false,
            head_clipped: false,
            pending_below: false,
        };
        assert_eq!(
            shell_copy_toast_message(ShellCopyAction::LastCommandWithOutput, &known),
            "Copied the last command and its output"
        );
        assert!(
            !shell_copy_toast_message(ShellCopyAction::LastCommandOutput, &known)
                .contains("guessed")
        );
    }
}
