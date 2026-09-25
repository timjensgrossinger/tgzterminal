use crate::config::validate_domain_name;
use crate::*;
use luahelper::impl_lua_conversion_dynamic;
use wezterm_dynamic::{FromDynamic, ToDynamic};

#[derive(Default, Debug, Clone, FromDynamic, ToDynamic)]
pub struct WslDomain {
    #[dynamic(validate = "validate_domain_name")]
    pub name: String,
    pub distribution: Option<String>,
    pub username: Option<String>,
    pub default_cwd: Option<PathBuf>,
    pub default_prog: Option<Vec<String>>,
}
impl_lua_conversion_dynamic!(WslDomain);

impl WslDomain {
    /// One built-in domain per registered distro.
    ///
    /// Read from the registry, never from `wsl.exe`. Upstream `LocalDomain`
    /// reaches this through `Config::wsl_domains()` twice per spawn, and
    /// `setup_mux` calls it on the GUI thread before any window exists. Asking
    /// `wsl -l -v` there cost a console window per call, seconds of blocking
    /// whenever the WSL service was waking up (a startup that looked like it
    /// never happened), and a failure cached as "no distros" dropped every WSL
    /// domain for the rest of the session.
    pub fn default_domains() -> Vec<WslDomain> {
        Self::domains_from_distros(&WslDistro::registered_distros().unwrap_or_default())
    }

