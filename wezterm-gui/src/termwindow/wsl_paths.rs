//! Windows <-> WSL path translation for the sidebar launchers.
//!
//! Only one direction actually needs code. `LocalDomain::fixup_command`
//! (`mux/src/domain.rs`) spawns WSL panes as
//! `wsl.exe --distribution <d> --cd <cwd> --exec <argv>`, and `wsl.exe --cd`
//! accepts a Windows path and translates it itself — so going
//! Windows -> WSL we simply pass the Windows path through untouched.
//!
//! The reverse (a WSL pane's Linux cwd handed to a Windows domain) has no
//! such helper, and neither does the project-root marker walk, which has to
//! stat a distro's filesystem from the Windows side. Both are covered here.
//!
//! The path functions are pure string functions with no `cfg` gating so they
//! can be tested on any host, including the macOS dev box where WSL does not
//! exist.
//!
//! This module also owns the process-global WSL distro list. Listing distros
//! means spawning `wsl.exe -l -v`, and `Config::wsl_domains()` does that on
//! *every call* when the user has not written `wsl_domains` by hand — which
//! the sidebar used to hit per frame per agent pane, freezing the window.
//! Fork code must go through [`wsl_domains`] here instead, which never
//! spawns on the calling thread.

use config::{WslDistro, WslDomain};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// UNC prefixes Windows exposes a distro's filesystem under. `wsl.localhost`
/// is the modern form; `wsl$` still works and is what older docs show.
const UNC_PREFIXES: [&str; 2] = [r"\\wsl.localhost\", r"\\wsl$\"];

/// How long a loaded distro list (and which of them are running) is served
/// before a background refresh.
const WSL_DISTRO_TTL: Duration = Duration::from_secs(60);
/// A refresh that has not reported back after this long is presumed dead,
/// so it cannot suppress every later refresh (see `herd_scan_is_due`).
const WSL_DISTRO_WATCHDOG: Duration = Duration::from_secs(30);

#[derive(Default)]
struct DistroCache {
    distros: Arc<Vec<WslDistro>>,
    refreshed_at: Option<Instant>,
    refresh_started_at: Option<Instant>,
}

static WSL_DISTRO_CACHE: LazyLock<Mutex<DistroCache>> =
    LazyLock::new(|| Mutex::new(DistroCache::default()));

/// The registered WSL distros, as last seen by a background refresh.
///
/// Never spawns on the calling thread. A cold cache is filled from the
/// registry right away (instant, no `wsl.exe`), with `state` unknown until
/// the background refresh has asked which distros are running; a stale one
/// returns what it has and kicks that refresh. Always empty off Windows.
pub fn cached_distros() -> Arc<Vec<WslDistro>> {
    let mut cache = WSL_DISTRO_CACHE.lock().unwrap();
    if cache.refreshed_at.is_none() && cache.distros.is_empty() {
        if let Ok(distros) = WslDistro::registered_distros() {
            cache.distros = Arc::new(distros);
        }
    }
    kick_refresh_if_due(&mut cache);
    Arc::clone(&cache.distros)
}

/// The registered distros with `state` filled in from `wsl.exe -l --running
/// -q`. Blocking (one hidden, bounded `wsl.exe`, and none at all when no
/// distro is registered), so call it from a worker thread only.
pub(crate) fn load_distros_with_state() -> anyhow::Result<Vec<WslDistro>> {
    let mut distros = WslDistro::load_distro_list()?;
    if distros.is_empty() {
        return Ok(distros);
    }
    match WslDistro::running_distro_names() {
        Ok(running) => WslDistro::mark_running(&mut distros, &running),
        Err(err) => {
            // Unknown counts as not running everywhere this is read, which
            // errs towards not booting anything.
            log::debug!("could not list running WSL distros: {err:#}");
            for distro in &mut distros {
                distro.state.clear();
            }
        }
    }
    Ok(distros)
}

/// Start loading the distro list now, so the first frame that needs it
/// finds it warm. Called once at GUI startup.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn warm_distro_cache() {
    let mut cache = WSL_DISTRO_CACHE.lock().unwrap();
    kick_refresh_if_due(&mut cache);
}

fn kick_refresh_if_due(cache: &mut DistroCache) {
    if !cfg!(windows) {
        return;
    }
    let now = Instant::now();
    if !crate::termwindow::render::sidebar::herd_scan_is_due(
        cache.refresh_started_at,
        cache.refreshed_at,
        WSL_DISTRO_TTL,
        WSL_DISTRO_WATCHDOG,
        now,
    ) {
        return;
    }
    cache.refresh_started_at = Some(now);
    let spawned = std::thread::Builder::new()
        .name("wsl-distro-list".into())
        .spawn(|| {
            let distros = match load_distros_with_state() {
                Ok(distros) => Some(distros),
                Err(err) => {
                    log::debug!("wsl distro list unavailable: {err:#}");
                    None
                }
            };
            let mut cache = WSL_DISTRO_CACHE.lock().unwrap();
            if let Some(distros) = distros {
                cache.distros = Arc::new(distros);
            }
            // Stamped on failure too: a machine without WSL must not respawn
            // `wsl.exe` every frame.
            cache.refreshed_at = Some(Instant::now());
            cache.refresh_started_at = None;
        });
    if let Err(err) = spawned {
        log::warn!("failed to start wsl distro refresh: {err:#}");
        cache.refresh_started_at = None;
    }
}

