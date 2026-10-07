//! The host list behind the sidebar's SSH quick-launch dropdown.
//!
//! Three parts:
//!
//! * a process-wide, self-refreshing cache of the hosts in the user's ssh
//!   config, filled by a worker thread so paint never parses those files
//!   (same shape as the WSL distro cache in `wsl_paths`);
//! * pure functions that turn hosts into dropdown rows, order them and filter
//!   them, which is where the unit tests are;
//! * the `TermWindow` glue for the dropdown's keyboard handling.

use crate::termwindow::{SshLaunchMenuState, SshQuickLaunchEntry, UIItemType};
use crate::TermWindow;
use config::keyassignment::{LauncherActionArgs, LauncherFlags};
use config::{ConfigHandle, SshDomain, SshMultiplexing, SshTransport};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use window::{Modifiers, WindowOps};

/// How long a parsed ssh config is trusted. Also the longest an edit to
/// `~/.ssh/config` takes to show up in an open window.
const SSH_HOSTS_TTL: Duration = Duration::from_secs(60);
/// A refresh that has not reported back after this long is presumed dead,
/// so it cannot suppress every later refresh (see `herd_scan_is_due`).
const SSH_HOSTS_WATCHDOG: Duration = Duration::from_secs(30);

/// Recently used hosts kept at the top of the dropdown.
pub(crate) const MAX_RECENT_HOSTS: usize = 8;

/// The dropdown shows a "type to filter" hint once it has this many rows.
const FILTER_HINT_MIN_ROWS: usize = 8;

/// One connectable host.
#[derive(Clone, Debug)]
pub(crate) struct SshHost {
    pub domain: SshDomain,
    /// Derived from the ssh config files rather than declared in Lua. Only
    /// these come in `SSH:`/`SSHMUX:` pairs.
    pub from_ssh_config: bool,
    /// Where the alias really points, when that says more than the alias.
    pub detail: Option<String>,
}

#[derive(Default)]
struct HostCache {
    hosts: Arc<Vec<SshHost>>,
    /// Bumped whenever a refresh changes `hosts`, so a window can tell its
    /// built rows are stale without comparing lists.
    epoch: u64,
    refreshed_at: Option<Instant>,
    refresh_started_at: Option<Instant>,
}

static SSH_HOST_CACHE: LazyLock<Mutex<HostCache>> =
    LazyLock::new(|| Mutex::new(HostCache::default()));

/// `user@hostname:port via jump` for an alias, leaving out whatever the alias
/// already says: the default port, the local user, a hostname equal to the
/// alias. `None` when nothing is left.
pub(crate) fn host_detail(
    alias: &str,
    options: &wezterm_ssh::ConfigMap,
    local_user: Option<&str>,
) -> Option<String> {
    let get = |key: &str| {
        options
            .get(key)
            .map(|s| s.as_str())
            .filter(|s| !s.is_empty())
    };
    let hostname = get("hostname").unwrap_or(alias);
    let user = get("user").filter(|user| Some(*user) != local_user);
    let port = get("port").filter(|port| *port != "22");
    let jump = get("proxyjump").filter(|jump| !jump.eq_ignore_ascii_case("none"));

    let mut detail = String::new();
    if hostname != alias || user.is_some() || port.is_some() {
        if let Some(user) = user {
            detail.push_str(user);
            detail.push('@');
        }
        detail.push_str(hostname);
        if let Some(port) = port {
            detail.push(':');
            detail.push_str(port);
        }
    }
    if let Some(jump) = jump {
        if !detail.is_empty() {
            detail.push(' ');
        }
        detail.push_str("via ");
        detail.push_str(jump);
    }
    (!detail.is_empty()).then_some(detail)
}

/// The alias a derived domain was made from.
fn domain_alias(name: &str) -> &str {
    name.strip_prefix("SSH:")
        .or_else(|| name.strip_prefix("SSHMUX:"))
        .unwrap_or(name)
}

