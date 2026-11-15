//! "New SSH connection": the form behind the sidebar's SSH button.
//!
//! A connection is written where every client looks for one: a `Host` block
//! in the user's `~/.ssh/config`, so `ssh`, `mosh` and `et` all accept its
//! name. Which of the three to start is the one thing the ssh config cannot
//! say; it is kept per host name in `tgz_ui_state` (names only).
//!
//! Rules the rest relies on:
//!
//! - **No passwords are stored.** OpenSSH has no config option for one, and
//!   keeping it would mean a plaintext file. "Password" sign-in means ssh
//!   asks for it in the terminal when connecting; a key is named by its file.
//! - The file is only ever appended to, and only after its existing text was
//!   read back successfully. A name that is already a `Host` there is refused
//!   rather than shadowed, since ssh takes the first match.
//! - Nothing from the form is logged.
//!
//! Pure apart from [`append_host_block`]; the painting and input live in
//! `render/sidebar/ssh_setup.rs`.

use config::SshTransport;
use std::path::{Path, PathBuf};

/// The fields of the form, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    Name,
    Host,
    User,
    Port,
    Auth,
    KeyFile,
    Transport,
}

impl Field {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Field::Name => "Name",
            Field::Host => "Host",
            Field::User => "User",
            Field::Port => "Port",
            Field::Auth => "Sign in with",
            Field::KeyFile => "Key file",
            Field::Transport => "Connect with",
        }
    }

    pub(crate) fn hint(self) -> &'static str {
        match self {
            Field::Name => "what the menu calls it, e.g. web1",
            Field::Host => "address or IP, e.g. example.com",
            Field::User => "optional",
            Field::Port => "22",
            Field::Auth | Field::Transport => "",
            Field::KeyFile => "~/.ssh/id_ed25519",
        }
    }

    pub(crate) fn is_text(self) -> bool {
        !matches!(self, Field::Auth | Field::Transport)
    }
}

/// How the user signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Auth {
    /// ssh asks in the terminal; nothing is stored.
    Password,
    /// A private key file (`IdentityFile`).
    Key,
}

impl Auth {
    pub(crate) const ALL: [Auth; 2] = [Auth::Password, Auth::Key];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Auth::Password => "Password",
            Auth::Key => "Key file",
        }
    }
}

/// The transports the form offers.
pub(crate) const TRANSPORTS: [SshTransport; 3] =
    [SshTransport::Ssh, SshTransport::Mosh, SshTransport::Et];

pub(crate) fn transport_label(transport: SshTransport) -> &'static str {
    match transport {
        SshTransport::Mosh => "Mosh",
        SshTransport::Et => "ET",
        _ => "SSH",
    }
}

/// The program a transport runs.
pub(crate) fn transport_program(transport: SshTransport) -> &'static str {
    match transport {
        SshTransport::Mosh => "mosh",
        SshTransport::Et => "et",
        _ => "ssh",
    }
}

/// What has been typed and chosen so far.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SshSetupForm {
    pub name: String,
    pub host: String,
    pub user: String,
    pub port: String,
    pub auth: Auth,
    pub key_file: String,
    pub transport: SshTransport,
    pub focus: Field,
    /// Why the last save was refused.
    pub error: Option<String>,
}

impl Default for SshSetupForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            user: String::new(),
            port: String::new(),
            auth: Auth::Password,
            key_file: String::new(),
            transport: SshTransport::Ssh,
            focus: Field::Name,
            error: None,
        }
    }
}

/// Longest text a field takes; a host line longer than this is a paste gone
/// wrong.
const MAX_FIELD_CHARS: usize = 255;

impl SshSetupForm {
    /// The fields shown: the key file only when signing in with a key.
    pub(crate) fn fields(&self) -> Vec<Field> {
        let mut fields = vec![
            Field::Name,
            Field::Host,
            Field::User,
            Field::Port,
            Field::Auth,
        ];
        if self.auth == Auth::Key {
            fields.push(Field::KeyFile);
        }
        fields.push(Field::Transport);
        fields
    }

    pub(crate) fn text(&self, field: Field) -> &str {
        match field {
            Field::Name => &self.name,
            Field::Host => &self.host,
            Field::User => &self.user,
            Field::Port => &self.port,
            Field::KeyFile => &self.key_file,
            Field::Auth | Field::Transport => "",
        }
    }