/// `Config::wsl_domains()` without the spawn: the user's own `wsl_domains`
/// verbatim, else the built-in domains derived from [`cached_distros`].
pub fn wsl_domains(config: &config::ConfigHandle) -> Vec<WslDomain> {
    match &config.wsl_domains {
        Some(domains) => domains.clone(),
        None => WslDomain::domains_from_distros(&cached_distros()),
    }
}

/// Run `wsl.exe` with `args` without flashing a console window, capturing
/// stdout, and give up (killing the child) after `timeout`.
///
/// A bare `Command::new("wsl.exe")` from a GUI process allocates a visible
/// console per call — the "ten cmd windows at startup" bug.
pub(crate) fn run_wsl_hidden(args: &[&str], timeout: Duration) -> Result<String, WslRunError> {
    let mut cmd = config::hidden_wsl_command();
    cmd.args(args);
    // Killed on timeout, and output a lingering Linux process keeps the pipe
    // open for is abandoned rather than waited on.
    let output =
        config::output_with_timeout(cmd, timeout).map_err(|err| match err
            .downcast_ref::<config::WslCommandTimedOut>()
        {
            Some(timed_out) => WslRunError::TimedOut(format!("{timed_out}; ran wsl.exe {args:?}")),
            None => WslRunError::Failed(format!("{err:#}")),
        })?;
    if !output.status.success() {
        // `wsl.exe`'s own complaints are UTF-16; a Linux program's are not.
        let stderr: String = config::decode_wsl_output(&output.stderr)
            .trim()
            .chars()
            .take(300)
            .collect();
        return Err(WslRunError::Failed(format!("{}: {stderr}", output.status)));
    }
    // What a Linux program printed, passed through by `wsl.exe` untouched.
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Why [`run_wsl_hidden`] has no output to offer.
#[derive(Debug)]
pub(crate) enum WslRunError {
    /// No answer before the timeout; the child was killed. Says what it had
    /// printed and what it was asked.
    TimedOut(String),
    /// It could not start, or exited unsuccessfully (with its stderr).
    Failed(String),
}

/// Upper bound on one agent probe. Longer than [`config::WSL_COMMAND_TIMEOUT`]: it
/// runs off the GUI thread, and asking a stopped distro first boots its VM,
/// which on a cold start can take longer than ten seconds. Generous because a
/// slow machine sits right at the edge: the same probe answered in 27s on one
/// run and hit this bound on the next.
const WSL_PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// Which shell found an agent inside WSL, and therefore which shell must
/// launch it.
///
/// `wsl.exe --exec` (what `LocalDomain::fixup_command` uses) bypasses the
/// user's shell entirely, so a CLI installed under `~/.local/bin`, npm-global
/// or nvm — all added to `PATH` by rc files — is invisible to it. Probing and
/// launching through the same shell keeps "found" and "launches" in agreement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WslProbeShell {
    /// `bash -lic`: reads `~/.profile` *and* `~/.bashrc`, where nvm lives
    /// (Debian's and Ubuntu's `.bashrc` return at once when not interactive).
    BashInteractive,
    /// `sh -lc`: `~/.profile` only, which already covers `~/.local/bin`.
    ShLogin,
}

impl WslProbeShell {
    fn shell(self) -> [&'static str; 2] {
        match self {
            Self::BashInteractive => ["bash", "-lic"],
            Self::ShLogin => ["sh", "-lc"],
        }
    }

    /// `command` run through this shell, so its rc-file `PATH` applies.
    /// Arguments travel as positional parameters, never re-parsed as shell.
    pub fn wrap(self, command: Vec<String>) -> Vec<String> {
        let [shell, flags] = self.shell();
        let mut argv = vec![
            shell.to_string(),
            flags.to_string(),
            r#"exec "$0" "$@""#.to_string(),
        ];
        argv.extend(command);
        argv
    }
}

/// An agent CLI found inside a WSL distro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WslAgentHit {
    pub distro: String,
    pub shell: WslProbeShell,
}

/// Marker each found command is echoed with, so noise an interactive rc file
/// prints to stdout can never be mistaken for a hit.
const WSL_PROBE_MARKER: &str = "tgz-found:";
/// Probe with the Windows-mounted PATH entries removed.
///
/// Every PATH miss makes `command -v` stat every remaining directory, and on
/// this distro the mounted ones (`/mnt/...`) go through the Windows 9p share,
/// where one stat can take seconds. Eight candidate programs then blow the
/// whole probe budget before a single hit is echoed. Everything that matters
/// for launching came from the distro's own rc files anyway.
const WSL_PROBE_SCRIPT: &str = r#"oldIFS=$IFS; IFS=:; p=""; for d in $PATH; do case $d in /mnt/*) ;; *) p="$p:$d";; esac; done; IFS=$oldIFS; PATH=${p#:}; export PATH; for c in "$@"; do command -v "$c" >/dev/null 2>&1 && printf 'tgz-found:%s\n' "$c"; done; exit 0"#;

/// The programs `output` reports found, restricted to those that were asked
/// about.
pub(crate) fn parse_wsl_probe_output(output: &str, programs: &[String]) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.trim().strip_prefix(WSL_PROBE_MARKER))
        .filter(|found| programs.iter().any(|program| program == found))
        .map(str::to_string)
        .collect()
}

