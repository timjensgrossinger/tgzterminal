//! Which files an agent session touched, read from its transcript.
//!
//! The Changes panel shows the whole working copy against its base, which in
//! a long-lived checkout includes other people's and older changes. Narrowing
//! it to "what this session did" needs the session's own record — and an
//! agent's edit tools are only half of it. The risky changes often go through
//! the shell: a Python script rewriting a file byte for byte to keep its line
//! endings and code page, a `patch` applied to a moved file, an `svn mv`. None
//! of those appear as an edit record, so the shell commands are read too.
//!
//! Two strengths come out:
//! - [`Touch::Edited`]: an edit tool named the file, or a command that
//!   certainly changes it (`svn mv`, `git rm`, `sed -i`, a `>` redirect, an
//!   `apply_patch` header);
//! - [`Touch::Mentioned`]: the file's path appears in a shell command. That
//!   includes reads (`cat`), so it is shown apart from real edits.
//!
//! Paths are kept as the session wrote them, made absolute against the
//! directory each command ran in. Matching them to a working copy is the
//! caller's job: only it knows which filesystem view the working copy is in.
//!
//! Pure apart from the file read; no GUI, mux or terminal types.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Most of a transcript read in one go. A week-long session can pass this;
/// its newest records are the ones that matter, so a longer one is read from
/// this far before its end, and later reads only append.
const MAX_TRANSCRIPT_BYTES: u64 = 64 * 1024 * 1024;
/// Transcripts whose read state is kept; one per panel-visible agent is the
/// realistic need.
const CACHE_CAP: usize = 32;

/// How a session touched a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Touch {
    /// Its path appears in a shell command.
    Mentioned,
    /// An edit tool or a known writing command changed it.
    Edited,
}

/// Every file a session touched, keyed by [`path_key`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TouchedPaths {
    paths: HashMap<String, Touch>,
    /// Each path as the session first spelled it. The key is folded (case,
    /// separators) for matching; a vendor's own records, such as Claude's
    /// file-history backups, are named after the spelling.
    spelled: HashMap<String, String>,
}

impl TouchedPaths {
    fn add(&mut self, path: &str, touch: Touch) {
        let key = path_key(path);
        if key.is_empty() {
            return;
        }
        // An edit tool's spelling is the one its backups are named after,
        // so it replaces a spelling seen first in a shell command.
        let edited_first_time = touch == Touch::Edited
            && self
                .paths
                .get(&key)
                .map_or(true, |seen| *seen < Touch::Edited);
        if edited_first_time || !self.spelled.contains_key(&key) {
            self.spelled.insert(key.clone(), path.trim().to_string());
        }
        let entry = self.paths.entry(key).or_insert(touch);
        *entry = (*entry).max(touch);
    }