    fn text_mut(&mut self, field: Field) -> Option<&mut String> {
        match field {
            Field::Name => Some(&mut self.name),
            Field::Host => Some(&mut self.host),
            Field::User => Some(&mut self.user),
            Field::Port => Some(&mut self.port),
            Field::KeyFile => Some(&mut self.key_file),
            Field::Auth | Field::Transport => None,
        }
    }

    /// Typing into the focused field. A port takes digits only.
    pub(crate) fn type_char(&mut self, c: char) {
        let focus = self.focus;
        if c.is_control() || (focus == Field::Port && !c.is_ascii_digit()) {
            return;
        }
        if let Some(text) = self.text_mut(focus) {
            if text.chars().count() < MAX_FIELD_CHARS {
                text.push(c);
                self.error = None;
            }
        }
    }

    /// Pasted text goes in as typed, one line of it.
    pub(crate) fn paste(&mut self, text: &str) {
        for c in text.lines().next().unwrap_or("").trim().chars() {
            self.type_char(c);
        }
    }

    pub(crate) fn backspace(&mut self) {
        let focus = self.focus;
        if let Some(text) = self.text_mut(focus) {
            text.pop();
            self.error = None;
        }
    }

    /// Next (`+1`) or previous (`-1`) field, wrapping.
    pub(crate) fn move_focus(&mut self, direction: i32) {
        let fields = self.fields();
        let at = fields.iter().position(|f| *f == self.focus).unwrap_or(0) as i32;
        let len = fields.len() as i32;
        self.focus = fields[(at + direction).rem_euclid(len) as usize];
    }

    /// Steps the focused choice; text fields ignore it.
    pub(crate) fn step_choice(&mut self, direction: i32) {
        match self.focus {
            Field::Auth => {
                let at = Auth::ALL.iter().position(|a| *a == self.auth).unwrap_or(0) as i32;
                self.auth = Auth::ALL[(at + direction).rem_euclid(Auth::ALL.len() as i32) as usize];
            }
            Field::Transport => {
                let at = TRANSPORTS
                    .iter()
                    .position(|t| *t == self.transport)
                    .unwrap_or(0) as i32;
                self.transport =
                    TRANSPORTS[(at + direction).rem_euclid(TRANSPORTS.len() as i32) as usize];
            }
            _ => return,
        }
        self.error = None;
    }
}

/// A connection that passed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewHost {
    pub name: String,
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub key_file: Option<String>,
    pub transport: SshTransport,
}

fn is_token(text: &str) -> bool {
    !text.is_empty()
        && !text
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '#')
}

/// Checks the form; the error is what the form shows.
pub(crate) fn validate(form: &SshSetupForm) -> Result<NewHost, String> {
    let name = form.name.trim();
    if name.is_empty() {
        return Err("Give the connection a name.".into());
    }
    if !is_token(name) || name.contains(['*', '?', '!', ',']) {
        return Err("A name is one word: no spaces, and none of * ? ! , # \"".into());
    }
    let host = form.host.trim();
    if host.is_empty() {
        return Err("Which host? Type its address or IP.".into());
    }
    if !is_token(host) {
        return Err("The host has a space or quote in it.".into());
    }
    let user = form.user.trim();
    if !user.is_empty() && !is_token(user) {
        return Err("The user name has a space or quote in it.".into());
    }
    let port = match form.port.trim() {
        "" => None,
        text => match text.parse::<u16>() {
            Ok(port) if port > 0 => Some(port),
            _ => return Err("The port is a number from 1 to 65535.".into()),
        },
    };
    let key_file = match form.auth {
        Auth::Password => None,
        Auth::Key => {
            let key = form.key_file.trim();
            if key.is_empty() {
                return Err("Which key file? Or sign in with a password.".into());
            }
            if key.contains('"') {
                return Err("The key file path has a quote in it.".into());
            }
            Some(key.to_string())
        }
    };
    Ok(NewHost {
        name: name.to_string(),
        host: host.to_string(),
        user: (!user.is_empty()).then(|| user.to_string()),
        port,
        key_file,
        transport: form.transport,
    })
}

/// `~` at the start of a path, as ssh itself would read it.
pub(crate) fn expand_home(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => home.join(rest),
        None if path == "~" => home.to_path_buf(),
        None => PathBuf::from(path),
    }
}

