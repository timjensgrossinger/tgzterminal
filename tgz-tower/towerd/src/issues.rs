//! Issue sources: Jira (Atlassian Rovo MCP), GitHub (`gh`) and GitLab (`glab`).
//!
//! Every source turns into the same `Issue`, and every watch shares the same
//! intake settings (role, triage, auto-launch, auto-fix). GitHub and GitLab
//! go through their official CLIs and your existing login there, so Tower
//! stores no token for them. Issue text is untrusted input; programs are
//! run with argument vectors, never through a shell.

use crate::jira::JiraWatch;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use tokio::process::Command;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Issue {
    /// Flight key: `ACME-12` (Jira), `REPO-12` (GitHub, GitLab).
    pub key: String,
    pub summary: String,
    pub description: String,
    pub status: String,
    pub url: String,
    /// Where it lives: Jira project key, `owner/repo` or `group/project`.
    pub project: String,
    /// GitHub/GitLab issue number.
    pub number: Option<u64>,
    pub created_unix: u64,
    /// Display name (Jira) or login (GitHub, GitLab).
    pub reporter: String,
    /// Comma-separated when there are several.
    pub assignee: String,
    pub priority: String,
    pub labels: Vec<String>,
    /// Jira Service Management SLAs.
    pub sla: Vec<tower_proto::SlaClock>,
    /// Jira: ids of the fields that held SLAs, found while the watch's SLA
    /// fields are still unknown.
    pub sla_fields: Vec<String>,
}

/// An issue's current text, status and comments.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Activity {
    pub summary: String,
    pub description: String,
    pub status: String,
    pub comments: Vec<Comment>,
    /// Who opened it, as comment authors are named.
    pub reporter: String,
    /// Jira Service Management SLAs, when the watch knows its SLA fields.
    pub sla: Vec<tower_proto::SlaClock>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub body: String,
    pub at_unix: u64,
}

fn ts(v: &Value, k: &str) -> u64 {
    v.get(k)
        .and_then(Value::as_str)
        .and_then(tower_core::work::parse_timestamp)
        .unwrap_or(0)
}

fn id_of(v: &Value) -> String {
    match v.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(other) if !other.is_null() => other.to_string(),
        _ => String::new(),
    }
}

