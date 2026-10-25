//! What each agent vendor's terms say about Tower launching and driving its
//! CLI, as data. `docs/VENDOR_TERMS.md` has the full quotes and the reasoning;
//! this file is the part the engine acts on.
//!
//! Nothing here is ever `Blocked`: the combinations vendors forbid outright
//! (reusing OAuth tokens, calling their backends, spoofing the official
//! client) are ones Tower has no code path for, and [`lint_settings`] /
//! [`lint_args`] keep it that way. What is left is either clearly allowed or
//! a grey zone, and a grey zone runs once the user has read the warning and
//! accepted it for that engine.

use serde::{Deserialize, Serialize};
use tower_proto::DriverKind;

/// Bump whenever a verdict or a quote changes: every acceptance given for an
/// older version is asked for again.
pub const MATRIX_VERSION: u32 = 1;
pub const MATRIX_AS_OF: &str = "2026-10-21";

/// Whose terms apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Vendor {
    Claude,
    Antigravity,
    Gemini,
    Codex,
    Opencode,
    /// An OpenAI-compatible HTTP endpoint (`driver = "llm"`): plain API use.
    Endpoint,
    /// A CLI Tower knows nothing about.
    Other,
}

impl Vendor {
    pub fn label(self) -> &'static str {
        match self {
            Vendor::Claude => "Claude",
            Vendor::Antigravity => "Antigravity",
            Vendor::Gemini => "Gemini CLI",
            Vendor::Codex => "Codex",
            Vendor::Opencode => "OpenCode",
            Vendor::Endpoint => "model endpoint",
            Vendor::Other => "agent",
        }
    }
}

/// How the engine signs in, as declared by its config (`auth = "…"`).
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Auth {
    /// The user's own plan login (claude.ai, ChatGPT, Google account).
    Subscription,
    /// A pay-as-you-go API key.
    ApiKey,
    /// Bedrock, Vertex, Foundry and the like: the cloud provider's terms.
    Cloud,
    /// Not declared. Treated as a subscription, the stricter reading.
    #[default]
    Unknown,
}

impl Auth {
    fn effective(self) -> Auth {
        match self {
            Auth::Unknown => Auth::Subscription,
            other => other,
        }
    }
}

/// How Tower runs the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The real interactive CLI in a visible terminal pane.
    Pane,
    /// A non-interactive run (`claude -p`, `codex exec`, `agy -p`).
    Headless,
    /// Agent Client Protocol over stdio.
    Acp,
}

/// Who wrote the input the agent is about to get.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// The brief a session starts with.
    Initial,
    /// Something the user typed or explicitly approved.
    #[allow(dead_code)] // Taken by the message-delivery ladder.
    Human,
    /// Something Tower generated on its own (a handoff result, gate output,
    /// an issue update).
    Tower,
}

