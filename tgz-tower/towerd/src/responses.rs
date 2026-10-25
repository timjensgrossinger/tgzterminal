//! Canned acknowledgements: the "we have it" note on a new issue or a new
//! customer comment, so a response-time clock is answered while the actual
//! work is still running. `[ack]` in `tower.toml`; a watch can override the
//! text and visibility.
//!
//! ```toml
//! [ack]
//! template = "Thanks, we have received this and are looking into it."
//! signature = "— posted by Tower"
//! visibility = "public"          # or "internal" (Jira: restricted to internal_role)
//! internal_role = "Service Desk Team"
//! ```
//! Whether an acknowledgement is drafted, asked for or posted is
//! `ack.post` in `[policy]`; the default is a draft you post yourself.
//!
//! The text must stay honest: it says the request was received, never that
//! work was done. Placeholders: `{key}`, `{summary}`, `{reporter}`, `{sla}`.

use serde::Deserialize;

pub const DEFAULT_TEMPLATE: &str =
    "Thanks, we have received this and are looking into it. We will get back to you here.";
pub const DEFAULT_SIGNATURE: &str = "— posted by Tower";

/// Who may read a comment Tower posts.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Everyone who can see the issue; in Jira Service Management, the
    /// customer too (and they are notified).
    #[default]
    Public,
    /// Jira: restricted to `internal_role`. Other trackers have no internal
    /// comments, so Tower refuses to post there.
    Internal,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct AckCfg {
    #[serde(default = "default_template")]
    pub template: String,
    #[serde(default = "default_signature")]
    pub signature: String,
    #[serde(default)]
    pub visibility: Visibility,
    #[serde(default = "default_role")]
    pub internal_role: String,
}

impl Default for AckCfg {
    fn default() -> Self {
        Self {
            template: default_template(),
            signature: default_signature(),
            visibility: Visibility::default(),
            internal_role: default_role(),
        }
    }
}

fn default_template() -> String {
    DEFAULT_TEMPLATE.into()
}
fn default_signature() -> String {
    DEFAULT_SIGNATURE.into()
}
fn default_role() -> String {
    "Service Desk Team".into()
}

/// What fills a template.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vars<'a> {
    pub key: &'a str,
    pub summary: &'a str,
    pub reporter: &'a str,
    /// The clock being answered, e.g. "First response".
    pub sla: &'a str,
}

/// The acknowledgement text: template with placeholders filled, then the
/// signature on its own line. Unknown placeholders stay as written, so a
/// typo shows instead of vanishing.
pub fn render(template: &str, signature: &str, v: &Vars) -> String {
    let reporter = if v.reporter.trim().is_empty() {
        "there"
    } else {
        v.reporter.trim()
    };
    let body = template
        .replace("{key}", v.key)
        .replace("{summary}", v.summary)
        .replace("{reporter}", reporter)
        .replace("{sla}", v.sla);
    let body = body.trim();
    if signature.trim().is_empty() {
        body.to_string()
    } else {
        format!("{body}\n\n{}", signature.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_filled_and_signed() {
        let v = Vars {
            key: "SD-7",
            summary: "VPN down",
            reporter: "Ana",
            sla: "First response",
        };
        assert_eq!(
            render("Hi {reporter}, {key} ({summary}) is in.", "— sig", &v),
            "Hi Ana, SD-7 (VPN down) is in.\n\n— sig"
        );
    }

    #[test]
    fn missing_reporter_and_signature_read_naturally() {
        let v = Vars::default();
        assert_eq!(render("Hi {reporter}.", "", &v), "Hi there.");
        assert_eq!(render("  x  ", "  ", &v), "x");
    }

    #[test]
    fn unknown_placeholders_stay_visible() {
        assert_eq!(render("{oops}", "", &Vars::default()), "{oops}");
    }

    #[test]
    fn the_default_text_claims_no_work() {
        let t = render(DEFAULT_TEMPLATE, DEFAULT_SIGNATURE, &Vars::default()).to_lowercase();
        for claim in ["fixed", "resolved", "done", "completed"] {
            assert!(!t.contains(claim), "{claim}");
        }
        assert!(t.contains("received"));
    }

    #[test]
    fn config_defaults_and_overrides() {
        let c: AckCfg = toml::from_str("").unwrap();
        assert_eq!(c, AckCfg::default());
        assert_eq!(c.visibility, Visibility::Public);
        let c: AckCfg =
            toml::from_str("visibility = \"internal\"\ninternal_role = \"Agents\"").unwrap();
        assert_eq!(c.visibility, Visibility::Internal);
        assert_eq!(c.internal_role, "Agents");
    }
}
