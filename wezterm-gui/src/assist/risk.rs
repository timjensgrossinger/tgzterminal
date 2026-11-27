//! How risky a suggested shell command looks (TGZTerminal Ask panel).
//!
//! Three labels, worked out locally from the command text. The classifier is
//! deliberately one-sided: it only ever says "Only looks" for a command it
//! recognises as read-only, and anything it does not know is "Changes
//! things". The order follows the usual pattern for this (Warp's agent
//! permissions, koda's bash safety tiers): the delete list is checked
//! first, then the read-only list, and command runners (`sudo`, `env`,
//! `xargs`, `sh -c`, `find -exec`, ...) are unwrapped so `env rm x` is not
//! mistaken for `env`. A pipeline or a `&&`/`;` chain takes the riskiest of
//! its parts, and any output redirection to a file is at least a change.
//!
//! It is a label for a human about to press a key, not a sandbox: obfuscated
//! commands can fool it, and the panel never runs anything on its own.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    OnlyLooks,
    Changes,
    Deletes,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Risk::OnlyLooks => "Only looks",
            Risk::Changes => "Changes things",
            Risk::Deletes => "Deletes",
        }
    }
}

/// Classify a whole command line (possibly several lines).
pub fn classify(command: &str) -> Risk {
    classify_depth(command, 0)
}

fn classify_depth(command: &str, depth: usize) -> Risk {
    if depth > 4 {
        return Risk::Changes;
    }
    let Some(segments) = split_segments(command) else {
        // Unbalanced quotes: we cannot see what it does.
        return Risk::Changes;
    };
    let mut worst = Risk::OnlyLooks;
    let mut any = false;
    for segment in segments {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        any = true;
        worst = worst.max(classify_simple(segment, depth));
        if worst == Risk::Deletes {
            break;
        }
    }
    if any {
        worst
    } else {
        Risk::OnlyLooks
    }
}

/// Split on `|`, `||`, `&&`, `;`, `&` and newlines outside quotes. Also
/// splits out `$(...)` and backtick bodies as their own segments, since they
/// run too. `None` for unbalanced quotes.
fn split_segments(command: &str) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut single = false;
    let mut double = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' if !single => {
                current.push(c);
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '\'' if !double => {
                single = !single;
                current.push(c);
            }
            '"' if !single => {
                double = !double;
                current.push(c);
            }
            '$' if !single && chars.peek() == Some(&'(') => {
                chars.next();
                let mut depth = 1;
                let mut inner = String::new();
                for c in chars.by_ref() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    inner.push(c);
                }
                segments.push(inner);
                current.push_str("SUBST");
            }
            '`' if !single => {
                let inner: String = chars.by_ref().take_while(|c| *c != '`').collect();
                segments.push(inner);
                current.push_str("SUBST");
            }
            '|' | '&' | ';' | '\n' if !single && !double => {
                // `2>&1` and `&>` are redirections, not separators.
                if c == '&' && (current.ends_with('>') || chars.peek() == Some(&'>')) {
                    current.push(c);
                    continue;
                }
                if let Some(next) = chars.peek() {
                    if (c == '|' && *next == '|') || (c == '&' && *next == '&') {
                        chars.next();
                    }
                }
                segments.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    if single || double {
        return None;
    }
    segments.push(current);
    Some(segments)
}

/// Is there an output redirection to something other than /dev/null or an
/// fd? (`> file`, `>> file`, `&> file`, `tee file`).
fn writes_file(words: &[String]) -> bool {
    let mut iter = words.iter().peekable();
    while let Some(word) = iter.next() {
        let target = if let Some(pos) = word.find('>') {
            let after = word[pos..].trim_start_matches('>').trim_start_matches('|');
            if after.is_empty() {
                iter.peek().map(|s| s.as_str()).unwrap_or("")
            } else {
                after
            }
        } else {
            continue;
        };
        let target = target.trim_start_matches('&');
        if target.is_empty() || target == "/dev/null" || target.chars().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        return true;
    }
    false
}