/// Parse the ssh config files. Blocking file I/O (including every `Include`),
/// so call it from a worker thread only.
fn load_hosts_from_ssh_config() -> Vec<SshHost> {
    let mut ssh_config = wezterm_ssh::Config::new();
    ssh_config.add_default_config_files();
    let local_user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok();
    let details: HashMap<String, Option<String>> = ssh_config
        .enumerate_hosts()
        .into_iter()
        .map(|host| {
            let detail = host_detail(&host, &ssh_config.for_host(&host), local_user.as_deref());
            (host, detail)
        })
        .collect();

    SshDomain::default_domains()
        .into_iter()
        .map(|domain| {
            let detail = details.get(domain_alias(&domain.name)).cloned().flatten();
            SshHost {
                domain,
                from_ssh_config: true,
                detail,
            }
        })
        .collect()
}

fn same_hosts(a: &[SshHost], b: &[SshHost]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| a.domain.name == b.domain.name && a.detail == b.detail)
}

fn kick_refresh_if_due(cache: &mut HostCache) {
    let now = Instant::now();
    if !crate::termwindow::render::sidebar::herd_scan_is_due(
        cache.refresh_started_at,
        cache.refreshed_at,
        SSH_HOSTS_TTL,
        SSH_HOSTS_WATCHDOG,
        now,
    ) {
        return;
    }
    cache.refresh_started_at = Some(now);
    let spawned = std::thread::Builder::new()
        .name("ssh-host-list".into())
        .spawn(|| {
            let hosts = load_hosts_from_ssh_config();
            let changed = {
                let mut cache = SSH_HOST_CACHE.lock().unwrap();
                let changed = !same_hosts(&cache.hosts, &hosts);
                if changed {
                    cache.hosts = Arc::new(hosts);
                    cache.epoch += 1;
                }
                cache.refreshed_at = Some(Instant::now());
                cache.refresh_started_at = None;
                changed
            };
            if changed {
                // The button is hidden while the list is empty, so a window
                // that painted before this landed has to paint again.
                promise::spawn::spawn_into_main_thread(async {
                    for gui_window in crate::frontend::front_end().gui_windows() {
                        gui_window.window.invalidate();
                    }
                })
                .detach();
            }
        });
    if let Err(err) = spawned {
        log::warn!("failed to start ssh host refresh: {err:#}");
        cache.refresh_started_at = None;
    }
}

/// Start parsing the ssh config now, so the first frame that needs the host
/// list finds it warm. Called once at GUI startup.
pub fn warm_ssh_host_cache() {
    kick_refresh_if_due(&mut SSH_HOST_CACHE.lock().unwrap());
}

/// Changes whenever [`ssh_hosts`] would return something different. Cheap and
/// non-blocking: safe to call from paint.
pub(crate) fn ssh_hosts_epoch(config: &ConfigHandle) -> u64 {
    if config.ssh_domains.is_some() {
        // Declared in Lua: only a config reload changes them, and the caller
        // already keys on the config generation.
        return 0;
    }
    let mut cache = SSH_HOST_CACHE.lock().unwrap();
    kick_refresh_if_due(&mut cache);
    cache.epoch
}

/// `Config::ssh_domains()` without the file parsing: the user's own
/// `ssh_domains` verbatim, else the hosts last read from the ssh config by
/// the background refresh (empty until the first one lands).
pub(crate) fn ssh_hosts(config: &ConfigHandle) -> Arc<Vec<SshHost>> {
    if let Some(domains) = &config.ssh_domains {
        return Arc::new(
            domains
                .iter()
                .map(|domain| SshHost {
                    domain: domain.clone(),
                    from_ssh_config: false,
                    detail: None,
                })
                .collect(),
        );
    }
    let mut cache = SSH_HOST_CACHE.lock().unwrap();
    kick_refresh_if_due(&mut cache);
    Arc::clone(&cache.hosts)
}

/// `host:port` split, for a port that is plainly a port. An IPv6 literal or
/// anything else with more than one colon is left whole.
fn split_host_port(address: &str) -> (&str, Option<&str>) {
    match address.split_once(':') {
        Some((host, port))
            if !host.is_empty() && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            (host, Some(port))
        }
        _ => (address, None),
    }
}

