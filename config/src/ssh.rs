use crate::config::validate_domain_name;
use crate::*;
use luahelper::impl_lua_conversion_dynamic;
use std::fmt::Display;
use std::str::FromStr;
use wezterm_dynamic::{FromDynamic, ToDynamic};

#[derive(Debug, Clone, Copy, FromDynamic, ToDynamic)]
pub enum SshBackend {
    Ssh2,
    LibSsh,
}

impl Default for SshBackend {
    fn default() -> Self {
        Self::LibSsh
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromDynamic, ToDynamic)]
pub enum SshMultiplexing {
    WezTerm,
    None,
    // TODO: Tmux-cc in the future?
}

impl Default for SshMultiplexing {
    fn default() -> Self {
        Self::WezTerm
    }
}

/// Which transport a sidebar SSH quick-launch entry uses to reach the host.
///
/// `WezTerm` and `Ssh` are driven through the wezterm mux domain subsystem
/// (`SpawnTabDomain::DomainName`). `Mosh` and `Et` bypass the mux entirely:
/// they are opaque CLI programs that own their own reconnect state, so the
/// sidebar spawns them as a plain shell command in a local-domain pane.
/// `Custom` is the same bypass — it lets the user supply an arbitrary argv
/// (a wrapper script, a Secretive-mediated `ssh` invocation, autossh, a
/// tunneling helper), so the dropdown can reach hosts the four known
/// transports cannot describe by name. The argv comes from the domain's
/// `custom_command` field, plus `extra_args`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromDynamic, ToDynamic)]
pub enum SshTransport {
    /// wezterm's native SSH multiplexer (default for `SSHMUX:` entries).
    WezTerm,
    /// Plain `ssh` via the wezterm SSH domain with no multiplexing
    /// (default for `SSH:` entries).
    Ssh,
    /// `mosh` — requires `mosh` (and `mosh-server` on the host) installed.
    Mosh,
    /// Eternal Terminal — requires `et` installed.
    Et,
    /// User-supplied argv (see `SshDomain::custom_command`). Used for
    /// Secretive-mediated ssh, autossh, tunneling wrappers, or anything the
    /// four built-in transports cannot name. Probes `$PATH` for the first
    /// element; bad/missing binaries silently hide the row, matching the
    /// mosh/et behavior.
    Custom,
}

impl Default for SshTransport {
    fn default() -> Self {
        Self::WezTerm
    }
}

impl SshTransport {
    /// Bare command name the sidebar probes on `$PATH` to decide whether this
    /// transport is installed. Returns `None` for transports that don't need
    /// a sidecar binary (they go through the built-in wezterm-ssh path) — the
    /// `Custom` variant is an exception: its binary is whatever the user put
    /// first in `custom_command`, so no fixed name applies and probing is
    /// done at the call site against that argv instead.
    pub fn binary_name(self) -> Option<&'static str> {
        match self {
            SshTransport::WezTerm | SshTransport::Ssh => None,
            SshTransport::Mosh => Some("mosh"),
            SshTransport::Et => Some("et"),
            SshTransport::Custom => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromDynamic, ToDynamic)]
pub enum Shell {
    /// Unknown command shell: no assumptions can be made
    Unknown,

    /// Posix shell compliant, such that `cd DIR ; exec CMD` behaves
    /// as it does in the bourne shell family of shells
    Posix,
    // TODO: Cmd, PowerShell in the future?
}

impl Default for Shell {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Default, Debug, Clone, FromDynamic, ToDynamic)]
pub struct SshDomain {
    /// The name of this specific domain.  Must be unique amongst
    /// all types of domain in the configuration file.
    #[dynamic(validate = "validate_domain_name")]
    pub name: String,

    /// identifies the host:port pair of the remote server.
    pub remote_address: String,

    /// Whether agent auth should be disabled
    #[dynamic(default)]
    pub no_agent_auth: bool,

    /// The username to use for authenticating with the remote host
    pub username: Option<String>,

    /// If true, connect to this domain automatically at startup
    #[dynamic(default)]
    pub connect_automatically: bool,

    #[dynamic(default = "default_read_timeout")]
    pub timeout: Duration,

    #[dynamic(default = "default_local_echo_threshold_ms")]
    pub local_echo_threshold_ms: Option<u64>,

