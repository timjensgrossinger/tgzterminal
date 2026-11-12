//! Edits a `wezterm.lua` for the Settings tab.
//!
//! The Lua file stays the one place a setting lives. Changing a setting
//! either rewrites the value where the file already sets it as a plain
//! literal, or records it in a marked block just before the file's final
//! `return`:
//!
//! ```lua
//! -- >>> TGZTerminal settings (managed by the Settings tab, edit freely)
//! config.diff_panel = config.diff_panel or {}
//! config.diff_panel.long_lines = 'Scroll'
//! -- <<< TGZTerminal settings
//! ```
//!
//! A setting the file computes (a function call, a conditional, two
//! assignments) is reported as locked and left alone: silently overriding
//! code somebody wrote is worse than sending them to their editor.
//!
//! This is not a Lua parser. It masks comments and strings, tokenizes what
//! is left and follows brackets and block keywords, which is enough to find
//! `name.path = literal` statements and `key = literal` fields of the
//! returned table. Everything it does not recognise it leaves untouched, and
//! the caller evaluates the edited text before it replaces the file.
//!
//! Pure: text in, text out.

use std::collections::BTreeMap;

const BLOCK_OPEN: &str = "-- >>> TGZTerminal settings";
const BLOCK_CLOSE: &str = "-- <<< TGZTerminal settings";
const BLOCK_OPEN_LINE: &str =
    "-- >>> TGZTerminal settings (managed by the Settings tab, edit freely)";

/// A value the Settings tab can write.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LuaValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// A font family: `wezterm.font('Name')`.
    Font(String),
}

impl LuaValue {
    /// The value as a Lua literal, strings in `quote`s. A font renders as its
    /// family name only: that is what is replaced inside an existing
    /// `wezterm.font(...)` call.
    fn literal(&self, quote: char) -> String {
        match self {
            LuaValue::Bool(b) => b.to_string(),
            LuaValue::Int(n) => n.to_string(),
            LuaValue::Float(f) if !f.is_finite() => "0".to_string(),
            LuaValue::Float(f) if f.fract() == 0.0 => format!("{f:.1}"),
            LuaValue::Float(f) => f.to_string(),
            LuaValue::Str(s) | LuaValue::Font(s) => quote_lua(s, quote),
        }
    }

    /// The value as the right-hand side of a statement in the block.
    fn statement_rhs(&self) -> String {
        match self {
            // `require` rather than a local the file may not have.
            LuaValue::Font(name) => {
                format!("require('wezterm').font({})", quote_lua(name, '\''))
            }
            other => other.literal('\''),
        }
    }
}

fn quote_lua(s: &str, quote: char) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\{:03}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Why a setting could not be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EditError {
    /// The file sets this itself in a way that is not a plain literal; the
    /// line (1-based) is where.
    Locked { line: usize },
    /// The file does not end in `return <name>` or `return { ... }`.
    UnsupportedShape,
}

/// Where a setting stands in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PathStatus {
    /// The file does not mention it.
    Absent,
    /// Set as a plain literal on this line; a change rewrites it there.
    InPlace { line: usize },
    /// Recorded in the managed block.
    Block,
    /// Set by code on this line.
    Locked { line: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Ident,
    Str,
    Num,
    /// A single `=`.
    Assign,
    Punct(u8),
    Other,
}

#[derive(Clone, Copy, Debug)]
struct Tok {
    kind: Kind,
    start: usize,
    end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SlotKind {
    /// A plain literal occupying `start..end`.
    Literal,
    /// The family name inside `wezterm.font("Name", ...)`.
    FontName,
    /// A table constructor: its fields are slots of their own.
    Table,
    /// Anything else: code.
    Opaque,
}

#[derive(Clone, Debug)]
struct Slot {
    path: Vec<String>,
    kind: SlotKind,
    start: usize,
    end: usize,
    /// Inside an `if`, a loop or a function: it may or may not run.
    conditional: bool,
}

enum Root {
    /// `return name`: byte offset of the `return`.
    Ident { name: String, return_start: usize },
    /// `return { ... }`: the `return`, and the offset just past the table
    /// (and a `;` after it).
    Table {
        return_start: usize,
        open_tok: usize,
        after: usize,
    },
}

/// Byte range of the managed block: whole lines, both markers included.
#[derive(Clone, Copy)]
struct BlockSpan {
    start: usize,
    end: usize,
}

struct Doc<'a> {
    src: &'a str,
    mask: Vec<u8>,
    toks: Vec<Tok>,
    /// Bracket and block-keyword depth before each token.
    brackets: Vec<u32>,
    blocks: Vec<u32>,
    block: Option<BlockSpan>,
}

/// `[`, `[=[`, ... at `i`: the number of `=`.
fn long_open(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'[') {
        return None;
    }
    let mut level = 0;
    while b.get(i + 1 + level) == Some(&b'=') {
        level += 1;
    }
    (b.get(i + 1 + level) == Some(&b'[')).then_some(level)
}

