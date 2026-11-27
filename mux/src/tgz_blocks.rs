//! Command blocks from OSC 133 marks (TGZTerminal).
//!
//! Splits pane rows into prompt-to-prompt command blocks, reading the
//! per-cell semantic bits straight off the [`Line`]s (see the module docs of
//! `wezterm-gui`'s `shell_copy.rs` for why this does not use
//! `get_semantic_zones`), plus a prompt-shape heuristic for panes without
//! shell integration. Pure and GUI-free, so the GUI (Copy, failure marks,
//! the Ask panel) and the mux server (`GetCommandBlocks`, `wezterm cli
//! get-blocks`) share one definition of "a command".

use std::ops::Range;
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
pub enum RowKind {
    Prompt,
    Input,
    Output,
    Blank,
}

/// One prompt-to-prompt region of the pane, in `StableRowIndex` space. All
/// ranges are half-open: the command occupies `prompt_start..input_end` and
/// its output `input_end..output_end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandBlock {
    pub prompt_start: StableRowIndex,
    /// End of the run of `Prompt`/`Input` rows.
    pub input_end: StableRowIndex,
    /// `== input_end` when the command printed nothing.
    pub output_end: StableRowIndex,
    /// The scan window began inside this block, so its head is missing.
    pub head_clipped: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastCommandPick {
    pub block: CommandBlock,
    /// A newer block exists below the picked one.
    pub pending_below: bool,
}

/// Sigils a prompt may end in. `'>'` is deliberately absent: it collides with
/// markdown blockquotes, git and mail quoting, bash's PS2, printed shell
/// redirections and a Python REPL's `>>>`. A false positive here silently
/// copies the wrong thing, and no macOS shell ships `'>'` as its PS1.
pub const SHELL_PROMPT_SIGILS: &[char] = &['$', '#', '%', '❯', '➜', '»'];

/// A prompt shape learned from one unambiguous bare prompt row, used to lock
/// every later match to the same prompt family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptShape {
    pub row: StableRowIndex,
    pub sigil: char,
}

// ---------------------------------------------------------------------------
// Row classification
// ---------------------------------------------------------------------------

