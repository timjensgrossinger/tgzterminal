//! Jira intake through the Atlassian Rovo MCP server.
//!
//! A watch polls a JQL query; new issues become flights. On landing, Tower
//! can (after your clearance) comment on the issue and move it along.
//! Tool names follow Atlassian's Rovo MCP server. Responses are parsed
//! defensively: issue text is untrusted input and every field is optional.

use crate::mcp_http::McpHttp;
use serde::Deserialize;
use serde_json::{json, Value};

pub const DEFAULT_MCP_URL: &str = "https://mcp.atlassian.com/v2/mcp";

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct JiraWatch {
    pub name: String,
    /// `yourteam.atlassian.net`.
    pub site: String,
    /// With an Atlassian API token: Basic auth as this account. Without it,
    /// the stored token is sent as a Bearer token.
    #[serde(default)]
    pub email: Option<String>,
    pub jql: String,
    /// Role new issues are cleared to (unless triage picks one).
    #[serde(default)]
    pub role: Option<String>,
    /// An `llm` role that reads each new issue (no tools) and picks a role.
    #[serde(default)]
    pub triage_role: Option<String>,
    /// Launch an agent as soon as an issue is cleared to a role.
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
    /// Offer to comment on the issue when its flight lands.
    #[serde(default = "yes")]
    pub comment_on_land: bool,
    /// Transition (by name) to apply with that comment, e.g. "Done".
    #[serde(default)]
    pub done_transition: Option<String>,
    #[serde(default)]
    pub mcp_url: Option<String>,
    /// Pass relevant comments and changes on to the flight.
    #[serde(default = "yes")]
    pub follow: bool,
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

impl JiraWatch {
    pub fn mcp_url(&self) -> String {
        self.mcp_url
            .clone()
            .unwrap_or_else(|| DEFAULT_MCP_URL.to_string())
    }

    pub fn auth_header(&self, token: &str) -> String {
        match &self.email {
            Some(email) => format!("Basic {}", base64(format!("{email}:{token}").as_bytes())),
            None => format!("Bearer {token}"),
        }
    }
}

pub use crate::issues::Issue;

pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Plain text from an Atlassian Document Format node (or a plain string).
pub fn adf_text(v: &Value) -> String {
    fn walk(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => out.push_str(s),
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(o) => {
                if let Some(t) = o.get("text").and_then(Value::as_str) {
                    out.push_str(t);
                }
                let ty = o.get("type").and_then(Value::as_str).unwrap_or("");
                if ty == "hardBreak" {
                    out.push('\n');
                }
                if let Some(c) = o.get("content") {
                    if ty == "listItem" {
                        out.push_str("- ");
                    }
                    walk(c, out);
                    if matches!(
                        ty,
                        "paragraph" | "heading" | "listItem" | "codeBlock" | "blockquote"
                    ) {
                        out.push('\n');
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    walk(v, &mut out);
    out.trim().to_string()
}

/// Issues from a search result, wherever the array sits.
pub fn parse_issues(v: &Value, site: &str) -> Vec<Issue> {
    let list = v
        .get("issues")
        .and_then(|i| {
            i.as_array()
                .or_else(|| i.get("nodes").and_then(Value::as_array))
        })
        .or_else(|| v.as_array())
        .cloned()
        .unwrap_or_default();
    list.iter()
        .filter_map(|i| {
            let key = i.get("key")?.as_str()?.to_string();
            let fields = i.get("fields").unwrap_or(&Value::Null);
            let summary = fields
                .get("summary")
                .or_else(|| i.get("summary"))
                .and_then(Value::as_str)
                .unwrap_or("(no summary)")
                .to_string();
            let description = fields
                .get("description")
                .or_else(|| i.get("description"))
                .map(adf_text)
                .unwrap_or_default();
            let status = fields
                .pointer("/status/name")
                .or_else(|| i.pointer("/status/name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let url = format!("https://{}/browse/{}", site.trim_end_matches('/'), key);
            Some(Issue {
                project: key.split('-').next().unwrap_or("").to_string(),
                key,
                summary,
                description,
                status,
                url,
                number: None,
            })
        })
        .collect()
}

/// The cloud id of `site` from `getAccessibleAtlassianResources`.
pub fn cloud_id(resources: &Value, site: &str) -> Option<String> {
    let site = site.trim().trim_end_matches('/').to_lowercase();
    resources.as_array()?.iter().find_map(|r| {
        let url = r.get("url")?.as_str()?.to_lowercase();
        (url.trim_end_matches('/').ends_with(&site))
            .then(|| r.get("id")?.as_str().map(str::to_string))?
    })
}

async fn connect(w: &JiraWatch, token: &str) -> Result<(McpHttp, String), String> {
    let mut c = McpHttp::connect(&w.mcp_url(), Some(w.auth_header(token))).await?;
    let resources = c
        .call_tool("getAccessibleAtlassianResources", json!({}))
        .await?;
    let id = cloud_id(&resources, &w.site).unwrap_or_else(|| w.site.clone());
    Ok((c, id))
}

pub async fn poll(w: &JiraWatch, token: &str) -> Result<Vec<Issue>, String> {
    let (mut c, cloud) = connect(w, token).await?;
    let v = c
        .call_tool(
            "searchJiraIssuesUsingJql",
            json!({
                "cloudId": cloud,
                "jql": w.jql,
                "maxResults": w.max_results,
                "fields": ["summary", "description", "status"],
            }),
        )
        .await?;
    Ok(parse_issues(&v, &w.site))
}

/// Comment on an issue and optionally apply a transition by name.
/// An issue's summary, description, status and comments.
pub fn parse_activity(v: &Value) -> crate::issues::Activity {
    let f = v.get("fields").unwrap_or(v);
    let comments = f
        .pointer("/comment/comments")
        .or_else(|| v.get("comments"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|c| crate::issues::Comment {
                    id: c
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    author: c
                        .pointer("/author/displayName")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    body: c.get("body").map(adf_text).unwrap_or_default(),
                    at_unix: c
                        .get("created")
                        .and_then(Value::as_str)
                        .and_then(tower_core::work::parse_timestamp)
                        .unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default();
    crate::issues::Activity {
        summary: f
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        description: f.get("description").map(adf_text).unwrap_or_default(),
        status: f
            .pointer("/status/name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        comments,
    }
}

pub async fn activity(
    w: &JiraWatch,
    token: &str,
    key: &str,
) -> Result<crate::issues::Activity, String> {
    let (mut c, cloud) = connect(w, token).await?;
    let v = c
        .call_tool(
            "getJiraIssue",
            json!({"cloudId": cloud, "issueIdOrKey": key, "fields": ["summary", "description", "status", "comment"]}),
        )
        .await?;
    Ok(parse_activity(&v))
}

/// Only a comment (a question for the reporter, say).
pub async fn comment(w: &JiraWatch, token: &str, key: &str, body: &str) -> Result<String, String> {
    let (mut c, cloud) = connect(w, token).await?;
    c.call_tool(
        "addCommentToJiraIssue",
        json!({"cloudId": cloud, "issueIdOrKey": key, "commentBody": body}),
    )
    .await?;
    Ok("commented".into())
}

pub async fn write_back(
    w: &JiraWatch,
    token: &str,
    key: &str,
    comment: &str,
) -> Result<String, String> {
    let (mut c, cloud) = connect(w, token).await?;
    c.call_tool(
        "addCommentToJiraIssue",
        json!({"cloudId": cloud, "issueIdOrKey": key, "commentBody": comment}),
    )
    .await?;
    let Some(target) = &w.done_transition else {
        return Ok("commented".into());
    };
    let t = c
        .call_tool(
            "getTransitionsForJiraIssue",
            json!({"cloudId": cloud, "issueIdOrKey": key}),
        )
        .await?;
    let list = t
        .get("transitions")
        .and_then(Value::as_array)
        .or_else(|| t.as_array())
        .cloned()
        .unwrap_or_default();
    let id = list.iter().find_map(|x| {
        let name = x.get("name")?.as_str()?;
        name.eq_ignore_ascii_case(target)
            .then(|| x.get("id").and_then(Value::as_str).map(str::to_string))?
    });
    match id {
        Some(id) => {
            c.call_tool(
                "transitionJiraIssue",
                json!({"cloudId": cloud, "issueIdOrKey": key, "transition": {"id": id}}),
            )
            .await?;
            Ok(format!("commented and moved to {target}"))
        }
        None => Ok(format!(
            "commented; no \"{target}\" transition is available"
        )),
    }
}

/// Where the API token lives: macOS Keychain, else a 0600 file.
pub mod secrets {
    use std::path::Path;

    const SERVICE: &str = "tower.jira";

    pub fn get(site: &str, data_dir: &Path) -> Option<String> {
        if let Ok(t) = std::env::var("TOWER_JIRA_TOKEN") {
            if !t.trim().is_empty() {
                return Some(t.trim().to_string());
            }
        }
        if cfg!(target_os = "macos") {
            let out = std::process::Command::new("security")
                .args(["find-generic-password", "-s", SERVICE, "-a", site, "-w"])
                .output()
                .ok()?;
            if out.status.success() {
                let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
                return (!t.is_empty()).then_some(t);
            }
            return None;
        }
        std::fs::read_to_string(file(site, data_dir))
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    }

    pub fn set(site: &str, token: &str, data_dir: &Path) -> std::io::Result<()> {
        if cfg!(target_os = "macos") {
            // `security` takes the secret as an argument; it is visible to
            // other local processes of this user for the instant it runs.
            let ok = std::process::Command::new("security")
                .args([
                    "add-generic-password",
                    "-U",
                    "-s",
                    SERVICE,
                    "-a",
                    site,
                    "-w",
                    token,
                ])
                .status()?
                .success();
            return if ok {
                Ok(())
            } else {
                Err(std::io::Error::other(
                    "security add-generic-password failed",
                ))
            };
        }
        let p = file(site, data_dir);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, token)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    fn file(site: &str, data_dir: &Path) -> std::path::PathBuf {
        let safe: String = site
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        data_dir.join("secrets").join(format!("jira-{safe}.token"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jira_activity_reads_adf_comments() {
        let a = parse_activity(&json!({"fields": {
            "summary": "SFTP user", "status": {"name": "In Progress"},
            "description": {"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "for partner X"}]}]},
            "comment": {"comments": [{"id": "10001", "author": {"displayName": "Ana"}, "created": "2026-10-19T11:30:00.000+0200",
                "body": {"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Use the EU host"}]}]}}]}
        }}));
        assert_eq!(a.status, "In Progress");
        assert_eq!(a.description, "for partner X");
        assert_eq!(
            (a.comments[0].body.as_str(), a.comments[0].at_unix),
            ("Use the EU host", 1_792_402_200)
        );
    }
    use crate::mcp_http::tests::fake_server;

    fn watch(url: &str) -> JiraWatch {
        toml::from_str::<JiraWatch>(&format!(
            "name = \"Backlog\"\nsite = \"acme.atlassian.net\"\nemail = \"me@acme.io\"\njql = \"project = ACME\"\nrole = \"approach\"\nmcp_url = \"{url}\"\ndone_transition = \"Done\""
        ))
        .unwrap()
    }

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"me@x.io:tok"), "bWVAeC5pbzp0b2s=");
    }

    #[test]
    fn adf_flattens_to_text() {
        let doc = json!({"type": "doc", "content": [
            {"type": "paragraph", "content": [{"type": "text", "text": "Login loops"}, {"type": "hardBreak"}, {"type": "text", "text": "on expiry"}]},
            {"type": "bulletList", "content": [{"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "step one"}]}]}]}
        ]});
        let t = adf_text(&doc);
        assert!(t.starts_with("Login loops\non expiry"));
        assert!(t.contains("- step one"));
        assert_eq!(adf_text(&json!("plain")), "plain");
    }

    #[test]
    fn issues_and_cloud_ids_parse_defensively() {
        let v = json!({"issues": [
            {"key": "ACME-1", "fields": {"summary": "Fix login", "status": {"name": "To Do"}, "description": "It loops"}},
            {"fields": {"summary": "no key"}},
            {"key": "ACME-2"}
        ]});
        let issues = parse_issues(&v, "acme.atlassian.net");
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].url, "https://acme.atlassian.net/browse/ACME-1");
        assert_eq!(issues[0].status, "To Do");
        assert_eq!(issues[1].summary, "(no summary)");
        let res = json!([{"id": "c-1", "url": "https://other.atlassian.net"}, {"id": "c-2", "url": "https://acme.atlassian.net/"}]);
        assert_eq!(cloud_id(&res, "acme.atlassian.net").as_deref(), Some("c-2"));
        assert_eq!(cloud_id(&res, "missing.atlassian.net"), None);
    }

    #[tokio::test]
    async fn polls_and_writes_back_through_mcp() {
        let url = fake_server(vec![
            (
                "getAccessibleAtlassianResources",
                json!([{"id": "c-2", "url": "https://acme.atlassian.net"}]),
            ),
            (
                "searchJiraIssuesUsingJql",
                json!({"issues": [{"key": "ACME-7", "fields": {"summary": "Crash on start"}}]}),
            ),
            ("addCommentToJiraIssue", json!({"id": "10001"})),
            (
                "getTransitionsForJiraIssue",
                json!({"transitions": [{"id": "31", "name": "Done"}]}),
            ),
            ("transitionJiraIssue", json!({})),
        ])
        .await;
        let w = watch(&url);
        let issues = poll(&w, "tok").await.unwrap();
        assert_eq!(issues[0].key, "ACME-7");
        assert_eq!(
            write_back(&w, "tok", "ACME-7", "Landed").await.unwrap(),
            "commented and moved to Done"
        );
        // Without an email the token goes out as Bearer, which the fake refuses.
        let mut bearer = w.clone();
        bearer.email = None;
        assert!(poll(&bearer, "tok").await.is_err());
    }
}
