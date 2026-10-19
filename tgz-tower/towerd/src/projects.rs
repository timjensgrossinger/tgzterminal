//! Which local project an issue belongs to.
//!
//! In order of trust:
//! 1. `[[project]]` rules in `tower.toml` (Jira keys, GitHub/GitLab repos);
//! 2. answers you gave before (remembered per Jira project or repo);
//! 3. a clone whose git remote is the issue's GitHub/GitLab repo;
//! 4. otherwise candidates scored by name and keywords, for triage (or you)
//!    to pick from.
//!
//! The index of local repos is a scan of `[projects] roots` on a worker
//! thread; `.git/config` is read as a file, so no `git` process per repo.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tower_core::router::has_word;

#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
pub struct ProjectRule {
    pub path: String,
    #[serde(default)]
    pub jira: Vec<String>,
    #[serde(default)]
    pub github: Vec<String>,
    #[serde(default)]
    pub gitlab: Vec<String>,
    /// Words in an issue that point here, e.g. ["sidebar", "renderer"].
    #[serde(default)]
    pub keywords: Vec<String>,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct ProjectsCfg {
    /// Folders searched for git repositories.
    #[serde(default = "default_roots")]
    pub roots: Vec<String>,
    #[serde(default = "default_depth")]
    pub depth: usize,
}

fn default_roots() -> Vec<String> {
    [
        "~/Documents",
        "~/code",
        "~/src",
        "~/projects",
        "~/dev",
        "~/Developer",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}
fn default_depth() -> usize {
    3
}

impl Default for ProjectsCfg {
    fn default() -> Self {
        Self {
            roots: default_roots(),
            depth: default_depth(),
        }
    }
}

/// How far auto-fix goes after the gates pass and you land the flight.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LandMode {
    /// Commit, push the branch and open a draft PR/MR (with your clearance).
    #[default]
    Pr,
    /// Commit on the local branch; nothing leaves the machine.
    Branch,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct AutofixCfg {
    #[serde(default)]
    pub land: LandMode,
    /// Gates when the repo sets none: detect `cargo test`, `npm test`, ….
    #[serde(default = "yes")]
    pub detect_gates: bool,
}

fn yes() -> bool {
    true
}

impl Default for AutofixCfg {
    fn default() -> Self {
        Self {
            land: LandMode::Pr,
            detect_gates: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct RepoInfo {
    pub path: String,
    pub name: String,
    /// Normalized: `github.com/owner/repo`.
    pub remotes: Vec<String>,
    /// First lines of the README.
    #[serde(default)]
    pub summary: String,
}

/// `git@github.com:o/r.git`, `https://github.com/o/r` -> `github.com/o/r`.
pub fn normalize_remote(url: &str) -> Option<String> {
    let u = url.trim();
    let rest = if let Some(r) = u.split("://").nth(1) {
        // Drop credentials and port.
        let r = r.rsplit('@').next().unwrap_or(r);
        let (host, path) = r.split_once('/')?;
        format!("{}/{}", host.split(':').next().unwrap_or(host), path)
    } else {
        let r = u.rsplit('@').next().unwrap_or(u);
        let (host, path) = r.split_once(':')?;
        format!("{host}/{path}")
    };
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    (rest.matches('/').count() >= 2).then(|| rest.to_lowercase())
}

/// Remote URLs from a `.git/config` file's text.
pub fn remotes_in_config(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let v = l.strip_prefix("url")?.trim_start().strip_prefix('=')?;
            normalize_remote(v)
        })
        .collect()
}

fn readme_summary(dir: &Path) -> String {
    for name in [
        "README.md",
        "README",
        "readme.md",
        "README.rst",
        "README.txt",
    ] {
        if let Ok(t) = std::fs::read_to_string(dir.join(name)) {
            let lines: Vec<&str> = t
                .lines()
                .map(str::trim)
                .filter(|l| {
                    !l.is_empty()
                        && !l.starts_with('#')
                        && !l.starts_with('[')
                        && !l.starts_with('<')
                        && !l.starts_with('!')
                })
                .take(3)
                .collect();
            return tower_core::shorten(&lines.join(" "), 240);
        }
    }
    String::new()
}

const SKIP: &[&str] = &[
    "node_modules",
    "target",
    "Library",
    "vendor",
    "dist",
    "build",
    "Applications",
];

/// Git repositories under `roots`, at most `depth` levels down. A repo's
/// own subfolders are not searched; worktrees (`.git` files) are skipped.
pub fn scan(roots: &[PathBuf], depth: usize) -> Vec<RepoInfo> {
    let mut out = vec![];
    let mut stack: Vec<(PathBuf, usize)> = roots.iter().map(|r| (r.clone(), 0)).collect();
    while let Some((dir, d)) = stack.pop() {
        let git = dir.join(".git");
        if git.is_dir() {
            let remotes = std::fs::read_to_string(git.join("config"))
                .map(|t| remotes_in_config(&t))
                .unwrap_or_default();
            out.push(RepoInfo {
                path: dir.display().to_string(),
                name: dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                remotes,
                summary: readme_summary(&dir),
            });
            continue;
        }
        if d >= depth || out.len() > 2000 {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || SKIP.contains(&name.as_str()) {
                continue;
            }
            // Never follow symlinks: no loops, no surprises.
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push((e.path(), d + 1));
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Where an issue comes from, for matching.
#[derive(Clone, Debug, PartialEq)]
pub struct Origin<'a> {
    /// `jira`, `github`, `gitlab`.
    pub kind: &'a str,
    /// Jira project key, `owner/repo` or `group/project`.
    pub project: &'a str,
    /// GitLab host for self-hosted instances.
    pub host: Option<&'a str>,
}

impl Origin<'_> {
    /// The key answers are remembered under.
    pub fn memory_key(&self) -> String {
        format!("{}:{}", self.kind, self.project.to_lowercase())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    /// Sure: the path and why.
    Found(String, String),
    /// Likely ones first, with a score; may be empty.
    Candidates(Vec<(String, u32)>),
}

pub fn resolve(
    origin: &Origin,
    text: &str,
    rules: &[ProjectRule],
    learned: &HashMap<String, String>,
    index: &[RepoInfo],
) -> Resolution {
    let proj = origin.project.to_lowercase();
    for r in rules {
        let hit = match origin.kind {
            "jira" => r.jira.iter().any(|k| k.eq_ignore_ascii_case(&proj)),
            "github" => r.github.iter().any(|k| k.eq_ignore_ascii_case(&proj)),
            "gitlab" => r.gitlab.iter().any(|k| k.eq_ignore_ascii_case(&proj)),
            _ => false,
        };
        if hit {
            return Resolution::Found(r.path.clone(), "a [[project]] rule".into());
        }
    }
    if let Some(p) = learned.get(&origin.memory_key()) {
        return Resolution::Found(p.clone(), "your earlier answer".into());
    }
    if matches!(origin.kind, "github" | "gitlab") && !proj.is_empty() {
        let host = origin
            .host
            .map(str::to_lowercase)
            .unwrap_or_else(|| format!("{}.com", origin.kind));
        let want = format!("{host}/{proj}");
        let clones: Vec<&RepoInfo> = index
            .iter()
            .filter(|i| i.remotes.iter().any(|r| *r == want))
            .collect();
        match clones.as_slice() {
            [one] => return Resolution::Found(one.path.clone(), format!("its clone of {want}")),
            [] => {}
            many => {
                return Resolution::Candidates(many.iter().map(|c| (c.path.clone(), 10)).collect())
            }
        }
    }
    let text = text.to_lowercase();
    let mut scored: Vec<(String, u32)> = vec![];
    let mut add = |path: &str, n: u32| match scored.iter_mut().find(|(p, _)| p == path) {
        Some((_, s)) => *s += n,
        None => scored.push((path.to_string(), n)),
    };
    for r in rules {
        let n = r
            .keywords
            .iter()
            .filter(|k| has_word(&text, &k.to_lowercase()))
            .count() as u32;
        if n > 0 {
            add(&r.path, 4 * n);
        }
    }
    for i in index {
        let name = i.name.to_lowercase();
        if name.len() >= 3
            && (has_word(&text, &name) || has_word(&text, &name.replace(['-', '_'], " ")))
        {
            add(&i.path, 3);
        }
    }
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    // One clear winner by name or keyword: take it.
    if let Some((p, top)) = scored.first() {
        let runner_up = scored.get(1).map(|(_, s)| *s).unwrap_or(0);
        if *top >= 3 && runner_up * 2 <= *top {
            return Resolution::Found(p.clone(), "its name or keywords are in the issue".into());
        }
    }
    Resolution::Candidates(scored)
}

/// The repo's own checks, when it sets no gates: what CI would run first.
pub fn detect_gates(dir: &Path) -> Vec<String> {
    if dir.join("Cargo.toml").is_file() {
        return vec!["cargo test".into()];
    }
    if let Ok(t) = std::fs::read_to_string(dir.join("package.json")) {
        let has_test = serde_json::from_str::<serde_json::Value>(&t)
            .ok()
            .and_then(|v| {
                v.pointer("/scripts/test")
                    .and_then(|s| s.as_str())
                    .map(str::to_string)
            })
            .map(|s| !s.contains("no test specified"))
            .unwrap_or(false);
        if has_test {
            let pm = if dir.join("pnpm-lock.yaml").is_file() {
                "pnpm"
            } else if dir.join("yarn.lock").is_file() {
                "yarn"
            } else if dir.join("bun.lockb").is_file() || dir.join("bun.lock").is_file() {
                "bun"
            } else {
                "npm"
            };
            return vec![format!("{pm} test")];
        }
        return vec![];
    }
    if dir.join("go.mod").is_file() {
        return vec!["go test ./...".into()];
    }
    if dir.join("pyproject.toml").is_file() || dir.join("pytest.ini").is_file() {
        return vec!["python -m pytest -q".into()];
    }
    if let Ok(t) = std::fs::read_to_string(dir.join("Makefile")) {
        if t.lines().any(|l| l.starts_with("test:")) {
            return vec!["make test".into()];
        }
    }
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(path: &str, remotes: &[&str]) -> RepoInfo {
        RepoInfo {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            remotes: remotes.iter().map(|s| s.to_string()).collect(),
            summary: String::new(),
        }
    }

    #[test]
    fn remotes_normalize() {
        for u in [
            "git@github.com:Tim/Tower.git",
            "https://github.com/tim/tower",
            "https://user:pw@github.com/tim/tower.git/",
            "ssh://git@github.com:22/tim/tower.git",
        ] {
            assert_eq!(
                normalize_remote(u).as_deref(),
                Some("github.com/tim/tower"),
                "{u}"
            );
        }
        assert_eq!(normalize_remote("/local/path"), None);
        let cfg = "[remote \"origin\"]\n\turl = git@gitlab.example.com:grp/sub/api.git\n\tfetch = +refs/heads/*";
        assert_eq!(
            remotes_in_config(cfg),
            vec!["gitlab.example.com/grp/sub/api"]
        );
    }

    #[test]
    fn resolution_order() {
        let index = vec![
            repo("/c/tower", &["github.com/tim/tower"]),
            repo("/c/billing-api", &["gitlab.com/acme/billing-api"]),
            repo("/c/web", &[]),
        ];
        let rules = vec![ProjectRule {
            path: "/c/web".into(),
            jira: vec!["WEB".into()],
            keywords: vec!["checkout".into()],
            ..Default::default()
        }];
        let mut learned = HashMap::new();
        let jira = |p| Origin {
            kind: "jira",
            project: p,
            host: None,
        };
        assert!(
            matches!(resolve(&jira("web"), "", &rules, &learned, &index), Resolution::Found(p, _) if p == "/c/web")
        );
        let gh = Origin {
            kind: "github",
            project: "Tim/Tower",
            host: None,
        };
        assert!(
            matches!(resolve(&gh, "", &rules, &learned, &index), Resolution::Found(p, _) if p == "/c/tower")
        );
        let gl = Origin {
            kind: "gitlab",
            project: "acme/billing-api",
            host: None,
        };
        assert!(
            matches!(resolve(&gl, "", &rules, &learned, &index), Resolution::Found(p, _) if p == "/c/billing-api")
        );
        // A name in the text wins when it is the only one.
        assert!(matches!(
            resolve(&jira("OPS"), "The billing api returns 500", &rules, &learned, &index),
            Resolution::Found(p, _) if p == "/c/billing-api"
        ));
        // Two equally likely: let triage or you decide.
        match resolve(
            &jira("OPS"),
            "tower breaks the checkout",
            &rules,
            &learned,
            &index,
        ) {
            Resolution::Candidates(c) => {
                assert_eq!(c[0].0, "/c/web", "keywords outrank names");
                assert_eq!(c.len(), 2);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            resolve(&jira("OPS"), "nothing", &rules, &learned, &index),
            Resolution::Candidates(vec![])
        );
        learned.insert("jira:ops".into(), "/c/tower".into());
        assert!(
            matches!(resolve(&jira("OPS"), "x", &rules, &learned, &index), Resolution::Found(p, _) if p == "/c/tower")
        );
    }

    #[test]
    fn scan_finds_repos_but_not_inside_them() {
        let d = std::env::temp_dir().join(format!("tower-scan-{}", crate::paths::random_u64()));
        std::fs::create_dir_all(d.join("a/.git")).unwrap();
        std::fs::write(
            d.join("a/.git/config"),
            "[remote \"origin\"]\n url = https://github.com/x/a\n",
        )
        .unwrap();
        std::fs::write(d.join("a/README.md"), "# A\n\nDoes a things.\n").unwrap();
        std::fs::create_dir_all(d.join("a/sub/.git")).unwrap();
        std::fs::create_dir_all(d.join("group/b/.git")).unwrap();
        std::fs::create_dir_all(d.join("node_modules/c/.git")).unwrap();
        std::fs::create_dir_all(d.join("x/y/z/deep/.git")).unwrap();
        let found = scan(&[d.clone()], 3);
        let names: Vec<_> = found.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert_eq!(found[0].remotes, vec!["github.com/x/a"]);
        assert_eq!(found[0].summary, "Does a things.");
    }

    #[test]
    fn gates_follow_the_stack() {
        let d = std::env::temp_dir().join(format!("tower-gates-{}", crate::paths::random_u64()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(detect_gates(&d).is_empty());
        std::fs::write(
            d.join("package.json"),
            r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#,
        )
        .unwrap();
        assert!(
            detect_gates(&d).is_empty(),
            "npm's placeholder is not a test"
        );
        std::fs::write(d.join("package.json"), r#"{"scripts":{"test":"vitest"}}"#).unwrap();
        std::fs::write(d.join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(detect_gates(&d), vec!["pnpm test"]);
        std::fs::write(d.join("Cargo.toml"), "").unwrap();
        assert_eq!(detect_gates(&d), vec!["cargo test"]);
    }
}