/// Offset just past the `]=*]` that closes a long bracket of `level`,
/// searching from `from`; the end of the text when it is never closed.
fn long_close(b: &[u8], from: usize, level: usize) -> usize {
    let mut i = from;
    while i < b.len() {
        if b[i] == b']' {
            let mut n = 0;
            while b.get(i + 1 + n) == Some(&b'=') {
                n += 1;
            }
            if n == level && b.get(i + 1 + n) == Some(&b']') {
                return i + 2 + n;
            }
        }
        i += 1;
    }
    b.len()
}

/// The text with comments turned into spaces and strings into runs of `"`,
/// byte for byte the same length, line breaks kept.
fn mask(src: &str) -> Vec<u8> {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize, with: u8| {
        for byte in &mut out[from..to] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = with;
            }
        }
    };
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'-' if b.get(i + 1) == Some(&b'-') => {
                let end = match long_open(b, i + 2) {
                    Some(level) => long_close(b, i + 4 + level, level),
                    None => b[i..]
                        .iter()
                        .position(|c| *c == b'\n')
                        .map_or(b.len(), |n| i + n),
                };
                blank(&mut out, i, end, b' ');
                i = end;
            }
            quote @ (b'"' | b'\'') => {
                let mut end = i + 1;
                while end < b.len() && b[end] != quote && b[end] != b'\n' {
                    end += if b[end] == b'\\' { 2 } else { 1 };
                }
                let end = (end + 1).min(b.len());
                blank(&mut out, i, end, b'"');
                i = end;
            }
            b'[' => match long_open(b, i) {
                Some(level) => {
                    let end = long_close(b, i + 2 + level, level);
                    blank(&mut out, i, end, b'"');
                    i = end;
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    out
}

fn tokenize(mask: &[u8]) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut i = 0;
    while i < mask.len() {
        let c = mask[i];
        let start = i;
        let kind = if c.is_ascii_whitespace() {
            i += 1;
            continue;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < mask.len() && (mask[i].is_ascii_alphanumeric() || mask[i] == b'_') {
                i += 1;
            }
            Kind::Ident
        } else if c.is_ascii_digit()
            || (c == b'.' && mask.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
        {
            while i < mask.len() {
                let d = mask[i];
                let exponent_sign = (d == b'+' || d == b'-')
                    && matches!(mask[i - 1], b'e' | b'E' | b'p' | b'P')
                    && i > start;
                if d.is_ascii_alphanumeric() || d == b'.' || d == b'_' || exponent_sign {
                    i += 1;
                } else {
                    break;
                }
            }
            Kind::Num
        } else if c == b'"' {
            while i < mask.len() && mask[i] == b'"' {
                i += 1;
            }
            Kind::Str
        } else if c == b'.' && mask.get(i + 1) == Some(&b'.') {
            while i < mask.len() && mask[i] == b'.' {
                i += 1;
            }
            Kind::Other
        } else if matches!(c, b'=' | b'~' | b'<' | b'>') && mask.get(i + 1) == Some(&b'=') {
            i += 2;
            Kind::Other
        } else if c == b':' && mask.get(i + 1) == Some(&b':') {
            i += 2;
            Kind::Other
        } else if c == b'=' {
            i += 1;
            Kind::Assign
        } else {
            i += 1;
            Kind::Punct(c)
        };
        toks.push(Tok {
            kind,
            start,
            end: i,
        });
    }
    toks
}

/// The managed block's lines, markers included, when both markers are there
/// in order.
fn find_block(src: &str) -> Option<BlockSpan> {
    let mut offset = 0;
    let mut start = None;
    for line in src.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if start.is_none() && trimmed.starts_with(BLOCK_OPEN) {
            start = Some(offset);
        } else if let Some(start) = start {
            if trimmed.starts_with(BLOCK_CLOSE) {
                return Some(BlockSpan {
                    start,
                    end: offset + line.len(),
                });
            }
        }
        offset += line.len();
    }
    None
}

impl<'a> Doc<'a> {
    fn new(src: &'a str) -> Self {
        let mask = mask(src);
        let block = find_block(src);
        let toks: Vec<Tok> = tokenize(&mask)
            .into_iter()
            .filter(|t| block.map_or(true, |b| t.start < b.start || t.start >= b.end))
            .collect();
        let mut brackets = Vec::with_capacity(toks.len());
        let mut blocks = Vec::with_capacity(toks.len());
        let (mut bracket, mut keyword) = (0u32, 0u32);
        for t in &toks {
            brackets.push(bracket);
            blocks.push(keyword);
            match t.kind {
                Kind::Punct(b'(' | b'{' | b'[') => bracket += 1,
                Kind::Punct(b')' | b'}' | b']') => bracket = bracket.saturating_sub(1),
                Kind::Ident => match &mask[t.start..t.end] {
                    b"function" | b"if" | b"do" | b"repeat" => keyword += 1,
                    b"end" | b"until" => keyword = keyword.saturating_sub(1),
                    _ => {}
                },
                _ => {}
            }
        }
        Self {
            src,
            mask,
            toks,
            brackets,
            blocks,
            block,
        }
    }

