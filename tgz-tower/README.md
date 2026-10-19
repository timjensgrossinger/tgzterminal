# Tower (experimental, untracked)

An agent team for TGZTerminal:
- give agents roles and launch them on flights (units of work);
- let them hand work to each other (Claude → a cheaper or local model and back);
- pull in Jira issues;
- decide in the Inbox what lands.

The UI is a GUI painted inside a terminal tab.
Plan: `~/.claude/plans/cosmic-toasting-tulip.md`.

## Run it

```sh
cargo build --manifest-path tgz-tower/Cargo.toml          # towerd (the `tower` binary)
TGZ_TOWER=1 cargo run -p wezterm-gui -- start --always-new-process
```

Tower is off twice over until it has been tested further. Release builds do
not compile it at all: only a build with `TGZ_TOWER=1` does. Even then nothing
opens until the Lua config switches it on:

```lua
config.tower = { enabled = true }
```

Open the Tower tab:
- with a key bound to `wezterm.action.EmitEvent 'tgz.tower.open'`, or
- from the sidebar's agent menu, using the "Agent insight" row.

The tab starts `towerd` on its own. After rebuilding towerd, run `pkill -f "tower daemon"` so the next tab starts the new one.

## How it works

| Piece | What it does |
|---|---|
| Roles | One TOML file each in `~/.config/wezterm/tower/roles/`. Tower reloads them on save; **Edit role** opens the file in `$EDITOR` |
| `driver = "pane"` | The real interactive `claude` opens in a split next to Tower: the first agent goes to the right, later ones stack below. It gets its role as an appended system prompt, the task as its first prompt, Claude Code hooks and the `tower` MCP server. Tower's own tools are pre-approved; everything else keeps Claude's normal permission prompts |
| `driver = "acp"` | `opencode acp`, Claude via `npx -y @agentclientprotocol/claude-agent-acp`, or any ACP agent runs as a child of towerd. Its chat streams into the agent drawer. Its requests to use Tower's own tools are allowed automatically; every other permission request lands in the Inbox. `acp_options = { mode = "acceptEdits" }` sets the agent's session options |
| `driver = "llm"` | One OpenAI-compatible chat call per message, no tools (`base_url`, optional `api_key_env`). Used for cheap chat agents and for Jira triage |
| Team tools (MCP) | `tower_flight`, `tower_roster`, `tower_handoff`, `tower_return`, `tower_post`, `tower_ask_human`, `tower_decide`, `tower_complete` |
| Messages | A message to a busy agent waits until its turn ends, then is typed or sent in. **Frequencies** has the Team channel plus one thread per open flight, which reaches that flight's agent |
| Where work happens | New flight and Launch ask **Where**. **New folder** (the default when Tower was opened from your home folder) creates `~/Tower/TWR-7-short-title` as a fresh git repo. **Here** is the folder Tower was opened from. **Git worktree** is a branch next to that repo. `[workspace]` in `tower.toml` sets `flights_root`. With `pretrust_claude = true` (the default), each new flight folder is marked trusted in `~/.claude.json`, so Claude starts there without the "trust this folder?" dialog. Only folders Tower created are trusted this way, and the file is backed up once to `~/.claude.json.tower-backup` |
| Auto assign | **Assign to → Auto** (the default in New flight when a triage role exists): the triage role reads the title and brief with no tools and picks the cheapest role that can do the whole task. The pick and its reason appear on the flight's timeline, then the agent launches. With no clear answer, the flight waits in Holding. `[triage] role` in `tower.toml` chooses the triage role (any `llm` or `acp` role). Unset, it's `ground-claude` (Claude Haiku over ACP, run as a one-shot plan-mode session with every tool request refused) or else the first `llm` role |
| Routing | Every flight gets a **tier** (low / medium / high) from a Rust port of [Threnody](https://github.com/timjensgrossinger/threnody)'s router: capped keyword signals, size (words, files), speed/quality words, reasoning words, and a **risk floor** (auth, secrets, tokens, injection, … are never below medium). Triage may move the tier, but never below the floor; **How hard** in New flight sets it outright. Roles with `driver = "routed"` (seeded: **Crew**) run on the first available engine of that tier from `[routing]` in `tower.toml`; when none is free, the next tier up is used, then a lower one, never below the floor. Every agent also gets the router's **effort** (`claude --effort`, ACP `effort` when the agent offers it) unless its role or engine sets `effort`; `agent = "<name>"` runs a subagent definition (`claude --agent`, ACP `agent`). Escalate on a routed flight moves it up a tier |
| Learned routing | A LinUCB contextual bandit over `tier:engine` arms, also ported from Threnody. `bandit = "shadow"` (default) records what it would pick (shown in the flight drawer) and learns from outcomes: landing is +1, minus a bit per failed gate run and per escalation and for bigger tiers; an escalated-away or deleted flight is −1. `"live"` follows it only once every candidate arm has `bandit_min_updates` outcomes. **Models** lists each engine, why it can't run (`ollama pull …`, not installed, key unset) and its outcome count |
| Handoffs | A handoff creates a child flight (`TWR-3.1`). Roles with `auto_release` take it immediately; otherwise you release or reject it in the Inbox |
| Gates | `[gates]` in `tower.toml`, or a repo's `.tower.toml`: commands that run when an agent completes a flight, then you land it. A failure sends the output back to the agent, and Escalate moves the flight up its role's ladder |
| Usage | Tokens and cost per agent and flight: from ACP turn results and cost updates, Claude transcripts (read incrementally on each `Stop`), and LLM responses |
| Intake | `[[jira]]` watches in `sources.toml` poll a JQL query through the Atlassian Rovo MCP (`https://mcp.atlassian.com/v2/mcp`). New issues become flights marked as untrusted input; an optional `triage_role` (an `llm` role, no tools) picks a role. When a Jira flight lands, the Inbox offers to comment on the issue and apply `done_transition` |
| Auto-fix | `autofix = true` on a `[[jira]]`, `[[github]]` or `[[gitlab]]` watch (GitHub/GitLab use the `gh`/`glab` CLIs and your login there). For each new issue Tower:<br>1. **Finds the project**, checking in order: `[[project]]` rules in `tower.toml` (Jira keys, repos, keywords), an answer you gave before for the same Jira project or repo, a clone whose git remote is the issue's repo, then a single name/keyword match among the git repos under `[projects] roots`. If several fit, triage picks; if it isn't confident, the Inbox asks "Which project is … in?" and remembers your answer.<br>2. **Fixes it** on a `tower/<key>` branch in a git worktree next to the repo (Tower-made, so pre-trusted for Claude), with the watch's role or Auto triage, and an instruction to reproduce, fix and verify.<br>3. **Runs gates**: the repo's own tests when none are configured (`cargo test`, `npm`/`pnpm`/`yarn`/`bun test`, `go test ./...`, `pytest`, `make test`).<br>4. **Lets you land it**: with `[autofix] land = "pr"` (default) Tower commits and asks before pushing and opening a draft PR (`gh`) or MR (`glab`); with `"branch"` it only commits locally. It then offers to comment on the issue, with the PR link.<br>An agent's questions arrive in the Inbox with **Post on issue**, which asks the reporter; paste their reply into the same item |
| Decisions | `tower_decide(question, options, recommendation, context)`: any agent puts a decision in the Inbox (design, scope, trade-offs) with one button per option (the recommended one highlighted), a text field for your own answer or a note, and **Post on issue** for flights from an issue. The choice arrives as a `[Tower]` message; agents are told not to make such decisions silently |
| Kinds of work | Auto-fix issues are classified `code`, `ops` (accounts, access, servers, installs), `design` (concepts, new systems) or `question`: a keyword guess, then project triage. Code gets a worktree of its project; the others get their own folder under the flights root and matching instructions: ops does what it may and otherwise writes the exact steps for a human; design drafts `DESIGN.md` and, where it helps, an HTML prototype; questions get an answer with sources |
| Outcomes and artifacts | `tower_complete(summary, outcome, artifacts)`: `resolved` (gates run), `proposed` (no access, not allowed: the summary is the solution for a human) or `declined` (with the reason). Proposals and refusals skip test gates; you still land them. `artifacts` (files inside the working directory) show in the flight drawer with **Open** and go into the issue comment (short Markdown inline) |
| Off-limits hosts | `[access] deny_hosts` in `tower.toml` (`payroll.internal`, `*.prod.example.com`): Claude agents get permission deny rules (`Bash(*host*)`, `WebFetch(domain:host)`), ACP tool calls naming them are refused without asking, and every agent is told to propose the steps instead |
| Following issues | For open flights from Jira/GitHub/GitLab (watch `follow = true`, the default) Tower fetches comments, title, description and status each poll. Its own comments (signed "— posted by Tower") and comments older than the flight are skipped; other field changes (invoicing, labels, …) are never fetched. The triage role filters what is relevant to the work; relevant updates go to the live agent as untrusted `[Tower]` messages, or to the Inbox with **Resume with this**. A closed issue asks whether to stop its agents |
| Models | `models.toml` lists model servers to check. A missing local model shows `ollama pull …` |

**Jira setup:**
1. Run `tower auth jira yourteam.atlassian.net` and paste an Atlassian API token. On macOS it's stored in the Keychain; elsewhere in a 0600 file.
2. Add a `[[jira]]` watch to `sources.toml`; the seeded file has a commented example.
3. Check it in the **Intake** view and use **Poll now**.

**Claude over ACP vs. in a pane:**
- **ACP** (roles **Approach Chat** and **Ground Claude**): Claude runs through Anthropic's Agent SDK, using your Claude login, and you chat with it in Tower's drawer.
  - Allowed with a subscription. Anthropic's [support article](https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan) says Agent SDK and third-party usage draw from your plan; since Oct 7, 2026, Max and Team plans also get monthly credits for it.
  - With `ANTHROPIC_API_KEY` set, it bills the API instead.
  - The cost shown for these agents is the API-equivalent figure the adapter reports.
- **Pane** (role **Approach**): the official interactive `claude` CLI next to Tower. This is the safest way to stay on plain subscription limits, and it's what Zed recommends.

**Seeded roles (first run only; Approach Chat and Ground Claude were also added to existing setups):**
- **Approach:** Claude.
- **Ground:** OpenCode with `opencode-go/kimi-k2.7-code`. The opencode-go key must be valid.
- **Ground Local:** OpenCode with Ollama `qwen3.6:27b`.
- **Review:** OpenCode with `opencode-go/glm-5.3`.
- **Triage:** `llm` with Ollama `qwen3.6:27b`.
- **Approach Chat:** Claude over ACP (`sonnet` → `opus`, `acceptEdits`).
- **Ground Claude:** Claude Haiku over ACP; takes handoffs automatically.
- **Crew:** routed: Tower picks the engine by tier. Seeded engines: low `qwen-local` (OpenCode + Ollama) then `claude-haiku` (ACP); medium `claude-sonnet` (pane) then `opencode-kimi`; high `claude-opus` (pane).

`max_concurrency` counts busy agents only. An agent still open on a landed flight doesn't block new launches.

**Keys in the Tower tab:**
- **Navigation:** `1`–`6` and `Tab` switch views, `n` opens New flight, `Esc` closes a sheet or drawer, `r` refreshes.
- **In text fields:** `Enter` submits a one-line field and adds a line in a multi-line one; `Ctrl+Enter` submits; `Tab` moves to the next field. IME composition is shown at the caret.

**Trust prompts:** in **New folder** flights, Claude starts without asking (see `pretrust_claude`). In **Here** or a worktree, Claude may ask whether to trust the folder the first time. Tower doesn't answer that for you: after 20 s the Inbox shows "… is waiting in its pane" with a **Focus pane** button.

## Security model

- **Access to the daemon:**
  - The local socket is 0600, and on Windows the named pipe rejects remote clients.
  - The master token (0600 file) gives full access and is only for the Tower view and the CLI.
  - Each agent gets its own token, passed through its environment. It only allows that agent's hook events and tool calls, so an agent can't release its own handoffs or approve its own landing through Tower.
  - Agents with shell access run as your user and could still read the master token file. Real isolation would need separate OS users.
- **Untrusted text:** issue and handoff text is task data. Triage has no tools, and Jira write-back always needs your clearance.
- **No bypass flags:** Tower never passes skip/bypass-permission flags.

## Debugging

- **Daemon:** `tower status` shows a summary, role warnings and intake errors.
- **Daemon log:** `~/Library/Application Support/wezterm/tower/towerd.log`. Start the daemon with `TOWER_DEBUG_ACP=1` to trace ACP methods (names and ids only).
- **State:** `state.json` in the same folder. Each agent has generated files and a `stderr.log` in `agents/<id>/`.
- **GUI log:** `WEZTERM_LOG=wezterm_gui::termwindow::tgz_tower=debug`.
- **Demo data:** run the daemon with `tower daemon --demo`.
- **Socket path length:** must stay under 104 characters on macOS.

## Platforms

- **macOS:** what this is developed and tested on.
- **Linux:** same code path as macOS (Unix socket), not exercised.
- **Windows:**
  - The daemon (named pipe `\\.\pipe\tgz-towerd-<user>`, shims, gates via `cmd /C`, SChannel TLS) compiles for `x86_64-pc-windows-msvc` but has never run.
  - The GUI's Windows branch (pipe client, `CREATE_NO_WINDOW` daemon start, Notepad for **Edit role**) hasn't even been compiled, because wezterm-gui needs C cross toolchains.
  - Claude panes on Windows don't show the role name in the sidebar (no POSIX shell for the shim).

## Tests

```sh
cargo test --manifest-path tgz-tower/Cargo.toml           # proto, core, towerd (engine flows, Jira/MCP/LLM against fakes)
TGZ_TOWER=1 cargo test -p wezterm-gui tgz_tower           # GUI: forms, model, layout, clipping
cargo check --manifest-path tgz-tower/Cargo.toml --target x86_64-pc-windows-msvc -p towerd
cargo +nightly fmt --all -- --check                       # repo fmt must stay clean
rustfmt +nightly --edition 2018 tgz-tower/gui/mod.rs      # format the GUI module
```

## Layout

| Path | Contents |
|---|---|
| `proto/` | Wire types and demo data. Also `#[path]`-included by `gui/`, so it must compile under edition 2018 |
| `core/` | Pure rules: handoff lifecycle, gates → landed, ladder, mentions, tool summaries; `router.rs` (tier, floor, effort), `bandit.rs` (LinUCB), `work.rs` (work kinds, outcomes, host deny matching, timestamps) |
| `towerd/` | See below |
| `gui/` | Compiled into wezterm-gui under `cfg(tgz_tower)`. Pure `forms`/`model`/`layout`/`text`/`clip`/`theme`; `glue.rs` is the only file that touches `TermWindow`; `client.rs` runs the daemon's pane commands through the mux |

`towerd/` files:
- `engine.rs`: one task owns all state.
- `server.rs`: endpoint and auth.
- `transport.rs`: the blocking client.
- `drivers/{pane,acp,llm}.rs`: the three agent drivers.
- `shim.rs`: `tower hook` and `tower mcp`.
- `jira.rs` and `mcp_http.rs`: Jira intake; `issues.rs`: the common issue/watch types and the GitHub/GitLab CLIs.
- `intake.rs` (a child module of `engine`): intake, kinds, project finding, worktrees, publishing; `followup.rs` (also a child module): decisions, artifacts, following issue activity; `projects.rs`: repo index, matching, gate detection.
- `usage.rs`: Claude transcripts.
- `routing.rs`: engine availability, picking, effort/agent flags per driver.
- `config.rs`, `gates.rs`, `store.rs`.

The tracked seam is about 39 lines, all cfg-gated:
- `wezterm-gui/build.rs`: `experimental_tab_views`
- `termwindow/mod.rs`: the `mod` declaration and the `EmitEvent` hook
- `render/pane.rs`: the first statement of `paint_pane`
- `mouseevent.rs`: the "Agent insight" row