    /// Show time since last response when waiting for a response.
    /// It is recommended to use
    /// <https://wezterm.org/config/lua/pane/get_metadata.html#since_last_response_ms>
    /// instead.
    #[dynamic(default)]
    pub overlay_lag_indicator: bool,

    /// The path to the wezterm binary on the remote host
    pub remote_wezterm_path: Option<String>,
    /// Override the entire `wezterm cli proxy` invocation that would otherwise
    /// be computed from remote_wezterm_path and other information.
    pub override_proxy_command: Option<String>,

    pub ssh_backend: Option<SshBackend>,

    /// If false, then don't use a multiplexer connection,
    /// just connect directly using ssh. This doesn't require
    /// that the remote host have wezterm installed, and is equivalent
    /// to using `wezterm ssh` to connect.
    #[dynamic(default)]
    pub multiplexing: SshMultiplexing,

    /// Which transport the sidebar SSH quick-launch uses to reach this host.
    /// Defaults to `WezTerm`, the wezterm-native mux path. Set `Ssh` for plain
    /// non-multiplexed ssh, `Mosh` for mobile-shell (requires `mosh` on
    /// `$PATH`), or `Et` for Eternal Terminal (requires `et` on `$PATH`).
    /// `Mosh`/`Et` bypass the wezterm mux and run as plain shell commands.
    #[dynamic(default)]
    pub transport: SshTransport,

    /// Extra argv appended after the transport's own arguments when launching
    /// via the sidebar SSH quick-launch. For `Mosh`/`Et` these go after the
    /// resolved `user@host` (and any port flag); for `WezTerm`/`Ssh` they are
    /// ignored (use `ssh_option` for those).
    #[dynamic(default)]
    pub extra_args: Vec<String>,

    /// Literal argv run by the sidebar SSH quick-launch when
    /// `transport = "Custom"`. Used for Secretive-mediated `ssh`, autossh,
    /// tunneling helpers, or any wrapper program the four built-in transports
    /// cannot express. Ignored for all other transports. The first element is
    /// probed on `$PATH` (and the usual fallback dirs); a missing binary
    /// silently hides the row. `extra_args` are appended after this argv.
    #[dynamic(default)]
    pub custom_command: Vec<String>,

    /// ssh_config option values
    #[dynamic(default)]
    pub ssh_option: HashMap<String, String>,

    pub default_prog: Option<Vec<String>>,

    #[dynamic(default)]
    pub assume_shell: Shell,
}
impl_lua_conversion_dynamic!(SshDomain);

impl SshDomain {
    pub fn default_domains() -> Vec<Self> {
        let mut config = wezterm_ssh::Config::new();
        config.add_default_config_files();

        let mut plain_ssh = vec![];
        let mut mux_ssh = vec![];
        for host in config.enumerate_hosts() {
            plain_ssh.push(Self {
                name: format!("SSH:{host}"),
                remote_address: host.to_string(),
                multiplexing: SshMultiplexing::None,
                local_echo_threshold_ms: default_local_echo_threshold_ms(),
                ..SshDomain::default()
            });

            mux_ssh.push(Self {
                name: format!("SSHMUX:{host}"),
                remote_address: host.to_string(),
                multiplexing: SshMultiplexing::WezTerm,
                local_echo_threshold_ms: default_local_echo_threshold_ms(),
                ..SshDomain::default()
            });
        }

        plain_ssh.append(&mut mux_ssh);
        plain_ssh
    }
}

#[derive(Clone, Debug)]
pub struct SshParameters {
    pub username: Option<String>,
    pub host_and_port: String,
}

impl Display for SshParameters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(user) = &self.username {
            write!(f, "{}@{}", user, self.host_and_port)
        } else {
            write!(f, "{}", self.host_and_port)
        }
    }
}

pub fn username_from_env() -> anyhow::Result<String> {
    #[cfg(unix)]
    const USER: &str = "USER";
    #[cfg(windows)]
    const USER: &str = "USERNAME";

    std::env::var(USER).with_context(|| format!("while resolving {} env var", USER))
}

impl FromStr for SshParameters {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('@').collect();

        if parts.len() == 2 {
            Ok(Self {
                username: Some(parts[0].to_string()),
                host_and_port: parts[1].to_string(),
            })
        } else if parts.len() == 1 {
            Ok(Self {
                username: None,
                host_and_port: parts[0].to_string(),
            })
        } else {
            bail!("failed to parse ssh parameters from `{}`", s);
        }
    }
}