    fn word(&self, i: usize) -> &str {
        self.toks
            .get(i)
            .filter(|t| t.kind == Kind::Ident)
            .and_then(|t| std::str::from_utf8(&self.mask[t.start..t.end]).ok())
            .unwrap_or("")
    }

    fn is_punct(&self, i: usize, c: u8) -> bool {
        self.toks.get(i).is_some_and(|t| t.kind == Kind::Punct(c))
    }

    fn is_kind(&self, i: usize, kind: Kind) -> bool {
        self.toks.get(i).is_some_and(|t| t.kind == kind)
    }

    fn line_of(&self, offset: usize) -> usize {
        self.src.as_bytes()[..offset.min(self.src.len())]
            .iter()
            .filter(|c| **c == b'\n')
            .count()
            + 1
    }

    /// Whether a line break separates the end of token `a` from token `b`.
    fn line_break_between(&self, a: usize, b: usize) -> bool {
        match (self.toks.get(a), self.toks.get(b)) {
            (Some(a), Some(b)) => self.mask[a.end..b.start].contains(&b'\n'),
            _ => true,
        }
    }

    /// The token index of the bracket closing the one at `open`.
    fn matching_close(&self, open: usize) -> Option<usize> {
        let depth = self.brackets[open] + 1;
        (open + 1..self.toks.len()).find(|j| {
            self.brackets[*j] == depth
                && matches!(self.toks[*j].kind, Kind::Punct(b')' | b'}' | b']'))
        })
    }

    fn root(&self) -> Result<Root, EditError> {
        let ret = (0..self.toks.len())
            .rev()
            .find(|i| self.word(*i) == "return" && self.brackets[*i] == 0 && self.blocks[*i] == 0)
            .ok_or(EditError::UnsupportedShape)?;
        let return_start = self.toks[ret].start;
        let ends_after =
            |i: usize| i >= self.toks.len() || (self.is_punct(i, b';') && i + 1 >= self.toks.len());
        if self.is_punct(ret + 1, b'{') {
            let close = self
                .matching_close(ret + 1)
                .ok_or(EditError::UnsupportedShape)?;
            if !ends_after(close + 1) {
                return Err(EditError::UnsupportedShape);
            }
            let after = self
                .toks
                .get(close + 1)
                .map_or(self.toks[close].end, |semi| semi.end);
            return Ok(Root::Table {
                return_start,
                open_tok: ret + 1,
                after,
            });
        }
        let name = self.word(ret + 1);
        if name.is_empty() || matches!(name, "nil" | "true" | "false") || !ends_after(ret + 2) {
            return Err(EditError::UnsupportedShape);
        }
        Ok(Root::Ident {
            name: name.to_string(),
            return_start,
        })
    }

    /// Classify the value starting at token `at`, recording its slot (and
    /// those of a table's fields). Returns the token index after what was
    /// recognised; `at` itself for code.
    fn value_at(
        &self,
        at: usize,
        path: &[String],
        conditional: bool,
        in_table: bool,
        slots: &mut Vec<Slot>,
    ) -> usize {
        let Some(first) = self.toks.get(at) else {
            return at;
        };
        // Whether the value ends where a plain literal would: at the next
        // field in a table, at the end of the statement otherwise.
        let ends_at = |last: usize| {
            let next = last + 1;
            if in_table {
                self.is_punct(next, b',') || self.is_punct(next, b';') || self.is_punct(next, b'}')
            } else {
                next >= self.toks.len()
                    || self.is_punct(next, b';')
                    || self.line_break_between(last, next)
            }
        };
        let mut push = |kind: SlotKind, start: usize, end: usize| {
            slots.push(Slot {
                path: path.to_vec(),
                kind,
                start,
                end,
                conditional,
            });
        };

        if first.kind == Kind::Punct(b'{') {
            push(SlotKind::Table, first.start, first.end);
            return self.walk_table(at, path, conditional, slots);
        }

        let literal_last = match first.kind {
            Kind::Str | Kind::Num => Some(at),
            Kind::Ident if matches!(self.word(at), "true" | "false") => Some(at),
            Kind::Punct(b'-') if self.is_kind(at + 1, Kind::Num) => Some(at + 1),
            _ => None,
        };
        if let Some(last) = literal_last {
            if ends_at(last) {
                push(SlotKind::Literal, first.start, self.toks[last].end);
                return last + 1;
            }
        }

        // wezterm.font("Name", ...)
        if self.word(at) == "wezterm"
            && self.is_punct(at + 1, b'.')
            && self.word(at + 2) == "font"
            && self.is_punct(at + 3, b'(')
            && self.is_kind(at + 4, Kind::Str)
        {
            if let Some(close) = self.matching_close(at + 3) {
                if ends_at(close) {
                    let name = self.toks[at + 4];
                    push(SlotKind::FontName, name.start, name.end);
                    return close + 1;
                }
            }
        }

        push(SlotKind::Opaque, first.start, first.end);
        at
    }

