//! Which CLI answers an Ask question, and with which flags (TGZTerminal).
//!
//! Vendor-neutral: a provider is a command line plus how to read its
//! stdout. The prompt always goes in on stdin. Two are built in:
//!
//! * `claude` — `claude -p` with stream-json output, on the user's own
//!   login. `--safe-mode` keeps the user's hooks, plugins, MCP servers and
//!   CLAUDE.md out of the run but, unlike `--bare`, still reads the plan
//!   login. Tools are limited to Read/Grep/Glob (or none), nothing can
//!   prompt (`--permission-prompts none`), reads of `assist.ask.deny_read`
//!   are refused by a permission rule, and the session is not saved.
//! * `codex` — `codex exec` in a read-only sandbox, plain text.
//!
//! Anything else comes from Lua `assist.providers` and replaces a built-in
//! of the same name. Its argv is used as given.

use config::{AssistConfig, AssistStreamFormat, AssistTools};

/// Instructions for every provider: appended to Claude's system prompt,
/// prepended to the prompt for the others.
pub const INSTRUCTIONS: &str = "\
You are answering from the Ask panel of a terminal emulator. The user's \
recent commands, their output and exit codes are in <terminal>. Answer the \
question briefly and concretely: name the cause, point at the file and line \
when you know it, then the fix. When you suggest shell commands, put each one \
in its own ```sh fenced block, ready to paste, without a prompt sigil. Never \
claim to have run anything: you can only read files, and the user decides \
what runs. Secrets in the context are masked as [REDACTED]; do not try to \
recover them.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub argv: Vec<String>,
    pub stream: AssistStreamFormat,
    /// Prepend [`INSTRUCTIONS`] to the prompt (the CLI has no flag for it).
    pub instructions_in_prompt: bool,
}

/// What a run may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunOptions {
    /// Alt+Enter: use `think_effort`.
    pub think: bool,
    /// Tools allowed for this pane (false for a remote pane, or when the
    /// config says `ContextOnly`).
    pub tools: bool,
}

