//! Which program opens a file picked in the tree, and how it is started.
//!
//! `vim` unless `file_browser.editor_command` names something else. The answer
//! is an argv for the window glue to spawn; nothing here starts a process.

use crate::termwindow::wsl_paths;
use std::path::Path;

/// The editor when none is configured.
pub const DEFAULT_EDITOR: &str = "vim";

/// The editor when none is configured, as a command: `vim`, else what
/// every Unix-like system has instead — Debian and Ubuntu often ship only
/// `vi` (vim.tiny) or `nano` — so a pick never ends in "command not found".
/// The file is `$1`.
const FALLBACK_SCRIPT: &str = "for e in vim vi nano; do if command -v \"$e\" >/dev/null 2>&1; then exec \"$e\" \"$@\"; fi; done; echo 'No terminal editor found: install vim, or set file_browser.editor_command' >&2; sleep 6";

/// The command that opens `file` with the configured editor, or with the
/// default's fallback chain when none is configured.
fn editor_command(configured: Option<&[String]>, file: String) -> Vec<String> {
    match configured {
        Some(argv) => with_arg(argv, file),
        None => vec![
            "sh".to_string(),
            "-c".to_string(),
            FALLBACK_SCRIPT.to_string(),
            DEFAULT_EDITOR.to_string(),
            file,
        ],
    }
}

