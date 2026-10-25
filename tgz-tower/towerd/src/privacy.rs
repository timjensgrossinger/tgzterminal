//! Sensitive work: what must only ever be seen by models inside your own
//! network.
//!
//! ```toml
//! # tower.toml
//! [privacy]
//! local_only_paths = ["~/src/payroll"]   # whole folders
//! return = "status_only"                 # what a cloud agent hears back
//!
//! # a repo's own .tower.toml
//! [privacy]
//! local_only = true                      # the whole repo
//! sensitive_paths = ["secrets/**", "customers/*.csv"]
//! ```
//!
//! Folder, repo, flight and intake-source marks are a hard rule: Tower never
//! starts a cloud model on such work (`Engine::launch` refuses). Sensitive
//! paths inside an otherwise normal repo are best effort: cloud agents Tower
//! starts are denied those paths by their own permission rules, which a shell
//! command can still get around.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// What a cloud agent hears back from a local-only handoff.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReturnMode {
    /// Only that it is done and how the checks went.
    #[default]
    StatusOnly,
    /// The local model's whole summary.
    Summary,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
pub struct PrivacyCfg {
    /// `tower.toml`: folders whose work is local-only (`~` expanded).
    #[serde(default)]
    pub local_only_paths: Vec<String>,
    /// A repo's `.tower.toml`: the whole repo is local-only.
    #[serde(default)]
    pub local_only: bool,
    /// A repo's `.tower.toml`: globs (relative to the repo) cloud agents
    /// must not read.
    #[serde(default)]
    pub sensitive_paths: Vec<String>,
    #[serde(default, rename = "return")]
    pub return_mode: ReturnMode,
    /// `tower.toml`: CLIs you start yourself that may still work in a
    /// local-only folder (e.g. `["opencode"]` when yours runs local models).
    /// Every other agent CLI is blocked there.
    #[serde(default)]
    pub allow_clis: Vec<String>,
}

fn expand(p: &str) -> PathBuf {
    let path = crate::config::expand_home(p.trim());
    path.canonicalize().unwrap_or(path)
}

/// A repo's merged `[privacy]` (see `config::repo_privacy`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoPrivacy {
    /// Why the repo's work is local-only, if it is.
    pub reason: Option<String>,
    /// Sensitive globs, each with the folder it is relative to.
    pub globs: Vec<(PathBuf, String)>,
}

/// Why work in `dir` must stay local: a `local_only_paths` folder it is
/// in, or its repo's own rule. `None` when it may go anywhere.
pub fn folder_reason(dir: &Path, global: &PrivacyCfg, repo: &RepoPrivacy) -> Option<String> {
    let dir = canonical_lossy(dir);
    for p in &global.local_only_paths {
        if p.trim().is_empty() {
            continue;
        }
        if starts_with_ci(&dir, &canonical_lossy(&expand(p))) {
            return Some(format!("{} is a local-only folder", p.trim()));
        }
    }
    repo.reason.clone()
}

/// `p` with symlinks resolved as far as it exists: the nearest existing
/// folder is canonicalized and the rest appended, normalized.
pub fn canonical_lossy(p: &Path) -> PathBuf {
    let p = normalize(p);
    let mut base = p.as_path();
    let mut rest = vec![];
    loop {
        if let Ok(c) = base.canonicalize() {
            let mut out = c;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                base = parent;
            }
            _ => return p,
        }
    }
}

/// `a/./b/../c` -> `a/c`, without touching the disk.
pub fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// macOS and Windows file systems ignore case, so the comparison does too.
fn case_blind() -> bool {
    cfg!(any(target_os = "macos", windows))
}

fn starts_with_ci(p: &Path, base: &Path) -> bool {
    if !case_blind() {
        return p.starts_with(base);
    }
    let low = |x: &Path| PathBuf::from(x.to_string_lossy().to_lowercase());
    low(p).starts_with(low(base))
}

/// Does `rel` (a path relative to the repo, `/`-separated) match `glob`?
/// `**` spans folders, `*` and `?` stay within one name; a glob without a
/// `/` matches the name anywhere.
pub fn glob_match(glob: &str, rel: &str) -> bool {
    let glob = glob.trim().trim_start_matches("./").trim_start_matches('/');
    let rel = rel.trim_start_matches("./").trim_start_matches('/');
    if glob.is_empty() {
        return false;
    }
    let (glob, rel) = if case_blind() {
        (glob.to_lowercase(), rel.to_lowercase())
    } else {
        (glob.to_string(), rel.to_string())
    };
    if !glob.contains('/') {
        return rel.split('/').any(|name| segment_match(&glob, name));
    }
    let g: Vec<&str> = glob.split('/').collect();
    let r: Vec<&str> = rel.split('/').collect();
    parts_match(&g, &r)
}

fn parts_match(g: &[&str], r: &[&str]) -> bool {
    match g.first() {
        None => r.is_empty(),
        Some(&"**") => (0..=r.len()).any(|i| parts_match(&g[1..], &r[i..])),
        Some(seg) => !r.is_empty() && segment_match(seg, r[0]) && parts_match(&g[1..], &r[1..]),
    }
}

fn segment_match(pat: &str, name: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let n: Vec<char> = name.chars().collect();
    fn go(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|i| go(&p[1..], &n[i..])),
            Some('?') => !n.is_empty() && go(&p[1..], &n[1..]),
            Some(c) => n.first() == Some(c) && go(&p[1..], &n[1..]),
        }
    }
    go(&p, &n)
}