fn user_at(user: Option<&str>, host: &str) -> String {
    match user {
        Some(user) => format!("{user}@{host}"),
        None => host.to_string(),
    }
}

/// The dropdown rows for `hosts`.
///
/// * A host from the ssh config arrives as an `SSH:`/`SSHMUX:` pair; only the
///   plain `SSH:` one becomes a row. Domains declared in Lua are listed
///   exactly as declared.
/// * `Mosh`/`Et`/`Custom` rows whose program `resolve` cannot find are
///   dropped, so the dropdown never offers a row that would fail to spawn.
/// * Hosts in `recents` (domain names, most recent first) lead the list.
pub(crate) fn build_entries(
    hosts: &[SshHost],
    resolve: &dyn Fn(&str) -> Option<PathBuf>,
    recents: &[String],
) -> Vec<SshQuickLaunchEntry> {
    let mut entries = Vec::new();
    for host in hosts {
        let domain = &host.domain;
        if host.from_ssh_config && domain.name.starts_with("SSHMUX:") {
            continue;
        }
        let transport = domain.transport;
        let user = domain.username.as_deref();
        let argv = match transport {
            SshTransport::WezTerm | SshTransport::Ssh => Vec::new(),
            SshTransport::Mosh | SshTransport::Et => {
                let Some(resolved) = transport.binary_name().and_then(resolve) else {
                    continue;
                };
                let mut argv = vec![resolved.to_string_lossy().into_owned()];
                match (transport, split_host_port(&domain.remote_address)) {
                    // mosh has no `host:port`; the port belongs to the ssh
                    // it bootstraps through.
                    (SshTransport::Mosh, (host, Some(port))) => {
                        argv.push(format!("--ssh=ssh -p {port}"));
                        argv.push(user_at(user, host));
                    }
                    // et takes `host:port` as written.
                    _ => argv.push(user_at(user, &domain.remote_address)),
                }
                argv.extend(domain.extra_args.iter().cloned());
                argv
            }
            SshTransport::Custom => {
                // The user-supplied argv is the source of truth: no host/user
                // synthesis, no port flag. An empty command is a config
                // error; skip it quietly rather than spawn an empty shell.
                let Some(resolved) = domain.custom_command.first().and_then(|c| resolve(c)) else {
                    continue;
                };
                let mut argv = vec![resolved.to_string_lossy().into_owned()];
                argv.extend(domain.custom_command.iter().skip(1).cloned());
                argv.extend(domain.extra_args.iter().cloned());
                argv
            }
        };

        // The bare user@host for mosh/et (the badge already says which
        // transport), otherwise the domain name without the conventional
        // prefix. Custom keeps the name verbatim: there is no host to show.
        let label = match transport {
            SshTransport::WezTerm | SshTransport::Ssh => domain_alias(&domain.name).to_string(),
            SshTransport::Mosh | SshTransport::Et => user_at(user, &domain.remote_address),
            SshTransport::Custom => domain.name.clone(),
        };
        let badge = match transport {
            SshTransport::Mosh => "mosh",
            SshTransport::Et => "et",
            SshTransport::Custom => "custom",
            SshTransport::Ssh => "ssh",
            SshTransport::WezTerm => match domain.multiplexing {
                SshMultiplexing::WezTerm => "mux",
                SshMultiplexing::None => "ssh",
            },
        };

        entries.push(SshQuickLaunchEntry {
            domain_name: domain.name.clone(),
            label,
            transport,
            argv,
            badge,
            detail: host.detail.clone(),
            recent: false,
        });
    }

    let mut ordered = Vec::with_capacity(entries.len());
    for name in recents.iter().take(MAX_RECENT_HOSTS) {
        if let Some(idx) = entries.iter().position(|e| &e.domain_name == name) {
            let mut entry = entries.remove(idx);
            entry.recent = true;
            ordered.push(entry);
        }
    }
    ordered.extend(entries);
    ordered
}

