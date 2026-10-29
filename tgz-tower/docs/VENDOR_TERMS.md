# Vendor terms: what Tower does with each agent CLI, and why

Tower starts other vendors' agent programs and, sometimes, sends them input
the user did not type. Every vendor has rules about that. This page records
what those rules say, how Tower reads them, and what Tower does as a result.
The code that acts on it is `towerd/src/compliance.rs`.

**This is not legal advice.** It is a reading of public text, as of
**2026-10-27** (`MATRIX_AS_OF`). The vendors changed their positions several
times in 2026. Tower's notes carry a version (`MATRIX_VERSION`), and an
acceptance you gave for an older version is asked for again.

## Rules Tower always follows

Every vendor forbids some form of the following. These are the actions that
got users suspended in 2026, so Tower has no code path for any of them:

- Tower never reads, copies, stores, refreshes or forwards OAuth tokens,
  keychain entries or credential files (`~/.claude/.credentials.json`,
  `~/.codex/auth.json`, the Antigravity/Gemini keyring entries).
- Tower never calls a vendor's backend itself, and never proxies requests,
  rewrites base URLs, or spoofs headers or user agents.
- Tower only launches the user's own installed, unmodified binary. It does
  not bundle, patch or pin one.
- Sign-in happens only through the vendor's own flow, inside the agent's own
  pane. Tower never drives a login.
- The settings Tower generates contain no sign-in keys (`apiKeyHelper`,
  `forceLoginMethod`, `env.ANTHROPIC_*`, base URLs). `lint_settings` enforces
  this at launch time; Tower refuses to start an agent if it is violated.
- The arguments Tower adds never switch off the agent's own permission
  prompts (`--dangerously-*`, `bypassPermissions`). `lint_args` enforces this.
- One person's account is used for that person's own work. Tower has no
  shared or pooled accounts.

## Sign-in modes

Each role or `[routing.engines.*]` entry can declare `auth`:

| `auth` | Meaning | Terms Tower applies |
|---|---|---|
| `subscription` | A plan login (claude.ai, ChatGPT, Google account) | The consumer or plan terms, as read below |
| `api_key` | A pay-as-you-go API key | API terms: allowed in every mode |
| `cloud` | Bedrock, Vertex, Foundry and the like | The cloud provider's terms: allowed in every mode |
| *(unset)* | Unknown | Read as `subscription`, the stricter reading |

Tower only reads what you declare. It never inspects credential files to
find out.

## Verdicts

A verdict is either **allowed** or a **grey zone**. A grey zone runs once
you have accepted the note for it in the Inbox (**I accept**). It is not
blocked: you decide for your own account. The note shows the vendor's text
and a link.

The **input** column in the tables below means:
- *Initial*: the brief a session starts with.
- *Human*: text you typed or explicitly approved.
- *Tower*: text Tower generated itself, such as a handoff result, gate
  output or an issue update.

### Anthropic: Claude Code

| Mode | Plan login | Note id |
|---|---|---|
| Interactive `claude` in a pane, Initial/Human input | **Allowed** | |
| Interactive pane, Tower-generated input typed in | Grey | `claude-tower-input` |
| Headless `claude -p` | **Allowed** | |
| ACP via `claude-agent-acp` (Agent SDK) | Grey | `claude-agent-sdk` |