/// One grey zone, with the text that makes it one.
#[derive(Debug, PartialEq, Eq)]
pub struct TermsRef {
    /// Stable id, used for the acceptance record and in the docs.
    pub id: &'static str,
    /// One line: what is unclear.
    pub summary: &'static str,
    /// The vendor's own words.
    pub quote: &'static str,
    pub url: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    /// Grey zone: runs once accepted for the engine.
    Warn(&'static TermsRef),
}

impl Verdict {
    /// Does running this need an acceptance the user has not given yet?
    /// `accepted` is the matrix version the user accepted for this engine.
    pub fn needs_ack(self, accepted: Option<u32>) -> bool {
        matches!(self, Verdict::Warn(_)) && accepted != Some(MATRIX_VERSION)
    }
}

pub static CLAUDE_TOWER_INPUT: TermsRef = TermsRef {
    id: "claude-tower-input",
    summary: "Tower sends messages it generated itself into a Claude session on your plan. \
              Anthropic has no text on this; the closest is the ban on automated access.",
    quote: "Except when you are accessing our Services via an Anthropic API Key or where we \
            otherwise explicitly permit it, to access the Services through automated or \
            non-human means, whether through a bot, script, or otherwise.",
    url: "https://www.anthropic.com/legal/consumer-terms",
};

pub static CLAUDE_HEADLESS: TermsRef = TermsRef {
    id: "claude-headless",
    summary: "Headless `claude -p` on your plan, started by Tower. Anthropic support says it \
              draws from your subscription; the Consumer Terms still ban automated access \
              unless explicitly permitted.",
    quote: "You can still use the Claude Agent SDK, claude -p, and third-party apps with your \
            subscription limits.",
    url: "https://support.claude.com/en/articles/15036540",
};

pub static CLAUDE_SDK: TermsRef = TermsRef {
    id: "claude-agent-sdk",
    summary: "An Agent SDK based adapter (claude-agent-acp) on your plan. Anthropic asks \
              products built on the SDK to use API keys.",
    quote: "Unless previously approved, Anthropic does not allow third party developers to \
            offer claude.ai login or rate limits for their products, including agents built \
            on the Claude Agent SDK.",
    url: "https://code.claude.com/docs/en/agent-sdk/overview",
};

pub static AGY_THIRD_PARTY: TermsRef = TermsRef {
    id: "antigravity-third-party",
    summary: "Tower drives the official Antigravity CLI on your Google login. Google bans \
              third-party software accessing the service and has not said whether launching \
              the official binary counts; the suspensions so far were for reused tokens.",
    quote: "Using third party software, tools, or services to access the Service (e.g. using \
            OpenClaw with Antigravity OAuth) is a breach of this Agreement.",
    url: "https://antigravity.google/terms",
};

pub static GEMINI_OAUTH: TermsRef = TermsRef {
    id: "gemini-google-login",
    summary: "Gemini CLI on a Google login. Consumer logins moved to the Antigravity CLI on \
              2026-06-18; Code Assist logins fall under the same third-party rule.",
    // Only the verified opening of the sentence; the page goes on to name
    // OpenClaw with Gemini CLI OAuth as a violation.
    quote: "Directly accessing the services powering Gemini CLI (for example, the Gemini Code \
            Assist service) using third-party …",
    url: "https://geminicli.com/docs/resources/tos-privacy/",
};

pub static CODEX_AUTOMATION: TermsRef = TermsRef {
    id: "codex-chatgpt-automation",
    summary: "Codex on your ChatGPT plan, driven without you typing. It works, but OpenAI \
              recommends API keys for automation.",
    quote: "API keys are still the recommended default for automation.",
    url: "https://learn.chatgpt.com/docs/auth",
};

pub static OPENCODE_PROVIDER: TermsRef = TermsRef {
    id: "opencode-provider-login",
    summary: "OpenCode on a provider login. The provider's terms apply; a Claude Pro/Max \
              login inside OpenCode is not allowed by Anthropic.",
    quote: "There are plugins that allow you to use your Claude Pro/Max models with OpenCode. \
            Anthropic explicitly prohibits this.",
    url: "https://opencode.ai/docs/providers/",
};

pub static UNKNOWN_VENDOR: TermsRef = TermsRef {
    id: "unknown-vendor",
    summary: "Tower does not know this CLI's terms. Check them before letting Tower start it \
              on your plan login.",
    quote: "",
    url: "",
};

/// Every grey zone, for the tests' uniqueness check and their harness.
#[cfg(test)]
pub static ALL: [&TermsRef; 8] = [
    &CLAUDE_TOWER_INPUT,
    &CLAUDE_HEADLESS,
    &CLAUDE_SDK,
    &AGY_THIRD_PARTY,
    &GEMINI_OAUTH,
    &CODEX_AUTOMATION,
    &OPENCODE_PROVIDER,
    &UNKNOWN_VENDOR,
];

/// Which vendor a role or engine command runs. Launchers (`npx -y`, `env`,
/// `bunx`) are looked through, and a package name counts like a program:
/// `npx -y @agentclientprotocol/claude-agent-acp@1.2` is Claude.
pub fn vendor_of(driver: DriverKind, command: &[String]) -> Vendor {
    if driver == DriverKind::Llm {
        return Vendor::Endpoint;
    }
    for arg in command {
        let name = program_name(arg);
        let vendor = match name.as_str() {
            "claude" | "claude-agent-acp" | "claude-code-acp" => Vendor::Claude,
            "agy" | "antigravity" | "antigravity-cli" => Vendor::Antigravity,
            "gemini" => Vendor::Gemini,
            "codex" | "codex-acp" => Vendor::Codex,
            "opencode" => Vendor::Opencode,
            _ => continue,
        };
        return vendor;
    }
    Vendor::Other
}

/// `/usr/local/bin/codex.exe` -> `codex`; `@scope/pkg@1.0` -> `pkg`.
fn program_name(arg: &str) -> String {
    let last = arg.rsplit(['/', '\\']).next().unwrap_or(arg);
    // An npm version suffix; a leading `@` was the scope and is gone now.
    let last = last.split('@').next().unwrap_or(last);
    let lower = last.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".ps1"] {
        if let Some(stem) = lower.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    lower
}

/// The terms verdict for running `vendor` signed in with `auth`, in `mode`,
/// with input from `input`.
pub fn verdict(vendor: Vendor, auth: Auth, mode: Mode, input: Input) -> Verdict {
    use Verdict::{Allowed, Warn};
    if vendor == Vendor::Endpoint {
        return Allowed;
    }
    if matches!(auth.effective(), Auth::ApiKey | Auth::Cloud) {
        return Allowed;
    }
    // From here on: a plan login (or an undeclared one).
    let tower_driven = input == Input::Tower;
    match (vendor, mode) {
        // Anthropic: "Nor does it prevent an end user from signing in to the
        // unmodified Claude Code binary with their own Claude subscription".
        (Vendor::Claude, Mode::Pane) if !tower_driven => Allowed,
        (Vendor::Claude, Mode::Pane) => Warn(&CLAUDE_TOWER_INPUT),
        (Vendor::Claude, Mode::Headless) => Warn(&CLAUDE_HEADLESS),
        (Vendor::Claude, Mode::Acp) => Warn(&CLAUDE_SDK),
        (Vendor::Antigravity, _) => Warn(&AGY_THIRD_PARTY),
        (Vendor::Gemini, _) => Warn(&GEMINI_OAUTH),
        // The official client, run by the account owner.
        (Vendor::Codex, Mode::Pane) if !tower_driven => Allowed,
        (Vendor::Codex, _) => Warn(&CODEX_AUTOMATION),
        (Vendor::Opencode, _) => Warn(&OPENCODE_PROVIDER),
        (Vendor::Other, _) | (Vendor::Endpoint, _) => Warn(&UNKNOWN_VENDOR),
    }
}

/// Settings keys that change how Claude Code signs in. Tower must never set
/// them: Anthropic forbids removing or restricting the binary's own login
/// methods, and routing requests elsewhere is what got third-party tools
/// blocked.
const FORBIDDEN_SETTINGS: [&str; 4] = [
    "apiKeyHelper",
    "forceLoginMethod",
    "forceLoginOrgUUID",
    "awsAuthRefresh",
];

const FORBIDDEN_ENV: [&str; 6] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "OPENAI_BASE_URL",
    "GOOGLE_GEMINI_BASE_URL",
];