const RUNNERS: &[&str] = &[
    "env",
    "nice",
    "nohup",
    "time",
    "command",
    "builtin",
    "exec",
    "caffeinate",
    "timeout",
    "stdbuf",
    "ionice",
    "chronic",
];

const SHELLS: &[&str] = &["sh", "bash", "zsh", "fish", "dash", "ksh"];

const DELETERS: &[&str] = &[
    "rm", "rmdir", "unlink", "shred", "srm", "trash", "dd", "truncate", "wipefs", "fdisk",
    "sfdisk", "parted",
];

/// Read-only on their own (any arguments), modulo the per-command checks
/// in `classify_simple`.
const LOOKERS: &[&str] = &[
    "ls",
    "ll",
    "la",
    "l",
    "cat",
    "bat",
    "less",
    "more",
    "head",
    "tail",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "ack",
    "fd",
    "tree",
    "wc",
    "file",
    "stat",
    "du",
    "df",
    "pwd",
    "echo",
    "printf",
    "which",
    "whereis",
    "type",
    "whoami",
    "id",
    "groups",
    "uname",
    "hostname",
    "date",
    "cal",
    "uptime",
    "printenv",
    "ps",
    "pgrep",
    "top",
    "htop",
    "btop",
    "lsof",
    "netstat",
    "ss",
    "ping",
    "traceroute",
    "dig",
    "nslookup",
    "host",
    "sort",
    "uniq",
    "cut",
    "tr",
    "jq",
    "yq",
    "column",
    "diff",
    "cmp",
    "comm",
    "md5",
    "md5sum",
    "shasum",
    "sha256sum",
    "cksum",
    "xxd",
    "hexdump",
    "od",
    "strings",
    "otool",
    "nm",
    "ldd",
    "readlink",
    "realpath",
    "basename",
    "dirname",
    "man",
    "tldr",
    "history",
    "jobs",
    "true",
    "false",
    "test",
    "[",
    "seq",
    "nproc",
    "sw_vers",
    "system_profiler",
    "sysctl",
    "vm_stat",
    "iostat",
    "free",
    "lsblk",
    "blkid",
    "mount",
    "ifconfig",
    "ip",
    "arp",
    "route",
    "awk",
    "gawk",
    "file",
    "locate",
    "mdfind",
    "codesign",
    "spctl",
    "plutil",
    "defaults",
    "launchctl",
    "log",
    "journalctl",
    "dmesg",
    "tput",
    "clear",
    "sleep",
    "watch",
    "nl",
    "fold",
    "fmt",
    "rev",
    "tac",
    "base64",
    "zcat",
    "zless",
    "xzcat",
    "bzcat",
    "unzip",
    "tar",
    "cd",
    "pushd",
    "popd",
];

/// Subcommands that only look, for tools whose other subcommands change
/// things.
fn looking_subcommand(program: &str, args: &[String]) -> Option<bool> {
    let first = args.iter().find(|arg| !arg.starts_with('-'))?.as_str();
    let looks = match program {
        "git" => {
            matches!(
                first,
                "status"
                    | "log"
                    | "diff"
                    | "show"
                    | "blame"
                    | "grep"
                    | "describe"
                    | "rev-parse"
                    | "ls-files"
                    | "ls-tree"
                    | "shortlog"
                    | "reflog"
                    | "whatchanged"
                    | "cat-file"
                    | "remote"
                    | "branch"
                    | "tag"
                    | "config"
            ) || (first == "stash" && args.iter().any(|arg| arg == "list" || arg == "show"))
        }
        "docker" | "podman" => matches!(
            first,
            "ps" | "images" | "logs" | "inspect" | "stats" | "top" | "version" | "info" | "history"
        ),
        "kubectl" | "oc" => matches!(
            first,
            "get"
                | "describe"
                | "logs"
                | "top"
                | "explain"
                | "version"
                | "api-resources"
                | "events"
        ),
        "brew" => matches!(
            first,
            "list" | "info" | "search" | "outdated" | "deps" | "doctor" | "config" | "leaves"
        ),
        "npm" | "pnpm" | "yarn" => matches!(
            first,
            "ls" | "list" | "outdated" | "view" | "info" | "why" | "audit" | "explain" | "root"
        ),
        "cargo" => matches!(
            first,
            "tree" | "metadata" | "search" | "version" | "locate-project"
        ),
        "systemctl" => matches!(
            first,
            "status" | "list-units" | "is-active" | "show" | "cat"
        ),
        "gh" => {
            matches!(first, "status" | "browse")
                || (args.len() >= 2
                    && matches!(
                        args[1].as_str(),
                        "list" | "view" | "status" | "diff" | "checks"
                    ))
        }
        _ => return None,
    };
    Some(looks)
}

