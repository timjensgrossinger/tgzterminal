//! What kind of work an issue is, how it ended, and which hosts are
//! off-limits. Pure helpers, no IO.

use crate::router::has_word;

/// The kind of work an issue asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkKind {
    /// A change in a code project.
    Code,
    /// IT / operations: accounts, access, servers, installs.
    Ops,
    /// A design, concept or proposal: the result is a document or prototype.
    Design,
    /// A question to answer or research.
    Question,
}

impl WorkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkKind::Code => "code",
            WorkKind::Ops => "ops",
            WorkKind::Design => "design",
            WorkKind::Question => "question",
        }
    }

    pub fn parse(s: &str) -> Option<WorkKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "code" | "dev" | "bug" | "feature" => Some(WorkKind::Code),
            "ops" | "it" | "infra" | "admin" | "access" => Some(WorkKind::Ops),
            "design" | "concept" | "proposal" | "architecture" => Some(WorkKind::Design),
            "question" | "research" => Some(WorkKind::Question),
            _ => None,
        }
    }
}

const OPS: &[&str] = &[
    "sftp",
    "ftp",
    "ssh key",
    "vpn",
    "new user",
    "user account",
    "create user",
    "create an account",
    "new account",
    "access to",
    "grant access",
    "revoke access",
    "permissions for",
    "password reset",
    "reset password",
    "unlock",
    "mailbox",
    "distribution list",
    "license",
    "licence",
    "onboard",
    "offboard",
    "laptop",
    "printer",
    "dns record",
    "certificate",
    "firewall",
    "whitelist",
    "allowlist",
    "server",
    "install",
    "provision",
    "quota",
    "backup",
    "restore",
];

const DESIGN: &[&str] = &[
    "design",
    "architecture",
    "new system",
    "concept",
    "proposal",
    "rfc",
    "mockup",
    "wireframe",
    "prototype",
    "draft a",
    "how should we",
    "evaluate options",
    "compare options",
];

const CODE: &[&str] = &[
    "bug",
    "crash",
    "exception",
    "stack trace",
    "stacktrace",
    "error in",
    "fails",
    "failing",
    "regression",
    "refactor",
    "endpoint",
    "api",
    "function",
    "test",
    "build",
    "compile",
    "pull request",
    "ui",
    "button",
];

/// A first guess from the issue text; triage can overrule it.
pub fn guess_kind(text: &str) -> WorkKind {
    let t = text.to_lowercase();
    let n = |words: &[&str]| words.iter().filter(|w| has_word(&t, w)).count();
    let (ops, design, code) = (n(OPS), n(DESIGN), n(CODE));
    if ops > code && ops >= design {
        WorkKind::Ops
    } else if design > code && design > ops {
        WorkKind::Design
    } else if code == 0 && ops == 0 && design == 0 && t.trim_end().ends_with('?') {
        WorkKind::Question
    } else {
        WorkKind::Code
    }
}

/// How an agent ended its flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Done and verified; gates run.
    Resolved,
    /// A solution for a human to carry out (no access, not allowed, …).
    Proposed,
    /// Cannot or must not be done; with the reason.
    Declined,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Resolved => "resolved",
            Outcome::Proposed => "proposed",
            Outcome::Declined => "declined",
        }
    }

    pub fn parse(s: &str) -> Outcome {
        match s.trim().to_ascii_lowercase().as_str() {
            "proposed" | "proposal" | "propose" | "needs_human" => Outcome::Proposed,
            "declined" | "decline" | "blocked" | "not_allowed" | "wont_do" => Outcome::Declined,
            _ => Outcome::Resolved,
        }
    }
}

/// `*.example.com` style match, case-insensitive.
fn glob(pat: &str, s: &str) -> bool {
    let (p, s) = (pat.to_lowercase(), s.to_lowercase());
    match p.split_once('*') {
        None => p == s,
        Some((pre, rest)) => {
            if !s.starts_with(pre) {
                return false;
            }
            let tail = &s[pre.len()..];
            if rest.is_empty() {
                return true;
            }
            (0..=tail.len())
                .filter(|i| tail.is_char_boundary(*i))
                .any(|i| glob(rest, &tail[i..]))
        }
    }
}