    /// The domains `default_domains` builds, from an already-loaded distro
    /// list.
    pub fn domains_from_distros(distros: &[WslDistro]) -> Vec<WslDomain> {
        distros
            .iter()
            .map(|distro| WslDomain {
                name: format!("WSL:{}", distro.name),
                distribution: Some(distro.name.clone()),
                username: None,
                default_cwd: Some("~".into()),
                default_prog: None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslDistro {
    pub name: String,
    /// `Running` / `Stopped` when known. The registry does not record it, so
    /// a list from [`WslDistro::registered_distros`] leaves this empty until
    /// [`WslDistro::mark_running`] fills it in.
    pub state: String,
    pub version: String,
    pub is_default: bool,
}

/// State strings [`WslDistro::mark_running`] writes. English on purpose:
/// `wsl -l -v` translates its own, so they cannot be compared against.
pub const WSL_STATE_RUNNING: &str = "Running";
pub const WSL_STATE_STOPPED: &str = "Stopped";

impl WslDistro {
    /// The registered distros, default first.
    ///
    /// Reads the registry, where WSL records every distro it has registered
    /// for the current user: instant, spawns nothing, starts no VM and does
    /// not depend on the display language. Falls back to parsing `wsl -l -v`
    /// only when that key cannot be read for a reason other than "absent".
    /// Blocking in that fallback, so call it from a worker thread.
    pub fn load_distro_list() -> anyhow::Result<Vec<Self>> {
        match Self::registered_distros() {
            Ok(distros) => Ok(distros),
            Err(err) => {
                log::debug!("WSL registry unreadable ({err:#}); asking wsl.exe");
                Self::load_distro_list_from_wsl_exe()
            }
        }
    }

    /// The registered distros according to the registry alone. An absent
    /// key means WSL has no distros for this user, which is not an error.
    pub fn registered_distros() -> anyhow::Result<Vec<Self>> {
        #[cfg(windows)]
        {
            registry::registered_distros()
        }
        #[cfg(not(windows))]
        {
            Ok(vec![])
        }
    }

    /// Names of the distros that are running now, from
    /// `wsl.exe -l --running -q` (names only, so nothing localized to parse).
    /// Blocking: a hidden, time-bounded `wsl.exe`. Call from a worker thread.
    pub fn running_distro_names() -> anyhow::Result<Vec<String>> {
        let mut cmd = hidden_wsl_command();
        cmd.args(["-l", "--running", "-q"]);
        let output = output_with_timeout(cmd, WSL_COMMAND_TIMEOUT)?;
        // "No running distributions" is a (translated) message and a failure
        // status, not an error worth reporting.
        if !output.status.success() {
            return Ok(vec![]);
        }
        Ok(decode_wsl_output(&output.stdout)
            .lines()
            .map(|line| line.trim().trim_start_matches('*').trim().to_string())
            .filter(|name| !name.is_empty())
            .collect())
    }

    /// Fill in `state` from the names [`WslDistro::running_distro_names`]
    /// reported. Names that match no registered distro (such as a translated
    /// "nothing is running" line) are simply ignored.
    pub fn mark_running(distros: &mut [Self], running: &[String]) {
        for distro in distros {
            let is_running = running
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&distro.name));
            distro.state = if is_running {
                WSL_STATE_RUNNING
            } else {
                WSL_STATE_STOPPED
            }
            .to_string();
        }
    }

    /// Whether [`WslDistro::mark_running`] saw this distro running.
    pub fn is_running(&self) -> bool {
        self.state.eq_ignore_ascii_case(WSL_STATE_RUNNING)
    }

    fn load_distro_list_from_wsl_exe() -> anyhow::Result<Vec<Self>> {
        let mut cmd = hidden_wsl_command();
        cmd.args(["-l", "-v"]);
        let output = output_with_timeout(cmd, WSL_COMMAND_TIMEOUT)?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::ensure!(
            output.status.success(),
            "wsl -l command invocation failed: {}",
            stderr
        );

        let wsl_list = decode_wsl_output(&output.stdout).replace("\r\n", "\n");

        Ok(parse_wsl_distro_list(&wsl_list))
    }
}

#[cfg(windows)]
mod registry {
    use super::WslDistro;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    /// Where WSL registers each distro for the current user: one subkey per
    /// distro GUID, plus a `DefaultDistribution` value naming the default.
    const LXSS_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

    pub(super) fn registered_distros() -> anyhow::Result<Vec<WslDistro>> {
        let lxss = match RegKey::predef(HKEY_CURRENT_USER).open_subkey(LXSS_KEY) {
            Ok(key) => key,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(err) => return Err(err.into()),
        };
        let default_guid: Option<String> = lxss.get_value("DefaultDistribution").ok();
        let mut entries = vec![];
        for guid in lxss.enum_keys().flatten() {
            let Ok(key) = lxss.open_subkey(&guid) else {
                continue;
            };
            entries.push(super::RegisteredDistro {
                guid,
                name: key.get_value("DistributionName").ok(),
                version: key.get_value("Version").ok(),
                state: key.get_value("State").ok(),
            });
        }
        Ok(super::distros_from_registry(
            default_guid.as_deref(),
            entries,
        ))
    }
}

/// One `Lxss\{guid}` subkey, as read from the registry.
#[cfg_attr(not(windows), allow(dead_code))]
struct RegisteredDistro {
    guid: String,
    name: Option<String>,
    version: Option<u32>,
    state: Option<u32>,
}

/// The registry's `State` for a fully registered distro. Others are
/// transient (installing, uninstalling, converting) and are not usable.
#[cfg_attr(not(windows), allow(dead_code))]
const LXSS_STATE_INSTALLED: u32 = 1;

#[cfg_attr(not(windows), allow(dead_code))]
fn distros_from_registry(
    default_guid: Option<&str>,
    entries: Vec<RegisteredDistro>,
) -> Vec<WslDistro> {
    let mut distros: Vec<WslDistro> = entries
        .into_iter()
        .filter(|entry| {
            entry
                .state
                .map_or(true, |state| state == LXSS_STATE_INSTALLED)
        })
        .filter_map(
            |RegisteredDistro {
                 guid,
                 name,
                 version,
                 ..
             }| {
                let name = name?.trim().to_string();
                if name.is_empty() {
                    return None;
                }
                let is_default =
                    default_guid.is_some_and(|default| default.eq_ignore_ascii_case(&guid));
                Some(WslDistro {
                    name,
                    state: String::new(),
                    version: version.map(|v| v.to_string()).unwrap_or_default(),
                    is_default,
                })
            },
        )
        .collect();
    // Registry enumeration order is GUID order, which means nothing: put the
    // default first, as `wsl -l` does, and the rest by name.
    distros.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    distros
}

/// Upper bound on one `wsl.exe` invocation. It normally answers in well under
/// a second; a WSL service that is starting, updating or wedged can take
/// forever.
pub const WSL_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// `wsl.exe` with no console window, an empty stdin and captured output.
///
/// A bare `Command::new("wsl.exe")` from a GUI process allocates a visible
/// console per call.
///
/// Stdin is a pipe that [`output_with_timeout`] closes at once, *not*
/// `Stdio::null()`: `NUL` is a character device, which is what a console
/// looks like, and a hidden `wsl.exe --exec` given it as stdin never returned
/// -- every WSL agent probe timed out while the same command with a closed
/// pipe answered in a tenth of a second.
pub fn hidden_wsl_command() -> std::process::Command {
    let mut cmd = std::process::Command::new("wsl.exe");
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// What [`output_with_timeout`] fails with when it had to kill the child, so
/// callers can tell "no answer" apart from "answered with an error".
#[derive(Debug)]
pub struct WslCommandTimedOut(pub std::time::Duration);

impl std::fmt::Display for WslCommandTimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "wsl.exe did not answer within {:?}", self.0)
    }
}