/// The text of a dropdown row.
pub(crate) fn entry_row_label(entry: &SshQuickLaunchEntry) -> String {
    match &entry.detail {
        Some(detail) => format!("{}  · {} · {}", entry.label, entry.badge, detail),
        None => format!("{}  · {}", entry.label, entry.badge),
    }
}

/// The entries matching `query`: every whitespace-separated word must appear,
/// case-insensitively, in the row's label, detail or transport badge.
pub(crate) fn filter_entries<'a>(
    entries: &'a [SshQuickLaunchEntry],
    query: &str,
) -> Vec<&'a SshQuickLaunchEntry> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    entries
        .iter()
        .filter(|entry| {
            let haystack = entry_row_label(entry).to_lowercase();
            words.iter().all(|word| haystack.contains(word))
        })
        .collect()
}

/// `recents` after connecting to `domain_name`: it moves to the front.
pub(crate) fn record_recent(recents: &[String], domain_name: &str) -> Vec<String> {
    let mut next = vec![domain_name.to_string()];
    next.extend(recents.iter().filter(|name| *name != domain_name).cloned());
    next.truncate(MAX_RECENT_HOSTS);
    next
}

/// What the dropdown shows in place of hosts when the filter matches none.
pub(crate) const NO_MATCH_ROW: &str = "No hosts match";

/// The last `cols` characters of `text`, led by an ellipsis when some were
/// cut: while typing, the end of the text is the part being written.
fn keep_tail(text: &str, cols: usize) -> String {
    let len = text.chars().count();
    if len <= cols {
        return text.to_string();
    }
    if cols == 0 {
        return String::new();
    }
    let tail: String = text.chars().skip(len - (cols - 1)).collect();
    format!("…{tail}")
}

/// The non-clickable first row of the dropdown, if it should have one: the
/// filter being typed, or a hint that typing filters once the list is long.
///
/// `max_cols` is the text width of a row. A query longer than that scrolls
/// with the typing, so the characters just typed are always the ones shown.
pub(crate) fn filter_row_label(query: &str, total: usize, max_cols: usize) -> Option<String> {
    const PREFIX: &str = "Filter: ";
    if !query.is_empty() {
        let room = max_cols.saturating_sub(PREFIX.chars().count()).max(1);
        Some(format!("{PREFIX}{}", keep_tail(query, room)))
    } else if total >= FILTER_HINT_MIN_ROWS {
        Some("Type to filter".to_string())
    } else {
        None
    }
}

/// Text columns the dropdown needs to show every row of `entries` in full,
/// so it can be sized once and not change width while the filter narrows it.
pub(crate) fn widest_row_cols(entries: &[SshQuickLaunchEntry]) -> usize {
    entries
        .iter()
        .map(|entry| entry_row_label(entry).chars().count())
        .chain(std::iter::once(NO_MATCH_ROW.chars().count()))
        .max()
        .unwrap_or(0)
}

impl TermWindow {
    /// `ShowSshHostMenu`: open the dropdown at its sidebar button, or, when
    /// that button is not on screen (sidebar hidden, or no hosts yet), fall
    /// back to the launcher's fuzzy domain list.
    pub(crate) fn show_ssh_host_menu(&mut self) {
        let button = self
            .ui_items
            .iter()
            .find(|item| matches!(item.item_type, UIItemType::SidebarSshLaunchButton))
            .map(|item| (item.x, item.y));
        match button {
            Some((x, y)) if !self.ssh_quick_launch_entries().is_empty() => {
                self.ssh_launch_menu = Some(SshLaunchMenuState {
                    x,
                    y,
                    query: String::new(),
                    scroll_offset: 0,
                });
                if let Some(window) = self.window.as_ref() {
                    window.invalidate();
                }
            }
            _ => {
                self.show_launcher_impl(
                    LauncherActionArgs {
                        title: Some("SSH hosts".to_string()),
                        flags: LauncherFlags::DOMAINS | LauncherFlags::FUZZY,
                        help_text: None,
                        fuzzy_help_text: None,
                        alphabet: None,
                    },
                    0,
                );
            }
        }
    }