    /// Record the fields of the table constructor opening at token `open`.
    /// Returns the token index after its closing brace.
    fn walk_table(
        &self,
        open: usize,
        path: &[String],
        conditional: bool,
        slots: &mut Vec<Slot>,
    ) -> usize {
        let depth = self.brackets[open] + 1;
        let block = self.blocks[open];
        let closes = |j: usize| self.is_punct(j, b'}') && self.brackets[j] == depth;
        let separates = |j: usize| {
            self.brackets[j] == depth
                && self.blocks[j] == block
                && (self.is_punct(j, b',') || self.is_punct(j, b';'))
        };
        let mut j = open + 1;
        while j < self.toks.len() {
            if closes(j) {
                return j + 1;
            }
            if separates(j) {
                j += 1;
                continue;
            }
            if self.is_kind(j, Kind::Ident) && self.is_kind(j + 1, Kind::Assign) {
                let mut field = path.to_vec();
                field.push(self.word(j).to_string());
                j = self.value_at(j + 2, &field, conditional, true, slots);
            }
            // The rest of the field, or all of one that is not `name = ...`.
            while j < self.toks.len() && !closes(j) && !separates(j) {
                j += 1;
            }
        }
        j
    }

    fn slots(&self, root: &Root) -> Vec<Slot> {
        let mut slots = Vec::new();
        match root {
            Root::Table { open_tok, .. } => {
                self.walk_table(*open_tok, &[], false, &mut slots);
            }
            Root::Ident { name, .. } => {
                for i in 0..self.toks.len() {
                    if self.word(i) != name || self.brackets[i] != 0 {
                        continue;
                    }
                    if i > 0 && (self.is_punct(i - 1, b'.') || self.is_punct(i - 1, b':')) {
                        continue;
                    }
                    let mut path = Vec::new();
                    let mut j = i + 1;
                    while self.is_punct(j, b'.') && self.is_kind(j + 1, Kind::Ident) {
                        path.push(self.word(j + 1).to_string());
                        j += 2;
                    }
                    if !self.is_kind(j, Kind::Assign) {
                        continue;
                    }
                    let conditional = self.blocks[i] > 0;
                    if path.is_empty() {
                        // `local config = { ... }`: its fields are settings.
                        if self.is_punct(j + 1, b'{') {
                            self.walk_table(j + 1, &[], conditional, &mut slots);
                        }
                    } else {
                        self.value_at(j + 1, &path, conditional, false, &mut slots);
                    }
                }
            }
        }
        slots
    }
}

/// What the managed block holds: settings by path, with their right-hand
/// side as written, and any other line somebody put there.
#[derive(Default)]
struct Block {
    entries: BTreeMap<Vec<String>, String>,
    extra: Vec<String>,
}

/// `name.a.b = rhs` split into its path and right-hand side.
fn parse_assignment(line: &str) -> Option<(String, Vec<String>, String)> {
    let (lhs, rhs) = line.split_once('=')?;
    if rhs.starts_with('=') {
        return None;
    }
    let mut parts = lhs.trim().split('.');
    let name = parts.next()?.to_string();
    let path: Vec<String> = parts.map(str::to_string).collect();
    let is_name = |s: &str| {
        !s.is_empty()
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !s.starts_with(|c: char| c.is_ascii_digit())
    };
    if path.is_empty() || !is_name(&name) || !path.iter().all(|p| is_name(p)) {
        return None;
    }
    Some((name, path, rhs.trim().to_string()))
}

fn parse_block(src: &str, span: BlockSpan) -> Block {
    let mut block = Block::default();
    let mut lines = src[span.start..span.end].lines();
    lines.next(); // opening marker
    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with(BLOCK_CLOSE) {
            break;
        }
        if trimmed.is_empty() {
            continue;
        }
        match parse_assignment(trimmed) {
            Some((name, path, rhs)) => {
                // `x.a = x.a or {}` is scaffolding, written again on output.
                let guard = format!("{name}.{} or {{}}", path.join("."));
                if rhs != guard {
                    block.entries.insert(path, rhs);
                }
            }
            None => block.extra.push(line.to_string()),
        }
    }
    block
}

