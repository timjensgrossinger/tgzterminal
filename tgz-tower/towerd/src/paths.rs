//! Where towerd keeps things.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Paths {
    pub socket: PathBuf,
    /// Data directory: the socket's directory (`DATA_DIR/tower`).
    pub data: PathBuf,
    /// `~/.config/wezterm/tower`: roles, models, gates.
    pub config: PathBuf,
    /// This binary, which agents run as `tower hook` / `tower mcp`.
    pub exe: PathBuf,
}

impl Paths {
    pub fn new(socket: PathBuf, config: Option<PathBuf>, data: Option<PathBuf>) -> Self {
        let data = data.unwrap_or_else(|| {
            if is_pipe(&socket) {
                default_data_dir()
            } else {
                socket
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."))
            }
        });
        let config = config.unwrap_or_else(default_config_dir);
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("tower"));
        Self {
            socket,
            data,
            config,
            exe,
        }
    }

    /// The master token file: next to the socket, or in the data dir for a
    /// named pipe (which is not a file).
    pub fn token(&self) -> PathBuf {
        if is_pipe(&self.socket) {
            self.data.join("towerd.token")
        } else {
            token_path(&self.socket)
        }
    }

    pub fn state(&self) -> PathBuf {
        self.data.join("state.json")
    }

    pub fn agent_dir(&self, agent_id: &str) -> PathBuf {
        self.data.join("agents").join(agent_id)
    }
}

pub fn token_path(socket: &Path) -> PathBuf {
    let mut p = socket.as_os_str().to_owned();
    p.push(".token");
    PathBuf::from(p)
}

/// `$XDG_CONFIG_HOME/wezterm/tower`, else `~/.config/wezterm/tower`, which is
/// where wezterm itself looks for its config on every platform.
pub fn default_config_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(x).join("wezterm").join("tower");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("wezterm").join("tower")
}

/// A Windows named pipe path, `\\.\pipe\...`.
pub fn is_pipe(p: &Path) -> bool {
    p.to_string_lossy().starts_with(r"\\.\pipe\")
}

/// The endpoint the Tower view uses when nothing else is given.
pub fn default_socket() -> PathBuf {
    if cfg!(windows) {
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
        let user: String = user.chars().filter(|c| c.is_alphanumeric()).collect();
        PathBuf::from(format!(r"\\.\pipe\tgz-towerd-{user}"))
    } else {
        default_data_dir().join("towerd.sock")
    }
}

/// Where the Tower view puts the socket: wezterm's data dir plus `tower`
/// (`~/Library/Application Support/wezterm/tower` on macOS).
pub fn default_data_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") {
        return home
            .join("Library")
            .join("Application Support")
            .join("wezterm")
            .join("tower");
    }
    if cfg!(windows) {
        if let Some(x) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(x).join("wezterm").join("tower");
        }
    }
    if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(x).join("wezterm").join("tower");
    }
    home.join(".local")
        .join("share")
        .join("wezterm")
        .join("tower")
}

/// A random-enough RFC 4122 v4 UUID without a dependency (Claude session ids).
pub fn uuid_v4() -> String {
    let a = random_u64();
    let b = random_u64();
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&a.to_le_bytes());
    bytes[8..].copy_from_slice(&b.to_le_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

pub fn random_u64() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    h.write_u32(std::process::id());
    h.finish()
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipes_keep_their_token_in_the_data_dir() {
        let pipe = PathBuf::from(r"\\.\pipe\tgz-towerd-tim");
        assert!(is_pipe(&pipe));
        let p = Paths::new(pipe, Some("c".into()), Some("d".into()));
        assert_eq!(p.token(), PathBuf::from("d").join("towerd.token"));
        let p = Paths::new(PathBuf::from("/x/towerd.sock"), None, None);
        assert_eq!(p.token(), PathBuf::from("/x/towerd.sock.token"));
        assert_eq!(p.data, PathBuf::from("/x"));
    }

    #[test]
    fn uuids_look_like_v4() {
        let u = uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"));
        assert_ne!(u, uuid_v4());
    }
}