/// The `Host` block for `ssh_config`. Paths are quoted, since a Windows
/// profile path often has a space in it.
pub(crate) fn host_block(new: &NewHost, product: &str) -> String {
    let mut block = format!("# Added by {product}\nHost {}\n", new.name);
    block.push_str(&format!("    HostName {}\n", new.host));
    if let Some(user) = &new.user {
        block.push_str(&format!("    User {user}\n"));
    }
    if let Some(port) = new.port {
        block.push_str(&format!("    Port {port}\n"));
    }
    if let Some(key) = &new.key_file {
        block.push_str(&format!("    IdentityFile \"{key}\"\n"));
        // Offer this key, not every key the agent holds: servers cut a
        // client off after a few failed keys.
        block.push_str("    IdentitiesOnly yes\n");
    }
    block
}

/// Whether `config_text` already has a `Host` line naming `name`. Matching
/// is literal (patterns are not expanded) and case-sensitive, as ssh's is.
pub(crate) fn host_is_defined(config_text: &str, name: &str) -> bool {
    config_text.lines().any(|line| {
        let mut words = line.split_whitespace();
        let first = words.next().unwrap_or("");
        let first = first.split('=').next().unwrap_or("");
        first.eq_ignore_ascii_case("host") && line_hosts(line).any(|host| host == name)
    })
}

/// The patterns of a `Host` line, `Host=a b` and `Host a b` alike.
fn line_hosts(line: &str) -> impl Iterator<Item = &str> {
    let rest = line.trim_start();
    let rest = &rest[rest
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(rest.len())..];
    rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=')
        .split_whitespace()
        .map(|host| host.trim_matches('"'))
}

/// `config_text` with `block` after it, separated by a blank line.
pub(crate) fn appended(config_text: &str, block: &str) -> String {
    let mut text = config_text.to_string();
    if !text.is_empty() {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        if !text.ends_with("\n\n") {
            text.push('\n');
        }
    }
    text.push_str(block);
    text
}

/// The user's ssh config file.
pub(crate) fn ssh_config_path(home: &Path) -> PathBuf {
    home.join(".ssh").join("config")
}