/// Is `path` (absolute, or relative to `cwd`) under one of `globs`? Paths
/// are normalized (`..`, `.`, symlinks where they exist) and compared
/// without case where the file system ignores it.
pub fn is_sensitive(path: &str, cwd: &Path, globs: &[(PathBuf, String)]) -> bool {
    let p = Path::new(path.trim());
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    let abs = canonical_lossy(&abs);
    globs.iter().any(|(base, g)| {
        let base = normalize(base);
        let rel = if case_blind() {
            let a = abs.to_string_lossy().to_lowercase();
            let b = base.to_string_lossy().to_lowercase();
            a.strip_prefix(&b).map(str::to_string)
        } else {
            abs.strip_prefix(&base)
                .ok()
                .map(|r| r.to_string_lossy().to_string())
        };
        match rel {
            Some(r) => {
                let r = r.replace('\\', "/");
                // A text prefix: "/repo-old" is not inside "/repo".
                if !(r.is_empty() || r.starts_with('/')) {
                    return false;
                }
                glob_match(g, r.trim_start_matches('/'))
            }
            None => false,
        }
    })
}

/// Claude Code permission rules keeping a cloud agent out of `globs`
/// (absolute, so they hold wherever in the repo the agent starts).
pub fn claude_deny_rules(globs: &[(PathBuf, String)]) -> Vec<String> {
    let mut out = vec![];
    for (base, g) in globs {
        let g = g.trim().trim_start_matches("./").trim_start_matches('/');
        if g.is_empty() {
            continue;
        }
        let base = base.to_string_lossy().replace('\\', "/");
        let base = base.trim_end_matches('/');
        let path = if g.contains('/') {
            format!("/{base}/{g}")
        } else {
            format!("/{base}/**/{g}")
        };
        out.push(format!("Read({path})"));
        out.push(format!("Edit({path})"));
    }
    out
}

/// For a cloud agent's system prompt.
pub fn prompt(globs: &[(PathBuf, String)]) -> String {
    if globs.is_empty() {
        return String::new();
    }
    let list: Vec<String> = globs
        .iter()
        .map(|(b, g)| format!("{}/{}", b.display(), g.trim().trim_start_matches("./")))
        .collect();
    format!(
        "\nSensitive paths (only local models may see them; Tower blocks them for you): {}. Never open, print, copy or summarise them, also not through a shell. If a task needs them, hand it to a local model with tower_delegate (name the files; set local_only) and keep its result out of your own answer.\n",
        list.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_cover_folders_names_and_wildcards() {
        assert!(glob_match("secrets/**", "secrets/a/b.key"));
        assert!(glob_match("secrets/**", "secrets"));
        assert!(!glob_match("secrets/**", "src/secrets.rs"));
        assert!(glob_match("customers/*.csv", "customers/acme.csv"));
        assert!(!glob_match("customers/*.csv", "customers/x/acme.csv"));
        assert!(
            glob_match("*.pem", "deploy/certs/prod.pem"),
            "a bare name matches anywhere"
        );
        assert!(glob_match("**/.env", ".env"));
        assert!(glob_match("./data/?.json", "data/a.json"));
        assert!(!glob_match("", "anything"));
    }

    #[test]
    fn paths_are_checked_relative_to_the_repo_and_cannot_dodge_it() {
        let repo = PathBuf::from("/w/repo");
        let globs = vec![(repo.clone(), "secrets/**".to_string())];
        assert!(is_sensitive("/w/repo/secrets/db.key", &repo, &globs));
        assert!(is_sensitive("secrets/db.key", &repo, &globs));
        assert!(is_sensitive("src/../secrets/db.key", &repo, &globs), "..");
        assert!(is_sensitive("/w/repo/./secrets/k", &repo, &globs), ".");
        assert!(!is_sensitive("/w/other/secrets/db.key", &repo, &globs));
        assert!(!is_sensitive("/w/repo-old/secrets/db.key", &repo, &globs));
        assert!(!is_sensitive("/w/repo/src/main.rs", &repo, &globs));
        if cfg!(target_os = "macos") {
            assert!(
                is_sensitive("/w/repo/Secrets/db.key", &repo, &globs),
                "case"
            );
        }
    }

    #[test]
    fn folders_and_repos_make_work_local_only() {
        let dir = std::env::temp_dir().join(format!("tower-priv-{}", crate::paths::random_u64()));
        let inner = dir.join("payroll").join("src");
        std::fs::create_dir_all(&inner).unwrap();
        let global = PrivacyCfg {
            local_only_paths: vec![dir.join("payroll").display().to_string()],
            ..Default::default()
        };
        let none = PrivacyCfg::default();
        let open = RepoPrivacy::default();
        assert!(folder_reason(&inner, &global, &open).is_some());
        assert!(folder_reason(&inner.join("..").join("src"), &global, &open).is_some());
        assert!(folder_reason(&dir, &global, &open).is_none());
        let sibling = dir.join("payroll-tower");
        std::fs::create_dir_all(&sibling).unwrap();
        assert!(folder_reason(&sibling, &global, &open).is_none());
        let repo = RepoPrivacy {
            reason: Some("x/.tower.toml marks it local-only".into()),
            ..Default::default()
        };
        assert!(folder_reason(&dir, &none, &repo)
            .unwrap()
            .contains(".tower.toml"));
    }

    #[test]
    fn claude_rules_are_absolute_and_deny_reading_and_editing() {
        let base = PathBuf::from("/w/repo");
        let rules = claude_deny_rules(&[
            (base.clone(), "secrets/**".into()),
            (base.clone(), "*.pem".into()),
            (base, " ".into()),
        ]);
        assert_eq!(
            rules,
            vec![
                "Read(//w/repo/secrets/**)",
                "Edit(//w/repo/secrets/**)",
                "Read(//w/repo/**/*.pem)",
                "Edit(//w/repo/**/*.pem)"
            ]
        );
        assert!(prompt(&[]).is_empty());
    }
}