fn render_block(block: &Block, name: &str, eol: &str) -> String {
    let mut out = String::new();
    out.push_str(BLOCK_OPEN_LINE);
    out.push_str(eol);
    let mut guarded: Vec<&[String]> = Vec::new();
    for (path, rhs) in &block.entries {
        for depth in 1..path.len() {
            let parent = &path[..depth];
            if !guarded.contains(&parent) {
                guarded.push(parent);
                let parent = parent.join(".");
                out.push_str(&format!("{name}.{parent} = {name}.{parent} or {{}}{eol}"));
            }
        }
        out.push_str(&format!("{name}.{} = {rhs}{eol}", path.join(".")));
    }
    for line in &block.extra {
        out.push_str(line);
        out.push_str(eol);
    }
    out.push_str(BLOCK_CLOSE);
    out.push_str(eol);
    out
}

fn eol_of(src: &str) -> &'static str {
    if src.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// A name for the returned table that the file does not already use.
fn fresh_name(doc: &Doc) -> String {
    let used = |name: &str| (0..doc.toks.len()).any(|i| doc.word(i) == name);
    if !used("config") {
        return "config".to_string();
    }
    (1..)
        .map(|n| {
            if n == 1 {
                "tgz_config".to_string()
            } else {
                format!("tgz_config{n}")
            }
        })
        .find(|name| !used(name))
        .unwrap_or_else(|| "tgz_config".to_string())
}

/// The file with its managed block replaced by `block` (dropped when empty).
fn write_block(doc: &Doc, root: &Root, block: &Block) -> String {
    let src = doc.src;
    let eol = eol_of(src);
    let empty = block.entries.is_empty() && block.extra.is_empty();

    if let Some(span) = doc.block {
        // The block only ever sits before a `return <name>`.
        let name = match root {
            Root::Ident { name, .. } => name.clone(),
            Root::Table { .. } => fresh_name(doc),
        };
        let mut out = String::with_capacity(src.len() + 128);
        out.push_str(&src[..span.start]);
        if empty {
            // Take the blank line that followed the block with it.
            let rest = &src[span.end..];
            out.push_str(rest.strip_prefix(eol).unwrap_or(rest));
        } else {
            out.push_str(&render_block(block, &name, eol));
            out.push_str(&src[span.end..]);
        }
        return out;
    }
    if empty {
        return src.to_string();
    }

    match root {
        Root::Ident { name, return_start } => {
            let line_start = src[..*return_start].rfind('\n').map_or(0, |n| n + 1);
            // `return` sharing a line with other code: break before it.
            let at = if src[line_start..*return_start].trim().is_empty() {
                line_start
            } else {
                *return_start
            };
            let mut out = String::with_capacity(src.len() + 256);
            out.push_str(&src[..at]);
            if at != line_start {
                out.push_str(eol);
            }
            out.push_str(&render_block(block, name, eol));
            out.push_str(eol);
            out.push_str(&src[at..]);
            out
        }
        Root::Table {
            return_start,
            after,
            ..
        } => {
            // `return { ... }` becomes `local config = { ... }`, the block,
            // and `return config`.
            let name = fresh_name(doc);
            let mut out = String::with_capacity(src.len() + 256);
            out.push_str(&src[..*return_start]);
            out.push_str(&format!("local {name} ="));
            out.push_str(&src[return_start + "return".len()..*after]);
            out.push_str(eol);
            out.push_str(eol);
            out.push_str(&render_block(block, &name, eol));
            out.push_str(eol);
            out.push_str(&format!("return {name}"));
            let tail = &src[*after..];
            if tail.is_empty() {
                out.push_str(eol);
            } else {
                out.push_str(tail);
            }
            out
        }
    }
}

fn same_path(slot: &Slot, path: &[&str]) -> bool {
    slot.path.len() == path.len() && slot.path.iter().zip(path).all(|(a, b)| a == b)
}

/// Where `path` stands in `src`.
pub(crate) fn status(src: &str, path: &[&str]) -> Result<PathStatus, EditError> {
    let doc = Doc::new(src);
    let root = doc.root()?;
    if let Some(span) = doc.block {
        let owned: Vec<String> = path.iter().map(|s| s.to_string()).collect();
        if parse_block(src, span).entries.contains_key(&owned) {
            return Ok(PathStatus::Block);
        }
    }
    let slots = doc.slots(&root);
    let mut matching = slots.iter().filter(|s| same_path(s, path));
    Ok(match (matching.next(), matching.next()) {
        (None, _) => PathStatus::Absent,
        (Some(slot), None)
            if !slot.conditional && matches!(slot.kind, SlotKind::Literal | SlotKind::FontName) =>
        {
            PathStatus::InPlace {
                line: doc.line_of(slot.start),
            }
        }
        (Some(slot), _) => PathStatus::Locked {
            line: doc.line_of(slot.start),
        },
    })
}