/// The first off-limits host mentioned in `text` (`user@host`, `host:22`,
/// URLs), if any.
pub fn denied_host(text: &str, deny: &[String]) -> Option<String> {
    if deny.is_empty() {
        return None;
    }
    let tokens = text
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | ':' | '*')));
    for tok in tokens {
        let host = tok.rsplit('@').next().unwrap_or(tok);
        let host = host.split(':').next().unwrap_or(host).trim_matches('.');
        if host.is_empty() {
            continue;
        }
        for d in deny {
            let d = d.trim();
            if !d.is_empty() && glob(d, host) {
                return Some(d.to_string());
            }
        }
    }
    None
}

/// Claude Code permission deny rules for off-limits hosts: any shell
/// command or web fetch that names them.
pub fn claude_deny_rules(deny: &[String]) -> Vec<String> {
    let mut out = vec![];
    for d in deny.iter().map(|d| d.trim()).filter(|d| !d.is_empty()) {
        let bare = d.trim_start_matches("*.");
        out.push(format!("Bash(*{bare}*)"));
        out.push(format!("WebFetch(domain:{d})"));
    }
    out
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Seconds since the epoch from an RFC 3339 / ISO 8601 timestamp
/// (`2026-10-19T09:30:00Z`, `…+02:00`, `….123+0200`).
pub fn parse_timestamp(s: &str) -> Option<u64> {
    let s = s.trim();
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, se) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let rest = &s[19..];
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match rest.chars().next() {
        Some('+') | Some('-') => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            let digits: String = rest[1..].chars().filter(|c| c.is_ascii_digit()).collect();
            let oh = digits.get(0..2)?.parse::<i64>().ok()?;
            let om = digits
                .get(2..4)
                .and_then(|m| m.parse::<i64>().ok())
                .unwrap_or(0);
            sign * (oh * 3600 + om * 60)
        }
        _ => 0,
    };
    let t = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - offset;
    u64::try_from(t).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_from_text() {
        assert_eq!(
            guess_kind("User needs a new SFTP user on the partner server"),
            WorkKind::Ops
        );
        assert_eq!(guess_kind("Please reset password for anna"), WorkKind::Ops);
        assert_eq!(
            guess_kind("Make a new system for supplier onboarding: concept and architecture"),
            WorkKind::Design
        );
        assert_eq!(guess_kind("Crash when saving a file"), WorkKind::Code);
        assert_eq!(
            guess_kind("Which vendor do we use for SMS?"),
            WorkKind::Question
        );
        assert_eq!(WorkKind::parse("IT"), Some(WorkKind::Ops));
        assert_eq!(Outcome::parse("blocked"), Outcome::Declined);
        assert_eq!(Outcome::parse(""), Outcome::Resolved);
    }

    #[test]
    fn hosts_are_matched_wherever_they_appear() {
        let deny = vec![
            "payroll.internal".to_string(),
            "*.prod.example.com".to_string(),
        ];
        assert_eq!(
            denied_host("ssh admin@payroll.internal:22 'ls'", &deny).as_deref(),
            Some("payroll.internal")
        );
        assert_eq!(
            denied_host("curl https://db1.prod.example.com/health", &deny).as_deref(),
            Some("*.prod.example.com")
        );
        assert_eq!(denied_host("ssh dev.example.com", &deny), None);
        assert_eq!(denied_host("anything", &[]), None);
        assert_eq!(
            claude_deny_rules(&deny),
            vec![
                "Bash(*payroll.internal*)",
                "WebFetch(domain:payroll.internal)",
                "Bash(*prod.example.com*)",
                "WebFetch(domain:*.prod.example.com)"
            ]
        );
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_timestamp("2026-10-19T09:30:00Z"), Some(1_792_402_200));
        assert_eq!(
            parse_timestamp("2026-10-19T11:30:00.123+0200"),
            Some(1_792_402_200)
        );
        assert_eq!(
            parse_timestamp("2026-10-19T11:30:00+02:00"),
            Some(1_792_402_200)
        );
        assert_eq!(parse_timestamp("yesterday"), None);
    }
}
