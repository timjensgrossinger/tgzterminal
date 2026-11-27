//! Ask panel window glue (TGZTerminal, experimental `assist.ask`).
//!
//! CMD+I opens a card over the bottom of the active shell pane. A question
//! (or Enter on an empty field: "why did the last command fail?") is sent
//! with the pane's recent command blocks, directory and selection to the
//! configured CLI — `claude -p` by default — on a worker thread. Its answer
//! streams into the card; the commands it suggests are listed with a risk
//! label, and the user inserts one at the prompt (Enter) or runs it
//! (CMD+Enter; anything that deletes asks once more). Nothing runs on its
//! own and nothing is typed into an agent pane.
//!
//! The pure parts are in `crate::assist`; painting, keys and clicks are in
//! `render/sidebar/assist.rs`.

use crate::assist::context::{self, ContextInput, Turn};
use crate::assist::provider::{self, RunOptions, INSTRUCTIONS};
use crate::assist::risk::{self, Risk};
use crate::assist::stream::{parse_claude_line, AssistEvent};
use crate::assist::suggest;
use crate::termwindow::render::sidebar::PaneToolbeltKind;
use crate::termwindow::{TermWindow, TermWindowNotif};
use config::AssistStreamFormat;
use mux::pane::{Pane, PaneId};
use mux::Mux;
use std::cell::RefCell;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use window::WindowOps;

/// Rows read from the bottom of the pane to find command blocks.
const SCAN_ROWS: usize = 4000;
/// stderr kept for an error message.
const STDERR_KEEP: usize = 4000;