    /// Every touched path: `(key, spelling, touch)`.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str, Touch)> {
        let spellings = &self.spelled;
        self.paths.iter().map(move |(key, touch)| {
            let spelled = spellings.get(key).map_or(key.as_str(), String::as_str);
            (key.as_str(), spelled, *touch)
        })
    }

    /// How `path` (absolute, in the session's own filesystem view) was
    /// touched, if at all.
    pub fn touch_of(&self, path: &str) -> Option<Touch> {
        self.paths.get(&path_key(path)).copied()
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// Normal form for comparing paths: `/` separators, `.` and `..` resolved
/// lexically, no trailing separator, and lower case for a Windows path (drive
/// letter or UNC), where case does not tell two files apart.
pub fn path_key(path: &str) -> String {
    let path = path.trim().replace('\\', "/");
    let (prefix, rest) = if path.starts_with("//") {
        ("//", &path[2..])
    } else if path.starts_with('/') {
        ("/", &path[1..])
    } else {
        ("", path.as_str())
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let joined = format!("{prefix}{}", parts.join("/"));
    let bytes = joined.as_bytes();
    let windows =
        prefix == "//" || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':');
    if windows {
        joined.to_ascii_lowercase()
    } else {
        joined
    }
}

fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with('\\')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// `path` made absolute against `cwd`, or `None` when neither says where it
/// is (a relative path with no directory, or `~`, whose home is not known
/// here).
fn resolve(cwd: Option<&str>, path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() || path.starts_with('~') || path.starts_with('$') {
        return None;
    }
    if is_absolute(path) {
        return Some(path.to_string());
    }
    let cwd = cwd.filter(|cwd| is_absolute(cwd))?;
    let sep = if cwd.contains('\\') && !cwd.contains('/') {
        '\\'
    } else {
        '/'
    };
    Some(format!("{}{sep}{path}", cwd.trim_end_matches(['/', '\\'])))
}

// ---------------------------------------------------------------------------
// Transcript records
// ---------------------------------------------------------------------------

/// Running state over one transcript, carried between incremental reads.
#[derive(Clone, Debug, Default)]
struct ScanState {
    touched: TouchedPaths,
    /// The directory the session was last seen in (Codex records it once per
    /// turn, not per call).
    cwd: Option<String>,
}

/// Fold one JSONL record into `state`. Claude and Codex records are told
/// apart by shape; anything else is ignored.
fn scan_record(line: &str, state: &mut ScanState) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    let str_field = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(|field| field.as_str())
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    // Claude: every record carries the directory it was written in.
    if let Some(cwd) = str_field(&value, "cwd") {
        state.cwd = Some(cwd);
    }
    // Codex: the directory arrives in the session header and each turn's
    // context.
    if let Some(payload) = value.get("payload") {
        let kind = value.get("type").and_then(|kind| kind.as_str());
        if matches!(kind, Some("session_meta" | "turn_context")) {
            if let Some(cwd) = str_field(payload, "cwd") {
                state.cwd = Some(cwd);
            }
        }
        scan_codex_call(payload, state);
        return;
    }
    // Claude assistant turn: tool_use blocks in the message content.
    let Some(blocks) = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_array())
    else {
        return;
    };
    for block in blocks {
        if block.get("type").and_then(|kind| kind.as_str()) != Some("tool_use") {
            continue;
        }
        let name = block
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or("");
        let input = block.get("input").unwrap_or(&serde_json::Value::Null);
        match name {
            "Edit" | "Write" | "MultiEdit" => {
                if let Some(path) = str_field(input, "file_path") {
                    if let Some(path) = resolve(state.cwd.as_deref(), &path) {
                        state.touched.add(&path, Touch::Edited);
                    }
                }
            }
            "NotebookEdit" => {
                if let Some(path) =
                    str_field(input, "notebook_path").or_else(|| str_field(input, "file_path"))
                {
                    if let Some(path) = resolve(state.cwd.as_deref(), &path) {
                        state.touched.add(&path, Touch::Edited);
                    }
                }
            }
            "Bash" | "PowerShell" => {
                if let Some(command) = str_field(input, "command") {
                    let cwd = state.cwd.clone();
                    scan_shell(&command, cwd.as_deref(), &mut state.touched);
                }
            }
            _ => {}
        }
    }
}