/// Adds `new` to the ssh config at `path`, creating it (and `~/.ssh`, owner
/// only) when missing. Blocking file I/O.
pub(crate) fn append_host_block(path: &Path, new: &NewHost, product: &str) -> Result<(), String> {
    let existing = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map_err(|_| format!("{} is not text; left it alone.", path.display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("Could not read {}: {err}", path.display())),
    };
    if host_is_defined(&existing, &new.name) {
        return Err(format!(
            "{} already has a host called {}. Pick another name.",
            path.display(),
            new.name
        ));
    }
    if let Some(dir) = path.parent() {
        if !dir.is_dir() {
            std::fs::create_dir_all(dir)
                .map_err(|err| format!("Could not create {}: {err}", dir.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
    }
    let text = appended(&existing, &host_block(new, product));
    // Written beside it and moved over it, so a failed write cannot leave
    // half a config behind.
    let tmp = path.with_extension("tgz-tmp");
    std::fs::write(&tmp, text.as_bytes())
        .map_err(|err| format!("Could not write {}: {err}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o777)
            .unwrap_or(0o600);
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
    }
    std::fs::rename(&tmp, path).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        format!("Could not write {}: {err}", path.display())
    })
}

/// The command that connects to `name` with `transport`, the program
/// resolved by `resolve`. `None` when that program is not installed.
pub(crate) fn connect_argv(
    name: &str,
    transport: SshTransport,
    resolve: &dyn Fn(&str) -> Option<PathBuf>,
) -> Option<Vec<String>> {
    let program = resolve(transport_program(transport))?;
    Some(vec![
        program.to_string_lossy().into_owned(),
        name.to_string(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled() -> SshSetupForm {
        SshSetupForm {
            name: "web1".into(),
            host: "example.com".into(),
            user: "tim".into(),
            port: "2222".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_filled_form_becomes_a_host_block() {
        let new = validate(&filled()).unwrap();
        assert_eq!(
            host_block(&new, "TGZTerminal"),
            "# Added by TGZTerminal\nHost web1\n    HostName example.com\n    User tim\n    Port 2222\n"
        );
        let mut form = filled();
        form.auth = Auth::Key;
        form.key_file = "C:\\Users\\Tim Grossinger\\.ssh\\id_ed25519".into();
        form.user.clear();
        form.port.clear();
        let block = host_block(&validate(&form).unwrap(), "X");
        assert!(
            block.contains("    IdentityFile \"C:\\Users\\Tim Grossinger\\.ssh\\id_ed25519\"\n")
        );
        assert!(block.contains("    IdentitiesOnly yes\n"));
        assert!(!block.contains("    User ") && !block.contains("    Port "));
    }

    #[test]
    fn the_form_refuses_what_ssh_config_cannot_hold() {
        let cases: [(&dyn Fn(&mut SshSetupForm), &str); 6] = [
            (&|f: &mut SshSetupForm| f.name.clear(), "name"),
            (
                &|f: &mut SshSetupForm| f.name = "two words".into(),
                "one word",
            ),
            (&|f: &mut SshSetupForm| f.name = "web*".into(), "one word"),
            (&|f: &mut SshSetupForm| f.host.clear(), "host"),
            (&|f: &mut SshSetupForm| f.port = "70000".into(), "port"),
            (
                &|f: &mut SshSetupForm| {
                    f.auth = Auth::Key;
                    f.key_file.clear();
                },
                "key file",
            ),
        ];
        for (break_it, says) in cases {
            let mut form = filled();
            break_it(&mut form);
            let err = validate(&form).unwrap_err();
            assert!(err.to_lowercase().contains(says), "{err}");
        }
    }

    #[test]
    fn typing_respects_the_field() {
        let mut form = SshSetupForm::default();
        form.paste("web1\nsecond line");
        assert_eq!(form.name, "web1");
        form.focus = Field::Port;
        form.paste("22a2");
        assert_eq!(form.port, "222");
        form.backspace();
        assert_eq!(form.port, "22");
        form.focus = Field::Auth;
        form.type_char('x');
        form.step_choice(1);
        assert_eq!(form.auth, Auth::Key);
    }

    #[test]
    fn the_key_file_field_shows_only_for_a_key() {
        let mut form = SshSetupForm::default();
        assert!(!form.fields().contains(&Field::KeyFile));
        form.auth = Auth::Key;
        assert!(form.fields().contains(&Field::KeyFile));
        form.focus = Field::Transport;
        form.move_focus(1);
        assert_eq!(form.focus, Field::Name);
        form.move_focus(-1);
        assert_eq!(form.focus, Field::Transport);
        form.focus = Field::Transport;
        form.step_choice(1);
        assert_eq!(form.transport, SshTransport::Mosh);
        form.step_choice(1);
        form.step_choice(1);
        assert_eq!(form.transport, SshTransport::Ssh);
    }

    #[test]
    fn an_existing_host_name_is_found_however_it_is_written() {
        let config = "Host  web1 web2\nHost=db\n  host \"quoted\"\nHostName web3\n";
        for name in ["web1", "web2", "db", "quoted"] {
            assert!(host_is_defined(config, name), "{name}");
        }
        assert!(!host_is_defined(config, "web3"));
        assert!(!host_is_defined(config, "WEB1"));
    }

    #[test]
    fn appending_keeps_the_file_and_separates_the_block() {
        assert_eq!(appended("", "Host a\n"), "Host a\n");
        assert_eq!(appended("Host b", "Host a\n"), "Host b\n\nHost a\n");
        assert_eq!(appended("Host b\n\n", "Host a\n"), "Host b\n\nHost a\n");
    }

    #[test]
    fn writing_appends_once_and_refuses_a_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".ssh").join("config");
        let new = validate(&filled()).unwrap();
        append_host_block(&path, &new, "T").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Added by T\nHost web1\n"));
        assert!(append_host_block(&path, &new, "T")
            .unwrap_err()
            .contains("already has a host called web1"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn connecting_runs_the_chosen_program_with_the_name() {
        let resolve =
            |program: &str| (program != "et").then(|| PathBuf::from(format!("/usr/bin/{program}")));
        assert_eq!(
            connect_argv("web1", SshTransport::Mosh, &resolve),
            Some(vec!["/usr/bin/mosh".to_string(), "web1".to_string()])
        );
        assert_eq!(connect_argv("web1", SshTransport::Et, &resolve), None);
        assert_eq!(
            expand_home("~/.ssh/id", Path::new("/home/t")),
            PathBuf::from("/home/t/.ssh/id")
        );
    }
}