/// `src` with `path` set to `value`: rewritten where the file sets it as a
/// literal, otherwise recorded in the managed block.
pub(crate) fn set_value(src: &str, path: &[&str], value: &LuaValue) -> Result<String, EditError> {
    let doc = Doc::new(src);
    let root = doc.root()?;
    let owned: Vec<String> = path.iter().map(|s| s.to_string()).collect();
    let mut block = doc
        .block
        .map(|span| parse_block(src, span))
        .unwrap_or_default();

    if !block.entries.contains_key(&owned) {
        let slots = doc.slots(&root);
        let matching: Vec<&Slot> = slots.iter().filter(|s| same_path(s, path)).collect();
        if let Some(first) = matching.first() {
            let locked = EditError::Locked {
                line: doc.line_of(first.start),
            };
            if matching.len() > 1 || first.conditional {
                return Err(locked);
            }
            let is_font = matches!(value, LuaValue::Font(_));
            let fits = match first.kind {
                SlotKind::Literal => !is_font,
                SlotKind::FontName => is_font,
                SlotKind::Table | SlotKind::Opaque => false,
            };
            if !fits {
                return Err(locked);
            }
            let old = &src[first.start..first.end];
            let quote = if old.starts_with('"') { '"' } else { '\'' };
            let mut out = String::with_capacity(src.len() + 16);
            out.push_str(&src[..first.start]);
            out.push_str(&value.literal(quote));
            out.push_str(&src[first.end..]);
            return Ok(out);
        }
    }

    block.entries.insert(owned, value.statement_rhs());
    Ok(write_block(&doc, &root, &block))
}

/// `src` without `path` in the managed block, or `None` when the block does
/// not hold it (a value the file sets itself is not removed).
pub(crate) fn unset_value(src: &str, path: &[&str]) -> Result<Option<String>, EditError> {
    let doc = Doc::new(src);
    let root = doc.root()?;
    let Some(span) = doc.block else {
        return Ok(None);
    };
    let mut block = parse_block(src, span);
    let owned: Vec<String> = path.iter().map(|s| s.to_string()).collect();
    if block.entries.remove(&owned).is_none() {
        return Ok(None);
    }
    Ok(Some(write_block(&doc, &root, &block)))
}