/// Destructive subcommands of tools that are not deleters by name.
fn deleting_subcommand(program: &str, args: &[String]) -> bool {
    let has = |flag: &str| args.iter().any(|arg| arg == flag);
    let first = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .map(String::as_str)
        .unwrap_or("");
    match program {
        "git" => match first {
            "clean" => true,
            "reset" => has("--hard") || has("--merge") || has("--keep"),
            "branch" => has("-D") || has("-d") || has("--delete"),
            "push" => has("--force") || has("-f") || has("--delete") || has("--force-with-lease"),
            "checkout" => has("--") || has("."),
            "restore" => true,
            "stash" => args.iter().any(|arg| arg == "drop" || arg == "clear"),
            "tag" => has("-d") || has("--delete"),
            "rm" => true,
            "gc" => args.iter().any(|arg| arg.starts_with("--prune")),
            "worktree" => args.iter().any(|arg| arg == "remove" || arg == "prune"),
            _ => false,
        },
        "docker" | "podman" => {
            matches!(first, "rm" | "rmi" | "prune")
                || args.iter().any(|arg| arg == "prune" || arg == "rm")
        }
        "kubectl" | "oc" => matches!(first, "delete" | "drain"),
        "helm" => matches!(first, "uninstall" | "delete"),
        "terraform" | "tofu" => first == "destroy" || has("-destroy"),
        "brew" => matches!(
            first,
            "uninstall" | "remove" | "rm" | "cleanup" | "autoremove"
        ),
        "npm" | "pnpm" | "yarn" => matches!(first, "uninstall" | "remove" | "rm" | "prune"),
        "cargo" => first == "clean",
        "rsync" => args
            .iter()
            .any(|arg| arg.starts_with("--delete") || arg == "--remove-source-files"),
        "diskutil" => args
            .iter()
            .any(|arg| arg.starts_with("erase") || arg == "zeroDisk" || arg == "randomDisk"),
        "find" => has("-delete"),
        "redis-cli" => args
            .iter()
            .any(|arg| arg.eq_ignore_ascii_case("flushall") || arg.eq_ignore_ascii_case("flushdb")),
        "psql" | "mysql" | "sqlite3" | "mariadb" | "duckdb" => args.iter().any(|arg| {
            let upper = arg.to_ascii_uppercase();
            upper.contains("DROP ") || upper.contains("DELETE ") || upper.contains("TRUNCATE ")
        }),
        "aws" => args.iter().any(|arg| {
            arg == "rm"
                || arg == "rb"
                || arg.starts_with("delete-")
                || arg.starts_with("terminate-")
        }),
        _ => false,
    }
}

