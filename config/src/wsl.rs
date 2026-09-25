use crate::config::validate_domain_name;
use crate::*;
use luahelper::impl_lua_conversion_dynamic;
use std::collections::HashMap;
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
    /// Memoized: this is reached through `Config::wsl_domains()`, which
    /// upstream `LocalDomain` calls twice per spawn — for every local pane,
    /// WSL or not — and listing distros means spawning `wsl.exe -l -v`. Only
    /// the very first call blocks (bounded by the loader's timeout); after
    /// that a stale list is served while one background thread refreshes it,
    /// so a hung WSL service can no longer stall a spawn.
    pub fn default_domains() -> Vec<WslDomain> {
        #[cfg(windows)]
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            use std::sync::Mutex;
            use std::time::{Duration, Instant};
            const DEFAULT_DOMAINS_TTL: Duration = Duration::from_secs(30);
            static CACHE: Mutex<Option<(Instant, Vec<WslDistro>)>> = Mutex::new(None);
            static REFRESHING: AtomicBool = AtomicBool::new(false);

            fn store(distros: Vec<WslDistro>) {
                *CACHE.lock().unwrap() = Some((Instant::now(), distros));
            }

            let cached = CACHE.lock().unwrap().clone();
            match cached {
                Some((loaded_at, distros)) => {
                    if loaded_at.elapsed() >= DEFAULT_DOMAINS_TTL
                        && !REFRESHING.swap(true, Ordering::AcqRel)
                    {
                        let spawned = std::thread::Builder::new()
                            .name("wsl-default-domains".into())
                            .spawn(|| {
                                // A failure is remembered too (as no distros),
                                // so a machine without WSL does not respawn
                                // `wsl.exe` on every call.
                                store(WslDistro::load_distro_list().unwrap_or_default());
                                REFRESHING.store(false, Ordering::Release);
                            });
                        if spawned.is_err() {
                            REFRESHING.store(false, Ordering::Release);
                        }
                    }
                    Self::domains_from_distros(&distros)
                }
                None => {
                    // Not holding the lock across the spawn: concurrent first
                    // callers may each load once, which is harmless.
                    let distros = WslDistro::load_distro_list().unwrap_or_default();
                    let domains = Self::domains_from_distros(&distros);
                    store(distros);
                    domains
                }
            }
        }

        #[cfg(not(windows))]
        {
            vec![]
        }
    }

    /// The domains `default_domains` builds, from an already-loaded distro
    /// list. `default_domains` spawns `wsl.exe` on every call; callers that
    /// cache the list use this to derive the same domains without a spawn.
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
    pub state: String,
    pub version: String,
    pub is_default: bool,
}

impl WslDistro {
    pub fn load_distro_list() -> anyhow::Result<Vec<Self>> {
        #[cfg(windows)]
        use std::os::windows::process::CommandExt;
        #[cfg(windows)]
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

        let mut cmd = std::process::Command::new("wsl.exe");
        cmd.arg("-l");
        cmd.arg("-v");
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let output = output_with_timeout(cmd, LOAD_DISTRO_LIST_TIMEOUT)?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::ensure!(
            output.status.success(),
            "wsl -l command invocation failed: {}",
            stderr
        );

        let wsl_list = utf16_to_utf8(&output.stdout)?.replace("\r\n", "\n");

        Ok(parse_wsl_distro_list(&wsl_list))
    }
}

/// `wsl -l -v` normally answers in well under a second; a WSL service that
/// is wedged can take forever, and this runs wherever `default_domains` is
/// first asked — including a pane spawn.
const LOAD_DISTRO_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// `cmd.output()`, but the child is killed once `timeout` passes.
fn output_with_timeout(
    mut cmd: std::process::Command,
    timeout: std::time::Duration,
) -> anyhow::Result<std::process::Output> {
    use std::io::Read;
    use std::time::Instant;

    let mut child = cmd.spawn()?;
    // Drained on threads so a full pipe cannot stall the child while it is
    // being polled.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
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
            // The readers are not joined: a process the killed child left
            // behind may still hold the pipes open.
            anyhow::bail!("wsl.exe did not answer within {timeout:?}");
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    Ok(std::process::Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Ungh: https://github.com/microsoft/WSL/issues/4456
///
/// Decoded pairwise rather than by reinterpreting the byte buffer as `&[u16]`:
/// a `Vec<u8>` carries no 2-byte alignment guarantee, so that cast was
/// undefined behaviour.
#[allow(dead_code)]
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
/// It tries to be robust in the face of future changes
/// by looking at the tabulated output headers, determining
/// where the columns are and then collecting the information
/// into a hashmap and then grokking from there.
#[allow(dead_code)]
fn parse_wsl_distro_list(output: &str) -> Vec<WslDistro> {
    let lines = output.lines().collect::<Vec<_>>();
    // Empty output (no distros, or a WSL build that prints nothing) used to
    // panic here on `lines[0]`, and this runs on the GUI thread.
    let Some(header) = lines.first().copied() else {
        return vec![];
    };

    // Determine where the field columns start
    let mut field_starts = vec![];
    {
        let mut last_char = ' ';
        for (idx, c) in header.char_indices() {
            if last_char == ' ' && c != ' ' {
                field_starts.push(idx);
            }
            last_char = c;
        }
    }

    fn opt_field_slice(s: &str, start: usize, end: Option<usize>) -> Option<&str> {
        if let Some(end) = end {
            s.get(start..end)
        } else {
            s.get(start..)
        }
    }

    // Now build up a name -> column position map
    let mut field_map = HashMap::new();
    {
        let mut iter = field_starts.into_iter().peekable();

        while let Some(start_idx) = iter.next() {
            let end_idx = iter.peek().copied();
            let Some(label) = opt_field_slice(header, start_idx, end_idx) else {
                continue;
            };
            let label = label.trim();
            field_map.insert(label, (start_idx, end_idx));
        }
    }

    let mut result = vec![];

    // and now process the output rows
    for line in lines.iter().skip(1) {
        if line.is_empty() {
            continue;
        }

        let is_default = line.starts_with("*");

        let mut fields = HashMap::new();
        for (label, (start_idx, end_idx)) in field_map.iter() {
            if let Some(value) = opt_field_slice(line, *start_idx, *end_idx) {
                fields.insert(*label, value.trim().to_string());
            } else {
                return result;
            }
        }

        result.push(WslDistro {
            name: fields.get("NAME").cloned().unwrap_or_default(),
            state: fields.get("STATE").cloned().unwrap_or_default(),
            version: fields.get("VERSION").cloned().unwrap_or_default(),
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