/// Where the editor will run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorHost {
    /// macOS or Linux. Started through the login `shell`, so the PATH the
    /// user set up there applies: a GUI app started from the Dock only has
    /// launchd's.
    Unix { shell: Option<String> },
    /// Windows, outside any distro.
    Windows,
    /// Inside a WSL distro. `own_domain`: the split inherits a domain that
    /// already runs in it; otherwise `wsl.exe` is started from Windows.
    Wsl { distro: String, own_domain: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorLaunch {
    /// Run `argv` in a new pane beside the target pane, in `cwd` (a path as
    /// the pane's own domain sees it).
    Split {
        argv: Vec<String>,
        cwd: Option<String>,
    },
    /// The editor opens a window of its own: start it and do not wait.
    Detached { argv: Vec<String> },
    /// Hand the file to the platform's default application.
    OpenWithSystem(String),
}

/// How to open `file`, a path on this machine.
///
/// `configured` is `file_browser.editor_command`; `gui` is
/// `file_browser.editor_is_gui`.
pub fn resolve_editor(
    configured: Option<&[String]>,
    gui: bool,
    host: &EditorHost,
    file: &Path,
) -> EditorLaunch {
    let configured = configured.filter(|argv| !argv.is_empty());
    let native = file.to_string_lossy().to_string();
    if gui {
        // A window of its own runs on this machine, where the file has its
        // native path. Without a command there is nothing to start but the
        // platform's handler.
        return match configured {
            Some(argv) => EditorLaunch::Detached {
                argv: with_arg(argv, native),
            },
            None => EditorLaunch::OpenWithSystem(native),
        };
    }
    let parent = file.parent().map(|dir| dir.to_string_lossy().to_string());
    match host {
        EditorHost::Unix { shell } => EditorLaunch::Split {
            argv: login_shell_argv(shell.as_deref(), editor_command(configured, native)),
            cwd: parent,
        },
        // No terminal editor ships with Windows: with none configured, the
        // file goes to whatever Windows opens it with.
        EditorHost::Windows => match configured {
            Some(argv) => EditorLaunch::Split {
                argv: with_arg(argv, native),
                cwd: parent,
            },
            None => EditorLaunch::OpenWithSystem(native),
        },
        EditorHost::Wsl { distro, own_domain } => {
            let Some(linux) = wsl_paths::windows_to_wsl(&native, distro) else {
                return EditorLaunch::OpenWithSystem(native);
            };
            let dir = linux
                .rsplit_once('/')
                .map(|(dir, _)| if dir.is_empty() { "/" } else { dir }.to_string());
            let command = wsl_paths::launch_wrap(editor_command(configured, linux));
            if *own_domain {
                EditorLaunch::Split {
                    argv: command,
                    cwd: dir,
                }
            } else {
                let mut argv = wsl_paths::wsl_exec_argv(distro, command);
                if let Some(dir) = &dir {
                    wsl_paths::set_wsl_exec_cwd(&mut argv, dir);
                }
                EditorLaunch::Split { argv, cwd: None }
            }
        }
    }
}

/// The program the configured editor runs, as a lower-case basename.
pub fn editor_program(configured: Option<&[String]>) -> String {
    let program = configured
        .and_then(|argv| argv.first())
        .map(String::as_str)
        .unwrap_or(DEFAULT_EDITOR);
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let name = name.to_ascii_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

/// Whether `program` (a basename) is a vim that understands [`vim_drop_keys`].
pub fn is_vim(program: &str) -> bool {
    let name = program.to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    matches!(name, "vim" | "nvim" | "vi" | "vimx" | "gvim")
}

/// Keys that make a running vim show `path`: out of whatever mode it is in
/// (`Ctrl-\ Ctrl-N`), then `:drop`, which switches to the file's window
/// when it is already open and edits it otherwise. The path is escaped as
/// vim's `fnameescape()` would; a Windows path keeps its backslashes, which
/// are its separators there.
pub fn vim_drop_keys(path: &str) -> String {
    let windows = path.as_bytes().get(1) == Some(&b':') || path.starts_with("\\\\");
    let mut escaped = String::with_capacity(path.len());
    for (index, c) in path.chars().enumerate() {
        let special = matches!(
            c,
            ' ' | '\t'
                | '\n'
                | '*'
                | '?'
                | '['
                | '{'
                | '`'
                | '$'
                | '%'
                | '#'
                | '\''
                | '"'
                | '|'
                | '!'
                | '<'
        ) || (c == '\\' && !windows)
            || (index == 0 && matches!(c, '+' | '>' | '-'));
        if special {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    format!("\x1c\x0e:drop {escaped}\r")
}

fn with_arg(argv: &[String], arg: String) -> Vec<String> {
    let mut argv = argv.to_vec();
    argv.push(arg);
    argv
}

/// `command` run by the user's login shell. Arguments travel as positional
/// parameters and are never parsed as shell. Shells that do not take `$0`
/// and `"$@"` that way (fish, nushell) are passed over for `/bin/sh`.
fn login_shell_argv(shell: Option<&str>, command: Vec<String>) -> Vec<String> {
    let posix = shell.filter(|shell| {
        let name = Path::new(shell)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        matches!(name, "sh" | "bash" | "zsh" | "dash" | "ksh")
    });
    let mut argv = vec![
        posix.unwrap_or("/bin/sh").to_string(),
        "-lc".to_string(),
        r#"exec "$0" "$@""#.to_string(),
    ];
    argv.extend(command);
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unix(shell: &str) -> EditorHost {
        EditorHost::Unix {
            shell: Some(shell.to_string()),
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn vim_by_default_through_the_login_shell() {
        let launch = resolve_editor(
            None,
            false,
            &unix("/bin/zsh"),
            Path::new("/repo/src/main.rs"),
        );
        assert_eq!(
            launch,
            EditorLaunch::Split {
                argv: strings(&[
                    "/bin/zsh",
                    "-lc",
                    r#"exec "$0" "$@""#,
                    "sh",
                    "-c",
                    FALLBACK_SCRIPT,
                    "vim",
                    "/repo/src/main.rs"
                ]),
                cwd: Some("/repo/src".into()),
            }
        );
    }

    /// The fallback chain run for real: with no `vim` on the PATH, `vi`
    /// gets the file.
    #[cfg(unix)]
    #[test]
    fn without_vim_the_default_falls_back_to_vi() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let vi = dir.path().join("vi");
        std::fs::write(
            &vi,
            format!("#!/bin/sh\necho \"vi $1\" > '{}'\n", out.display()),
        )
        .unwrap();
        std::fs::set_permissions(&vi, std::fs::Permissions::from_mode(0o755)).unwrap();
        let argv = editor_command(None, "/r/a b.txt".to_string());
        let status = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .env("PATH", format!("{}:/bin", dir.path().display()))
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "vi /r/a b.txt\n");
    }

    #[test]
    fn configured_editor_replaces_vim_and_keeps_its_arguments() {
        let configured = strings(&["nvim", "-p"]);
        let EditorLaunch::Split { argv, .. } = resolve_editor(
            Some(&configured),
            false,
            &unix("/bin/bash"),
            Path::new("/r/a b.txt"),
        ) else {
            panic!("expected a split");
        };
        assert_eq!(&argv[3..], strings(&["nvim", "-p", "/r/a b.txt"]));
    }

    #[test]
    fn an_empty_command_counts_as_none() {
        let EditorLaunch::Split { argv, .. } =
            resolve_editor(Some(&[]), false, &unix("/bin/sh"), Path::new("/r/x"))
        else {
            panic!("expected a split");
        };
        assert_eq!(&argv[3..6], strings(&["sh", "-c", FALLBACK_SCRIPT]));
    }

    #[test]
    fn shells_without_posix_arguments_fall_back_to_sh() {
        for shell in ["/opt/homebrew/bin/fish", "/usr/bin/nu"] {
            let EditorLaunch::Split { argv, .. } =
                resolve_editor(None, false, &unix(shell), Path::new("/r/x"))
            else {
                panic!("expected a split");
            };
            assert_eq!(argv[0], "/bin/sh");
        }
        let EditorLaunch::Split { argv, .. } = resolve_editor(
            None,
            false,
            &EditorHost::Unix { shell: None },
            Path::new("/r/x"),
        ) else {
            panic!("expected a split");
        };
        assert_eq!(argv[0], "/bin/sh");
    }

    #[test]
    fn gui_editors_start_detached_with_the_native_path() {
        let configured = strings(&["code", "-r"]);
        let file = PathBuf::from(r"\\wsl.localhost\Ubuntu\home\t\a.rs");
        let launch = resolve_editor(
            Some(&configured),
            true,
            &EditorHost::Wsl {
                distro: "Ubuntu".into(),
                own_domain: true,
            },
            &file,
        );
        assert_eq!(
            launch,
            EditorLaunch::Detached {
                argv: strings(&["code", "-r", r"\\wsl.localhost\Ubuntu\home\t\a.rs"]),
            }
        );
        assert_eq!(
            resolve_editor(None, true, &unix("/bin/zsh"), Path::new("/r/x")),
            EditorLaunch::OpenWithSystem("/r/x".into())
        );
    }

    #[test]
    fn windows_without_an_editor_uses_the_system_handler() {
        assert_eq!(
            resolve_editor(None, false, &EditorHost::Windows, Path::new("C:/r/x.txt")),
            EditorLaunch::OpenWithSystem("C:/r/x.txt".into())
        );
        let configured = strings(&["nvim"]);
        let EditorLaunch::Split { argv, .. } = resolve_editor(
            Some(&configured),
            false,
            &EditorHost::Windows,
            Path::new("C:/r/x.txt"),
        ) else {
            panic!("expected a split");
        };
        assert_eq!(argv, strings(&["nvim", "C:/r/x.txt"]));
    }

    #[test]
    fn wsl_runs_vim_inside_the_distro_with_the_linux_path() {
        let file = PathBuf::from(r"\\wsl.localhost\Ubuntu\home\t\src\a.rs");
        let host = EditorHost::Wsl {
            distro: "Ubuntu".into(),
            own_domain: true,
        };
        let EditorLaunch::Split { argv, cwd } = resolve_editor(None, false, &host, &file) else {
            panic!("expected a split");
        };
        assert_eq!(argv[0], "sh");
        assert_eq!(
            &argv[argv.len() - 2..],
            strings(&["vim", "/home/t/src/a.rs"])
        );
        assert_eq!(cwd.as_deref(), Some("/home/t/src"));
    }

    #[test]
    fn wsl_without_its_own_domain_goes_through_wsl_exe() {
        let file = PathBuf::from(r"\\wsl.localhost\Ubuntu\home\t\a.rs");
        let host = EditorHost::Wsl {
            distro: "Ubuntu".into(),
            own_domain: false,
        };
        let EditorLaunch::Split { argv, cwd } = resolve_editor(None, false, &host, &file) else {
            panic!("expected a split");
        };
        assert_eq!(
            &argv[..6],
            strings(&["wsl.exe", "-d", "Ubuntu", "--cd", "/home/t", "--exec"])
        );
        assert_eq!(cwd, None);
    }

    #[test]
    fn vim_is_told_to_drop_onto_the_file() {
        assert_eq!(
            vim_drop_keys("/r/src/main.rs"),
            "\x1c\x0e:drop /r/src/main.rs\r"
        );
        assert_eq!(
            vim_drop_keys("/r/a b%#.md"),
            "\x1c\x0e:drop /r/a\\ b\\%\\#.md\r"
        );
        assert_eq!(vim_drop_keys("+x"), "\x1c\x0e:drop \\+x\r");
        assert_eq!(vim_drop_keys(r"C:\r\a b"), "\x1c\x0e:drop C:\\r\\a\\ b\r");
        assert_eq!(
            vim_drop_keys(r"/r/back\slash"),
            "\x1c\x0e:drop /r/back\\\\slash\r"
        );
    }

    #[test]
    fn the_editor_program_is_its_basename() {
        assert_eq!(editor_program(None), "vim");
        let nvim = strings(&["/opt/homebrew/bin/nvim", "-p"]);
        assert_eq!(editor_program(Some(&nvim)), "nvim");
        let code = strings(&[r"C:\Tools\Code.EXE"]);
        assert_eq!(editor_program(Some(&code)), "code");
        assert!(is_vim("nvim") && is_vim("VIM.exe") && !is_vim("hx") && !is_vim("nano"));
    }

    #[test]
    fn a_file_in_another_distro_goes_to_the_system_handler() {
        let file = PathBuf::from(r"\\wsl.localhost\Debian\x");
        let host = EditorHost::Wsl {
            distro: "Ubuntu".into(),
            own_domain: true,
        };
        assert!(matches!(
            resolve_editor(None, false, &host, &file),
            EditorLaunch::OpenWithSystem(_)
        ));
    }
}