pub fn classify_row(line: &Line) -> RowKind {
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
pub fn first_input_run(line: &Line) -> Option<Range<usize>> {
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
pub fn prompt_run_text(line: &Line) -> String {
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

pub fn row_text(line: &Line) -> String {
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
pub fn command_blocks(
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
        // Trailing blank rows are not output. A live pane that is not full has
        // empty viewport rows under the prompt the user is sitting at; counted
        // as output, they made that idle prompt the "last command" — and a
        // no-output command coalesced with it was copied with no output.
        let mut output_end = k;
        while output_end > j && kinds[output_end - 1] == RowKind::Blank {
            output_end -= 1;
        }
        blocks.push(CommandBlock {
            prompt_start: first_row + start as StableRowIndex,
            input_end: first_row + j as StableRowIndex,
            output_end: first_row + output_end as StableRowIndex,
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
/// This deliberately skips an idle prompt at the bottom (the blank rows below
/// it are never counted as output, see [`command_blocks`]), a command typed but
/// not yet run, and a command running with nothing printed yet — none of those
/// have output to copy. A command running *with* partial output is chosen:
/// clicking Copy in the middle of a `cargo build` should hand over the bytes
/// so far.
pub fn pick_last_command(blocks: &[CommandBlock]) -> Option<LastCommandPick> {
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
pub fn split_repeated_prompt_rows(
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
pub fn typed_input_below(
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
pub fn join_rows(rows: &[Line], spans: &[Range<usize>]) -> String {
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

pub fn output_text(rows: &[Line]) -> String {
    let spans: Vec<Range<usize>> = rows.iter().map(|line| 0..line.len()).collect();
    join_rows(rows, &spans)
}

pub fn command_text(rows: &[Line]) -> String {
    let spans: Vec<Range<usize>> = rows
        .iter()
        .map(|line| first_input_run(line).unwrap_or(0..0))
        .collect();
    // A prompt row with no typed input contributes an empty segment; trimming
    // the joined result keeps it from showing up as a trailing blank line.
    join_rows(rows, &spans).trim_end().to_string()
}

pub fn trim_blank_rows(kinds: &[RowKind], rows: Range<usize>) -> Range<usize> {
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
pub fn is_bare_prompt_row(text: &str) -> bool {
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
pub fn is_command_prompt_row(text: &str, sigil: char) -> bool {
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
pub fn learn_prompt_shape(
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
/// what makes the copy payload's `pending_below` true for a guessed boundary.
pub fn heuristic_command_row(
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
pub fn heuristic_output_end(
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

pub fn command_after_sigil(text: &str, sigil: char) -> String {
    let text = text.trim_end();
    let mut needle = String::with_capacity(sigil.len_utf8() + 1);
    needle.push(sigil);
    needle.push(' ');
    match text.rfind(&needle) {
        Some(idx) => text[idx + needle.len()..].trim().to_string(),
        None => String::new(),
    }
}

pub fn index_of(row: StableRowIndex, first_row: StableRowIndex, len: usize) -> usize {
    ((row - first_row).max(0) as usize).min(len)
}

// ---------------------------------------------------------------------------
// Recent command blocks as text (Ask panel, failure marks, GetCommandBlocks)
// ---------------------------------------------------------------------------

/// One finished (or running) command, as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockText {
    /// The typed command; may span several rows (PS2 continuations).
    pub command: String,
    /// Its output, blank edges trimmed.
    pub output: String,
    /// First row of the block (its prompt).
    pub prompt_row: StableRowIndex,
    /// One past the last output row.
    pub output_end: StableRowIndex,
    /// The OSC 133;D status the shell reported, when it reported one.
    pub exit_status: Option<i32>,
    /// No shell integration: the boundary came from a prompt-shaped row.
    pub guessed: bool,
    /// The block started above the rows that were read.
    pub head_clipped: bool,
}

/// The status reported for the block starting at `prompt_start`.
///
/// `wezterm.sh` reports a command's status from `precmd`, with the cursor on
/// the row after the output — the row the next prompt starts on — so a
/// status belongs to the newest block whose prompt starts *above* its row,
/// up to and including the next block's first row.
fn status_for(
    statuses: &[(StableRowIndex, i32)],
    prompt_start: StableRowIndex,
    next_start: Option<StableRowIndex>,
) -> Option<i32> {
    statuses
        .iter()
        .filter(|(row, _)| *row > prompt_start && next_start.map_or(true, |next| *row <= next))
        .last()
        .map(|(_, status)| *status)
}

/// Up to `n` most recent command blocks in `rows` (oldest first).
///
/// `first_row` is the stable index of `rows[0]`; `scan_is_clipped` says the
/// pane holds older rows than these. Skipped: the idle prompt at the bottom
/// (nothing typed), and a command typed but not run (no output, no status).
/// Without any OSC 133 marks, at most one block is guessed from the prompt
/// shape and it carries no exit status.
pub fn last_blocks(
    rows: &[Line],
    first_row: StableRowIndex,
    scan_is_clipped: bool,
    statuses: &[(StableRowIndex, i32)],
    n: usize,
) -> Vec<BlockText> {
    if n == 0 || rows.is_empty() {
        return vec![];
    }
    let kinds: Vec<RowKind> = rows.iter().map(classify_row).collect();
    let mut blocks: Vec<CommandBlock> = Vec::new();
    for block in command_blocks(&kinds, first_row, scan_is_clipped) {
        blocks.extend(split_repeated_prompt_rows(rows, first_row, block));
    }
    if blocks.is_empty() {
        return guessed_last_block(&kinds, rows, first_row)
            .into_iter()
            .collect();
    }

    let len = rows.len();
    let mut out = Vec::new();
    for (i, block) in blocks.iter().enumerate() {
        let next_start = blocks.get(i + 1).map(|next| next.prompt_start);
        let input =
            index_of(block.prompt_start, first_row, len)..index_of(block.input_end, first_row, len);
        let output = trim_blank_rows(
            &kinds,
            index_of(block.input_end, first_row, len)..index_of(block.output_end, first_row, len),
        );
        let command = command_text(&rows[input]);
        let output_text = output_text(&rows[output]);
        let exit_status = status_for(statuses, block.prompt_start, next_start);
        let is_last = next_start.is_none();
        if command.is_empty() && output_text.is_empty() {
            continue;
        }
        if is_last && output_text.is_empty() && exit_status.is_none() {
            // Typed at the prompt and not run yet.
            continue;
        }
        out.push(BlockText {
            command,
            output: output_text,
            prompt_row: block.prompt_start,
            output_end: block.output_end,
            exit_status,
            guessed: false,
            head_clipped: block.head_clipped,
        });
    }
    let skip = out.len().saturating_sub(n);
    out.split_off(skip)
}

fn guessed_last_block(
    kinds: &[RowKind],
    rows: &[Line],
    first_row: StableRowIndex,
) -> Option<BlockText> {
    let len = rows.len();
    let shape = learn_prompt_shape(kinds, rows, first_row, 0..len)?;
    let cmd_row = heuristic_command_row(rows, first_row, &shape, 0..len)?;
    let cmd_idx = index_of(cmd_row, first_row, len);
    let shape_idx = index_of(shape.row, first_row, len);
    let output_end = heuristic_output_end(rows, &shape, cmd_idx, shape_idx);
    let output = trim_blank_rows(kinds, (cmd_idx + 1)..output_end);
    Some(BlockText {
        command: command_after_sigil(&row_text(&rows[cmd_idx]), shape.sigil),
        output: output_text(&rows[output]),
        prompt_row: cmd_row,
        output_end: first_row + output_end as StableRowIndex,
        exit_status: None,
        guessed: true,
        head_clipped: false,
    })
}

/// Read up to `max_rows` rows from the bottom of `pane`'s buffer and return
/// its `n` most recent command blocks with their exit statuses.
///
/// Empty on the alternate screen: vim/less/htop have no prompts, and the
/// scrollback behind them is the pre-app session. One `get_lines` call per
/// 2000 rows, so the pane's terminal lock is never held for long.
pub fn pane_last_blocks(pane: &dyn crate::pane::Pane, n: usize, max_rows: usize) -> Vec<BlockText> {
    const CHUNK: StableRowIndex = 2000;
    if n == 0 || pane.is_alt_screen_active() {
        return vec![];
    }
    let dims = pane.get_dimensions();
    let end = dims.physical_top + dims.viewport_rows as StableRowIndex;
    let start = (end - max_rows.max(1) as StableRowIndex).max(dims.scrollback_top);
    let mut rows: Vec<Line> = Vec::new();
    let mut first_row = start;
    let mut at = start;
    while at < end {
        let to = (at + CHUNK).min(end);
        let (got_first, lines) = pane.get_lines(at..to);
        if lines.is_empty() {
            break;
        }
        if rows.is_empty() {
            first_row = got_first;
        } else if got_first != first_row + rows.len() as StableRowIndex {
            // A hole (rows scrolled away while reading): keep what is contiguous.
            break;
        }
        at = got_first + lines.len() as StableRowIndex;
        rows.extend(lines);
    }
    let read_end = first_row + rows.len() as StableRowIndex;
    let statuses = pane.get_command_statuses(first_row..read_end + 1);
    last_blocks(
        &rows,
        first_row,
        first_row > dims.scrollback_top,
        &statuses,
        n,
    )
}

/// Viewport rows that belong to commands which exited non-zero.
///
/// `kinds` are the viewport rows, `top` the stable index of the first one,
/// `statuses` every reported status from `top` down, in row order (rows
/// below the viewport included: a failed command with long output may start
/// on screen and report its status further down). Each span runs from the
/// first row of the command (its prompt) to the row before its status; a
/// command that began above the viewport starts the span at row 0.
pub fn failed_block_rows(
    kinds: &[RowKind],
    top: StableRowIndex,
    statuses: &[(StableRowIndex, i32)],
) -> Vec<Range<usize>> {
    let is_command = |kind: RowKind| matches!(kind, RowKind::Prompt | RowKind::Input);
    let len = kinds.len();
    let bottom = top + len as StableRowIndex;
    let mut spans = Vec::new();
    // Rows above this index belong to an earlier, already-reported command.
    let mut boundary = 0usize;
    for &(row, status) in statuses {
        if row <= top {
            continue;
        }
        let end = ((row - top) as usize).min(len);
        if status != 0 {
            let mut start = boundary;
            let mut i = end;
            let mut in_command = false;
            while i > boundary {
                i -= 1;
                if is_command(kinds[i]) {
                    in_command = true;
                    start = i;
                } else if in_command {
                    break;
                }
            }
            if start < end {
                spans.push(start..end);
            }
        }
        boundary = end;
        if row >= bottom {
            break;
        }
    }
    spans
}

/// Masks secrets in text that leaves the process (`GetCommandBlocks`).
pub type Redactor = Box<dyn Fn(&str) -> String + Send + Sync>;

static REDACTOR: std::sync::OnceLock<Redactor> = std::sync::OnceLock::new();

/// Installed once by the GUI with its secret-masking rules. A headless mux
/// server has none, and its blocks go out as they are.
pub fn set_redactor(redactor: Redactor) {
    let _ = REDACTOR.set(redactor);
}

fn redact(text: &str) -> String {
    match REDACTOR.get() {
        Some(redactor) => redactor(text),
        None => text.to_string(),
    }
}

/// Output kept per block for `GetCommandBlocks`, from the end.
const SERVED_OUTPUT_MAX: usize = 16 * 1024;

/// [`pane_last_blocks`] for another process: secrets masked, output capped
/// to its last [`SERVED_OUTPUT_MAX`] bytes, optionally only failed commands.
pub fn pane_last_blocks_redacted(
    pane: &dyn crate::pane::Pane,
    last: usize,
    failed_only: bool,
) -> Vec<BlockText> {
    let wanted = if failed_only { 200 } else { last };
    let mut blocks = pane_last_blocks(pane, wanted, 20_000);
    if failed_only {
        blocks.retain(|block| block.exit_status.map_or(false, |status| status != 0));
    }
    let skip = blocks.len().saturating_sub(last);
    blocks.drain(..skip);
    for block in blocks.iter_mut() {
        block.command = redact(&block.command);
        let mut output = redact(&block.output);
        if output.len() > SERVED_OUTPUT_MAX {
            let mut start = output.len() - SERVED_OUTPUT_MAX;
            while !output.is_char_boundary(start) {
                start += 1;
            }
            output = format!("[earlier output cut]\n{}", &output[start..]);
        }
        block.output = output;
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use termwiz::cell::CellAttributes;
    use termwiz::surface::SEQ_ZERO;

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

    fn prompt(input: &str) -> Line {
        row(
            &format!("$ {input}"),
            &format!("pp{}", "i".repeat(input.len())),
        )
    }

    fn out(text: &str) -> Line {
        row(text, "")
    }

    fn plain(text: &str) -> Line {
        Line::from_text(text, &CellAttributes::default(), SEQ_ZERO, None)
    }

    fn session() -> Vec<Line> {
        vec![
            prompt("echo hi"), // 0
            out("hi"),         // 1
            prompt("false"),   // 2 (status 0 for echo reported here)
            prompt("ls"),      // 3 (status 1 for false)
            out("a b"),        // 4
            prompt(""),        // 5 idle (status 0 for ls)
        ]
    }

    #[test]
    fn blocks_carry_command_output_and_status() {
        let blocks = last_blocks(&session(), 0, false, &[(2, 0), (3, 1), (5, 0)], 10);
        let got: Vec<(&str, &str, Option<i32>)> = blocks
            .iter()
            .map(|b| (b.command.as_str(), b.output.as_str(), b.exit_status))
            .collect();
        assert_eq!(
            got,
            vec![
                ("echo hi", "hi", Some(0)),
                ("false", "", Some(1)),
                ("ls", "a b", Some(0)),
            ]
        );
    }

    #[test]
    fn only_the_newest_n_are_returned_oldest_first() {
        let blocks = last_blocks(&session(), 0, false, &[(2, 0), (3, 1), (5, 0)], 2);
        let commands: Vec<&str> = blocks.iter().map(|b| b.command.as_str()).collect();
        assert_eq!(commands, vec!["false", "ls"]);
    }

    #[test]
    fn a_command_typed_but_not_run_is_skipped() {
        let mut rows = session();
        rows.pop();
        rows.push(prompt("rm -rf build"));
        let blocks = last_blocks(&rows, 0, false, &[(2, 0), (3, 1), (5, 0)], 10);
        assert_eq!(blocks.last().unwrap().command, "ls");
    }

    #[test]
    fn stable_rows_are_offset_by_first_row() {
        let blocks = last_blocks(&session(), 100, false, &[(103, 1)], 10);
        assert_eq!(blocks[1].command, "false");
        assert_eq!(blocks[1].prompt_row, 102);
        assert_eq!(blocks[1].exit_status, Some(1));
        assert_eq!(blocks[0].exit_status, None);
    }

    #[test]
    fn without_marks_one_block_is_guessed() {
        let rows = vec![
            plain("user@box ~ % make"),
            plain("error: nope"),
            plain("user@box ~ %"),
        ];
        let blocks = last_blocks(&rows, 0, false, &[], 5);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].guessed);
        assert_eq!(blocks[0].command, "make");
        assert_eq!(blocks[0].output, "error: nope");
        assert_eq!(blocks[0].exit_status, None);
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(last_blocks(&[], 0, false, &[], 3).is_empty());
        assert!(last_blocks(&session(), 0, false, &[], 0).is_empty());
    }

    fn kinds(rows: &[Line]) -> Vec<RowKind> {
        rows.iter().map(classify_row).collect()
    }

    #[test]
    fn a_failed_command_is_marked_from_its_prompt_to_its_status() {
        // echo hi (0) / false (1) / ls (0) / idle
        let spans = failed_block_rows(&kinds(&session()), 0, &[(2, 0), (3, 1), (5, 0)]);
        assert_eq!(spans, vec![2..3]);
    }

    #[test]
    fn a_failure_with_output_covers_the_output() {
        let rows = vec![prompt("make"), out("error 1"), out("error 2"), prompt("")];
        assert_eq!(failed_block_rows(&kinds(&rows), 10, &[(13, 2)]), vec![0..3]);
    }

    #[test]
    fn a_command_that_began_above_the_viewport_starts_at_row_zero() {
        let rows = vec![out("..."), out("FAILED"), prompt("")];
        assert_eq!(failed_block_rows(&kinds(&rows), 50, &[(52, 1)]), vec![0..2]);
    }

    #[test]
    fn a_status_below_the_viewport_marks_down_to_the_bottom() {
        let rows = vec![prompt("ok"), prompt("cargo test"), out("running")];
        let spans = failed_block_rows(&kinds(&rows), 0, &[(1, 0), (40, 101), (41, 1)]);
        assert_eq!(spans, vec![1..3]);
    }

    #[test]
    fn statuses_at_or_above_the_top_are_ignored_and_successes_mark_nothing() {
        let rows = vec![prompt("ls"), out("a"), prompt("")];
        assert!(failed_block_rows(&kinds(&rows), 5, &[(3, 1), (5, 1), (7, 0)]).is_empty());
    }
}