/// A Codex `response_item` payload: a shell call or an `apply_patch`.
fn scan_codex_call(payload: &serde_json::Value, state: &mut ScanState) {
    let kind = payload.get("type").and_then(|kind| kind.as_str());
    if !matches!(
        kind,
        Some("function_call" | "custom_tool_call" | "local_shell_call")
    ) {
        return;
    }
    let name = payload
        .get("name")
        .and_then(|name| name.as_str())
        .unwrap_or("");
    // `arguments` is a JSON document inside a string; `input` is raw text
    // (apply_patch); `action` is the local shell's own shape.
    let arguments = payload
        .get("arguments")
        .and_then(|args| args.as_str())
        .and_then(|args| serde_json::from_str::<serde_json::Value>(args).ok())
        .or_else(|| payload.get("action").cloned())
        .unwrap_or(serde_json::Value::Null);
    let workdir = arguments
        .get("workdir")
        .and_then(|dir| dir.as_str())
        .map(str::to_string)
        .or_else(|| state.cwd.clone());
    if let Some(patch) = payload.get("input").and_then(|input| input.as_str()) {
        if name == "apply_patch" || patch.contains("*** Begin Patch") {
            scan_apply_patch(patch, workdir.as_deref(), &mut state.touched);
            return;
        }
    }
    let command = match arguments.get("cmd").or_else(|| arguments.get("command")) {
        Some(serde_json::Value::String(command)) => Some(command.clone()),
        // `["bash", "-lc", "<script>"]`: the script is what ran.
        Some(serde_json::Value::Array(argv)) => {
            let argv: Vec<&str> = argv.iter().filter_map(|arg| arg.as_str()).collect();
            match argv.as_slice() {
                [_, flag, script] if flag.starts_with('-') && flag.contains('c') => {
                    Some(script.to_string())
                }
                _ => shlex::try_join(argv.iter().copied()).ok(),
            }
        }
        _ => None,
    };
    if let Some(command) = command {
        // A patch handed to `apply_patch` through the shell (older Codex).
        if command.contains("*** Begin Patch") {
            scan_apply_patch(&command, workdir.as_deref(), &mut state.touched);
        } else {
            scan_shell(&command, workdir.as_deref(), &mut state.touched);
        }
    }
}

/// `*** Add File: p`, `*** Update File: p`, `*** Delete File: p` and
/// `*** Move to: p` headers of a Codex patch.
fn scan_apply_patch(patch: &str, cwd: Option<&str>, touched: &mut TouchedPaths) {
    for line in patch.lines() {
        let path = [
            "*** Add File:",
            "*** Update File:",
            "*** Delete File:",
            "*** Move to:",
        ]
        .iter()
        .find_map(|header| line.strip_prefix(header));
        if let Some(path) = path.and_then(|path| resolve(cwd, path.trim())) {
            touched.add(&path, Touch::Edited);
        }
    }
}

// ---------------------------------------------------------------------------
// Shell commands
// ---------------------------------------------------------------------------

/// Commands whose non-option arguments are files they change.
const WRITING_COMMANDS: &[&str] = &["mv", "cp", "rm", "touch", "tee", "truncate", "unlink"];
/// `svn`/`git` subcommands whose non-option arguments are files they change.
const WRITING_SUBCOMMANDS: &[&str] = &[
    "mv", "move", "rename", "ren", "cp", "copy", "rm", "del", "delete", "remove", "add", "revert",
    "restore", "checkout",
];

/// Collect the files a shell command touches. `cwd` is where it started; a
/// leading `cd dir` moves it for the commands after it.
fn scan_shell(command: &str, cwd: Option<&str>, touched: &mut TouchedPaths) {
    let mut cwd = cwd.map(str::to_string);
    for segment in split_segments(command) {
        let words = shell_words(segment);
        let Some(first) = words.first() else {
            continue;
        };
        if first == "cd" || first == "pushd" {
            if let Some(dir) = words.get(1).and_then(|dir| resolve(cwd.as_deref(), dir)) {
                cwd = Some(dir);
            }
            continue;
        }
        scan_words(&words, cwd.as_deref(), touched);
        // Whatever else the segment holds — a heredoc body, a `python -c`
        // program, a quoted list — may name files too.
        for candidate in path_like_tokens(segment) {
            if let Some(path) = resolve(cwd.as_deref(), &candidate) {
                touched.add(&path, Touch::Mentioned);
            }
        }
    }
}