fn classify_simple(segment: &str, depth: usize) -> Risk {
    let Some(mut words) = shlex::split(segment) else {
        return Risk::Changes;
    };
    // Leading `VAR=value` assignments.
    while words
        .first()
        .is_some_and(|word| word.contains('=') && !word.starts_with('=') && !word.starts_with('-'))
    {
        words.remove(0);
    }
    if words.is_empty() {
        return Risk::OnlyLooks;
    }
    let redirect = writes_file(&words);
    let floor = if redirect {
        Risk::Changes
    } else {
        Risk::OnlyLooks
    };

    let program = words[0].rsplit('/').next().unwrap_or("").to_string();
    let args: Vec<String> = words[1..]
        .iter()
        .filter(|word| !word.contains('>'))
        .cloned()
        .collect();

    // Runners: classify what they run.
    if program == "sudo" || program == "doas" {
        let rest = skip_flags(&args);
        if rest.is_empty() {
            return Risk::Changes;
        }
        return classify_depth(
            &shlex::try_join(rest.iter().map(String::as_str)).unwrap_or_default(),
            depth + 1,
        )
        .max(Risk::Changes)
        .max(floor);
    }
    if RUNNERS.contains(&program.as_str()) {
        let rest: Vec<&String> = args
            .iter()
            .skip_while(|arg| {
                arg.starts_with('-')
                    || (program == "env" && arg.contains('='))
                    || (program == "timeout"
                        && arg.chars().next().is_some_and(|c| c.is_ascii_digit()))
            })
            .collect();
        if rest.is_empty() {
            return floor;
        }
        let inner = shlex::try_join(rest.iter().map(|s| s.as_str())).unwrap_or_default();
        return classify_depth(&inner, depth + 1).max(floor);
    }
    if program == "xargs" {
        let rest = skip_flags(&args);
        if rest.is_empty() {
            // xargs with no command runs echo.
            return floor;
        }
        let inner = shlex::try_join(rest.iter().map(String::as_str)).unwrap_or_default();
        return classify_depth(&inner, depth + 1).max(floor);
    }
    if SHELLS.contains(&program.as_str()) {
        if let Some(pos) = args
            .iter()
            .position(|arg| arg == "-c" || arg == "-lc" || arg == "-ic")
        {
            return match args.get(pos + 1) {
                Some(script) => classify_depth(script, depth + 1).max(floor),
                None => Risk::Changes,
            };
        }
        if args.is_empty() || args.iter().any(|arg| arg == "--version") {
            return floor;
        }
        // Running a script file: we cannot see inside it.
        return Risk::Changes;
    }
    if program == "eval" || program == "source" || program == "." {
        if program == "eval" {
            return classify_depth(&args.join(" "), depth + 1).max(floor);
        }
        return Risk::Changes;
    }

    if DELETERS.contains(&program.as_str()) || program.starts_with("mkfs") {
        return Risk::Deletes;
    }
    if deleting_subcommand(&program, &args) {
        return Risk::Deletes;
    }

    // `find` runs other programs with -exec / -ok.
    if program == "find" {
        if let Some(pos) = args
            .iter()
            .position(|arg| matches!(arg.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir"))
        {
            let inner: Vec<&str> = args[pos + 1..]
                .iter()
                .map(String::as_str)
                .take_while(|arg| *arg != ";" && *arg != "+" && *arg != "\\;")
                .filter(|arg| *arg != "{}")
                .collect();
            let inner = shlex::try_join(inner).unwrap_or_default();
            return classify_depth(&inner, depth + 1)
                .max(Risk::Changes)
                .max(floor);
        }
        if args
            .iter()
            .any(|arg| arg.starts_with("-fprint") || arg == "-fls")
        {
            return Risk::Changes;
        }
        return floor;
    }

    // Anything that only reports its version or help.
    if args.len() == 1
        && matches!(
            args[0].as_str(),
            "--version" | "-V" | "--help" | "-h" | "version"
        )
    {
        return floor;
    }

    if let Some(looks) = looking_subcommand(&program, &args) {
        if looks && !writes_in_place(&program, &args) {
            return floor;
        }
        return Risk::Changes;
    }

    if LOOKERS.contains(&program.as_str()) {
        if writes_in_place(&program, &args) {
            return Risk::Changes;
        }
        return floor;
    }
    match program.as_str() {
        "sed" if !writes_in_place(&program, &args) => floor,
        "curl" | "http" | "xh" => {
            let changes = args.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "-o" | "-O"
                        | "--output"
                        | "--remote-name"
                        | "-d"
                        | "--data"
                        | "-F"
                        | "--form"
                        | "-T"
                        | "--upload-file"
                ) || arg.starts_with("--data")
            }) || args.windows(2).any(|pair| {
                (pair[0] == "-X" || pair[0] == "--request")
                    && !pair[1].eq_ignore_ascii_case("GET")
                    && !pair[1].eq_ignore_ascii_case("HEAD")
            });
            if changes {
                Risk::Changes
            } else {
                floor
            }
        }
        _ => Risk::Changes,
    }
}