impl std::error::Error for WslCommandTimedOut {}

/// How long to wait for a child's output pipes to close once the child itself
/// has exited. A process it started (WSL's own helpers) can inherit and hold
/// them for as long as it lives.
const PIPE_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// `cmd.output()`, but bounded: the child is killed once `timeout` passes,
/// and output still unread [`PIPE_DRAIN_GRACE`] after it exits is abandoned
/// rather than waited for forever.
pub fn output_with_timeout(
    mut cmd: std::process::Command,
    timeout: std::time::Duration,
) -> anyhow::Result<std::process::Output> {
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::Instant;

    let mut child = cmd.spawn()?;
    // End of input straight away (see `hidden_wsl_command`).
    drop(child.stdin.take());
    // Drained on threads so a full pipe cannot stall the child while it is
    // being polled. Results come back over channels so waiting for them can
    // be given up on; a reader stuck on an inherited pipe just exits with the
    // process later.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(WslCommandTimedOut(timeout).into());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    let grace_ends = Instant::now() + PIPE_DRAIN_GRACE;
    let collect = |rx: mpsc::Receiver<Vec<u8>>| {
        rx.recv_timeout(grace_ends.saturating_duration_since(Instant::now()))
            .unwrap_or_default()
    };
    Ok(std::process::Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

/// Text `wsl.exe` printed about itself (not a Linux program's output, which
/// is passed through as bytes).
///
/// That is UTF-16LE (<https://github.com/microsoft/WSL/issues/4456>), unless
/// the user set `WSL_UTF8=1`, in which case it is UTF-8; tell them apart by
/// the NUL high bytes ASCII text has in UTF-16.
pub fn decode_wsl_output(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xff, 0xfe]).unwrap_or(bytes);
    let looks_utf16 = bytes.len() >= 2
        && bytes.len() % 2 == 0
        && bytes.iter().skip(1).step_by(2).filter(|b| **b == 0).count() * 2 >= bytes.len() / 2;
    if looks_utf16 {
        if let Ok(text) = utf16_to_utf8(bytes) {
            return text;
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Decoded pairwise rather than by reinterpreting the byte buffer as `&[u16]`:
/// a `Vec<u8>` carries no 2-byte alignment guarantee, so that cast was
/// undefined behaviour.
fn utf16_to_utf8(bytes: &[u8]) -> anyhow::Result<String> {
    if bytes.len() % 2 != 0 {
        anyhow::bail!("input data has odd length, cannot be utf16");
    }
    let wide: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&wide).map_err(|_| anyhow!("wsl -l -v output is not valid utf16"))
}

/// This function parses the `wsl -l -v` output.
///
/// Columns are taken by position -- name, state (everything between), version
/// (last) -- because the headers are translated: a German `wsl -l -v` does not
/// say `STATE`, so looking the columns up by their English labels found no
/// names at all. Positions are counted in characters, not bytes, since a
/// translated state such as `Wird ausgeführt` shifts every later byte offset.
fn parse_wsl_distro_list(output: &str) -> Vec<WslDistro> {
    let lines = output.lines().collect::<Vec<_>>();
    // Empty output (no distros, or a WSL build that prints nothing) used to
    // panic here on `lines[0]`, and this runs on the GUI thread.
    let Some(header) = lines.first().copied() else {
        return vec![];
    };

    // Determine where the field columns start, in characters.
    let mut column_starts = vec![];
    {
        let mut last_char = ' ';
        for (idx, c) in header.chars().enumerate() {
            if last_char == ' ' && c != ' ' {
                column_starts.push(idx);
            }
            last_char = c;
        }
    }
    let Some(&name_start) = column_starts.first() else {
        return vec![];
    };

    let mut result = vec![];

    for line in lines.iter().skip(1) {
        if line.trim().is_empty() {
            continue;
        }

        let is_default = line.trim_start().starts_with('*');
        let chars: Vec<char> = line.chars().collect();
        let column = |index: usize| -> String {
            let start = column_starts[index].min(chars.len());
            let end = column_starts
                .get(index + 1)
                .copied()
                .unwrap_or(chars.len())
                .min(chars.len());
            chars[start..end]
                .iter()
                .collect::<String>()
                .trim()
                .to_string()
        };

        // A row that ends before the second column is truncated output.
        if column_starts.len() >= 2 && chars.len() <= column_starts[1] {
            break;
        }
        let name = {
            let end = column_starts
                .get(1)
                .copied()
                .unwrap_or(chars.len())
                .min(chars.len());
            chars[name_start.min(end)..end]
                .iter()
                .collect::<String>()
                .trim()
                .to_string()
        };
        if name.is_empty() {
            continue;
        }
        let (state, version) = match column_starts.len() {
            0 | 1 => (String::new(), String::new()),
            2 => (column(1), String::new()),
            n => {
                let state_end = column_starts[n - 1].min(chars.len());
                let state_start = column_starts[1].min(state_end);
                (
                    chars[state_start..state_end]
                        .iter()
                        .collect::<String>()
                        .trim()
                        .to_string(),
                    column(n - 1),
                )
            }
        };

        result.push(WslDistro {
            name,
            state,
            version,
            is_default,
        });
    }

    result
}

#[cfg(test)]
#[test]
fn test_parse_wsl_distro_list() {
    let data = "  NAME                   STATE           VERSION
* Arch                   Running         2
  docker-desktop-data    Stopped         2
  docker-desktop         Stopped         2
  Ubuntu                 Stopped         2
  nvim                   Stopped         2";

    assert_eq!(
        parse_wsl_distro_list(data),
        vec![
            WslDistro {
                name: "Arch".to_string(),
                state: "Running".to_string(),
                version: "2".to_string(),
                is_default: true
            },
            WslDistro {
                name: "docker-desktop-data".to_string(),
                state: "Stopped".to_string(),
                version: "2".to_string(),
                is_default: false
            },
            WslDistro {
                name: "docker-desktop".to_string(),
                state: "Stopped".to_string(),
                version: "2".to_string(),
                is_default: false
            },
            WslDistro {
                name: "Ubuntu".to_string(),
                state: "Stopped".to_string(),
                version: "2".to_string(),
                is_default: false
            },
            WslDistro {
                name: "nvim".to_string(),
                state: "Stopped".to_string(),
                version: "2".to_string(),
                is_default: false
            },
        ]
    );
}

#[cfg(test)]
#[test]
fn parse_wsl_distro_list_tolerates_empty_and_truncated_output() {
    assert_eq!(parse_wsl_distro_list(""), vec![]);
    assert_eq!(parse_wsl_distro_list("  NAME   STATE"), vec![]);
    // A row shorter than the header stops parsing instead of panicking.
    assert_eq!(
        parse_wsl_distro_list("  NAME                   STATE           VERSION\n* Arch"),
        vec![]
    );
}

#[cfg(test)]
#[test]
fn utf16_to_utf8_decodes_little_endian_pairs() {
    let bytes: Vec<u8> = "NAME".encode_utf16().flat_map(u16::to_le_bytes).collect();
    assert_eq!(utf16_to_utf8(&bytes).unwrap(), "NAME");
    assert!(utf16_to_utf8(&[0x41]).is_err());
}

#[cfg(test)]
#[test]
fn parse_wsl_distro_list_reads_translated_headers_by_position() {
    // A German `wsl -l -v`: translated headers, a multi-word state with a
    // non-ASCII character in it.
    let data = "  NAME            STATUS             VERSION\n\
                * Ubuntu          Wird ausgeführt    2\n  \
                Debian          Beendet            2";
    assert_eq!(
        parse_wsl_distro_list(data),
        vec![
            WslDistro {
                name: "Ubuntu".to_string(),
                state: "Wird ausgeführt".to_string(),
                version: "2".to_string(),
                is_default: true
            },
            WslDistro {
                name: "Debian".to_string(),
                state: "Beendet".to_string(),
                version: "2".to_string(),
                is_default: false
            },
        ]
    );
}

#[cfg(test)]
#[test]
fn registry_entries_become_distros_default_first() {
    let entry = |guid: &str, name: Option<&str>, state: Option<u32>| RegisteredDistro {
        guid: guid.to_string(),
        name: name.map(str::to_string),
        version: Some(2),
        state,
    };
    let distros = distros_from_registry(
        Some("{B}"),
        vec![
            entry("{A}", Some("docker-desktop"), Some(1)),
            entry("{b}", Some("Ubuntu"), Some(1)),
            entry("{C}", Some("Arch"), None),
            entry("{D}", Some("Half-installed"), Some(3)),
            entry("{E}", None, Some(1)),
            entry("{F}", Some("  "), Some(1)),
        ],
    );
    let names: Vec<(&str, bool)> = distros
        .iter()
        .map(|d| (d.name.as_str(), d.is_default))
        .collect();
    assert_eq!(
        names,
        vec![("Ubuntu", true), ("Arch", false), ("docker-desktop", false)]
    );
    assert!(distros
        .iter()
        .all(|d| d.version == "2" && d.state.is_empty()));
}

#[cfg(test)]
#[test]
fn running_names_mark_state_case_insensitively() {
    let mut distros = vec![
        WslDistro {
            name: "Ubuntu".into(),
            state: String::new(),
            version: "2".into(),
            is_default: true,
        },
        WslDistro {
            name: "Debian".into(),
            state: String::new(),
            version: "2".into(),
            is_default: false,
        },
    ];
    WslDistro::mark_running(
        &mut distros,
        &[
            "ubuntu".to_string(),
            "Es werden keine Distributionen ausgeführt.".to_string(),
        ],
    );
    assert!(distros[0].is_running());
    assert!(!distros[1].is_running());
    assert_eq!(distros[1].state, WSL_STATE_STOPPED);
}

#[cfg(test)]
#[test]
fn wsl_output_is_decoded_as_utf16_or_utf8() {
    let utf16: Vec<u8> = "Ubuntu\r\nDebian\r\n"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    assert_eq!(decode_wsl_output(&utf16), "Ubuntu\r\nDebian\r\n");
    let mut with_bom = vec![0xff, 0xfe];
    with_bom.extend_from_slice(&utf16);
    assert_eq!(decode_wsl_output(&with_bom), "Ubuntu\r\nDebian\r\n");
    // WSL_UTF8=1
    assert_eq!(decode_wsl_output("Ubuntu\n".as_bytes()), "Ubuntu\n");
    assert_eq!(decode_wsl_output(b""), "");
}