    /// Keys typed while the dropdown is open edit its filter. Returns true
    /// when the key was consumed.
    pub(crate) fn ssh_menu_key_input(
        &mut self,
        key: ::termwiz::input::KeyCode,
        modifiers: Modifiers,
        context: &dyn WindowOps,
    ) -> bool {
        use ::termwiz::input::KeyCode as KC;
        let Some(menu) = self.ssh_launch_menu.as_mut() else {
            return false;
        };
        match key {
            KC::Escape => {
                self.ssh_launch_menu = None;
            }
            KC::Backspace => {
                menu.query.pop();
                menu.scroll_offset = 0;
            }
            KC::Enter => {
                let query = std::mem::take(&mut menu.query);
                self.ssh_launch_menu = None;
                let entries = self.ssh_quick_launch_entries();
                let first = filter_entries(&entries, &query)
                    .first()
                    .map(|entry| entry.domain_name.clone());
                if let Some(domain_name) = first {
                    self.spawn_ssh_quick_launch_entry(&domain_name);
                }
            }
            KC::Char(c)
                if !modifiers.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER)
                    && !c.is_control() =>
            {
                menu.query.push(c);
                menu.scroll_offset = 0;
            }
            _ => return false,
        }
        context.invalidate();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain(name: &str, address: &str) -> SshDomain {
        SshDomain {
            name: name.to_string(),
            remote_address: address.to_string(),
            ..SshDomain::default()
        }
    }

    fn derived(alias: &str) -> Vec<SshHost> {
        vec![
            (format!("SSH:{alias}"), SshMultiplexing::None),
            (format!("SSHMUX:{alias}"), SshMultiplexing::WezTerm),
        ]
        .into_iter()
        .map(|(name, multiplexing)| SshHost {
            domain: SshDomain {
                multiplexing,
                ..domain(&name, alias)
            },
            from_ssh_config: true,
            detail: None,
        })
        .collect()
    }

    fn declared(domain: SshDomain) -> SshHost {
        SshHost {
            domain,
            from_ssh_config: false,
            detail: None,
        }
    }

    fn found(command: &str) -> Option<PathBuf> {
        Some(PathBuf::from(format!("/usr/bin/{command}")))
    }

    fn missing(_: &str) -> Option<PathBuf> {
        None
    }

    fn names(entries: &[SshQuickLaunchEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.domain_name.as_str()).collect()
    }