/// The command's own words: who runs, which files a writing command names,
/// where output is redirected.
fn scan_words(words: &[String], cwd: Option<&str>, touched: &mut TouchedPaths) {
    let mut edit = |word: &str| {
        if let Some(path) = resolve(cwd, word) {
            touched.add(&path, Touch::Edited);
        }
    };
    // Redirect targets, wherever they are: `> file`, `>> file`, `>file`.
    for (idx, word) in words.iter().enumerate() {
        if let Some(target) = word.strip_prefix(">>").or_else(|| word.strip_prefix('>')) {
            if !target.is_empty() && !target.starts_with('&') {
                edit(target);
            } else if target.is_empty() {
                if let Some(next) = words.get(idx + 1) {
                    edit(next);
                }
            }
        }
    }
    let program = words[0].rsplit(['/', '\\']).next().unwrap_or("");
    let program = program.strip_suffix(".exe").unwrap_or(program);
    let operands = |from: usize| {
        words[from..]
            .iter()
            .take_while(|word| !word.starts_with('>') && !word.starts_with('<'))
            .filter(|word| !word.starts_with('-'))
            .cloned()
            .collect::<Vec<_>>()
    };
    match program {
        "svn" | "git" => {
            // Skip global options to the subcommand.
            let Some(sub_at) = words
                .iter()
                .skip(1)
                .position(|word| !word.starts_with('-'))
                .map(|at| at + 1)
            else {
                return;
            };
            if WRITING_SUBCOMMANDS.contains(&words[sub_at].as_str()) {
                for operand in operands(sub_at + 1) {
                    edit(&operand);
                }
            }
        }
        "sed" | "perl" if words.iter().any(|word| word.starts_with("-i")) => {
            // The last operand is the file; earlier ones are the script.
            if let Some(file) = operands(1).last() {
                edit(file);
            }
        }
        "patch" => {
            // `patch file < diff`: the named file is the target; `-i diff` /
            // `< diff` is the patch itself and only mentioned.
            let mut skip_next = false;
            for word in &words[1..] {
                if skip_next {
                    skip_next = false;
                    continue;
                }
                if matches!(word.as_str(), "-i" | "-d" | "-p" | "-o" | "<") {
                    skip_next = true;
                    continue;
                }
                if word.starts_with('-') || word.starts_with('<') || word.starts_with('>') {
                    continue;
                }
                edit(word);
            }
        }
        program if WRITING_COMMANDS.contains(&program) => {
            for operand in operands(1) {
                edit(&operand);
            }
        }
        _ => {}
    }
}

/// Split a script into simple commands at `&&`, `||`, `;`, `|` and newlines,
/// outside quotes. A heredoc's body stays with the command that opened it,
/// so a `cd` inside it does not move the directory of what follows.
fn split_segments(script: &str) -> Vec<&str> {
    let bytes = script.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quote: Option<u8> = None;
    let mut heredoc: Option<String> = None;
    let mut idx = 0;
    while idx < bytes.len() {
        let byte = bytes[idx];
        if let Some(delim) = &heredoc {
            // Inside a heredoc body: it ends at a line that is the delimiter.
            if byte == b'\n' {
                let line_start = idx + 1;
                let line_end = script[line_start..]
                    .find('\n')
                    .map_or(bytes.len(), |end| line_start + end);
                if script[line_start..line_end].trim() == delim {
                    heredoc = None;
                    segments.push(&script[start..line_end]);
                    start = line_end;
                    idx = line_end;
                    continue;
                }
            }
            idx += 1;
            continue;
        }
        match quote {
            Some(open) => {
                if byte == open {
                    quote = None;
                } else if byte == b'\\' && open == b'"' {
                    idx += 1;
                }
            }
            None => match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'\\' => idx += 1,
                b'<' if bytes.get(idx + 1) == Some(&b'<') => {
                    let rest = script[idx + 2..].trim_start_matches(['-', ' ']);
                    let delim: String = rest
                        .trim_start_matches(['\'', '"'])
                        .chars()
                        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                        .collect();
                    // A word, not a number: `$((1<<3))` is a shift.
                    if delim.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_') {
                        heredoc = Some(delim);
                    }
                    idx += 1;
                }
                b';' | b'\n' | b'|' | b'&' => {
                    segments.push(&script[start..idx]);
                    // `&&` / `||` are one separator.
                    if matches!(byte, b'|' | b'&') && bytes.get(idx + 1) == Some(&byte) {
                        idx += 1;
                    }
                    start = idx + 1;
                }
                _ => {}
            },
        }
        idx += 1;
    }
    segments.push(&script[start.min(bytes.len())..]);
    segments
        .into_iter()
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// A simple command's words, quotes removed. Falls back to splitting on
/// whitespace when the quoting does not balance (a heredoc body, for one).
fn shell_words(segment: &str) -> Vec<String> {
    let first_line = segment.lines().next().unwrap_or("");
    shlex::split(first_line).unwrap_or_else(|| {
        first_line
            .split_whitespace()
            .map(|word| word.trim_matches(['\'', '"']).to_string())
            .collect()
    })
}