/// Distros worth probing, in preference order.
///
/// `wsl.exe -d` boots a stopped distro, which takes seconds and is not the
/// user's to pay for an agent they may not have. So: the default distro
/// (booted or not — it is where a new WSL tab lands — but see
/// [`probe_wsl_agents`] for how rarely), then any running one. Docker
/// Desktop's utility distros never hold a user's CLI.
pub(crate) fn wsl_probe_order(distros: &[WslDistro]) -> Vec<&WslDistro> {
    let usable =
        |distro: &&WslDistro| !distro.name.is_empty() && !distro.name.starts_with("docker-desktop");
    let mut ordered: Vec<&WslDistro> = distros
        .iter()
        .filter(usable)
        .filter(|distro| distro.is_default)
        .collect();
    ordered.extend(
        distros
            .iter()
            .filter(usable)
            .filter(|distro| !distro.is_default && distro.is_running()),
    );
    ordered
}

/// What one earlier probe of a distro found, kept so a stopped distro is not
/// booted again just to be asked the same question.
struct ProbedDistro {
    asked: Vec<String>,
    found: std::collections::HashMap<String, WslProbeShell>,
}

/// Process-wide, so a second window (or the periodic re-probe) reuses what
/// the first probe learned instead of booting the distro once more.
static PROBED_DISTROS: LazyLock<Mutex<std::collections::HashMap<String, ProbedDistro>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Which of `programs` each probed distro can run, first distro wins.
///
/// Blocking — one hidden, time-bounded `wsl.exe` per distro and shell, never
/// per program. Call from a worker thread only.
///
/// Only running distros are asked again. The default distro is asked even
/// when stopped, which boots it, but only while nothing is known about it:
/// after one look its answer is remembered, where the old policy re-booted it
/// every five minutes for as long as the terminal was open.
pub(crate) fn probe_wsl_agents(
    distros: &[WslDistro],
    users: &std::collections::HashMap<String, String>,
    programs: &[String],
) -> std::collections::HashMap<String, WslAgentHit> {
    let mut hits = std::collections::HashMap::new();
    if programs.is_empty() {
        return hits;
    }
    for distro in wsl_probe_order(distros) {
        let missing: Vec<String> = programs
            .iter()
            .filter(|program| !hits.contains_key(*program))
            .cloned()
            .collect();
        if missing.is_empty() {
            break;
        }
        let remembered = if distro.is_running() {
            None
        } else {
            PROBED_DISTROS
                .lock()
                .unwrap()
                .get(&distro.name)
                .filter(|probed| missing.iter().all(|program| probed.asked.contains(program)))
                .map(|probed| probed.found.clone())
        };
        let found = match remembered {
            Some(found) => found,
            None => {
                // A slow machine sits right at the bound: the same probe
                // answered in 27s on one run and hit this bound on the next.
                // Retry once before giving up, and remember only answers.
                let found = match probe_distro(distro, users.get(&distro.name), &missing) {
                    Some(found) => Some(found),
                    None => {
                        log::info!(
                            "wsl agent probe: {} did not answer; retrying once",
                            distro.name
                        );
                        probe_distro(distro, users.get(&distro.name), &missing)
                    }
                };
                if let Some(found) = &found {
                    for (program, shell) in found {
                        log::info!("wsl agent probe: found {program} in {}", distro.name);
                    }
                    PROBED_DISTROS.lock().unwrap().insert(
                        distro.name.clone(),
                        ProbedDistro {
                            asked: missing.clone(),
                            found: found.clone(),
                        },
                    );
                }
                let last = found
                    .is_none()
                    .then(|| {
                        PROBED_DISTROS
                            .lock()
                            .unwrap()
                            .get(&distro.name)
                            .map(|probed| probed.found.clone())
                    })
                    .flatten();
                if last.is_some() {
                    log::info!(
                        "wsl agent probe: {} gave no answer; keeping its last one",
                        distro.name
                    );
                }
                answer_or_last_known(found, last)
            }
        };
        for (program, shell) in found {
            if missing.contains(&program) {
                hits.entry(program).or_insert_with(|| WslAgentHit {
                    distro: distro.name.clone(),
                    shell,
                });
            }
        }
    }
    hits
}

/// A distro's answer, or what it answered last time when it gave none.
///
/// A probe that times out says nothing about what is installed, yet an empty
/// answer used to replace the last good one for a whole probe TTL: every
/// WSL-only agent vanished from the launcher, and once the last pane in the
/// distro had closed the resume menus stopped reading it too, so its sessions
/// (the one just exited included) went missing.
fn answer_or_last_known(
    answer: Option<std::collections::HashMap<String, WslProbeShell>>,
    last: Option<std::collections::HashMap<String, WslProbeShell>>,
) -> std::collections::HashMap<String, WslProbeShell> {
    answer.or(last).unwrap_or_default()
}

/// Every distro a probe has found an agent CLI in.
///
/// Wider than the probe's hits, which keep only the first distro per program:
/// an agent installed in two distros has sessions in both.
pub(crate) fn distros_with_agents() -> Vec<String> {
    PROBED_DISTROS
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, probed)| !probed.found.is_empty())
        .map(|(name, _)| name.clone())
        .collect()
}

/// Most distros [`remember_session_distros`] keeps: far more than anyone
/// registers, and only a bound on a list that is never pruned.
const MAX_SESSION_DISTROS: usize = 16;

/// WSL distros agents have run in, so the resume menus keep reading their
/// session files once the last pane there has closed.
///
/// The herd reads only distros with a live pane, because it scans every 500ms
/// and reading a stopped distro's share boots it. The resume menus scan when
/// the user opens one, so they can afford a stopped distro, but had no way to
/// know about it: an agent pane *is* the agent, so `exit` closed the last pane
/// in its distro, and unless the WSL probe happened to hold a hit for it, the
/// distro and every session in it dropped out of the menu.
#[derive(Default)]
struct SessionDistros {
    known: BTreeSet<String>,
    /// What the state file holds; `None` until it has been read.
    saved: Option<BTreeSet<String>>,
}