/// A config file for a machine that has none.
pub(crate) fn new_file() -> String {
    "local wezterm = require 'wezterm'\n\
     local config = wezterm.config_builder()\n\
     \n\
     return config\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE_STYLE: &str = r#"local wezterm = require 'wezterm'

return {
  -- return { font_size = 1 } in a comment must not confuse anything
  front_end = "WebGpu",
  font = wezterm.font("Cascadia Code", { weight = "Regular" }),
  font_size = 16.0, -- big
  harfbuzz_features = { "liga=0", "calt=0" },
  window_padding = {
    left = 12,
    right = 12,
  },
  rich_input = {
    enabled = true,
    dock_rows = 3,
  },
  keys = {
    { key = "Space", mods = "CTRL|SHIFT", action = wezterm.action.ToggleDockedInput },
  },
}
"#;

    const BUILDER_STYLE: &str = r#"local wezterm = require 'wezterm'
local config = wezterm.config_builder()

config.sidebar_position = 'Left'
config.font_size = is_laptop and 13 or 15
config.rich_input = { enabled = true, docked = false }
if wezterm.target_triple:find('windows') then
  config.default_prog = { 'pwsh.exe' }
  config.sidebar_width = 30
end

return config
"#;

    fn set(src: &str, path: &[&str], value: LuaValue) -> String {
        set_value(src, path, &value).unwrap()
    }

    #[test]
    fn a_literal_field_is_rewritten_where_it_stands() {
        let out = set(TABLE_STYLE, &["font_size"], LuaValue::Float(14.0));
        assert!(out.contains("  font_size = 14.0, -- big\n"));
        // Nothing else moved.
        assert_eq!(out.len(), TABLE_STYLE.len());
        assert!(!out.contains(BLOCK_OPEN));

        let out = set(TABLE_STYLE, &["rich_input", "dock_rows"], LuaValue::Int(5));
        assert!(out.contains("    dock_rows = 5,\n"));
        let out = set(TABLE_STYLE, &["window_padding", "left"], LuaValue::Int(0));
        assert!(out.contains("    left = 0,\n    right = 12,\n"));
    }

    #[test]
    fn a_string_keeps_its_quote_style() {
        let out = set(TABLE_STYLE, &["front_end"], LuaValue::Str("OpenGL".into()));
        assert!(out.contains("front_end = \"OpenGL\","));
        let out = set(
            BUILDER_STYLE,
            &["sidebar_position"],
            LuaValue::Str("Right".into()),
        );
        assert!(out.contains("config.sidebar_position = 'Right'\n"));
    }

    #[test]
    fn a_font_call_gets_a_new_family_and_keeps_its_options() {
        let out = set(
            TABLE_STYLE,
            &["font"],
            LuaValue::Font("JetBrains Mono".into()),
        );
        assert!(out.contains(r#"font = wezterm.font("JetBrains Mono", { weight = "Regular" }),"#));
    }

    #[test]
    fn a_new_setting_turns_a_returned_table_into_a_local_and_a_block() {
        let out = set(
            TABLE_STYLE,
            &["sidebar_footer_max_per_row"],
            LuaValue::Int(4),
        );
        assert!(out.contains("\nlocal config = {\n"));
        assert!(!out.contains("\nreturn {"));
        let tail = "}\n\n\
            -- >>> TGZTerminal settings (managed by the Settings tab, edit freely)\n\
            config.sidebar_footer_max_per_row = 4\n\
            -- <<< TGZTerminal settings\n\
            \n\
            return config\n";
        assert!(out.ends_with(tail), "{out}");

        // A second one joins the block; the file is now `return config`.
        let out = set(
            &out,
            &["diff_panel", "long_lines"],
            LuaValue::Str("Scroll".into()),
        );
        assert!(out.contains(
            "config.diff_panel = config.diff_panel or {}\n\
             config.diff_panel.long_lines = 'Scroll'\n\
             config.sidebar_footer_max_per_row = 4\n"
        ));
        assert_eq!(out.matches(BLOCK_OPEN).count(), 1);
        assert_eq!(out.matches("return config").count(), 1);

        // Changing it again edits the block entry.
        let out = set(&out, &["sidebar_footer_max_per_row"], LuaValue::Int(3));
        assert!(out.contains("config.sidebar_footer_max_per_row = 3\n"));
        assert!(!out.contains("= 4\n"));
    }

    #[test]
    fn the_block_goes_before_the_return_of_a_builder_file() {
        let out = set(BUILDER_STYLE, &["sidebar_auto_hide"], LuaValue::Bool(true));
        assert!(out.ends_with(
            "end\n\n\
             -- >>> TGZTerminal settings (managed by the Settings tab, edit freely)\n\
             config.sidebar_auto_hide = true\n\
             -- <<< TGZTerminal settings\n\
             \n\
             return config\n"
        ));
    }

    #[test]
    fn a_field_of_an_inline_table_is_rewritten_in_place() {
        let out = set(
            BUILDER_STYLE,
            &["rich_input", "docked"],
            LuaValue::Bool(true),
        );
        assert!(out.contains("config.rich_input = { enabled = true, docked = true }\n"));
    }

    #[test]
    fn code_is_locked_and_left_alone() {
        // Computed.
        assert_eq!(
            set_value(BUILDER_STYLE, &["font_size"], &LuaValue::Float(12.0)),
            Err(EditError::Locked { line: 5 })
        );
        // Inside an `if`.
        assert_eq!(
            set_value(BUILDER_STYLE, &["sidebar_width"], &LuaValue::Int(40)),
            Err(EditError::Locked { line: 9 })
        );
        // A function call that is not wezterm.font.
        assert_eq!(
            status(TABLE_STYLE, &["font"]),
            Ok(PathStatus::InPlace { line: 6 })
        );
        assert!(matches!(
            set_value(TABLE_STYLE, &["font"], &LuaValue::Str("x".into())),
            Err(EditError::Locked { .. })
        ));
        // A whole table where a scalar is wanted.
        assert!(matches!(
            set_value(TABLE_STYLE, &["rich_input"], &LuaValue::Bool(false)),
            Err(EditError::Locked { .. })
        ));
        // Set twice.
        let twice = "local c = {}\nc.font_size = 1\nc.font_size = 2\nreturn c\n";
        assert_eq!(
            set_value(twice, &["font_size"], &LuaValue::Int(3)),
            Err(EditError::Locked { line: 2 })
        );
    }

    #[test]
    fn status_tells_the_four_cases_apart() {
        assert_eq!(
            status(BUILDER_STYLE, &["tab_max_width"]),
            Ok(PathStatus::Absent)
        );
        assert_eq!(
            status(BUILDER_STYLE, &["sidebar_position"]),
            Ok(PathStatus::InPlace { line: 4 })
        );
        assert_eq!(
            status(BUILDER_STYLE, &["font_size"]),
            Ok(PathStatus::Locked { line: 5 })
        );
        let out = set(BUILDER_STYLE, &["tab_max_width"], LuaValue::Int(20));
        assert_eq!(status(&out, &["tab_max_width"]), Ok(PathStatus::Block));
    }

    #[test]
    fn a_block_entry_wins_over_a_literal_earlier_in_the_file() {
        let src = "local config = {}\n\
                   config.font_size = 12\n\
                   -- >>> TGZTerminal settings (x)\n\
                   config.font_size = 14\n\
                   -- <<< TGZTerminal settings\n\
                   return config\n";
        let out = set(src, &["font_size"], LuaValue::Int(15));
        assert!(out.contains("config.font_size = 12\n"));
        assert!(out.contains("config.font_size = 15\n"));
        assert!(!out.contains("= 14"));
    }

    #[test]
    fn unset_removes_from_the_block_and_drops_an_empty_one() {
        let one = set(BUILDER_STYLE, &["sidebar_auto_hide"], LuaValue::Bool(true));
        let two = set(&one, &["tab_max_width"], LuaValue::Int(20));
        let back = unset_value(&two, &["tab_max_width"]).unwrap().unwrap();
        assert_eq!(back, one);
        let none = unset_value(&back, &["sidebar_auto_hide"]).unwrap().unwrap();
        assert_eq!(none, BUILDER_STYLE);
        // Not in the block: nothing to remove, the file's own line stays.
        assert_eq!(unset_value(BUILDER_STYLE, &["sidebar_position"]), Ok(None));
    }

    #[test]
    fn lines_somebody_added_to_the_block_survive() {
        let src = "local config = {}\n\
                   -- >>> TGZTerminal settings (x)\n\
                   -- my note\n\
                   config.a = 1\n\
                   config.keys = { }\n\
                   -- <<< TGZTerminal settings\n\
                   return config\n";
        let out = set(src, &["b"], LuaValue::Int(2));
        assert!(out.contains("-- my note\n"));
        assert!(out.contains("config.a = 1\n"));
        assert!(out.contains("config.b = 2\n"));
        assert!(out.contains("config.keys = { }\n"));
    }

    #[test]
    fn strings_and_comments_are_not_code() {
        let src = "local config = {}\n\
                   --[[ config.font_size = 1\n\
                   return nothing ]]\n\
                   local s = [==[ config.font_size = 2 ]==]\n\
                   local t = 'return x' .. \"config.font_size = 3\"\n\
                   config.font_size = 4\n\
                   return config -- done\n";
        assert_eq!(
            status(src, &["font_size"]),
            Ok(PathStatus::InPlace { line: 6 })
        );
        let out = set(src, &["font_size"], LuaValue::Int(5));
        assert!(out.contains("config.font_size = 5\nreturn config -- done\n"));
    }

    #[test]
    fn commas_inside_a_function_do_not_split_a_field() {
        let src = "return {\n  \
                   on_x = function() return 1, 2 end,\n  \
                   font_size = 10,\n  \
                   f = wezterm.format({ a = 1, font_size = 99 }),\n\
                   }\n";
        assert_eq!(
            status(src, &["font_size"]),
            Ok(PathStatus::InPlace { line: 3 })
        );
    }

    #[test]
    fn line_endings_negative_numbers_and_escapes() {
        let src = "local config = {}\r\nconfig.x = -3\r\nreturn config\r\n";
        let out = set(src, &["x"], LuaValue::Int(7));
        assert!(out.contains("config.x = 7\r\n"));
        let out = set(src, &["y"], LuaValue::Str("it's \\ here".into()));
        assert!(out.contains("config.y = 'it\\'s \\\\ here'\r\n"));
        assert!(!out.replace("\r\n", "").contains('\n'));
        assert_eq!(LuaValue::Float(1.25).literal('\''), "1.25");
        assert_eq!(LuaValue::Float(2.0).literal('\''), "2.0");
    }

    #[test]
    fn a_name_in_use_is_not_reused_for_the_returned_table() {
        let src = "local config = require 'mine'\nreturn { font_size = 1 }\n";
        let out = set(src, &["tab_max_width"], LuaValue::Int(9));
        assert!(out.contains("local tgz_config = { font_size = 1 }"));
        assert!(out.ends_with("return tgz_config\n"));
    }

    #[test]
    fn a_file_that_returns_something_else_is_refused() {
        for src in [
            "return require('other')\n",
            "local c = {}\n",
            "return a, b\n",
            "return { a = 1 }, 2\n",
        ] {
            assert_eq!(
                set_value(src, &["x"], &LuaValue::Int(1)),
                Err(EditError::UnsupportedShape),
                "{src}"
            );
        }
    }

    #[test]
    fn a_new_file_takes_a_setting() {
        let out = set(&new_file(), &["sidebar_auto_hide"], LuaValue::Bool(true));
        assert!(out.contains("config.sidebar_auto_hide = true\n"));
        assert!(out.ends_with("\nreturn config\n"));
    }

    #[test]
    fn a_key_three_tables_deep_gets_a_guard_for_each_table() {
        let out = set(
            "local config = {}\nreturn config\n",
            &["sidebar_buttons", "custom1", "label"],
            LuaValue::Str("Deploy".into()),
        );
        assert!(
            out.contains(
                "config.sidebar_buttons = config.sidebar_buttons or {}\n\
                 config.sidebar_buttons.custom1 = config.sidebar_buttons.custom1 or {}\n\
                 config.sidebar_buttons.custom1.label = 'Deploy'\n"
            ),
            "{out}"
        );
        let out = set(
            &out,
            &["sidebar_buttons", "custom1", "command"],
            LuaValue::Str("make deploy".into()),
        );
        assert_eq!(out.matches("custom1 or {}").count(), 1, "{out}");
        assert_eq!(
            status(&out, &["sidebar_buttons", "custom1", "command"]),
            Ok(PathStatus::Block)
        );
    }
}