What the sources say:
- **Allowed:** "Nor does it prevent an end user from signing in to the
  unmodified Claude Code binary with their own Claude subscription" — 
  [Legal and compliance](https://code.claude.com/docs/en/legal-and-compliance).
- **Against automation:** "Except when you are accessing our Services via an
  Anthropic API Key or where we otherwise explicitly permit it, to access the
  Services through automated or non-human means, whether through a bot,
  script, or otherwise." —
  [Consumer Terms](https://www.anthropic.com/legal/consumer-terms).
- **For `-p` and the SDK on a plan:** "You can still use the Claude Agent
  SDK, claude -p, and third-party apps with your subscription limits." —
  [Support, Oct 7 2026 update](https://support.claude.com/en/articles/15036540).
- **Against SDK-based products on a plan login:** "Unless previously
  approved, Anthropic does not allow third party developers to offer
  claude.ai login or rate limits for their products, including agents built
  on the Claude Agent SDK." —
  [Agent SDK overview](https://code.claude.com/docs/en/agent-sdk/overview).
- **Limits:** "Advertised usage limits for Pro and Max plans assume ordinary,
  individual usage of Claude Code and the Agent SDK." — Legal and compliance.
  Anthropic defines this nowhere; the limits themselves (five-hour window,
  weekly cap, Max/Team monthly API credits) are the practical definition, and
  Claude Code itself ships routines, `/loop`, agent teams and workflows that
  "spawn dozens of agents" to plan subscribers. What the sentence reserves
  the right to act on is the bot-farm shape: pooled or shared accounts,
  round-the-clock loops with nobody behind them, tokens lifted into other
  products. One person's own routines and parallel flights are the usage
  the plan is priced for. Tower's markers of "individual" are the rules at
  the top of this page, not a flight count.
- **Redistributing a build:** "preinstalling or running Claude Code in your
  products or services (e.g. in hosted sandboxes or other agent
  infrastructure) requires agreeing to our Commercial Terms of Service" plus
  three conditions: the binary is not modified, no authentication method is
  removed or restricted, and each end user authenticates and is billed under
  their own credentials — Legal and compliance. Tower meets all three by
  construction (see the rules above); whoever ships a build that bundles or
  launches Claude Code accepts the Commercial Terms as the distributor. The
  same page allows saying in plain text that a product runs Claude Code, but
  not "Claude Code" or "Anthropic" in a product, feature or company name or
  logo.

The automated-access clause has an exception for what Anthropic "otherwise
explicitly permit[s]", and the support article names `claude -p` as covered
by a plan. That is the permission, so headless runs are allowed, whatever the
prompt's origin: a `-p` call is scripted by definition. (Until matrix
version 2 Tower rated it grey; the Oct 7 2026 wording settled it.) Today no
Tower driver runs `claude -p` itself — the `llm` driver is an HTTP endpoint —
so the row is there for a future headless driver and for a role whose
`command` is a user-written `-p` wrapper. Note the
billing side: on Max and Team, `-p` and the SDK draw on the plan's monthly
API credits, which "can't be used in Claude Code or the Claude apps", so
headless and interactive usage come out of different pools.

Anthropic publishes no text on programmatically typing into a running
session. Typing into a session you started is less automated than `-p`, but
the automated-access clause is the only text that covers it, so Tower still
treats it as a grey zone.

For Tower-generated messages, Tower uses Claude's own mechanisms before
typing anything into the pane:
- A `Stop` hook that returns `decision: "block"` with the queued message.
- `PostToolUse` `additionalContext` for issue updates.

Typing remains only for an agent that is already idle. Claude's Channels
are not used, because they need a `--dangerously-…` flag that Tower's own
launch check refuses.

### Google: Antigravity CLI (`agy`) and Gemini CLI

| Mode | Google login | Note id |
|---|---|---|
| `agy` in a pane, headless `agy -p`, any input | Grey | `antigravity-third-party` |
| `agy` over ACP | Only a community adapter exists, so Tower does not seed it; grey if you add it | `antigravity-third-party` |
| Gemini CLI, any mode | Grey | `gemini-google-login` |
| Any mode, Gemini API key or Vertex | **Allowed** | |

What the sources say:
- "Using third party software, tools, or services to access the Service
  (e.g. using OpenClaw with Antigravity OAuth) is a breach of this
  Agreement." — [Antigravity terms §6](https://antigravity.google/terms).
- The suspensions reported in February and September 2026 were for reusing
  Antigravity OAuth tokens in other tools. None of them involved launching
  the official binary.
- Several forum threads ask exactly Tower's question: may a local app launch
  the official `agy` as a subprocess, without touching its tokens? Google
  has not answered any of them. The only reply saying "yes" is not from
  Google staff.
- Gemini CLI: consumer Google logins moved to the Antigravity CLI on
  2026-06-18. The Gemini CLI terms call "Directly accessing the services
  powering Gemini CLI (for example, the Gemini Code Assist service) using
  third-party …" a violation; the page names OpenClaw with Gemini CLI OAuth
  as the example. ([Gemini CLI terms](https://geminicli.com/docs/resources/tos-privacy/))

How Tower runs Antigravity:
- It launches `agy` interactively in a visible pane, like Claude.
- `agy` takes MCP servers only from config files, so Tower writes
  `.agents/mcp_config.json` only in worktrees it created itself, and adds the
  file to that worktree's `.git/info/exclude`. In your own checkout it asks
  first. Tower never edits `~/.gemini/config`.

### OpenAI: Codex CLI

| Mode | ChatGPT login | Note id |
|---|---|---|
| Interactive `codex` in a pane, Initial/Human input | **Allowed** | |
| Pane with Tower-generated input, `codex exec`, `codex-acp` | Grey | `codex-chatgpt-automation` |

What the sources say:
- "API keys are still the recommended default for automation." —
  [Codex auth](https://learn.chatgpt.com/docs/auth).
- `codex exec` reuses saved CLI authentication by default.
- OpenAI's Terms of Use ban circumventing rate limits and sharing
  credentials. They say nothing specific about third-party launchers.
- OpenAI runs a "Sign in with ChatGPT" program for open-source apps. Tower
  could register under it, which would turn this grey zone into an approval.

### OpenCode

| Mode | Note id |
|---|---|
| Any mode on a provider login | Grey: `opencode-provider-login` |
| API keys | **Allowed** |

The provider's terms apply. OpenCode's own docs say: "There are plugins that
allow you to use your Claude Pro/Max models with OpenCode. Anthropic
explicitly prohibits this." — [OpenCode providers](https://opencode.ai/docs/providers/).
**Do not sign OpenCode in with a Claude plan.** Tower cannot see which
provider OpenCode uses, so its note says so.

The exception is an ACP role whose every model is local (`all_local`, e.g.
the built-in Local role): Tower then switches every other provider off
(`enabled_providers` in `OPENCODE_CONFIG_CONTENT`), so no provider login is
in use and the note does not apply.

### "Connect agents"

The Models view can add one MCP server named `tower` to the user-wide
Claude Code (`~/.claude.json` `mcpServers`) and Codex
(`~/.codex/config.toml` `[mcp_servers.tower]`) configs, on the user's
click. It adds a tool server, and for Claude Code two hooks (`tower guard`) that
only block prompts and deny file reads in local-only folders, to the
unmodified CLIs the user runs and signs in to themselves; it sets no login, key, base URL or bypass flag (the
`lint_settings` / `lint_args` rules are unchanged), and Tower never types
into those sessions.

### Anything else

An agent CLI Tower does not recognise gets the `unknown-vendor` note: read
its terms before letting Tower start it on a plan login.

An OpenAI-compatible HTTP endpoint (`driver = "llm"`) is plain API use and
always allowed.

## ACP (Agent Client Protocol)

- The agent owns sign-in. `initialize` returns `authMethods`, and the client
  only picks a method id. Tower never sees credentials, which is the clean
  design.
- `session/new { cwd, mcpServers }`: agents must support stdio MCP servers,
  which is how Tower hands its `tower` tools to any ACP agent.
- `session/prompt` is the protocol's own way to send a message. Follow-ups
  over ACP need no typing.
- `session/request_permission` becomes an Inbox item; `session/cancel`
  stops the agent.
- **The vendor still matters.** ACP is only the transport; the terms of the
  agent behind it still apply. `claude-agent-acp` is built on the Agent SDK,
  so the SDK note above applies to it.
  Tower shows that note in Setup before any flight (and the Board points at
  it); a launch before acceptance is refused, and triage over the same
  adapter waits too.

## Questions to put to the vendors

Tower's maintainers can ask these to move a grey zone to allowed:
- **Anthropic:** may an open-source local orchestrator launch the unmodified
  `claude` binary on a user's plan, including `-p` and Tower-generated
  follow-ups? Contact: <https://www.anthropic.com/contact-sales>.
- **Google:** the same question for `agy` and `agy -p`, asked on the
  Antigravity forum or through support.
- **OpenAI:** register Tower under "Sign in with ChatGPT" for open-source
  apps.