impl SessionDistros {
    /// Add `names`, up to [`MAX_SESSION_DISTROS`].
    fn remember(&mut self, names: impl IntoIterator<Item = String>) {
        for name in names {
            if self.known.len() >= MAX_SESSION_DISTROS {
                break;
            }
            if !name.is_empty() {
                self.known.insert(name);
            }
        }
    }

    /// The set to write, when it differs from what the file holds.
    fn unsaved(&self) -> Option<BTreeSet<String>> {
        let saved = self.saved.as_ref()?;
        (*saved != self.known).then(|| self.known.clone())
    }
}

static SESSION_DISTROS: LazyLock<Mutex<SessionDistros>> = LazyLock::new(Default::default);

/// [`SESSION_DISTROS`], locked. Taken from paint, so a poisoned lock (a panic
/// mid-insert, which leaves a valid set) must not become a panic there.
fn session_distros() -> std::sync::MutexGuard<'static, SessionDistros> {
    SESSION_DISTROS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Seed the remembered distros from the state file, once per process.
///
/// Reads one small local file; call at window creation, never from paint.
pub(crate) fn load_session_distros() {
    if !cfg!(windows) || session_distros().saved.is_some() {
        return;
    }
    let saved: BTreeSet<String> = super::tgz_ui_state::load_wsl_session_distros()
        .into_iter()
        .collect();
    let mut distros = session_distros();
    if distros.saved.is_none() {
        distros.remember(saved.iter().cloned());
        distros.saved = Some(saved);
    }
}

/// Remember distros agents run in. Memory only, so the paint path may call it.
pub(crate) fn remember_session_distros(names: impl IntoIterator<Item = String>) {
    if cfg!(windows) {
        session_distros().remember(names);
    }
}

/// Every distro remembered so far, this process and earlier ones.
pub(crate) fn remembered_session_distros() -> BTreeSet<String> {
    session_distros().known.clone()
}

/// Write the remembered distros out if they grew since the file was read.
///
/// Blocking file I/O: worker thread only. The lock is not held across the
/// write, because the herd path takes it from paint.
pub(crate) fn persist_session_distros() {
    if !cfg!(windows) {
        return;
    }
    let Some(unsaved) = session_distros().unsaved() else {
        return;
    };
    super::tgz_ui_state::save_wsl_session_distros(&unsaved);
    session_distros().saved = Some(unsaved);
}

/// Ask one distro which of `programs` it has, trying `bash -lic` and then
/// `sh -lc` for whatever the first did not find. `None` when the distro gave
/// no answer at all, which must not be mistaken for "none installed".
fn probe_distro(
    distro: &WslDistro,
    user: Option<&String>,
    programs: &[String],
) -> Option<std::collections::HashMap<String, WslProbeShell>> {
    let mut found = std::collections::HashMap::new();
    let mut answered = false;
    // `sh -lc` first: the cheapest shell, and it finds anything `~/.profile`
    // puts on PATH. Only what it misses (nvm, set up in `.bashrc`) is asked
    // of an interactive bash.
    for shell in [WslProbeShell::ShLogin, WslProbeShell::BashInteractive] {
        let missing: Vec<&str> = programs
            .iter()
            .filter(|program| !found.contains_key(*program))
            .map(String::as_str)
            .collect();
        if missing.is_empty() {
            break;
        }
        let [sh, flags] = shell.shell();
        let mut args = vec!["-d", distro.name.as_str()];
        if let Some(user) = user {
            args.extend(["-u", user.as_str()]);
        }
        // `--exec`, not `--`: the latter re-joins argv into a string for
        // the user's shell to re-parse.
        args.extend(["--exec", sh, flags, WSL_PROBE_SCRIPT, sh]);
        args.extend(missing.iter().copied());
        let output = match run_wsl_hidden(&args, WSL_PROBE_TIMEOUT) {
            Ok(output) => output,
            Err(WslRunError::TimedOut(detail)) => {
                log::info!("wsl agent probe: {} did not answer: {detail}", distro.name);
                // An unresponsive distro will not answer the next shell
                // either; waiting for it again only delays the others.
                break;
            }
            Err(WslRunError::Failed(reason)) => {
                // Typically the shell does not exist (no bash); `sh` next.
                log::info!(
                    "wsl agent probe: {} via {shell:?} failed: {reason}",
                    distro.name
                );
                continue;
            }
        };
        answered = true;
        let asked: Vec<String> = missing.iter().map(|program| program.to_string()).collect();
        for program in parse_wsl_probe_output(&output, &asked) {
            found.entry(program).or_insert(shell);
        }
    }
    answered.then_some(found)
}

/// `command` started in `distro` through `wsl.exe` from a Windows shell, for
/// when that distro has no registered domain to spawn into (a hand-written
/// `wsl_domains` that leaves it out). `command` should already be wrapped by
/// the probing shell (see [`WslProbeShell::wrap`]).
pub(crate) fn wsl_exec_argv(distro: &str, command: Vec<String>) -> Vec<String> {
    let mut argv = vec![
        "wsl.exe".to_string(),
        "-d".to_string(),
        distro.to_string(),
        "--exec".to_string(),
    ];
    argv.extend(command);
    argv
}