/// `gh issue view --json title,body,state,comments`.
pub fn parse_github_activity(v: &Value) -> Activity {
    Activity {
        reporter: login(v, "/author/login"),
        sla: vec![],
        summary: s(v, "title"),
        description: s(v, "body"),
        status: s(v, "state").to_lowercase(),
        comments: v
            .get("comments")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|c| Comment {
                        id: {
                            let id = id_of(c);
                            if id.is_empty() {
                                format!("{}:{}", s(c, "createdAt"), s(c, "body").len())
                            } else {
                                id
                            }
                        },
                        author: c
                            .pointer("/author/login")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        body: s(c, "body"),
                        at_unix: ts(c, "createdAt"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// GitLab REST: the issue, and its notes (system notes dropped).
pub fn parse_gitlab_activity(issue: &Value, notes: &Value) -> Activity {
    Activity {
        reporter: login(issue, "/author/username"),
        sla: vec![],
        summary: s(issue, "title"),
        description: s(issue, "description"),
        status: s(issue, "state"),
        comments: notes
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|n| !n.get("system").and_then(Value::as_bool).unwrap_or(false))
                    .map(|n| Comment {
                        id: id_of(n),
                        author: n
                            .pointer("/author/username")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        body: s(n, "body"),
                        at_unix: ts(n, "created_at"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// `group/sub/project` for a GitLab API path.
fn url_encode_path(p: &str) -> String {
    p.replace('%', "%25").replace('/', "%2F")
}

/// The issue's current state from GitHub or GitLab.
pub async fn activity_cli(w: &Watch, number: u64) -> Result<Activity, String> {
    let env = env_for(w);
    match w {
        Watch::Github(g) => {
            let argv: Vec<String> = [
                "gh",
                "issue",
                "view",
                &number.to_string(),
                "-R",
                &g.repo,
                "--json",
                "title,body,state,comments,author",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            let out = run(&argv, None, &env, None).await?;
            let v: Value =
                serde_json::from_str(out.trim()).map_err(|e| format!("bad JSON: {e}"))?;
            Ok(parse_github_activity(&v))
        }
        Watch::Gitlab(g) => {
            let base = format!("projects/{}/issues/{number}", url_encode_path(&g.project));
            let issue = run(
                &["glab".into(), "api".into(), base.clone()],
                None,
                &env,
                None,
            )
            .await?;
            let notes = run(
                &[
                    "glab".into(),
                    "api".into(),
                    format!("{base}/notes?sort=asc&per_page=100"),
                ],
                None,
                &env,
                None,
            )
            .await?;
            let iv: Value =
                serde_json::from_str(issue.trim()).map_err(|e| format!("bad JSON: {e}"))?;
            let nv: Value =
                serde_json::from_str(notes.trim()).map_err(|e| format!("bad JSON: {e}"))?;
            Ok(parse_gitlab_activity(&iv, &nv))
        }
        Watch::Jira(_) => Err("use jira::activity".into()),
    }
}

fn default_interval() -> u64 {
    120
}
fn default_max() -> u32 {
    20
}
fn yes() -> bool {
    true
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct GithubWatch {
    pub name: String,
    /// `owner/repo`.
    pub repo: String,
    /// Extra `gh issue list --search` terms, e.g. `label:bug no:assignee`.
    #[serde(default)]
    pub search: Option<String>,
    /// Only issues assigned to this login (`@me` for you).
    #[serde(default)]
    pub assignee: Option<String>,
    /// Only issues with all of these labels.
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub triage_role: Option<String>,
    #[serde(default)]
    pub auto_launch: bool,
    /// Find the project, fix the issue in a worktree, verify, then land.
    #[serde(default)]
    pub autofix: bool,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_max")]
    pub max_results: u32,
    #[serde(default = "yes")]
    pub comment_on_land: bool,
    /// Pass relevant comments and changes on to the flight.
    #[serde(default = "yes")]
    pub follow: bool,
    /// Work from this source is sensitive: local models only, triage
    /// included (see `[privacy]`).
    #[serde(default)]
    pub local_only: bool,
    /// Per-action overrides of `[policy.actions]` for this watch.
    #[serde(default)]
    pub policy: crate::policy::Overrides,
    /// Tower's own response clock: answer within this many hours.
    #[serde(default)]
    pub respond_within_hours: Option<u64>,
    /// Acknowledgement text for this watch (see `[ack]`).
    #[serde(default)]
    pub ack_template: Option<String>,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct GitlabWatch {
    pub name: String,
    /// `group/project` (or `group/sub/project`).
    pub project: String,
    /// Self-hosted GitLab, e.g. `gitlab.example.com`.
    #[serde(default)]
    pub host: Option<String>,
    /// Only issues with this label.
    #[serde(default)]
    pub label: Option<String>,
    /// Only issues assigned to this username (`@me` for you).
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub triage_role: Option<String>,
    #[serde(default)]
    pub auto_launch: bool,
    #[serde(default)]
    pub autofix: bool,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_max")]
    pub max_results: u32,
    #[serde(default = "yes")]
    pub comment_on_land: bool,
    /// Pass relevant comments and changes on to the flight.
    #[serde(default = "yes")]
    pub follow: bool,
    /// Work from this source is sensitive: local models only, triage
    /// included (see `[privacy]`).
    #[serde(default)]
    pub local_only: bool,
    /// Per-action overrides of `[policy.actions]` for this watch.
    #[serde(default)]
    pub policy: crate::policy::Overrides,
    /// Tower's own response clock: answer within this many hours.
    #[serde(default)]
    pub respond_within_hours: Option<u64>,
    /// Acknowledgement text for this watch (see `[ack]`).
    #[serde(default)]
    pub ack_template: Option<String>,
}

/// Any intake watch.
#[derive(Clone, Debug, PartialEq)]
pub enum Watch {
    Jira(JiraWatch),
    Github(GithubWatch),
    Gitlab(GitlabWatch),
}

macro_rules! common {
    ($self:ident, $w:ident => $e:expr) => {
        match $self {
            Watch::Jira($w) => $e,
            Watch::Github($w) => $e,
            Watch::Gitlab($w) => $e,
        }
    };
}

impl Watch {
    pub fn name(&self) -> &str {
        common!(self, w => &w.name)
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Watch::Jira(_) => "jira",
            Watch::Github(_) => "github",
            Watch::Gitlab(_) => "gitlab",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Watch::Jira(_) => "Jira",
            Watch::Github(_) => "GitHub",
            Watch::Gitlab(_) => "GitLab",
        }
    }
    pub fn role(&self) -> Option<&String> {
        common!(self, w => w.role.as_ref())
    }
    pub fn triage_role(&self) -> Option<&String> {
        common!(self, w => w.triage_role.as_ref())
    }
    pub fn auto_launch(&self) -> bool {
        common!(self, w => w.auto_launch)
    }
    pub fn autofix(&self) -> bool {
        common!(self, w => w.autofix)
    }
    pub fn workdir(&self) -> Option<&String> {
        common!(self, w => w.workdir.as_ref())
    }
    pub fn interval_secs(&self) -> u64 {
        common!(self, w => w.interval_secs)
    }
    pub fn comment_on_land(&self) -> bool {
        common!(self, w => w.comment_on_land)
    }
    pub fn follow(&self) -> bool {
        common!(self, w => w.follow)
    }
    pub fn local_only(&self) -> bool {
        common!(self, w => w.local_only)
    }
    pub fn respond_within_hours(&self) -> Option<u64> {
        common!(self, w => w.respond_within_hours.filter(|h| *h > 0))
    }
    pub fn ack_template(&self) -> Option<&String> {
        common!(self, w => w.ack_template.as_ref())
    }
    /// Comment visibility set on the watch; only Jira has internal comments.
    pub fn ack_visibility(&self) -> Option<crate::responses::Visibility> {
        match self {
            Watch::Jira(j) => j.ack_visibility,
            _ => None,
        }
    }
    /// The watch's `policy = { … }` table as written.
    pub fn own_policy(&self) -> &crate::policy::Overrides {
        common!(self, w => &w.policy)
    }
    /// The watch's overrides with the deprecated knobs folded in:
    /// `auto_launch` / `autofix` mean `intake.launch = "auto"` (and
    /// `intake.triage = "auto"`), unless the table says otherwise.
    pub fn policy(&self) -> crate::policy::Overrides {
        use crate::policy::{Action, Level};
        let mut out = self.own_policy().clone();
        if self.auto_launch() || self.autofix() {
            for a in [Action::IntakeLaunch, Action::IntakeTriage] {
                out.entry(a.key().to_string()).or_insert(Level::Auto);
            }
        }
        out
    }
    /// What it polls, for the Intake view.
    pub fn query(&self) -> String {
        match self {
            Watch::Jira(w) => w.jql.clone(),
            Watch::Github(w) => match &w.search {
                Some(s) => format!("{} · {}", w.repo, s),
                None => w.repo.clone(),
            },
            Watch::Gitlab(w) => match &w.label {
                Some(l) => format!("{} · label {}", w.project, l),
                None => w.project.clone(),
            },
        }
    }
}

/// `tgzterminal` -> `TGZTERMINAL`, for flight keys like `TGZTERMINAL-12`.
pub fn key_prefix(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '-'
            }
        })
        .collect();
    out.truncate(24);
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "ISSUE".into()
    } else {
        out
    }
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn login(v: &Value, pointer: &str) -> String {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// `[{login: a}, {login: b}]` -> `a, b`.
fn names(list: Option<&Value>, key: &str) -> String {
    list.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get(key).and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// Labels as GitHub (`[{name}]`) or GitLab (`["name"]`) lists them.
fn label_names(list: Option<&Value>) -> Vec<String> {
    list.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().or_else(|| x.get("name").and_then(Value::as_str)))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `gh issue list --json number,title,body,url,state`.
pub fn parse_github(v: &Value, repo: &str) -> Vec<Issue> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    let n = i.get("number")?.as_u64()?;
                    Some(Issue {
                        key: format!("{}-{n}", key_prefix(repo)),
                        summary: s(i, "title"),
                        description: s(i, "body"),
                        status: s(i, "state").to_lowercase(),
                        url: s(i, "url"),
                        project: repo.to_string(),
                        number: Some(n),
                        created_unix: ts(i, "createdAt"),
                        reporter: login(i, "/author/login"),
                        assignee: names(i.get("assignees"), "login"),
                        labels: label_names(i.get("labels")),
                        ..Default::default()
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `glab issue list --output json`.
pub fn parse_gitlab(v: &Value, project: &str) -> Vec<Issue> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    let n = i.get("iid")?.as_u64()?;
                    Some(Issue {
                        key: format!("{}-{n}", key_prefix(project)),
                        summary: s(i, "title"),
                        description: s(i, "description"),
                        status: s(i, "state"),
                        url: s(i, "web_url"),
                        project: project.to_string(),
                        number: Some(n),
                        created_unix: ts(i, "created_at"),
                        reporter: login(i, "/author/username"),
                        assignee: names(i.get("assignees"), "username"),
                        labels: label_names(i.get("labels")),
                        ..Default::default()
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The argv that lists a watch's open issues.
pub fn list_argv(w: &Watch) -> Option<Vec<String>> {
    match w {
        Watch::Jira(_) => None,
        Watch::Github(g) => {
            let mut a: Vec<String> = [
                "gh",
                "issue",
                "list",
                "--state",
                "open",
                "--json",
                "number,title,body,url,state,author,assignees,labels,createdAt",
                "-R",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            a.push(g.repo.clone());
            a.extend(["--limit".to_string(), g.max_results.to_string()]);
            if let Some(q) = g.search.as_ref().filter(|q| !q.trim().is_empty()) {
                a.extend(["--search".to_string(), q.clone()]);
            }
            if let Some(who) = g.assignee.as_ref().filter(|x| !x.trim().is_empty()) {
                a.extend(["--assignee".to_string(), who.clone()]);
            }
            for l in g.labels.iter().filter(|l| !l.trim().is_empty()) {
                a.extend(["--label".to_string(), l.clone()]);
            }
            Some(a)
        }
        Watch::Gitlab(g) => {
            let mut a: Vec<String> = ["glab", "issue", "list", "--output", "json", "-R"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            a.push(g.project.clone());
            a.extend(["--per-page".to_string(), g.max_results.to_string()]);
            if let Some(l) = g.label.as_ref().filter(|l| !l.trim().is_empty()) {
                a.extend(["--label".to_string(), l.clone()]);
            }
            if let Some(who) = g.assignee.as_ref().filter(|x| !x.trim().is_empty()) {
                a.extend(["--assignee".to_string(), who.clone()]);
            }
            Some(a)
        }
    }
}

/// The argv that comments on an issue, with the body on stdin where the
/// CLI allows it.
pub fn comment_argv(w: &Watch, number: u64, body: &str) -> Option<(Vec<String>, Option<String>)> {
    match w {
        Watch::Jira(_) => None,
        Watch::Github(g) => Some((
            vec![
                "gh".into(),
                "issue".into(),
                "comment".into(),
                number.to_string(),
                "-R".into(),
                g.repo.clone(),
                "--body-file".into(),
                "-".into(),
            ],
            Some(body.to_string()),
        )),
        Watch::Gitlab(g) => Some((
            vec![
                "glab".into(),
                "issue".into(),
                "note".into(),
                number.to_string(),
                "-R".into(),
                g.project.clone(),
                "-m".into(),
                body.to_string(),
            ],
            None,
        )),
    }
}

fn env_for(w: &Watch) -> Vec<(String, String)> {
    match w {
        Watch::Gitlab(GitlabWatch { host: Some(h), .. }) => vec![("GITLAB_HOST".into(), h.clone())],
        _ => vec![],
    }
}

/// Run a CLI with a timeout; stdout on success, a short reason otherwise.
pub async fn run(
    argv: &[String],
    stdin: Option<String>,
    env: &[(String, String)],
    cwd: Option<&std::path::Path>,
) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    let (prog, args) = argv.split_first().ok_or("empty command")?;
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .envs(env.iter().cloned())
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{prog} is not installed"),
        _ => format!("{prog}: {e}"),
    })?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(text.as_bytes()).await;
    }
    let out = tokio::time::timeout(Duration::from_secs(90), child.wait_with_output())
        .await
        .map_err(|_| format!("{prog} timed out"))?
        .map_err(|e| format!("{prog}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        Err(format!(
            "{prog} failed: {}",
            tower_core::shorten(if err.is_empty() { "no output" } else { err }, 300)
        ))
    }
}

/// Open issues of a GitHub or GitLab watch.
pub async fn poll_cli(w: &Watch) -> Result<Vec<Issue>, String> {
    let argv = list_argv(w).ok_or("not a CLI watch")?;
    let out = run(&argv, None, &env_for(w), None).await?;
    let v: Value = serde_json::from_str(out.trim()).map_err(|e| format!("bad JSON: {e}"))?;
    Ok(match w {
        Watch::Github(g) => parse_github(&v, &g.repo),
        Watch::Gitlab(g) => parse_gitlab(&v, &g.project),
        Watch::Jira(_) => vec![],
    })
}

/// Comment on a GitHub or GitLab issue.
pub async fn comment_cli(w: &Watch, number: u64, body: &str) -> Result<String, String> {
    let (argv, stdin) = comment_argv(w, number, body).ok_or("not a CLI watch")?;
    run(&argv, stdin, &env_for(w), None).await?;
    Ok("commented".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn gh() -> Watch {
        Watch::Github(GithubWatch {
            name: "bugs".into(),
            repo: "tim/tgz-terminal".into(),
            search: Some("label:bug".into()),
            assignee: Some("@me".into()),
            labels: vec!["bug".into(), "p1".into()],
            role: None,
            triage_role: None,
            auto_launch: true,
            autofix: true,
            workdir: None,
            interval_secs: 120,
            max_results: 5,
            comment_on_land: true,
            local_only: false,
            follow: true,
            policy: Default::default(),
            respond_within_hours: Some(24),
            ack_template: None,
        })
    }

    #[test]
    fn deprecated_knobs_fold_into_the_watch_policy() {
        use crate::policy::Level;
        let w = gh();
        let p = w.policy();
        assert_eq!(p.get("intake.launch"), Some(&Level::Auto));
        assert_eq!(p.get("intake.triage"), Some(&Level::Auto));
        // An explicit table entry wins over the knob.
        let Watch::Github(mut g) = w else {
            unreachable!()
        };
        g.policy.insert("intake.launch".into(), Level::Confirm);
        let p = Watch::Github(g).policy();
        assert_eq!(p.get("intake.launch"), Some(&Level::Confirm));
    }

    #[test]
    fn activity_parses_from_both_clis() {
        let gh = parse_github_activity(&json!({
            "title": "T", "body": "B", "state": "OPEN",
            "comments": [{"id": "IC_1", "author": {"login": "ana"}, "body": "more info", "createdAt": "2026-10-19T09:30:00Z"}]
        }));
        assert_eq!(gh.status, "open");
        assert_eq!(
            (gh.comments[0].author.as_str(), gh.comments[0].at_unix),
            ("ana", 1_792_402_200)
        );
        let gl = parse_gitlab_activity(
            &json!({"title": "T", "description": "D", "state": "opened"}),
            &json!([{"id": 9, "body": "changed the milestone", "system": true}, {"id": 10, "body": "hi", "author": {"username": "bo"}, "created_at": "2026-10-19T09:30:00.000Z"}]),
        );
        assert_eq!(gl.comments.len(), 1);
        assert_eq!(
            (gl.comments[0].id.as_str(), gl.comments[0].author.as_str()),
            ("10", "bo")
        );
        assert_eq!(url_encode_path("grp/sub/api"), "grp%2Fsub%2Fapi");
    }

    #[test]
    fn github_issues_parse_into_flight_keys() {
        let v = json!([
            {"number": 12, "title": "Crash on resize", "body": "steps", "url": "https://github.com/tim/tgz-terminal/issues/12", "state": "OPEN"},
            {"title": "no number"}
        ]);
        let i = parse_github(&v, "tim/tgz-terminal");
        assert_eq!(i.len(), 1);
        assert_eq!(i[0].key, "TGZ-TERMINAL-12");
        assert_eq!(
            (i[0].number, i[0].project.as_str()),
            (Some(12), "tim/tgz-terminal")
        );
        assert!(parse_github(&json!({"x": 1}), "a/b").is_empty());
    }

    #[test]
    fn gitlab_issues_parse() {
        let v = json!([{"iid": 3, "title": "T", "description": "D", "web_url": "u", "state": "opened"}]);
        let i = parse_gitlab(&v, "grp/sub/api");
        assert_eq!(
            (i[0].key.as_str(), i[0].description.as_str()),
            ("API-3", "D")
        );
    }

    #[test]
    fn argv_never_needs_a_shell() {
        let a = list_argv(&gh()).unwrap();
        assert_eq!(&a[..3], ["gh", "issue", "list"]);
        assert!(a
            .windows(2)
            .any(|w| w[0] == "--search" && w[1] == "label:bug"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-R" && w[1] == "tim/tgz-terminal"));
        let evil = "$(rm -rf ~) `x`";
        let (c, stdin) = comment_argv(&gh(), 12, evil).unwrap();
        assert!(
            !c.iter().any(|x| x.contains("rm -rf")),
            "body goes on stdin"
        );
        assert_eq!(stdin.as_deref(), Some(evil));
        assert_eq!(key_prefix("a/b c"), "B-C");
        assert_eq!(key_prefix("///"), "ISSUE");
        assert_eq!(gh().query(), "tim/tgz-terminal · label:bug");
    }

    #[test]
    fn assignee_and_label_filters_reach_the_cli() {
        let a = list_argv(&gh()).unwrap();
        assert!(a.windows(2).any(|w| w[0] == "--assignee" && w[1] == "@me"));
        let labels: Vec<_> = a
            .windows(2)
            .filter(|w| w[0] == "--label")
            .map(|w| w[1].clone())
            .collect();
        assert_eq!(labels, vec!["bug".to_string(), "p1".to_string()]);
        assert_eq!(gh().respond_within_hours(), Some(24));
    }

    #[test]
    fn tracker_people_and_labels_are_read() {
        let gh = parse_github(
            &json!([{"number": 1, "title": "T", "author": {"login": "ana"},
                "assignees": [{"login": "bo"}, {"login": "cy"}],
                "labels": [{"name": "bug"}], "createdAt": "2026-10-19T09:30:00Z"}]),
            "o/r",
        );
        assert_eq!(gh[0].reporter, "ana");
        assert_eq!(gh[0].assignee, "bo, cy");
        assert_eq!(gh[0].labels, vec!["bug".to_string()]);
        assert_eq!(gh[0].created_unix, 1_792_402_200);
        let gl = parse_gitlab(
            &json!([{"iid": 2, "title": "T", "author": {"username": "dee"},
                "assignees": [{"username": "eve"}], "labels": ["ops", "p2"],
                "created_at": "2026-10-19T09:30:00Z"}]),
            "g/p",
        );
        assert_eq!(
            (gl[0].reporter.as_str(), gl[0].assignee.as_str()),
            ("dee", "eve")
        );
        assert_eq!(gl[0].labels, vec!["ops".to_string(), "p2".to_string()]);
    }
}