/// Every token in `text` that looks like a file path: it has a separator or a
/// short extension, and only characters paths are made of. URLs, options,
/// numbers and version strings are left out.
fn path_like_tokens(text: &str) -> Vec<String> {
    let is_separator = |ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '"' | '\''
                    | '`'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | ','
                    | ';'
                    | '<'
                    | '>'
                    | '|'
                    | '&'
                    | '='
                    | '*'
                    | '?'
                    | '!'
            )
    };
    let mut out = Vec::new();
    for token in text.split(is_separator) {
        let token = token.trim_end_matches(['.', ':']);
        if token.len() < 3 || token.starts_with('-') || token.contains("://") {
            continue;
        }
        if !token.chars().all(|ch| {
            ch.is_alphanumeric()
                || matches!(
                    ch,
                    '/' | '\\' | '.' | '_' | '-' | '+' | '@' | ':' | '~' | '$'
                )
        }) {
            continue;
        }
        let name = token.rsplit(['/', '\\']).next().unwrap_or(token);
        let has_extension = name.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty()
                && (1..=10).contains(&ext.len())
                && ext.chars().all(|ch| ch.is_ascii_alphanumeric())
                && ext.chars().any(|ch| ch.is_ascii_alphabetic())
        });
        let has_separator = token.contains('/') || token.contains('\\');
        if has_extension || (has_separator && !name.is_empty()) {
            out.push(token.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Reading a transcript
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct CacheEntry {
    /// Bytes consumed so far: always just past a complete line.
    offset: u64,
    state: ScanState,
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, CacheEntry>>> = LazyLock::new(Default::default);

/// Every file the session behind `transcript` touched so far.
///
/// Incremental: a later call reads only what was appended since. A file that
/// shrank was rewritten and is read again from the start. One read covers at
/// most [`MAX_TRANSCRIPT_BYTES`]; a longer stretch is read from that far
/// before the end, so the oldest records are the ones left out.
///
/// Blocking file I/O: call it off the GUI thread.
pub fn touched_by(transcript: &Path) -> std::io::Result<TouchedPaths> {
    let len = std::fs::metadata(transcript)?.len();
    let mut entry = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(transcript)
        .cloned()
        .unwrap_or_default();
    if entry.offset > len {
        entry = CacheEntry::default();
    }
    // Past the cap, keep the newest records: what the agent is doing now is
    // what the filter is for.
    let state = &mut entry.state;
    let skipped = super::incremental::read_appended(
        transcript,
        len,
        &mut entry.offset,
        MAX_TRANSCRIPT_BYTES,
        &mut |line| scan_record(line, state),
    )?;
    if skipped {
        log::debug!(
            "touched files: {} is {len} bytes; reading its last {MAX_TRANSCRIPT_BYTES}",
            transcript.display()
        );
    }
    let touched = entry.state.touched.clone();
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.len() >= CACHE_CAP && !cache.contains_key(transcript) {
        cache.clear();
    }
    cache.insert(transcript.to_path_buf(), entry);
    Ok(touched)
}

/// The set of paths in `touched`, for tests.
#[cfg(test)]
fn keys(touched: &TouchedPaths) -> std::collections::HashSet<(String, Touch)> {
    touched
        .paths
        .iter()
        .map(|(path, touch)| (path.clone(), *touch))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_tool(cwd: &str, name: &str, input: serde_json::Value) -> String {
        serde_json::json!({
            "type": "assistant",
            "cwd": cwd,
            "message": { "content": [{ "type": "tool_use", "name": name, "input": input }] },
        })
        .to_string()
    }

    fn scan_lines(lines: &[String]) -> TouchedPaths {
        let mut state = ScanState::default();
        for line in lines {
            scan_record(line, &mut state);
        }
        state.touched
    }

    fn set(entries: &[(&str, Touch)]) -> std::collections::HashSet<(String, Touch)> {
        entries
            .iter()
            .map(|(path, touch)| (path.to_string(), *touch))
            .collect()
    }

    #[test]
    fn edit_tools_name_their_files() {
        let touched = scan_lines(&[
            claude_tool(
                "/ws",
                "Edit",
                serde_json::json!({ "file_path": "/ws/a/A.java" }),
            ),
            claude_tool(
                "/ws",
                "Write",
                serde_json::json!({ "file_path": "/ws/a/New.java" }),
            ),
            claude_tool(
                "/ws",
                "MultiEdit",
                serde_json::json!({ "file_path": "/ws/b/B.java" }),
            ),
            claude_tool(
                "/ws",
                "NotebookEdit",
                serde_json::json!({ "notebook_path": "/ws/n.ipynb" }),
            ),
            claude_tool(
                "/ws",
                "Read",
                serde_json::json!({ "file_path": "/ws/ignored.txt" }),
            ),
        ]);
        assert_eq!(
            keys(&touched),
            set(&[
                ("/ws/a/A.java", Touch::Edited),
                ("/ws/a/New.java", Touch::Edited),
                ("/ws/b/B.java", Touch::Edited),
                ("/ws/n.ipynb", Touch::Edited),
            ])
        );
    }

    #[test]
    fn svn_mv_and_cd_then_patch() {
        let touched = scan_lines(&[
            claude_tool(
                "/ws/CDP4JClient",
                "Bash",
                serde_json::json!({ "command":
                    "svn mv src/DocumentSupressorController.java src/PurchaseOrderHeadController.java" }),
            ),
            claude_tool(
                "/ws",
                "Bash",
                serde_json::json!({ "command":
                    "cd ServiceLayerFile/purchaseorder && patch -p0 ServiceLayerPartPurchaseorderBean.java < /tmp/x.diff" }),
            ),
        ]);
        let keys = keys(&touched);
        assert!(keys.contains(&(
            "/ws/CDP4JClient/src/DocumentSupressorController.java".into(),
            Touch::Edited
        )));
        assert!(keys.contains(&(
            "/ws/CDP4JClient/src/PurchaseOrderHeadController.java".into(),
            Touch::Edited
        )));
        assert!(keys.contains(&(
            "/ws/ServiceLayerFile/purchaseorder/ServiceLayerPartPurchaseorderBean.java".into(),
            Touch::Edited
        )));
        // The patch file is mentioned, not edited.
        assert!(keys.contains(&("/tmp/x.diff".into(), Touch::Mentioned)));
    }

    #[test]
    fn a_python_heredoc_names_the_files_it_rewrites() {
        let script = "python3 - <<'EOF'\n\
            p = 'src/main/ControllerMap.java'\n\
            data = open(p, 'rb').read()\n\
            open(\"texts.properties\", \"wb\").write(data.replace(b'a', b'b'))\n\
            EOF\n\
            echo done";
        let touched = scan_lines(&[claude_tool(
            "/ws/CDP4JClient",
            "Bash",
            serde_json::json!({ "command": script }),
        )]);
        assert_eq!(
            touched.touch_of("/ws/CDP4JClient/src/main/ControllerMap.java"),
            Some(Touch::Mentioned)
        );
        assert_eq!(
            touched.touch_of("/ws/CDP4JClient/texts.properties"),
            Some(Touch::Mentioned)
        );
        assert_eq!(touched.touch_of("/ws/CDP4JClient/done"), None);
    }

    #[test]
    fn redirects_and_in_place_edits_are_edits() {
        let touched = scan_lines(&[claude_tool(
            "/r",
            "Bash",
            serde_json::json!({ "command":
                "sed -i 's/a/b/' conf/app.yml; echo x > out.txt && cat in.txt >> log/all.log" }),
        )]);
        assert_eq!(touched.touch_of("/r/conf/app.yml"), Some(Touch::Edited));
        assert_eq!(touched.touch_of("/r/out.txt"), Some(Touch::Edited));
        assert_eq!(touched.touch_of("/r/log/all.log"), Some(Touch::Edited));
        assert_eq!(touched.touch_of("/r/in.txt"), Some(Touch::Mentioned));
    }

    #[test]
    fn codex_patches_and_shell_calls() {
        let meta = serde_json::json!({
            "type": "session_meta", "payload": { "cwd": "/repo" }
        })
        .to_string();
        let patch = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-a\n+b\n*** Add File: src/new.rs\n+x\n*** Delete File: old.rs\n*** End Patch",
            },
        })
        .to_string();
        let exec = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "arguments": serde_json::json!({ "cmd": "git mv a.txt b.txt", "workdir": "/repo/sub" }).to_string(),
            },
        })
        .to_string();
        let shell = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "shell",
                "arguments": serde_json::json!({ "command": ["bash", "-lc", "rm build/out.o"] }).to_string(),
            },
        })
        .to_string();
        let touched = scan_lines(&[meta, patch, exec, shell]);
        for path in [
            "/repo/src/lib.rs",
            "/repo/src/new.rs",
            "/repo/old.rs",
            "/repo/sub/a.txt",
            "/repo/sub/b.txt",
            "/repo/build/out.o",
        ] {
            assert_eq!(touched.touch_of(path), Some(Touch::Edited), "{path}");
        }
    }

    #[test]
    fn a_shift_is_not_a_heredoc() {
        let touched = scan_lines(&[claude_tool(
            "/r",
            "Bash",
            serde_json::json!({ "command": "echo $((1<<3)) && rm build/out.o" }),
        )]);
        assert_eq!(touched.touch_of("/r/build/out.o"), Some(Touch::Edited));
    }

    #[test]
    fn a_bare_word_or_url_is_not_a_path() {
        let touched = scan_lines(&[claude_tool(
            "/r",
            "Bash",
            serde_json::json!({ "command": "curl -s https://example.com/a.json | jq .items; ls -la; echo 1.2.3" }),
        )]);
        assert!(touched.is_empty(), "{:?}", keys(&touched));
    }

    #[test]
    fn windows_paths_compare_without_case() {
        let touched = scan_lines(&[claude_tool(
            "C:\\ws",
            "Edit",
            serde_json::json!({ "file_path": "C:\\ws\\Mod\\A.java" }),
        )]);
        assert_eq!(touched.touch_of("c:/WS/mod/a.java"), Some(Touch::Edited));
        assert_eq!(path_key("/a/./b/../c/"), "/a/c");
    }

    #[test]
    fn reads_are_incremental_and_wait_for_whole_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.jsonl");
        let first = claude_tool(
            "/r",
            "Edit",
            serde_json::json!({ "file_path": "/r/one.rs" }),
        );
        let second = claude_tool(
            "/r",
            "Edit",
            serde_json::json!({ "file_path": "/r/two.rs" }),
        );
        // The second record is still being written: no newline yet.
        std::fs::write(&path, format!("{first}\n{}", &second[..10])).unwrap();
        let touched = touched_by(&path).unwrap();
        assert_eq!(touched.len(), 1);
        std::fs::write(&path, format!("{first}\n{second}\n")).unwrap();
        let touched = touched_by(&path).unwrap();
        assert_eq!(touched.touch_of("/r/two.rs"), Some(Touch::Edited));
        assert_eq!(touched.len(), 2);
        // Rewritten shorter: read again from the start.
        std::fs::write(&path, format!("{second}\n")).unwrap();
        assert_eq!(touched_by(&path).unwrap().len(), 1);
    }
}