/// Distro name for a domain, or `None` when the domain is not a WSL domain.
///
/// Prefers a configured `wsl_domains` entry, whose `distribution` may differ
/// from its `name`; falls back to stripping the `WSL:` prefix that
/// `WslDomain::default_domains()` generates.
pub fn distro_for_domain(domain_name: &str, config: &config::ConfigHandle) -> Option<String> {
    for domain in wsl_domains(config) {
        if domain.name == domain_name {
            return Some(
                domain
                    .distribution
                    .clone()
                    .unwrap_or_else(|| domain.name.clone()),
            );
        }
    }
    domain_name
        .strip_prefix("WSL:")
        .map(str::trim)
        .filter(|distro| !distro.is_empty())
        .map(str::to_string)
}

/// True when `domain_name` looks like a WSL domain.
pub fn is_wsl_domain(domain_name: &str, config: &config::ConfigHandle) -> bool {
    distro_for_domain(domain_name, config).is_some()
}

/// A Linux path as seen inside `distro`, rewritten so Windows can open it.
///
/// - `/mnt/c/foo` -> `C:\foo` (a drive mount is a real Windows path)
/// - `/home/tim`  -> `\\wsl.localhost\Ubuntu\home\tim`
///
/// Returns `None` for relative paths, which callers treat as "no usable cwd"
/// and fall back to the target domain's default.
pub fn wsl_to_windows(linux_path: &str, distro: &str) -> Option<PathBuf> {
    let path = linux_path.trim();
    if !path.starts_with('/') || distro.trim().is_empty() {
        return None;
    }

    if let Some(rest) = strip_mnt_drive(path) {
        let (drive, tail) = rest;
        let tail = tail.replace('/', "\\");
        return Some(PathBuf::from(if tail.is_empty() {
            // `/mnt/c` is the drive root, which needs the trailing separator:
            // `C:` alone means "current directory on C:" to Windows.
            format!("{}:\\", drive.to_ascii_uppercase())
        } else {
            format!("{}:\\{}", drive.to_ascii_uppercase(), tail)
        }));
    }

    let tail = path.trim_start_matches('/').replace('/', "\\");
    Some(PathBuf::from(format!(
        r"\\wsl.localhost\{}\{}",
        distro.trim(),
        tail
    )))
}

/// `path` without `prefix`, compared ASCII-case-insensitively.
///
/// Never slices `path` at a byte offset that might fall inside a character:
/// `&path[..16]` on `C:\Users\Jens.Müller\…` splits the `ü` and panics, and
/// this runs on the GUI thread for every WSL pane, every frame.
fn strip_prefix_ignore_ascii_case<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let head = path.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        path.get(prefix.len()..)
    } else {
        None
    }
}

/// The inverse of [`wsl_to_windows`]: a Windows path rewritten as the distro
/// sees it. Used to map a UNC marker-walk result back into the distro's view.
///
/// - `\\wsl.localhost\Ubuntu\home\tim` -> `/home/tim` (also the `\\wsl$\` form)
/// - `C:\foo` -> `/mnt/c/foo`
///
/// A UNC path naming a *different* distro yields `None`: that directory is not
/// reachable under the target distro's own root.
pub fn windows_to_wsl(win_path: &str, distro: &str) -> Option<String> {
    let path = win_path.trim();
    if path.is_empty() {
        return None;
    }

    for prefix in UNC_PREFIXES {
        // Windows path comparison is case-insensitive, and so are distro names
        // as far as the UNC share is concerned.
        if let Some(rest) = strip_prefix_ignore_ascii_case(path, prefix) {
            let (unc_distro, tail) = match rest.find(['\\', '/']) {
                Some(idx) => (&rest[..idx], &rest[idx + 1..]),
                None => (rest, ""),
            };
            if !unc_distro.eq_ignore_ascii_case(distro.trim()) {
                return None;
            }
            return Some(format!("/{}", tail.replace('\\', "/")));
        }
    }

    // Drive-letter path: C:\foo or C:/foo, and bare `C:` meaning the root.
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let tail = path[2..].trim_start_matches(['\\', '/']).replace('\\', "/");
        return Some(if tail.is_empty() {
            format!("/mnt/{}", drive)
        } else {
            format!("/mnt/{}/{}", drive, tail)
        });
    }

    None
}

/// The distro a `\\wsl.localhost\<distro>\...` path (or its `\\wsl$\` form)
/// lives in: a Windows program can sit in such a directory too, and the
/// working copy there belongs to that distro's tools.
pub fn distro_of_unc(win_path: &str) -> Option<String> {
    let path = win_path.trim();
    let rest = UNC_PREFIXES
        .iter()
        .find_map(|prefix| strip_prefix_ignore_ascii_case(path, prefix))?;
    let distro = rest.split(['\\', '/']).next()?;
    (!distro.is_empty()).then(|| distro.to_string())
}