/// Problems with a settings file Tower generated for an agent; empty when
/// it touches nothing about sign-in.
pub fn lint_settings(settings: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(obj) = settings.as_object() else {
        return out;
    };
    for key in FORBIDDEN_SETTINGS {
        if obj.contains_key(key) {
            out.push(format!("sets `{key}`"));
        }
    }
    if let Some(env) = obj.get("env").and_then(|e| e.as_object()) {
        for key in FORBIDDEN_ENV {
            if env.contains_key(key) {
                out.push(format!("sets env `{key}`"));
            }
        }
    }
    out
}

/// Problems with launch arguments Tower itself adds: no flag that switches
/// the agent's own safety prompts off.
pub fn lint_args(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|a| {
            a.starts_with("--dangerously")
                || a.as_str() == "--yolo"
                || a.as_str() == "--permission-mode=bypassPermissions"
        })
        .map(|a| format!("adds `{a}`"))
        .chain(
            args.windows(2)
                .filter(|w| w[0] == "--permission-mode" && w[1] == "bypassPermissions")
                .map(|_| "adds `--permission-mode bypassPermissions`".to_string()),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn vendor_is_found_through_launchers_and_paths() {
        let pane = DriverKind::Pane;
        let acp = DriverKind::Acp;
        assert_eq!(vendor_of(pane, &cmd(&["claude"])), Vendor::Claude);
        assert_eq!(
            vendor_of(pane, &cmd(&["/opt/homebrew/bin/codex"])),
            Vendor::Codex
        );
        assert_eq!(
            vendor_of(
                acp,
                &cmd(&["npx", "-y", "@agentclientprotocol/claude-agent-acp"])
            ),
            Vendor::Claude
        );
        assert_eq!(
            vendor_of(
                acp,
                &cmd(&["npx", "-y", "@agentclientprotocol/codex-acp@0.4.1"])
            ),
            Vendor::Codex
        );
        assert_eq!(vendor_of(acp, &cmd(&["opencode", "acp"])), Vendor::Opencode);
        assert_eq!(vendor_of(acp, &cmd(&["gemini", "--acp"])), Vendor::Gemini);
        assert_eq!(
            vendor_of(pane, &cmd(&["env", "FOO=1", "agy"])),
            Vendor::Antigravity
        );
        assert_eq!(
            vendor_of(pane, &cmd(&["C:\\tools\\Claude.EXE"])),
            Vendor::Claude
        );
        assert_eq!(vendor_of(pane, &cmd(&["goose"])), Vendor::Other);
        assert_eq!(vendor_of(pane, &[]), Vendor::Other);
        assert_eq!(
            vendor_of(DriverKind::Llm, &cmd(&["claude"])),
            Vendor::Endpoint
        );
    }

    #[test]
    fn api_keys_and_cloud_are_always_allowed() {
        for vendor in [
            Vendor::Claude,
            Vendor::Antigravity,
            Vendor::Gemini,
            Vendor::Codex,
            Vendor::Opencode,
            Vendor::Other,
        ] {
            for mode in [Mode::Pane, Mode::Headless, Mode::Acp] {
                for input in [Input::Initial, Input::Human, Input::Tower] {
                    for auth in [Auth::ApiKey, Auth::Cloud] {
                        assert_eq!(
                            verdict(vendor, auth, mode, input),
                            Verdict::Allowed,
                            "{vendor:?} {auth:?} {mode:?} {input:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_claude_pane_on_a_plan_is_allowed_until_tower_types_into_it() {
        let v = |input| verdict(Vendor::Claude, Auth::Subscription, Mode::Pane, input);
        assert_eq!(v(Input::Initial), Verdict::Allowed);
        assert_eq!(v(Input::Human), Verdict::Allowed);
        assert_eq!(v(Input::Tower), Verdict::Warn(&CLAUDE_TOWER_INPUT));
    }

    #[test]
    fn claude_headless_and_acp_on_a_plan_are_grey() {
        assert_eq!(
            verdict(
                Vendor::Claude,
                Auth::Subscription,
                Mode::Headless,
                Input::Initial
            ),
            Verdict::Warn(&CLAUDE_HEADLESS)
        );
        assert_eq!(
            verdict(Vendor::Claude, Auth::Subscription, Mode::Acp, Input::Human),
            Verdict::Warn(&CLAUDE_SDK)
        );
    }

    #[test]
    fn an_undeclared_login_is_read_as_a_plan_login() {
        for mode in [Mode::Pane, Mode::Headless, Mode::Acp] {
            for input in [Input::Initial, Input::Human, Input::Tower] {
                assert_eq!(
                    verdict(Vendor::Claude, Auth::Unknown, mode, input),
                    verdict(Vendor::Claude, Auth::Subscription, mode, input)
                );
            }
        }
    }

    #[test]
    fn google_logins_are_grey_in_every_mode() {
        for mode in [Mode::Pane, Mode::Headless, Mode::Acp] {
            assert_eq!(
                verdict(
                    Vendor::Antigravity,
                    Auth::Subscription,
                    mode,
                    Input::Initial
                ),
                Verdict::Warn(&AGY_THIRD_PARTY)
            );
            assert_eq!(
                verdict(Vendor::Gemini, Auth::Subscription, mode, Input::Initial),
                Verdict::Warn(&GEMINI_OAUTH)
            );
        }
    }

    #[test]
    fn codex_pane_is_allowed_but_driving_it_is_grey() {
        let v = |mode, input| verdict(Vendor::Codex, Auth::Subscription, mode, input);
        assert_eq!(v(Mode::Pane, Input::Initial), Verdict::Allowed);
        assert_eq!(
            v(Mode::Pane, Input::Tower),
            Verdict::Warn(&CODEX_AUTOMATION)
        );
        assert_eq!(
            v(Mode::Headless, Input::Initial),
            Verdict::Warn(&CODEX_AUTOMATION)
        );
        assert_eq!(v(Mode::Acp, Input::Human), Verdict::Warn(&CODEX_AUTOMATION));
    }

    #[test]
    fn endpoints_are_plain_api_use() {
        assert_eq!(
            verdict(
                Vendor::Endpoint,
                Auth::Unknown,
                Mode::Headless,
                Input::Tower
            ),
            Verdict::Allowed
        );
    }

    #[test]
    fn acceptance_is_per_matrix_version() {
        let warn = Verdict::Warn(&CLAUDE_HEADLESS);
        assert!(warn.needs_ack(None));
        assert!(warn.needs_ack(Some(MATRIX_VERSION - 1)));
        assert!(!warn.needs_ack(Some(MATRIX_VERSION)));
        assert!(!Verdict::Allowed.needs_ack(None));
    }

    #[test]
    fn every_grey_zone_has_a_unique_id_and_a_source() {
        let mut ids: Vec<&str> = ALL.iter().map(|t| t.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), ALL.len());
        for t in ALL.iter().filter(|t| t.id != UNKNOWN_VENDOR.id) {
            assert!(t.url.starts_with("https://"), "{}", t.id);
            assert!(!t.quote.is_empty(), "{}", t.id);
        }
    }

    #[test]
    fn generated_claude_settings_touch_nothing_about_sign_in() {
        let exe = std::path::Path::new("/usr/local/bin/tower");
        for deny in [
            vec![],
            vec!["WebFetch(domain:payroll.internal)".to_string()],
        ] {
            let settings = crate::drivers::pane::settings_json(exe, &deny);
            assert_eq!(lint_settings(&settings), Vec::<String>::new());
        }
    }

    #[test]
    fn lint_catches_auth_overrides() {
        let bad = serde_json::json!({
            "apiKeyHelper": "/bin/echo key",
            "env": { "ANTHROPIC_BASE_URL": "https://proxy.example", "PATH": "/bin" }
        });
        assert_eq!(
            lint_settings(&bad),
            vec![
                "sets `apiKeyHelper`".to_string(),
                "sets env `ANTHROPIC_BASE_URL`".to_string()
            ]
        );
        assert!(lint_settings(&serde_json::json!("not an object")).is_empty());
    }

    #[test]
    fn lint_catches_permission_bypass_flags() {
        assert!(lint_args(&cmd(&["--session-id", "x", "--allowedTools", "mcp__tower"])).is_empty());
        assert_eq!(
            lint_args(&cmd(&["--dangerously-skip-permissions"])).len(),
            1
        );
        assert_eq!(
            lint_args(&cmd(&["--permission-mode", "bypassPermissions"])).len(),
            1
        );
        assert_eq!(lint_args(&cmd(&["--yolo"])).len(), 1);
    }
}