/// Flags that turn a looking tool into an editing one.
fn writes_in_place(program: &str, args: &[String]) -> bool {
    match program {
        "sed" | "perl" => args
            .iter()
            .any(|arg| arg.starts_with("-i") || arg == "--in-place"),
        "tar" => {
            args.first().is_some_and(|mode| {
                !mode.starts_with('-') && (mode.contains('x') || mode.contains('c'))
            }) || args.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "-x" | "-c"
                        | "--extract"
                        | "--create"
                        | "-cf"
                        | "-xf"
                        | "-czf"
                        | "-xzf"
                        | "-cjf"
                        | "-xjf"
                )
            })
        }
        "unzip" => !args
            .iter()
            .any(|arg| arg == "-l" || arg == "-t" || arg == "-v"),
        "defaults" => args
            .iter()
            .any(|arg| matches!(arg.as_str(), "write" | "delete" | "import" | "rename")),
        "launchctl" => args.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "load"
                    | "unload"
                    | "bootstrap"
                    | "bootout"
                    | "kickstart"
                    | "kill"
                    | "remove"
                    | "enable"
                    | "disable"
                    | "setenv"
            )
        }),
        "mount" | "ip" | "route" | "ifconfig" | "arp" => args.len() > 1,
        "git" => {
            args.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "--set-upstream-to"
                        | "-m"
                        | "-M"
                        | "add"
                        | "rename"
                        | "set-url"
                        | "pop"
                        | "apply"
                        | "push"
                        | "save"
                        | "--unset"
                        | "--add"
                        | "--replace-all"
                )
            }) || (args.first().is_some_and(|a| a == "config")
                && args.iter().filter(|a| !a.starts_with('-')).count() > 2)
                || (args.first().is_some_and(|a| a == "tag")
                    && args.iter().filter(|a| !a.starts_with('-')).count() > 1)
                || (args.first().is_some_and(|a| a == "branch")
                    && args.iter().filter(|a| !a.starts_with('-')).count() > 1)
        }
        "codesign" => !args.iter().any(|arg| {
            arg.starts_with("-d")
                || arg.starts_with("-v")
                || arg == "--verify"
                || arg == "--display"
        }),
        "plutil" => args.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "-convert" | "-insert" | "-replace" | "-remove"
            )
        }),
        "sysctl" => args.iter().any(|arg| arg.contains('=') || arg == "-w"),
        "log" => args.first().is_some_and(|a| a == "erase" || a == "config"),
        "awk" | "gawk" => args
            .iter()
            .any(|arg| arg.contains("system(") || arg.contains("> \"") || arg.contains(">\"")),
        _ => false,
    }
}