/// Prints the directory of the program in the foreground of pane `$1` of the
/// GUI whose socket is `$2`: the leader of the terminal's foreground process
/// group, or failing that the pane's oldest process (its shell).
const PANE_CWD_SCRIPT: &str = r#"pane=$1; sock=$2; best=; fg=
for f in $(grep -lzxF "WEZTERM_PANE=$pane" /proc/[0-9]*/environ 2>/dev/null); do
  grep -qzxF "WEZTERM_UNIX_SOCKET=$sock" "$f" 2>/dev/null || continue
  d=${f%/environ}; pid=${d#/proc/}
  s=$(cat "$d/stat" 2>/dev/null) || continue
  set -- ${s##*) }
  if [ -z "$best" ] || [ "$pid" -lt "$best" ]; then best=$pid; fi
  if [ "$pid" = "$6" ]; then fg=$pid; break; fi
done
p=${fg:-$best}
[ -n "$p" ] && readlink "/proc/$p/cwd""#;

/// Where the program running in pane `pane_id` is, as `distro` sees it.
///
/// The Windows side cannot answer this: a WSL pane's process tree ends at
/// `wsl.exe`, whose own directory is wherever it was started from, while the
/// shell inside moves around freely. The distro's `/proc` knows, and the
/// processes of a pane are recognisable by the `WEZTERM_PANE` and
/// `WEZTERM_UNIX_SOCKET` they inherit through `WSLENV`.
///
/// Spawns `wsl.exe`; never call it on the GUI thread.
pub(crate) fn pane_cwd_in_distro(
    distro: &str,
    user: Option<&str>,
    pane_id: usize,
) -> Option<String> {
    let socket = std::env::var("WEZTERM_UNIX_SOCKET").ok()?;
    let pane = pane_id.to_string();
    let mut args = vec!["--distribution", distro];
    if let Some(user) = user {
        args.extend(["--user", user]);
    }
    args.extend(["--exec", "sh", "-c", PANE_CWD_SCRIPT, "sh", &pane, &socket]);
    let output = run_wsl_hidden(&args, config::WSL_COMMAND_TIMEOUT)
        .map_err(|err| log::debug!("no directory for pane {pane_id} from {distro}: {err:?}"))
        .ok()?;
    let cwd = output.lines().next()?.trim();
    cwd.starts_with('/').then(|| cwd.to_string())
}

/// Split `/mnt/<drive>[/tail]` into the drive letter and the remaining path.
/// Only single-letter mounts count: `/mnt/data` is an ordinary directory.
/// Recover the Linux path from a UNC whose host is *not* a WSL share.
///
/// A shell inside a distro that emits OSC 7 reports `file://<host>/home/me/p`,
/// and on Windows `Url::to_file_path` turns that into `\\<host>\home\me\p` --
/// a UNC naming the machine, not `wsl.localhost`. [`windows_to_wsl`] rightly
/// refuses it, which left the pane's cwd untranslated and meant a WSL agent
/// never bound to its pane.
///
/// Only meaningful for a pane already known to live in a WSL domain; the caller
/// owns that check. A genuine network share (`\\server\share\dir`) would be
/// mistranslated, but the result is only ever compared against a session's cwd,
/// so the worst case is the same failure to bind as before.
pub(crate) fn unc_host_path_to_linux(win_path: &str) -> Option<String> {
    let path = win_path.trim();
    let rest = path.strip_prefix(r"\\")?;
    // A WSL share is `windows_to_wsl`'s job, and it knows about distro names.
    if UNC_PREFIXES
        .iter()
        .any(|prefix| strip_prefix_ignore_ascii_case(path, prefix).is_some())
    {
        return None;
    }
    // Drop the host component; what follows is the absolute path inside it.
    let tail = rest.split_once(['\\', '/']).map(|(_, tail)| tail)?;
    if tail.is_empty() {
        return None;
    }
    Some(format!("/{}", tail.replace('\\', "/")))
}

/// The UNC path of a distro's `/home`, whose children are candidate users.
///
/// The agent CLIs are usually installed inside the distro and write their
/// session files to the distro's home, which the Windows home knows nothing
/// about; this is where the herd scan has to look for them.
pub(crate) fn wsl_home_base(distro: &str) -> Option<PathBuf> {
    wsl_to_windows("/home", distro)
}

/// The UNC path of one WSL user's home directory.
///
/// `root` is special: its home is `/root`, not `/home/root`. Several distro
/// images default to root, and `wsl -u root` is common, so treating it like any
/// other user pointed at a directory that does not exist and made those agents
/// invisible.
///
/// The user name reaches a path join, so a separator in it would escape the
/// distro's `/home` entirely; such a name is refused rather than sanitised.
pub(crate) fn wsl_home(distro: &str, user: &str) -> Option<PathBuf> {
    let user = user.trim();
    if user.is_empty() || user.contains('/') || user.contains('\\') {
        return None;
    }
    if user == "root" {
        return wsl_root_home(distro);
    }
    wsl_to_windows(&format!("/home/{user}"), distro)
}

/// The UNC path of a distro's `/root`, the home of a root-default distro.
pub(crate) fn wsl_root_home(distro: &str) -> Option<PathBuf> {
    wsl_to_windows("/root", distro)
}

fn strip_mnt_drive(path: &str) -> Option<(char, &str)> {
    let rest = path.strip_prefix("/mnt/")?;
    let (drive, tail) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => (rest, ""),
    };
    let mut chars = drive.chars();
    let letter = chars.next()?;
    if chars.next().is_some() || !letter.is_ascii_alphabetic() {
        return None;
    }
    Some((letter, tail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_with_no_answer_keeps_the_last_one() {
        let last = std::collections::HashMap::from([(
            "claude".to_string(),
            WslProbeShell::BashInteractive,
        )]);
        assert_eq!(answer_or_last_known(None, Some(last.clone())), last);
        // A real answer, even an empty one, is news and replaces it.
        let empty = std::collections::HashMap::new();
        assert!(answer_or_last_known(Some(empty), Some(last)).is_empty());
        assert!(answer_or_last_known(None, None).is_empty());
    }

    #[test]
    fn session_distros_are_bounded_and_written_only_when_they_grow() {
        let mut distros = SessionDistros::default();
        distros.remember(["Ubuntu".to_string()]);
        // Not read from the file yet: writing now would drop what it holds.
        assert_eq!(distros.unsaved(), None);

        distros.saved = Some(BTreeSet::from(["Ubuntu".to_string()]));
        assert_eq!(distros.unsaved(), None);
        distros.remember(["Ubuntu".to_string(), String::new(), "Debian".to_string()]);
        assert_eq!(
            distros.unsaved(),
            Some(BTreeSet::from(["Debian".to_string(), "Ubuntu".to_string()]))
        );

        distros.remember((0..40).map(|n| format!("distro-{n}")));
        assert_eq!(distros.known.len(), MAX_SESSION_DISTROS);
        assert!(distros.known.contains("Ubuntu") && distros.known.contains("Debian"));
    }

    fn distro(name: &str, state: &str, is_default: bool) -> WslDistro {
        WslDistro {
            name: name.to_string(),
            state: state.to_string(),
            version: "2".to_string(),
            is_default,
        }
    }

    #[test]
    fn non_ascii_paths_never_split_a_character() {
        // Byte 16 of this path falls inside the `ü`, exactly where the
        // `\\wsl.localhost\` prefix check used to slice.
        let path = r"C:\Users\Jens.Müller\src";
        assert!(!path.is_char_boundary(16));
        assert_eq!(
            windows_to_wsl(path, "Ubuntu").as_deref(),
            Some("/mnt/c/Users/Jens.Müller/src")
        );
        assert_eq!(unc_host_path_to_linux(path), None);
        assert_eq!(
            unc_host_path_to_linux(r"\\hö\home\jürgen").as_deref(),
            Some("/home/jürgen")
        );
        assert_eq!(windows_to_wsl("ü", "Ubuntu"), None);
        assert_eq!(windows_to_wsl(r"\\wsl.localhöst\x", "Ubuntu"), None);
    }

    #[test]
    fn probe_output_only_counts_marked_lines_for_asked_programs() {
        let programs = vec!["claude".to_string(), "codex".to_string()];
        let output =
            "welcome from .bashrc\nclaude\ntgz-found:claude\ntgz-found:rm\n  tgz-found:codex\n";
        assert_eq!(
            parse_wsl_probe_output(output, &programs),
            vec!["claude", "codex"]
        );
    }

    #[test]
    fn probe_order_is_default_then_running_and_skips_docker_and_stopped() {
        let distros = vec![
            distro("docker-desktop", "Running", false),
            distro("Debian", "Stopped", false),
            distro("Arch", "Running", false),
            distro("Ubuntu", "Stopped", true),
        ];
        let names: Vec<&str> = wsl_probe_order(&distros)
            .into_iter()
            .map(|distro| distro.name.as_str())
            .collect();
        assert_eq!(names, vec!["Ubuntu", "Arch"]);
    }

    #[test]
    fn wrapped_commands_pass_arguments_positionally() {
        assert_eq!(
            WslProbeShell::BashInteractive.wrap(vec![
                "claude".into(),
                "--resume".into(),
                "a b".into()
            ]),
            vec![
                "bash",
                "-lic",
                r#"exec "$0" "$@""#,
                "claude",
                "--resume",
                "a b"
            ]
        );
        assert_eq!(
            WslProbeShell::ShLogin.wrap(vec!["codex".into()])[..2],
            ["sh", "-lc"]
        );
    }

    #[test]
    fn mnt_drive_becomes_a_windows_drive_path() {
        assert_eq!(
            wsl_to_windows("/mnt/c/Users/tim/proj", "Ubuntu"),
            Some(PathBuf::from(r"C:\Users\tim\proj"))
        );
        // Lowercase drive letters are uppercased, matching Windows convention.
        assert_eq!(
            wsl_to_windows("/mnt/d/data", "Ubuntu"),
            Some(PathBuf::from(r"D:\data"))
        );
    }

    #[test]
    fn mnt_drive_root_keeps_its_trailing_separator() {
        // `C:` without a separator means "current dir on C:", not the root.
        assert_eq!(
            wsl_to_windows("/mnt/c", "Ubuntu"),
            Some(PathBuf::from(r"C:\"))
        );
        assert_eq!(
            wsl_to_windows("/mnt/c/", "Ubuntu"),
            Some(PathBuf::from(r"C:\"))
        );
    }

    #[test]
    fn multi_letter_mnt_entry_is_not_a_drive() {
        // /mnt/data is a normal directory inside the distro, not a mount.
        assert_eq!(
            wsl_to_windows("/mnt/data/x", "Ubuntu"),
            Some(PathBuf::from(r"\\wsl.localhost\Ubuntu\mnt\data\x"))
        );
    }

    #[test]
    fn distro_internal_path_becomes_a_unc_path() {
        assert_eq!(
            wsl_to_windows("/home/tim/proj", "Ubuntu-22.04"),
            Some(PathBuf::from(r"\\wsl.localhost\Ubuntu-22.04\home\tim\proj"))
        );
    }

    #[test]
    fn relative_paths_and_blank_distros_do_not_translate() {
        assert_eq!(wsl_to_windows("relative/path", "Ubuntu"), None);
        assert_eq!(wsl_to_windows("", "Ubuntu"), None);
        assert_eq!(wsl_to_windows("/home/tim", "  "), None);
    }

    #[test]
    fn unc_path_maps_back_to_a_linux_path() {
        assert_eq!(
            windows_to_wsl(r"\\wsl.localhost\Ubuntu\home\tim", "Ubuntu"),
            Some("/home/tim".to_string())
        );
        // The older \\wsl$\ share form is still in wide use.
        assert_eq!(
            windows_to_wsl(r"\\wsl$\Ubuntu\home\tim", "Ubuntu"),
            Some("/home/tim".to_string())
        );
        // Share and distro names compare case-insensitively on Windows.
        assert_eq!(
            windows_to_wsl(r"\\WSL.LOCALHOST\ubuntu\home", "Ubuntu"),
            Some("/home".to_string())
        );
    }

    #[test]
    fn a_wsl_share_path_names_its_distro() {
        assert_eq!(
            distro_of_unc(r"\\wsl.localhost\Ubuntu\home\tim\proj").as_deref(),
            Some("Ubuntu")
        );
        assert_eq!(distro_of_unc(r"\\WSL$\Debian").as_deref(), Some("Debian"));
        assert_eq!(distro_of_unc(r"C:\Users\tim"), None);
        assert_eq!(distro_of_unc(r"\\server\share\dir"), None);
        assert_eq!(distro_of_unc(r"\\wsl.localhost\"), None);
    }

    #[test]
    fn unc_path_for_another_distro_is_rejected() {
        // Debian's /home is not reachable from inside Ubuntu.
        assert_eq!(
            windows_to_wsl(r"\\wsl.localhost\Debian\home\tim", "Ubuntu"),
            None
        );
    }

    #[test]
    fn drive_path_maps_to_mnt() {
        assert_eq!(
            windows_to_wsl(r"C:\Users\tim", "Ubuntu"),
            Some("/mnt/c/Users/tim".to_string())
        );
        // Forward slashes appear when the path came from a file:// URL.
        assert_eq!(
            windows_to_wsl("C:/Users/tim", "Ubuntu"),
            Some("/mnt/c/Users/tim".to_string())
        );
        assert_eq!(windows_to_wsl(r"C:\", "Ubuntu"), Some("/mnt/c".to_string()));
        assert_eq!(windows_to_wsl("C:", "Ubuntu"), Some("/mnt/c".to_string()));
    }

    #[test]
    fn unmappable_windows_paths_return_none() {
        assert_eq!(windows_to_wsl(r"\\server\share\x", "Ubuntu"), None);
        assert_eq!(windows_to_wsl("relative\\path", "Ubuntu"), None);
        assert_eq!(windows_to_wsl("", "Ubuntu"), None);
    }

    #[test]
    fn drive_paths_round_trip_both_ways() {
        let linux = "/mnt/c/Users/tim/proj";
        let win = wsl_to_windows(linux, "Ubuntu").unwrap();
        assert_eq!(
            windows_to_wsl(&win.to_string_lossy(), "Ubuntu"),
            Some(linux.to_string())
        );
    }

    #[test]
    fn distro_paths_round_trip_both_ways() {
        let linux = "/home/tim/proj";
        let win = wsl_to_windows(linux, "Ubuntu").unwrap();
        assert_eq!(
            windows_to_wsl(&win.to_string_lossy(), "Ubuntu"),
            Some(linux.to_string())
        );
    }

    #[test]
    fn wsl_home_paths_are_unc() {
        assert_eq!(
            wsl_home_base("Ubuntu"),
            Some(PathBuf::from(r"\\wsl.localhost\Ubuntu\home"))
        );
        assert_eq!(
            wsl_home("Ubuntu", "tim"),
            Some(PathBuf::from(r"\\wsl.localhost\Ubuntu\home\tim"))
        );
    }

    #[test]
    fn wsl_home_refuses_a_user_that_is_really_a_path() {
        assert_eq!(wsl_home("Ubuntu", "../../etc"), None);
        assert_eq!(wsl_home("Ubuntu", r"a\b"), None);
        assert_eq!(wsl_home("Ubuntu", ""), None);
        assert_eq!(wsl_home("", "tim"), None);
    }

    #[test]
    fn an_osc7_unc_naming_the_machine_still_yields_a_linux_path() {
        // What `file:///home/me/proj` emitted inside a distro becomes on the
        // Windows side once `Url::to_file_path` has resolved it.
        assert_eq!(
            unc_host_path_to_linux(r"\\DESKTOP-ABC\home\me\proj"),
            Some("/home/me/proj".to_string())
        );
    }

    #[test]
    fn a_wsl_share_is_left_to_windows_to_wsl() {
        // That form carries a distro name, which only `windows_to_wsl` can check.
        assert_eq!(
            unc_host_path_to_linux(r"\\wsl.localhost\Ubuntu\home\me"),
            None
        );
        assert_eq!(unc_host_path_to_linux(r"\\wsl$\Ubuntu\home\me"), None);
    }

    #[test]
    fn a_non_unc_path_is_not_a_host_path() {
        assert_eq!(unc_host_path_to_linux(r"C:\Users\me"), None);
        assert_eq!(unc_host_path_to_linux("/home/me"), None);
        assert_eq!(unc_host_path_to_linux(r"\\DESKTOP-ABC"), None);
    }

    #[test]
    fn root_lives_at_slash_root_not_under_home() {
        // /home/root does not exist; a distro running as root keeps its agent
        // state in /root.
        assert_eq!(
            wsl_home("Ubuntu", "root"),
            Some(PathBuf::from(r"\\wsl.localhost\Ubuntu\root"))
        );
        assert_eq!(
            wsl_root_home("Ubuntu"),
            Some(PathBuf::from(r"\\wsl.localhost\Ubuntu\root"))
        );
    }
}