/// What a click on the card hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistHit {
    /// Anywhere on the card that is not a control: swallows the click.
    Body,
    Suggestion(usize),
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Phase {
    /// Waiting for a question.
    Editing,
    Running {
        started: Instant,
    },
    Done,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Looked {
    pub id: String,
    pub label: String,
    pub ok: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Suggestion {
    pub command: String,
    pub risk: Risk,
}

pub(crate) struct AssistCard {
    pub pane_id: PaneId,
    /// The provider's display name ("Claude", "Codex", ...).
    pub provider: String,
    pub input: String,
    pub phase: Phase,
    /// What the next (or the running) question sends.
    pub chips: Vec<String>,
    pub question: String,
    pub answer: String,
    pub thinking: bool,
    pub looked: Vec<Looked>,
    pub suggestions: Vec<Suggestion>,
    pub selected: Option<usize>,
    pub history: Vec<Turn>,
    /// "2.1s · $0.002" after a run.
    pub meta: Option<String>,
    /// The last command failed, so Enter on an empty field explains it.
    pub last_failed: bool,
    pub has_blocks: bool,
    /// First wrapped answer line shown.
    pub scroll: usize,
    run: Option<Arc<RunHandle>>,
    generation: u64,
    /// Wrapped answer lines for (answer length, width), so a paint does not
    /// re-measure every word.
    pub wrap_cache: RefCell<Option<(usize, u32, Vec<String>)>>,
}

impl AssistCard {
    pub(crate) fn is_running(&self) -> bool {
        matches!(self.phase, Phase::Running { .. })
    }
}

/// A running CLI: what is needed to stop it from the GUI thread.
struct RunHandle {
    child: Mutex<Option<Child>>,
    stopped: AtomicBool,
}

impl RunHandle {
    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(mut child) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for AssistCard {
    fn drop(&mut self) {
        if let Some(run) = self.run.take() {
            run.stop();
        }
    }
}

fn provider_display_name(name: &str) -> String {
    match name {
        "claude" => "Claude".to_string(),
        "codex" => "Codex".to_string(),
        other => other.to_string(),
    }
}

fn toast(title: &str, message: String) {
    crate::termwindow::tgz_notify::status(wezterm_toast_notification::ToastNotification {
        title: title.to_string(),
        message,
        url: None,
        timeout: Some(Duration::from_secs(6)),
    });
}

impl TermWindow {
    /// CMD+I: open the card for `pane`, or close it when it is already open
    /// for that pane.
    pub(crate) fn open_assist(&mut self, pane: &Arc<dyn Pane>) {
        if !self.config.assist.ask.enabled {
            toast(
                "Ask is off",
                "Turn on assist.ask.enabled (Settings → Assist) to ask about a pane.".into(),
            );
            return;
        }
        if let Some(card) = self.assist.as_ref() {
            if card.pane_id == pane.pane_id() {
                self.close_assist();
                return;
            }
        }
        if matches!(
            self.sidebar_copy_kind(pane),
            Some(PaneToolbeltKind::Agent(_))
        ) {
            toast(
                "Ask is for shell panes",
                "This pane runs an agent; ask it directly.".into(),
            );
            return;
        }
        if crate::termwindow::tgz_settings::is_settings_pane(pane) {
            return;
        }
        let blocks = mux::tgz_blocks::pane_last_blocks(
            &**pane,
            self.config.assist.ask.context_blocks.max(1),
            SCAN_ROWS,
        );
        let last_failed = blocks
            .last()
            .and_then(|block| block.exit_status)
            .map_or(false, |status| status != 0);
        let mut card = AssistCard {
            pane_id: pane.pane_id(),
            provider: provider_display_name(self.config.assist.ask.provider.trim()),
            input: String::new(),
            phase: Phase::Editing,
            chips: vec![],
            question: String::new(),
            answer: String::new(),
            thinking: false,
            looked: vec![],
            suggestions: vec![],
            selected: None,
            history: vec![],
            meta: None,
            last_failed,
            has_blocks: !blocks.is_empty(),
            scroll: 0,
            run: None,
            generation: 0,
            wrap_cache: RefCell::new(None),
        };
        card.chips = self.assist_context(pane, "", &[]).chips;
        self.assist = Some(card);
        self.invalidate_assist();
    }

    /// Stop the running CLI of the card for `pane_id`, keeping the card.
    pub(crate) fn assist_stop_run(&mut self, pane_id: PaneId) {
        if let Some(card) = self.assist.as_mut() {
            if card.pane_id == pane_id {
                if let Some(run) = card.run.take() {
                    run.stop();
                }
                // Late events from the stopped run are ignored.
                card.generation += 1;
                card.thinking = false;
                if card.phase == Phase::Done {
                    finish(card);
                }
            }
        }
        self.invalidate_assist();
    }

    pub(crate) fn close_assist(&mut self) {
        // Dropping the card stops a running CLI.
        self.assist = None;
        self.invalidate_assist();
    }

    fn invalidate_assist(&self) {
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Everything the question would send, masked and within budget.
    fn assist_context(
        &mut self,
        pane: &Arc<dyn Pane>,
        question: &str,
        history: &[Turn],
    ) -> context::BuiltPrompt {
        let ask = self.config.assist.ask.clone();
        let blocks = mux::tgz_blocks::pane_last_blocks(&**pane, ask.context_blocks, SCAN_ROWS);
        let selection = self.selection_text(pane);
        let screen = if pane.is_alt_screen_active() {
            let dims = pane.get_dimensions();
            let (_, lines) =
                pane.get_lines(dims.physical_top..dims.physical_top + dims.viewport_rows as isize);
            Some(
                lines
                    .iter()
                    .map(|line| line.as_str().trim_end().to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        } else {
            None
        };
        let cwd = crate::termwindow::composer::pane_cwd(pane);
        let remote = self.assist_remote_label(pane);
        let rules = self.secret_rules();
        let redact = move |text: &str| rules.redact(text);
        context::build(
            &ContextInput {
                question,
                cwd: cwd.as_deref(),
                remote: remote.as_deref(),
                blocks: &blocks,
                selection: Some(selection.as_str()),
                screen: screen.as_deref(),
                history,
            },
            &redact,
            ask.max_context_bytes,
        )
    }

    /// `None` for a local pane; else a short name for where it runs.
    fn assist_remote_label(&self, pane: &Arc<dyn Pane>) -> Option<String> {
        if !self.file_tree_needs_picker(pane) {
            return None;
        }
        let domain = Mux::get()
            .get_domain(pane.domain_id())
            .map(|domain| domain.domain_name().to_string())
            .unwrap_or_else(|| "remote".to_string());
        Some(if domain == "local" {
            "remote shell".to_string()
        } else {
            domain
        })
    }

    /// Enter / Alt+Enter in the question field.
    pub(crate) fn assist_ask(&mut self, think: bool) {
        let Some(card) = self.assist.as_ref() else {
            return;
        };
        if card.is_running() {
            return;
        }
        let typed = card.input.trim().to_string();
        let question = if !typed.is_empty() {
            typed
        } else if card.last_failed {
            "Why did the last command fail, and how do I fix it?".to_string()
        } else if card.has_blocks {
            "Explain the output of the last command.".to_string()
        } else {
            return;
        };
        let pane_id = card.pane_id;
        let history = card.history.clone();
        let Some(pane) = Mux::get().get_pane(pane_id) else {
            self.close_assist();
            return;
        };

        let remote = self.assist_remote_label(&pane).is_some();
        let built = self.assist_context(&pane, &question, &history);
        let launch = match provider::launch(
            &self.config.assist,
            RunOptions {
                think,
                tools: !remote,
            },
        ) {
            Ok(launch) => launch,
            Err(err) => return self.assist_fail(err),
        };
        let prompt = if launch.instructions_in_prompt {
            format!("{}\n\n{}", INSTRUCTIONS, built.text)
        } else {
            built.text.clone()
        };
        let cwd = if remote {
            None
        } else {
            crate::termwindow::composer::pane_cwd(&pane).map(PathBuf::from)
        };
        log::info!(
            "ask: {} request, {} bytes of context, {} masked",
            launch.argv[0],
            built.text.len(),
            built.masked
        );

        let Some(card) = self.assist.as_mut() else {
            return;
        };
        if let Some(run) = card.run.take() {
            run.stop();
        }
        card.generation += 1;
        card.question = question;
        card.input.clear();
        card.chips = built.chips;
        card.answer.clear();
        card.thinking = false;
        card.looked.clear();
        card.suggestions.clear();
        card.selected = None;
        card.meta = None;
        card.scroll = 0;
        card.phase = Phase::Running {
            started: Instant::now(),
        };
        let generation = card.generation;
        let handle = Arc::new(RunHandle {
            child: Mutex::new(None),
            stopped: AtomicBool::new(false),
        });
        card.run = Some(Arc::clone(&handle));

        let Some(window) = self.window.clone() else {
            return;
        };
        let timeout = Duration::from_secs(self.config.assist.ask.timeout_secs.max(5));
        let cwd_text = cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
        let spawned = std::thread::Builder::new()
            .name("assist-ask".into())
            .spawn(move || {
                run_provider(
                    launch.argv,
                    launch.stream,
                    prompt,
                    cwd,
                    cwd_text,
                    timeout,
                    handle,
                    window,
                    pane_id,
                    generation,
                )
            });
        if let Err(err) = spawned {
            self.assist_fail(format!("could not start a worker thread: {err:#}"));
        }
        self.invalidate_assist();
    }

    fn assist_fail(&mut self, message: String) {
        if let Some(card) = self.assist.as_mut() {
            card.phase = Phase::Failed(message);
            card.thinking = false;
        }
        self.invalidate_assist();
    }

    /// One event from the worker, for the card that started it.
    fn assist_apply(&mut self, pane_id: PaneId, generation: u64, update: Update) {
        let Some(card) = self.assist.as_mut() else {
            return;
        };
        if card.pane_id != pane_id || card.generation != generation {
            return;
        }
        match update {
            Update::Event(AssistEvent::Text(text)) => {
                card.thinking = false;
                card.answer.push_str(&text);
            }
            Update::Event(AssistEvent::Thinking) => card.thinking = true,
            Update::Event(AssistEvent::Tool { id, label }) => {
                card.thinking = false;
                card.looked.push(Looked {
                    id,
                    label,
                    ok: true,
                });
            }
            Update::Event(AssistEvent::ToolFailed { id }) => {
                if let Some(look) = card.looked.iter_mut().find(|look| look.id == id) {
                    look.ok = false;
                }
            }
            Update::Event(AssistEvent::Done {
                ok,
                text,
                cost_usd,
                duration_ms,
            }) => {
                if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
                    card.answer = text;
                }
                let mut meta = Vec::new();
                if let Some(ms) = duration_ms {
                    meta.push(format!("{:.1}s", ms as f64 / 1000.));
                }
                if let Some(cost) = cost_usd {
                    meta.push(format!("${:.4}", cost));
                }
                card.meta = (!meta.is_empty()).then(|| meta.join(" · "));
                if ok {
                    finish(card);
                } else {
                    card.phase = Phase::Failed(if card.answer.trim().is_empty() {
                        "The request failed.".to_string()
                    } else {
                        card.answer.clone()
                    });
                }
            }
            Update::Exited { ok, stderr } => {
                if card.is_running() {
                    if ok && !card.answer.trim().is_empty() {
                        finish(card);
                    } else {
                        card.phase = Phase::Failed(explain_failure(&stderr, ok));
                    }
                }
                card.run = None;
            }
        }
        card.wrap_cache.replace(None);
        self.invalidate_assist();
    }

    /// Enter on a suggestion: paste it at the prompt. CMD+Enter also runs
    /// it; a command that deletes asks first.
    pub(crate) fn assist_use_suggestion(&mut self, index: usize, run: bool) {
        let Some(card) = self.assist.as_ref() else {
            return;
        };
        let Some(suggestion) = card.suggestions.get(index).cloned() else {
            return;
        };
        let pane_id = card.pane_id;
        if run && suggestion.risk == Risk::Deletes && self.config.assist.risk_labels {
            self.chrome_dialog = Some(crate::termwindow::ChromeDialog {
                title: "Run a command that deletes?".to_string(),
                body: "Ask labelled this suggestion as deleting something.".to_string(),
                lines: vec![(suggestion.command.clone(), String::new())],
                confirm_label: "Run".to_string(),
                on_confirm: Some(Box::new(move |tw: &mut TermWindow| {
                    tw.assist_send(pane_id, &suggestion.command, true);
                })),
            });
            self.invalidate_assist();
            return;
        }
        self.assist_send(pane_id, &suggestion.command, run);
    }

    fn assist_send(&mut self, pane_id: PaneId, command: &str, run: bool) {
        let Some(pane) = Mux::get().get_pane(pane_id) else {
            return;
        };
        // The card only opens on shell panes, but the pane may have started
        // an agent since.
        if matches!(
            self.sidebar_copy_kind(&pane),
            Some(PaneToolbeltKind::Agent(_))
        ) {
            toast(
                "Not sent",
                "That pane now runs an agent; nothing was typed into it.".into(),
            );
            return;
        }
        // A model's text never carries escape sequences or line breaks into
        // the shell: the paste is the command and nothing else.
        let command: String = command
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .collect();
        if command.trim().is_empty() {
            return;
        }
        if let Err(err) = pane.send_paste(&command) {
            log::warn!("ask: could not paste into pane {pane_id}: {err:#}");
            return;
        }
        if run {
            if let Err(err) = pane.writer().write_all(b"\r") {
                log::warn!("ask: could not press Enter in pane {pane_id}: {err:#}");
            }
        }
        self.close_assist();
    }
}

impl TermWindow {
    /// Register `wezterm cli mcp` with Claude Code, user-wide, through
    /// Claude's own CLI (no editing of its config files). Off the GUI thread;
    /// the outcome is a toast.
    pub(crate) fn connect_assist_mcp(&mut self) {
        if !self.config.assist.mcp.enabled {
            toast(
                "Terminal tools are off",
                "Turn on assist.mcp.enabled first (Settings → Assist).".into(),
            );
            return;
        }
        let cli = match std::env::current_exe() {
            Ok(exe) => {
                let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
                let name = if cfg!(windows) {
                    "wezterm.exe"
                } else {
                    "wezterm"
                };
                dir.join(name)
            }
            Err(err) => {
                toast("Not connected", format!("cannot find this app: {err}"));
                return;
            }
        };
        let spawned = std::thread::Builder::new()
            .name("assist-connect".into())
            .spawn(move || {
                let Some(claude) = resolve_program("claude") else {
                    toast(
                        "Not connected",
                        "Claude Code (claude) was not found.".into(),
                    );
                    return;
                };
                let output = Command::new(&claude)
                    .args(["mcp", "add", "--scope", "user", "tgzterminal", "--"])
                    .arg(&cli)
                    .args(["cli", "mcp"])
                    .stdin(Stdio::null())
                    .output();
                match output {
                    Ok(out) if out.status.success() => toast(
                        "Connected",
                        "Claude Code can now read your panes' recent commands \
                         (tgzterminal MCP server). New Claude sessions pick it up."
                            .into(),
                    ),
                    Ok(out) => {
                        let err = String::from_utf8_lossy(&out.stderr).into_owned();
                        let msg = if err.contains("already exists") {
                            "Already connected.".to_string()
                        } else {
                            last_line(&err).to_string()
                        };
                        toast("Not connected", msg);
                    }
                    Err(err) => toast("Not connected", format!("{err}")),
                }
            });
        if let Err(err) = spawned {
            log::warn!("assist: could not start the connect thread: {err:#}");
        }
    }
}

fn finish(card: &mut AssistCard) {
    card.phase = Phase::Done;
    card.thinking = false;
    card.suggestions = suggest::extract(&card.answer)
        .into_iter()
        .map(|command| Suggestion {
            risk: risk::classify(&command),
            command,
        })
        .collect();
    card.selected = None;
    card.history.push(Turn {
        question: card.question.clone(),
        answer: card.answer.clone(),
    });
    // Two earlier turns are plenty of conversation for a terminal question.
    let excess = card.history.len().saturating_sub(2);
    card.history.drain(..excess);
}

fn explain_failure(stderr: &str, exited_ok: bool) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        return if exited_ok {
            "No answer came back.".to_string()
        } else {
            "The command failed without saying why.".to_string()
        };
    }
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("log in") || lower.contains("login") || lower.contains("authenticat") {
        return format!("Not logged in? {}", last_line(stderr));
    }
    last_line(stderr).to_string()
}

fn last_line(text: &str) -> &str {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(text)
        .trim()
}

/// What the worker sends back.
enum Update {
    Event(AssistEvent),
    Exited { ok: bool, stderr: String },
}

/// Where to look for a CLI that is not on the GUI's PATH (an app started
/// from Finder gets a minimal PATH).
fn fallback_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs_next::home_dir() {
        for rel in [
            ".local/bin",
            ".claude/local",
            ".npm-global/bin",
            ".bun/bin",
            ".volta/bin",
            ".cargo/bin",
            "bin",
        ] {
            dirs.push(home.join(rel));
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
        dirs.push(PathBuf::from(dir));
    }
    dirs
}

fn resolve_program(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{program}.exe"), program.to_string()]
    } else {
        vec![program.to_string()]
    };
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    path_dirs
        .into_iter()
        .chain(fallback_dirs())
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// The worker: start the CLI, feed it the prompt, stream its answer back,
/// enforce the timeout. Runs off the GUI thread.
#[allow(clippy::too_many_arguments)]
fn run_provider(
    argv: Vec<String>,
    stream: AssistStreamFormat,
    prompt: String,
    cwd: Option<PathBuf>,
    cwd_text: Option<String>,
    timeout: Duration,
    handle: Arc<RunHandle>,
    window: ::window::Window,
    pane_id: PaneId,
    generation: u64,
) {
    let send = |update: Update| {
        window.notify(TermWindowNotif::Apply(Box::new(
            move |tw: &mut TermWindow| {
                tw.assist_apply(pane_id, generation, update);
            },
        )));
    };

    let Some(program) = resolve_program(&argv[0]) else {
        send(Update::Exited {
            ok: false,
            stderr: format!(
                "'{}' was not found. Install it, or pick another assist.ask.provider.",
                argv[0]
            ),
        });
        return;
    };
    let mut command = Command::new(&program);
    command
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Node-based CLIs find `node` next to themselves (nvm, volta).
    let mut path_dirs: Vec<PathBuf> = program
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .collect();
    if let Some(path) = std::env::var_os("PATH") {
        path_dirs.extend(std::env::split_paths(&path));
    }
    path_dirs.extend(fallback_dirs());
    if let Ok(path) = std::env::join_paths(path_dirs) {
        command.env("PATH", path);
    }
    match cwd.filter(|dir| dir.is_dir()) {
        Some(dir) => {
            command.current_dir(dir);
        }
        // No readable directory (remote pane): run somewhere empty-ish.
        None => {
            command.current_dir(std::env::temp_dir());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            send(Update::Exited {
                ok: false,
                stderr: format!("could not start {}: {err}", program.display()),
            });
            return;
        }
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *handle.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
    if handle.stopped.load(Ordering::SeqCst) {
        handle.stop();
        return;
    }

    // Write the prompt on its own thread: a CLI that does not read stdin
    // until it has printed something must not deadlock us.
    if let Some(mut stdin) = stdin {
        std::thread::spawn(move || {
            let _ = stdin.write_all(prompt.as_bytes());
            // Dropping closes it: end of the prompt.
        });
    }
    let stderr_reader = stderr.map(|mut stderr| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf);
            let text = String::from_utf8_lossy(&buf).into_owned();
            let cut = text.len().saturating_sub(STDERR_KEEP);
            let mut start = cut;
            while !text.is_char_boundary(start) {
                start += 1;
            }
            text[start..].to_string()
        })
    });
    {
        let handle = Arc::clone(&handle);
        std::thread::spawn(move || {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if handle.stopped.load(Ordering::SeqCst)
                    || handle.child.lock().map(|c| c.is_none()).unwrap_or(true)
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            log::info!("ask: stopping a request that ran past {:?}", timeout);
            handle.stop();
        });
    }

    if let Some(stdout) = stdout {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if handle.stopped.load(Ordering::SeqCst) {
                break;
            }
            match stream {
                AssistStreamFormat::ClaudeStreamJson => {
                    for event in parse_claude_line(&line, cwd_text.as_deref()) {
                        send(Update::Event(event));
                    }
                }
                AssistStreamFormat::Text => {
                    send(Update::Event(AssistEvent::Text(format!("{line}\n"))));
                }
            }
        }
    }

    let child = handle
        .child
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    let ok = match child {
        Some(mut child) => child.wait().map(|status| status.success()).unwrap_or(false),
        // Stopped (Esc, a new question, the timeout).
        None => false,
    };
    let stderr = stderr_reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    let stopped = handle.stopped.load(Ordering::SeqCst);
    send(Update::Exited {
        ok,
        stderr: if stopped && stderr.trim().is_empty() {
            "Stopped.".to_string()
        } else {
            stderr
        },
    });
}