    fn options(pairs: &[(&str, &str)]) -> wezterm_ssh::ConfigMap {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn ssh_config_host_becomes_one_plain_ssh_row() {
        let mut hosts = derived("web1");
        hosts.extend(derived("db1"));
        let entries = build_entries(&hosts, &found, &[]);
        assert_eq!(names(&entries), vec!["SSH:web1", "SSH:db1"]);
        assert_eq!(entries[0].label, "web1");
        assert_eq!(entries[0].badge, "ssh");
    }

    #[test]
    fn declared_domains_are_listed_as_declared() {
        // A user who declares both wants both, and the badge tells them apart.
        let hosts = vec![
            declared(SshDomain {
                multiplexing: SshMultiplexing::None,
                ..domain("SSH:box", "box")
            }),
            declared(domain("SSHMUX:box", "box")),
        ];
        let entries = build_entries(&hosts, &found, &[]);
        assert_eq!(names(&entries), vec!["SSH:box", "SSHMUX:box"]);
        assert_eq!(entries[0].badge, "ssh");
        assert_eq!(entries[1].badge, "mux");
    }

    #[test]
    fn sidecar_rows_need_their_program() {
        let hosts = vec![declared(SshDomain {
            transport: SshTransport::Mosh,
            username: Some("tim".to_string()),
            ..domain("roaming", "box.example.com")
        })];
        assert!(build_entries(&hosts, &missing, &[]).is_empty());

        let entries = build_entries(&hosts, &found, &[]);
        assert_eq!(entries[0].label, "tim@box.example.com");
        assert_eq!(
            entries[0].argv,
            vec!["/usr/bin/mosh", "tim@box.example.com"]
        );
    }

    #[test]
    fn mosh_takes_the_port_through_ssh_and_et_takes_it_inline() {
        let mosh = declared(SshDomain {
            transport: SshTransport::Mosh,
            username: Some("tim".to_string()),
            extra_args: vec!["--predict=always".to_string()],
            ..domain("m", "box:2222")
        });
        let et = declared(SshDomain {
            transport: SshTransport::Et,
            ..domain("e", "box:2022")
        });
        let entries = build_entries(&[mosh, et], &found, &[]);
        assert_eq!(
            entries[0].argv,
            vec![
                "/usr/bin/mosh",
                "--ssh=ssh -p 2222",
                "tim@box",
                "--predict=always"
            ]
        );
        assert_eq!(entries[1].argv, vec!["/usr/bin/et", "box:2022"]);
    }

    #[test]
    fn host_port_split_leaves_ipv6_alone() {
        assert_eq!(split_host_port("box:22"), ("box", Some("22")));
        assert_eq!(split_host_port("box"), ("box", None));
        assert_eq!(split_host_port("fe80::1"), ("fe80::1", None));
        assert_eq!(split_host_port("box:ssh"), ("box:ssh", None));
    }

    #[test]
    fn custom_rows_use_the_command_verbatim() {
        let hosts = vec![
            declared(SshDomain {
                transport: SshTransport::Custom,
                custom_command: vec!["autossh".to_string(), "-M0".to_string(), "box".to_string()],
                ..domain("tunnel", "ignored")
            }),
            declared(SshDomain {
                transport: SshTransport::Custom,
                ..domain("empty", "ignored")
            }),
        ];
        let entries = build_entries(&hosts, &found, &[]);
        assert_eq!(names(&entries), vec!["tunnel"]);
        assert_eq!(entries[0].argv, vec!["/usr/bin/autossh", "-M0", "box"]);
        assert_eq!(entries[0].badge, "custom");
    }

    #[test]
    fn recent_hosts_lead_in_recency_order() {
        let mut hosts = derived("a");
        hosts.extend(derived("b"));
        hosts.extend(derived("c"));
        let recents = vec![
            "SSH:c".to_string(),
            "SSH:gone".to_string(),
            "SSH:a".to_string(),
        ];
        let entries = build_entries(&hosts, &found, &recents);
        assert_eq!(names(&entries), vec!["SSH:c", "SSH:a", "SSH:b"]);
        assert_eq!(
            entries.iter().map(|e| e.recent).collect::<Vec<_>>(),
            vec![true, true, false]
        );
    }

    #[test]
    fn record_recent_moves_to_front_and_caps() {
        let recents = vec!["a".to_string(), "b".to_string()];
        assert_eq!(record_recent(&recents, "b"), vec!["b", "a"]);
        assert_eq!(record_recent(&recents, "c"), vec!["c", "a", "b"]);

        let many: Vec<String> = (0..20).map(|n| n.to_string()).collect();
        let next = record_recent(&many, "new");
        assert_eq!(next.len(), MAX_RECENT_HOSTS);
        assert_eq!(next[0], "new");
    }

    #[test]
    fn filter_matches_every_word_across_label_detail_and_badge() {
        let mut hosts = derived("web-prod");
        hosts[0].detail = Some("deploy@10.0.0.5 via bastion".to_string());
        hosts.extend(derived("web-staging"));
        hosts.extend(derived("db-prod"));
        let entries = build_entries(&hosts, &found, &[]);

        let hits = |query: &str| -> Vec<&str> {
            filter_entries(&entries, query)
                .into_iter()
                .map(|e| e.label.as_str())
                .collect()
        };
        assert_eq!(hits(""), vec!["web-prod", "web-staging", "db-prod"]);
        assert_eq!(hits("PROD"), vec!["web-prod", "db-prod"]);
        assert_eq!(hits("web prod"), vec!["web-prod"]);
        assert_eq!(hits("bastion"), vec!["web-prod"]);
        assert_eq!(hits("10.0"), vec!["web-prod"]);
        assert!(hits("nope").is_empty());
    }

    #[test]
    fn row_label_appends_the_detail() {
        let mut hosts = derived("web");
        hosts[0].detail = Some("deploy@10.0.0.5:2222".to_string());
        let entries = build_entries(&hosts, &found, &[]);
        assert_eq!(
            entry_row_label(&entries[0]),
            "web  · ssh · deploy@10.0.0.5:2222"
        );
        let plain = build_entries(&derived("web"), &found, &[]);
        assert_eq!(entry_row_label(&plain[0]), "web  · ssh");
    }

    #[test]
    fn host_detail_says_only_what_the_alias_does_not() {
        let me = Some("tim");
        // Everything default: nothing to add.
        assert_eq!(
            host_detail(
                "box",
                &options(&[("hostname", "box"), ("user", "tim"), ("port", "22")]),
                me
            ),
            None
        );
        assert_eq!(
            host_detail(
                "web",
                &options(&[("hostname", "10.0.0.5"), ("user", "tim"), ("port", "22")]),
                me
            )
            .as_deref(),
            Some("10.0.0.5")
        );
        assert_eq!(
            host_detail(
                "web",
                &options(&[("hostname", "web"), ("user", "deploy"), ("port", "2222")]),
                me
            )
            .as_deref(),
            Some("deploy@web:2222")
        );
        assert_eq!(
            host_detail(
                "inner",
                &options(&[("hostname", "inner"), ("proxyjump", "bastion")]),
                me
            )
            .as_deref(),
            Some("via bastion")
        );
        assert_eq!(
            host_detail(
                "inner",
                &options(&[("hostname", "10.1.1.1"), ("proxyjump", "none")]),
                me
            )
            .as_deref(),
            Some("10.1.1.1")
        );
    }

    #[test]
    fn filter_row_appears_for_a_query_or_a_long_list() {
        assert_eq!(filter_row_label("", 3, 30), None);
        assert_eq!(
            filter_row_label("", FILTER_HINT_MIN_ROWS, 30).as_deref(),
            Some("Type to filter")
        );
        assert_eq!(
            filter_row_label("web", 3, 30).as_deref(),
            Some("Filter: web")
        );
    }

    #[test]
    fn a_long_filter_scrolls_with_the_typing() {
        // 14 columns: "Filter: " takes 8, leaving 6 for the query.
        assert_eq!(
            filter_row_label("abcdef", 3, 14).as_deref(),
            Some("Filter: abcdef")
        );
        assert_eq!(
            filter_row_label("abcdefgh", 3, 14).as_deref(),
            Some("Filter: …defgh")
        );
        assert_eq!(
            filter_row_label("abcdefghi", 3, 14).as_deref(),
            Some("Filter: …efghi")
        );
        // Never wider than the row, however narrow that is.
        assert_eq!(filter_row_label("abc", 3, 0).as_deref(), Some("Filter: …"));
        assert_eq!(keep_tail("abc", 3), "abc");
    }

    #[test]
    fn dropdown_width_covers_the_longest_row() {
        let mut hosts = derived("web");
        hosts[0].detail = Some("deploy@10.0.0.5:2222 via bastion".to_string());
        let entries = build_entries(&hosts, &found, &[]);
        assert_eq!(
            widest_row_cols(&entries),
            "web  · ssh · deploy@10.0.0.5:2222 via bastion"
                .chars()
                .count()
        );
        // Short lists still fit the "no match" row.
        assert_eq!(
            widest_row_cols(&build_entries(&derived("a"), &found, &[])),
            NO_MATCH_ROW.len()
        );
    }

    #[test]
    fn same_hosts_compares_names_and_details() {
        let a = derived("web");
        let mut b = derived("web");
        assert!(same_hosts(&a, &b));
        b[0].detail = Some("10.0.0.5".to_string());
        assert!(!same_hosts(&a, &b));
        assert!(!same_hosts(&a, &a[..1]));
    }
}