fn skip_flags(args: &[String]) -> Vec<String> {
    args.iter()
        .skip_while(|arg| arg.starts_with('-'))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table() {
        use Risk::*;
        let cases: &[(&str, Risk)] = &[
            ("ls -la", OnlyLooks),
            ("cat Cargo.toml | grep version", OnlyLooks),
            ("git status", OnlyLooks),
            ("git log -3 --oneline", OnlyLooks),
            ("git diff HEAD~1 -- src", OnlyLooks),
            ("rg 'fn main' src", OnlyLooks),
            ("npm test -- --test-name-pattern=304", Changes),
            ("cargo build", Changes),
            ("echo hi > out.txt", Changes),
            ("echo hi >> out.txt", Changes),
            ("make 2>&1 | tail -20", Changes),
            ("ls 2>/dev/null", OnlyLooks),
            ("ls &>/dev/null", OnlyLooks),
            ("grep foo file 2>&1", OnlyLooks),
            ("cat a | tee b", Changes),
            ("rm -rf build", Deletes),
            ("sudo rm -rf /", Deletes),
            ("sudo ls", Changes),
            ("env FOO=1 rm x", Deletes),
            ("FOO=1 rm x", Deletes),
            ("xargs rm < files", Deletes),
            ("find . -name '*.o' | xargs rm", Deletes),
            ("find . -name '*.o' -delete", Deletes),
            ("find . -name '*.rs' -exec rm {} \\;", Deletes),
            ("find . -name '*.rs' -exec grep foo {} +", Changes),
            ("find . -name '*.rs'", OnlyLooks),
            ("sh -c 'rm -rf x'", Deletes),
            ("bash -lc \"ls\"", OnlyLooks),
            ("eval rm x", Deletes),
            ("echo $(rm x)", Deletes),
            ("echo `rm x`", Deletes),
            ("git clean -fdx", Deletes),
            ("git reset --hard HEAD", Deletes),
            ("git reset HEAD~1", Changes),
            ("git push --force", Deletes),
            ("git push", Changes),
            ("git checkout -- .", Deletes),
            ("git checkout main", Changes),
            ("git branch -D old", Deletes),
            ("git branch", OnlyLooks),
            ("git stash drop", Deletes),
            ("git stash list", OnlyLooks),
            ("git stash", Changes),
            ("docker rm -f web", Deletes),
            ("docker system prune -a", Deletes),
            ("docker ps -a", OnlyLooks),
            ("kubectl delete pod x", Deletes),
            ("kubectl get pods", OnlyLooks),
            ("terraform destroy", Deletes),
            ("terraform plan", Changes),
            ("psql -c 'DROP TABLE users'", Deletes),
            ("psql -c 'select 1'", Changes),
            ("sed -i '' 's/a/b/' f", Changes),
            ("sed 's/a/b/' f", OnlyLooks),
            ("curl https://example.com", OnlyLooks),
            ("curl -o x https://example.com", Changes),
            ("curl -X POST https://example.com", Changes),
            ("tar tzf a.tgz", OnlyLooks),
            ("tar xzf a.tgz", Changes),
            ("unzip -l a.zip", OnlyLooks),
            ("unzip a.zip", Changes),
            ("defaults read com.apple.dock", OnlyLooks),
            ("defaults write com.apple.dock x -int 1", Changes),
            ("python3 script.py", Changes),
            ("./deploy.sh", Changes),
            ("node --version", OnlyLooks),
            ("cd src && ls", OnlyLooks),
            ("ls; rm x", Deletes),
            ("ls || true", OnlyLooks),
            ("echo 'it''s' \"unterminated", Changes),
            ("", OnlyLooks),
            ("mkfs.ext4 /dev/sdb1", Deletes),
            ("dd if=/dev/zero of=/dev/disk2", Deletes),
            ("rsync -a --delete src/ dst/", Deletes),
            ("rsync -a src/ dst/", Changes),
            ("brew uninstall foo", Deletes),
            ("brew list", OnlyLooks),
            ("cargo clean", Deletes),
            ("which claude", OnlyLooks),
            ("timeout 5 rm x", Deletes),
            ("nohup ls", OnlyLooks),
            ("awk '{print $1}' f", OnlyLooks),
        ];
        let mut failures = Vec::new();
        for (command, want) in cases {
            let got = classify(command);
            if got != *want {
                failures.push(format!("{command:?}: got {got:?}, want {want:?}"));
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }

    #[test]
    fn unknown_commands_never_only_look() {
        for command in ["frobnicate --all", "mystery", "./x", "make"] {
            assert_ne!(classify(command), Risk::OnlyLooks, "{command}");
        }
    }
}