pub fn launch(config: &AssistConfig, options: RunOptions) -> Result<Launch, String> {
    let name = config.ask.provider.trim();
    if let Some(custom) = config.providers.get(name) {
        if custom.argv.is_empty() {
            return Err(format!("assist.providers.{} has an empty argv", name));
        }
        return Ok(Launch {
            argv: custom.argv.clone(),
            stream: custom.stream,
            instructions_in_prompt: true,
        });
    }
    let tools = options.tools && config.ask.tools == AssistTools::ReadOnly;
    let effort = if options.think {
        &config.ask.think_effort
    } else {
        &config.ask.effort
    };
    match name {
        "claude" => Ok(Launch {
            argv: claude_argv(config, tools, effort),
            stream: AssistStreamFormat::ClaudeStreamJson,
            instructions_in_prompt: false,
        }),
        "codex" => {
            let mut argv: Vec<String> = [
                "codex",
                "exec",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            if let Some(model) = &config.ask.model {
                argv.push("-m".into());
                argv.push(model.clone());
            }
            if !effort.trim().is_empty() {
                argv.push("-c".into());
                argv.push(format!("model_reasoning_effort=\"{}\"", effort.trim()));
            }
            argv.push("-".into());
            Ok(Launch {
                argv,
                stream: AssistStreamFormat::Text,
                instructions_in_prompt: true,
            })
        }
        other => Err(format!(
            "unknown assist.ask.provider '{}': use 'claude', 'codex' or one from assist.providers",
            other
        )),
    }
}

fn claude_argv(config: &AssistConfig, tools: bool, effort: &str) -> Vec<String> {
    let mut argv: Vec<String> = [
        "claude",
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--safe-mode",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--permission-prompts",
        "none",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    argv.push("--tools".into());
    argv.push(if tools {
        "Read,Grep,Glob".into()
    } else {
        String::new()
    });
    let deny: Vec<String> = config
        .ask
        .deny_read
        .iter()
        .filter(|pattern| !pattern.trim().is_empty())
        .map(|pattern| format!("Read({})", pattern.trim()))
        .collect();
    if tools && !deny.is_empty() {
        argv.push("--settings".into());
        argv.push(serde_json::json!({ "permissions": { "deny": deny } }).to_string());
    }
    argv.push("--append-system-prompt".into());
    argv.push(INSTRUCTIONS.into());
    if let Some(model) = &config.ask.model {
        argv.push("--model".into());
        argv.push(model.clone());
    }
    if !effort.trim().is_empty() {
        argv.push("--effort".into());
        argv.push(effort.trim().to_string());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::AssistProviderConfig;

    fn options(tools: bool) -> RunOptions {
        RunOptions {
            think: false,
            tools,
        }
    }

    fn value_after<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
        argv.iter()
            .position(|arg| arg == flag)
            .and_then(|pos| argv.get(pos + 1))
            .map(String::as_str)
    }

    #[test]
    fn claude_is_read_only_unsaved_and_never_bare() {
        let launch = launch(&AssistConfig::default(), options(true)).unwrap();
        let argv = &launch.argv;
        assert_eq!(argv[0], "claude");
        assert_eq!(launch.stream, AssistStreamFormat::ClaudeStreamJson);
        assert_eq!(value_after(argv, "--tools"), Some("Read,Grep,Glob"));
        assert_eq!(value_after(argv, "--permission-prompts"), Some("none"));
        assert_eq!(value_after(argv, "--effort"), Some("low"));
        assert!(argv.contains(&"--safe-mode".to_string()));
        assert!(argv.contains(&"--no-session-persistence".to_string()));
        // --bare would ignore the plan login.
        assert!(!argv.contains(&"--bare".to_string()));
        let settings = value_after(argv, "--settings").unwrap();
        assert!(settings.contains("Read(**/.env*)"), "{}", settings);
        assert!(!argv
            .iter()
            .any(|arg| arg.contains("bypass") || arg.contains("dangerously")));
    }

    #[test]
    fn no_tools_for_a_remote_pane_or_context_only() {
        let argv = launch(&AssistConfig::default(), options(false))
            .unwrap()
            .argv;
        assert_eq!(value_after(&argv, "--tools"), Some(""));
        assert!(value_after(&argv, "--settings").is_none());

        let mut config = AssistConfig::default();
        config.ask.tools = AssistTools::ContextOnly;
        let argv = launch(&config, options(true)).unwrap().argv;
        assert_eq!(value_after(&argv, "--tools"), Some(""));
    }

    #[test]
    fn think_longer_raises_the_effort_and_a_model_is_passed() {
        let mut config = AssistConfig::default();
        config.ask.model = Some("opus".into());
        let argv = launch(
            &config,
            RunOptions {
                think: true,
                tools: true,
            },
        )
        .unwrap()
        .argv;
        assert_eq!(value_after(&argv, "--effort"), Some("high"));
        assert_eq!(value_after(&argv, "--model"), Some("opus"));
    }

    #[test]
    fn codex_reads_stdin_in_a_read_only_sandbox() {
        let mut config = AssistConfig::default();
        config.ask.provider = "codex".into();
        let launch = launch(&config, options(true)).unwrap();
        assert_eq!(
            &launch.argv[..2],
            &["codex".to_string(), "exec".to_string()]
        );
        assert_eq!(value_after(&launch.argv, "--sandbox"), Some("read-only"));
        assert_eq!(launch.argv.last().map(String::as_str), Some("-"));
        assert!(launch.instructions_in_prompt);
    }

    #[test]
    fn a_user_provider_replaces_a_built_in_and_unknown_names_fail() {
        let mut config = AssistConfig::default();
        config.providers.insert(
            "claude".into(),
            AssistProviderConfig {
                argv: vec!["my-llm".into(), "--stdin".into()],
                stream: AssistStreamFormat::Text,
            },
        );
        assert_eq!(launch(&config, options(true)).unwrap().argv[0], "my-llm");
        config.ask.provider = "nope".into();
        assert!(launch(&config, options(true)).is_err());
    }
}
