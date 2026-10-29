# Tower (experimental)

An agent team for TGZTerminal:
- give agents roles and launch them on flights (units of work);
- let them hand work to each other (Claude → a cheaper or local model and back);
- pull in Jira, GitHub and GitLab issues;
- decide on the Board what lands, and in **Setup** what Tower may do on its own.

The UI is a GUI painted inside a terminal tab, with three views: **Board** (flights by
stage, with what is waiting on you as a strip at the top), **Setup** (roles, models,
routing, policy, sources, schedules, extras) and **History** (what ran by itself). A
fourth, **Team**, appears when the frequencies extra is on. `tower flight …` and
`tower inbox …` do the same from a shell, with no tab open.
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
- from the sidebar's agent menu (right-click the agent button), using the "Tower" row. The row only appears when Tower is compiled in and enabled.

The tab starts `towerd` on its own. After rebuilding towerd, run `pkill -f "tower daemon"` so the next tab starts the new one.

## How it works

| Piece | What it does |
|---|---|
| Roles | One TOML file each in `~/.config/wezterm/tower/roles/`. Tower reloads them on save; **Edit role** opens the file in `$EDITOR`. A first run seeds roles only for programs that are installed (see the table under Seeded roles): the whole-task roles (Claude chat over ACP, Claude in a pane, Auto model), the subtask helpers, Code review, Triage, and one pane role per other CLI found. Nothing is seeded for a program that is not there; `.seeded` remembers what was added, so a deleted role stays deleted |
| `driver = "pane"` | The real interactive CLI opens in a split next to Tower: the first agent goes to the right, later ones stack below. **Claude Code** gets its role as an appended system prompt, the task as its first prompt, Claude Code hooks and the `tower` MCP server; Tower's own tools are pre-approved and everything else keeps Claude's normal permission prompts. **Codex** gets `-c mcp_servers.tower.*`, `-c developer_instructions` and `-c notify` (turn ends reach Tower through `tower notify`). **OpenCode** gets Tower's server through `OPENCODE_CONFIG_CONTENT` (your `opencode.json` is untouched). **Antigravity** (`agy`) reads servers only from `.agents/mcp_config.json`, which Tower writes only in folders it made, git-excluded, never over a file it did not write. **Gemini CLI** gets no tools in a pane (use `gemini --experimental-acp`). Any other CLI gets its brief typed in. When the daemon starts, it adds a role for each of these CLIs that is installed, once each (`.seeded` remembers, so a deleted role stays deleted). CLIs without a turn-end signal never look idle, so messages to them are typed in right away |
| Role MCP servers | `[mcp.<name>]` in a role file (`command`, `args`, `env`) hands a server to that role's agents, in each CLI's own way. Secrets in `env` go into the agent's environment or a 0600 file, never onto a command line |
| `driver = "acp"` | `opencode acp`, Claude via `npx -y @agentclientprotocol/claude-agent-acp`, `codex-acp`, `gemini --experimental-acp`, or any ACP agent runs as a child of towerd. Its chat streams into the agent drawer. Its requests to use Tower's own tools are allowed automatically; every other permission request lands in the Inbox. `acp_options = { mode = "acceptEdits" }` sets the agent's session options. With `auth = "api_key"`, Tower picks the agent's API-key sign-in from its `authMethods`, and refuses to start it if it has none. Otherwise Tower triggers no sign-in at all |
| `driver = "llm"` | One OpenAI-compatible chat call per message, no tools (`base_url`, optional `api_key_env`). Used for cheap chat agents and for Jira triage |
| `driver = "swarm"` | No agent of its own: Tower splits the flight's task and runs the parts on `worker` (default: the routed role), at most `max_agents` at once. See Fan-out |
| Team tools (MCP) | `tower_flight`, `tower_roster`, `tower_handoff`, `tower_fanout`, `tower_return`, `tower_post`, `tower_ask_human`, `tower_decide`, `tower_complete` |
| Fan-out | One task split into parts that run at the same time as child flights, in waves. An agent calls **`tower_fanout`** with its parts (a brief each, the files each owns, which parts wait for which), or you give the flight to **Split & run · Auto** (`swarm`) and Tower splits it itself: the triage model writes the plan with no tools, and when it cannot run (or its plan does not hold up) Tower makes one part per file the task names; a task that does not split goes to the worker role whole. Plans are checked before anything starts (no cycles, no unknown parts, no paths outside the folder, roles that can run, local-only work on local roles), and two parts that write the same file go into different waves. Starting one follows **`handoff.fanout`**, asked once for the whole plan (all-local parts follow `handoff.local`). A wave starts when the one before it is over; at most `[fanout] max_agents` parts run at once (the rest queue, as do parts whose role is at `max_concurrency`). A part that fails (its agent exits, it never calls `tower_return`, it runs past `child_timeout_mins`, its flight is deleted) ends its wave with what `[fanout] on_failure` says: `auto` goes on for an agent's fan-out (what needs the failed part is skipped) and asks you for Tower's own (**Retry them**, **Go on without them**, **Stop**). At the end the asker gets **one** combined result as a `[Tower]` message (`tower_await` with the `x-N` id works too); for a swarm flight the flight's gates run on the combined result instead, and landing follows `land`. The flight drawer shows each part's state and links to its child flight. Parts share the flight's folder: the packet tells each agent to stay inside its own files, and the gates are the backstop. Pane-role parts need the Tower tab open, like any pane agent |
| Messages | A message to a busy agent waits until its turn ends. For a **Claude pane**, it is handed back through the `Stop` hook (`decision: "block"`), so Claude keeps working on it and nothing is typed into the pane. If Claude is already continuing because of Tower's hook, Tower does not block again: no loops, and the message is typed in once the agent is idle. Issue updates reach a busy Claude pane after its next tool call (`PostToolUse` `additionalContext`), without ending its turn. ACP agents get messages as `session/prompt`. **Team** (an extra, off by default) has the Team channel plus one thread per open flight, which reaches that flight's agent |
| Scouts | An extra (`[extras] scouts`, off by default). `[[scout]]` in `sources.toml`: a role, of any vendor, reads mail or Teams with its own M365 tools (Tower has no Microsoft login) and reports each message that needs work with `tower_intake_submit`. Only scout agents may call that tool, and ids already seen are skipped. Each item becomes a flight in a folder of its own, marked untrusted, with a reply clock for customers (24 h for mail by default; customer means a sender outside `internal_domains`). The agent working it writes its proposed reply to `reply.md`, to copy and send yourself: Tower cannot send. **Run scout** in the Intake view starts a run. Runs on a timer happen only with `scout.run = "auto"` while armed; `confirm` asks once per interval, and the default is off |
| Where work happens | New flight and Launch ask **Where**. **New folder** (the default when Tower was opened from your home folder) creates `~/Tower/TWR-7-short-title` as a fresh git repo. **Here** is the folder Tower was opened from. **Git worktree** is a branch next to that repo. `[workspace]` in `tower.toml` sets `flights_root`. With `pretrust_claude = true` (the default), each new flight folder is marked trusted in `~/.claude.json`, so Claude starts there without the "trust this folder?" dialog. Only folders Tower created are trusted this way, and the file is backed up once to `~/.claude.json.tower-backup` |
| Auto assign | **Assign to → Auto** (the default in New flight when a triage role exists): the triage role reads the title and brief with no tools and picks the cheapest role that can do the whole task. The pick and its reason appear on the flight's timeline, then the agent launches. Triage is only offered roles that can run right now (program installed, no warning, terms accepted, and inside your network for local-only work). With no clear answer, a failed call, or an answer naming a role that cannot run, the flight goes to the **fallback**: `[triage] fallback_role` if it can run, else the routed role (**Whole task · Auto model**, which picks its model by the router's tier), else the only role that can run; with none of those it waits in Holding. When no triage role can run at all, Auto uses that same fallback straight away. **Heuristics only** (a toggle under Auto, or `--heuristic`) skips the triage call and uses the fallback by the router's tier, with no model call. `[triage] role` in `tower.toml` chooses the triage role (any `llm` or `acp` role). Unset, it's `task-claude-chat` (Claude over ACP, run as a one-shot plan-mode session with every tool request refused), else `sub-claude`, else the first `llm` role |
| Routing | Every flight gets a **tier** (low / medium / high) from a Rust port of [Threnody](https://github.com/timjensgrossinger/threnody)'s router: capped keyword signals, size (words, files), speed/quality words, reasoning words, and a **risk floor** (auth, secrets, tokens, injection, … are never below medium). Triage may move the tier, but never below the floor; **How hard** in New flight sets it outright. Roles with `driver = "routed"` (seeded: **Whole task · Auto model**, id `auto`) run on the first available engine of that tier from `[routing]` in `tower.toml`; when none is free, the next tier up is used, then a lower one, never below the floor. Every agent also gets the router's **effort** (`claude --effort`, ACP `effort` when the agent offers it) unless its role or engine sets `effort`; `agent = "<name>"` runs a subagent definition (`claude --agent`, ACP `agent`). Escalate on a routed flight moves it up a tier |
| Learned routing | A LinUCB contextual bandit over `tier:engine` arms, also ported from Threnody. `bandit = "shadow"` (default) records what it would pick (shown in the flight drawer) and learns from outcomes: landing is +1, minus a bit per failed gate run and per escalation and for bigger tiers; an escalated-away or deleted flight is −1. `"live"` follows it only once every candidate arm has `bandit_min_updates` outcomes. **Models** lists each engine, why it can't run (`ollama pull …`, not installed, key unset) and its outcome count |
| Handoffs | A handoff creates a child flight (`TWR-3.1`). `handoff.release` in `[policy]` decides: by default you release or reject it in the Inbox; at `auto` it starts while Tower is armed |
| Gates | `[gates]` in `tower.toml`, or a repo's `.tower.toml`: commands that run when an agent completes a flight, then you land it. A failure sends the output back to the agent, and Escalate moves the flight up its role's ladder |
| Usage | Tokens and cost per agent and flight: from ACP turn results and cost updates, Claude transcripts (read incrementally on each `Stop`), and LLM responses |
| Intake | `[[jira]]` watches in `sources.toml` poll a JQL query through the Atlassian Rovo MCP (`https://mcp.atlassian.com/v2/mcp`). New issues become flights marked as untrusted input; an optional `triage_role` (an `llm` role, no tools) picks a role. Flights carry the tracker's reporter, assignee, priority and labels. GitHub/GitLab watches can filter by `assignee` (`@me`) and labels. When a flight lands, Tower offers to comment on the issue and apply `done_transition` (a transition id, or its name) |
| Response clocks | Jira Service Management SLAs are read automatically. Tower finds them by shape, so field ids and languages don't matter: the first poll reads every field once, later polls only the SLA fields. Other trackers can use `respond_within_hours`, a clock Tower keeps itself. It runs from when the issue was opened, or from when Tower first saw it if that is later, and a reply from anyone but the reporter stops it. Cards show the most urgent countdown and the drawer lists every clock. A **Deadline** item appears at half and 80% of the time, and at breach |
| Acknowledgements | One "we have it" comment per running response-clock cycle, so a new ticket and each new customer comment both get one. `ack.post` decides: a draft on the flight's timeline (default), **Post / Skip** in the Inbox, or posted while armed. Visibility is always explicit (`[ack] visibility` or the watch's `ack_visibility`). In Jira Service Management **public means the customer sees it and gets an email**. `internal` restricts it to `internal_role` and exists only in Jira. Tower never retries a comment whose result it could not confirm, and it skips its own comments by id |
| Auto-fix | `autofix = true` on a `[[jira]]`, `[[github]]` or `[[gitlab]]` watch (GitHub/GitLab use the `gh`/`glab` CLIs and your login there). For each new issue Tower:<br>1. **Finds the project**, checking in order: `[[project]]` rules in `tower.toml` (Jira keys, repos, keywords), an answer you gave before for the same Jira project or repo, a clone whose git remote is the issue's repo, then a single name/keyword match among the git repos under `[projects] roots`. If several fit, triage picks; if it isn't confident, the Inbox asks "Which project is … in?" and remembers your answer.<br>2. **Fixes it** on a `tower/<key>` branch in a git worktree next to the repo (Tower-made, so pre-trusted for Claude), with the watch's role or Auto triage, and an instruction to reproduce, fix and verify.<br>3. **Runs gates**: the repo's own tests when none are configured (`cargo test`, `npm`/`pnpm`/`yarn`/`bun test`, `go test ./...`, `pytest`, `make test`).<br>4. **Lets you land it**: with `[autofix] land = "pr"` (default) Tower commits and asks before pushing and opening a draft PR (`gh`) or MR (`glab`); with `"branch"` it only commits locally. It then offers to comment on the issue, with the PR link.<br>An agent's questions arrive in the Inbox with **Post on issue**, which asks the reporter; paste their reply into the same item |
| Decisions | `tower_decide(question, options, recommendation, context)`: any agent puts a decision in the Inbox (design, scope, trade-offs) with one button per option (the recommended one highlighted), a text field for your own answer or a note, and **Post on issue** for flights from an issue. The choice arrives as a `[Tower]` message; agents are told not to make such decisions silently |
| Kinds of work | Auto-fix issues are classified `code`, `ops` (accounts, access, servers, installs), `design` (concepts, new systems) or `question`: a keyword guess, then project triage. Code gets a worktree of its project; the others get their own folder under the flights root and matching instructions: ops does what it may and otherwise writes the exact steps for a human; design drafts `DESIGN.md` and, where it helps, an HTML prototype; questions get an answer with sources |
| Outcomes and artifacts | `tower_complete(summary, outcome, artifacts)`: `resolved` (gates run), `proposed` (no access, not allowed: the summary is the solution for a human) or `declined` (with the reason). Proposals and refusals skip test gates; you still land them. `artifacts` (files inside the working directory) show in the flight drawer with **Open** and go into the issue comment (short Markdown inline) |
| Off-limits hosts | `[access] deny_hosts` in `tower.toml` (`payroll.internal`, `*.prod.example.com`): Claude agents get permission deny rules (`Bash(*host*)`, `WebFetch(domain:host)`), ACP tool calls naming them are refused without asking, and every agent is told to propose the steps instead |
| Following issues | For open flights from Jira/GitHub/GitLab (watch `follow = true`, the default) Tower fetches comments, title, description and status each poll. Its own comments (by id, and anything signed "— posted by Tower") and comments older than the flight are skipped; other field changes (invoicing, labels, …) are never fetched. The triage role filters what is relevant to the work; relevant updates go to the live agent as untrusted `[Tower]` messages, or to the Inbox with **Resume with this**. A closed issue asks whether to stop its agents |
| Models | `models.toml` lists model servers to check. A missing local model shows `ollama pull …`. The localhost Ollama entry is seeded live only when `ollama` is installed; a server that is not running reads as a fact, not a warning |
| Policy | What Tower may do without a click, per action: `[policy.actions]` in `tower.toml`, overridable per watch with `policy = { … }`. See **Policy and Armed** below |
| Schedules | Setup → **Schedules** → **New schedule** (key `s`): start a flight at a time. When is a cron line in local time (`30 8 * * 1-5`, `@daily`) or an interval (`every 2h`); the form shows what it means and the next run. A role or **Auto**, a tier, and where it runs: a new folder each run, a fixed folder, or a fresh worktree of a repo. Title and task may use `{date}`, `{weekday}` and `{since}` (the last run that landed). Templates: morning PR review, nightly dependency audit, weekly stale-branch report, daily digest. Stored in `schedules.toml` (written by the form; hand edits are picked up). A run is skipped while the previous run's flight is still open (unless `overlap = "run"`); a run missed while towerd was down is skipped unless `catch_up = true`. After `pause_after_failures` failed runs in a row (default 3: declined, a failed gate, a launch that could not start, deleted before landing) Tower pauses the schedule and asks in the Inbox. Each card shows the next run and a dot per recent run; a dot opens its flight. **Run now** fires it at once. `tower routines` prints the same from the command line |
| Other routines | An extra (`[extras] machine_routines`, off by default): Setup lists, read-only, your launchd agents (`~/Library/LaunchAgents`, through `plutil` and `launchctl list`: schedule, next run, last exit status, when its log last changed, **looks stalled** for an interval job silent for 3 intervals) and your crontab lines. Vendor jobs in `~/Library` (updaters) are folded away. **Open file** and **Open log** only open them; Tower never loads, unloads, edits or runs these, and never reads a plist's `EnvironmentVariables` |
| Activity | **History** view (key `3`) always lists what ran by itself (from `audit.jsonl`), each entry opening its flight. With the `activity` extra on, 7/30/90 days of tiles (ran by itself, scheduled runs, landed, asked you, spent, gate failures), flights started per day by origin (you, schedule, intake, auto-fix, scout, handoff), actions Tower ran without a click per day, how runs ended, spend per day, and the latest automatic actions (each opens its flight). Counters are per local day in `state.json` (90 days); on first start they are rebuilt from the flights and `audit.jsonl` |
| Vendor terms | A role or routing engine in a vendor-terms grey zone (for example Claude over the Agent SDK on a plan login, or Antigravity on a Google login) puts the vendor's text in Setup up front, and the Board points at it, so you accept it once before the first flight. A launch before that is refused with an Inbox item; triage over the same program waits too. Details: [`docs/VENDOR_TERMS.md`](docs/VENDOR_TERMS.md) |
| Extras | `[extras]` in `tower.toml`, also toggled in Setup: `scouts`, `machine_routines`, `activity`, `frequencies`, all off by default. Off means the daemon stops doing that work (no scout runs, no launchd scans, `tower_post` and `tower_intake_submit` refused, no `scout.run` policy row) and the view hides the surfaces; the code stays |
| Command line | `tower flight new --title <t> [--brief <b> \| --brief-file <f> \| --brief -] [--role <id> \| --heuristic \| --fanout] [--tier low\|medium\|high] [--here \| --new-folder \| --worktree] [--local-only] [--hands-free] [--no-launch] [--json]`, `tower flight list [--all]`, `tower flight show <key\|id>` (with a line per fan-out part), `tower inbox list`, `tower inbox answer <id> approve\|deny\|<option-id> [--text …]`. The same messages the view sends; ACP and llm roles start with no Tower tab open, a pane role still needs one. Exit 1: the daemon said no; 2: usage; 3: no daemon |

## Policy and Armed

Nothing happens by itself unless you choose both **what** may happen and
**when**:

- **What:** each action has a level: click it in the Policy view, or set it
  in `tower.toml`:
  - `off`: never.
  - `draft`: prepared and shown; you do it yourself.
  - `confirm`: one click in the Inbox.
  - `auto`: runs on its own.
- **When:** `auto` only runs while Tower is **Armed**. Arm it in Setup
  (key `2`). It turns itself off after `armed_ttl_secs` (default 8 h,
  at most 24 h), and a daemon restart disarms it too. While disarmed, every
  `auto` action asks like `confirm`.
- **How much:** at most `max_auto_per_hour` automatic runs per action
  (default 20). Above that, Tower asks.
- **Schedules are the exception:** `schedule.run` defaults to `auto` and
  does not need Armed, because saving or resuming a schedule was your OK
  for it to run on time. Set it to `confirm` to be asked at every run
  (one Inbox item per schedule), or `off` to stop all schedules; a
  schedule's own `policy = "confirm"` overrides it. Only the master token
  (the Tower view, the CLI) can create schedules; an agent cannot. What a
  scheduled flight's agent then wants to write outside Tower (comments,
  transitions, PRs, landing, messages) follows its own actions above.
- **Local handoffs are the other exception:** `handoff.local` defaults to
  `auto` and does not need Armed either. It covers handing a subtask to a
  role that only runs models inside your network (`tower_delegate`, or a
  handoff to such a role) and giving the result back to the agent that
  asked. Nothing leaves your network, and the answer only goes to the
  agent that asked for it. It is still counted against `max_auto_per_hour`
  and written to `audit.jsonl`. Handoffs to cloud roles keep
  `handoff.release`.

```toml
[policy]
armed_ttl_secs = 28800
max_auto_per_hour = 20

[policy.actions]
"intake.launch" = "confirm"     # start an agent on a new issue
"intake.triage" = "confirm"     # ask the triage role which role an issue needs
"ack.post" = "draft"            # the "received" note on a new issue
"issue.comment" = "confirm"     # comment when a flight lands
"issue.transition" = "confirm"  # move the issue to done_transition
"pr.draft" = "confirm"          # push and open a draft PR after an auto-fix
"land" = "confirm"              # land after gates pass
"handoff.release" = "confirm"   # start the next role on a handoff
"handoff.fanout" = "confirm"    # split a task into parallel parts (once per plan)
"handoff.local" = "auto"        # hand a subtask to a local model (needs no Armed)
"scout.run" = "off"             # mail/Teams scouts
"pane.message" = "confirm"      # Tower-written messages into a running agent
"schedule.run" = "auto"         # a schedule starts its flight (needs no Armed)
```

Defaults: `confirm` everywhere, except `ack.post = "draft"`,
`scout.run = "off"`, `schedule.run = "auto"` and `handoff.local = "auto"`. A watch can override any of these, e.g.
`policy = { "intake.launch" = "auto" }`.

There is no action for sending mail or Teams messages.

`pane.message` covers everything Tower or another agent writes into a
running agent: handoff results, gate-failure output, posts and @mentions
from other agents, and issue updates. On `confirm`, these wait in the Inbox
as **Send / Drop**, merged into one item per agent. What you type yourself
(Frequencies, answers, decisions, Send back) always goes straight through.

Filtering issue updates for relevance is a model run, so it follows
`intake.triage`. Until that runs on its own, every update comes to you.

What still runs with no click: polling and reading your watches, scanning
local repos for projects, probing model servers, and running a flight's gate
commands after its agent calls `tower_complete`. None of these write
anywhere outside Tower, and none of them start an agent.

**Hands-free flights** (a toggle in New flight and on a schedule, or `--hands-free`): while
Tower is armed, what would ask on that flight runs instead: releasing a
handoff (`handoff.release`), starting a fan-out (`handoff.fanout`), and
passing Tower's text on to its agents (`pane.message`), for the flight and
every child it hands down. Only `confirm` is raised to `auto`: an action you
set to `off` or `draft` stays that way, and **landing still follows `land`
and `[gates] human`**. A failed gate goes straight to the flight's agent to
fix, and only reaches the Inbox once the failures reach
`[gates] failures_per_rung`. Disarmed, a hands-free flight asks like any
other, and `max_auto_per_hour` still applies. Agent questions, permission
requests and vendor-terms notes always reach you. The agent's brief tells it
to decide reasonable details itself; audit lines for raised actions carry
`"via": "hands_free"`.

Every decision and every external write is logged to `audit.jsonl` in the
data folder: one JSON line each, file mode 0600, rotated at 5 MB.

**Old knobs:** these still work, but now only while armed, and Tower logs a
note when it sees them:
- `auto_launch` and `autofix` on a watch mean `intake.launch` and
  `intake.triage` = `auto`.
- `[gates] human = false` means `land = "auto"`.
- `auto_release` on a role means `handoff.release = "auto"`.

**Jira setup:**
1. Run `tower auth jira yourteam.atlassian.net` and paste an Atlassian API token. On macOS it's stored in the Keychain; elsewhere in a 0600 file.
2. Add a `[[jira]]` watch to `sources.toml`; the seeded file has a commented example.
3. Check it under Setup → **Sources** and use **Poll now**.

**Claude over ACP vs. in a pane:**
- **Pane** (role **Claude (your session)**): the official interactive `claude` CLI next to Tower, signed in with your own plan. Anthropic explicitly allows this, so it needs no terms note. It needs the Tower tab open.
- **ACP** (role **Claude**, the default, and the seeded routing engines): Claude runs through Anthropic's Agent SDK (`claude-agent-acp`), you chat with it in Tower's drawer, and `tower flight new` can start it with no tab open.
  - On a plan login this is a grey zone. Anthropic's [support article](https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan) says Agent SDK and third-party usage draw from your plan (since Oct 7, 2026, Max and Team plans also get monthly credits for it). The [Agent SDK overview](https://code.claude.com/docs/en/agent-sdk/overview) asks SDK-based products to use API keys. Setup shows this up front and asks you to accept it once; see [`docs/VENDOR_TERMS.md`](docs/VENDOR_TERMS.md).
  - With `ANTHROPIC_API_KEY` set and `auth = "api_key"` in the role, it bills the API and needs no note.
  - The cost shown for these agents is the API-equivalent figure the adapter reports.

**Seeded roles (only for programs that are installed; added once each).** A role's name says what it does and who runs it; the card shows its `summary` (for you) and Edit role shows its `mission` (what the agent is told):

| Role (id) | Needs | What it does |
|---|---|---|
| **Whole task · Claude chat** (`task-claude-chat`) | `claude` | Owns a task end to end; Claude talks in Tower's drawer over ACP, `sonnet` → `opus`, `acceptEdits`. The default triage role. |
| **Whole task · Claude** (`task-claude`) | `claude` | Owns a task end to end in a `claude` pane you can watch and type into, `sonnet` → `opus`. |
| **Whole task · Auto model** (`auto`) | `claude` | Owns a task end to end; Tower picks the engine by tier (seeded engines: low `claude-haiku`, medium `claude-sonnet`, high `claude-opus`; `qwen-local` and `opencode-kimi` are in the file, commented out). The default for Auto flights. |
| **Split & run · Auto** (`swarm`) | `claude` | Tower splits the task into parts and runs them in parallel on the Auto model role (`worker = "…"` picks another), then checks the combined result. See Fan-out. |
| **Subtask · Claude Haiku** (`sub-claude`) | `claude` | Runs one handed-off subtask with Claude Haiku over ACP, then returns the result. |
| **Subtask · OpenCode** (`sub-opencode`) | `opencode` | Runs one handed-off subtask with OpenCode (`opencode-go/kimi-k2.7-code` → `glm-5.3`; needs a valid opencode-go key), then returns the result. |
| **Code review · OpenCode** (`review`) | `opencode` | Reviews a change against its acceptance criteria. Never edits. |
| **Subtask · Local model** (`sub-local`) | `ollama` | The subtask helper on Ollama `qwen3.6:27b`; nothing leaves your network. |
| **Triage · Local LLM** (`triage`) | `ollama` | A tool-less `llm` role that reads a task and picks who takes it. |
| **Whole task · Codex / OpenCode / Antigravity / Gemini** (`task-codex` …) | that CLI | One pane role per other CLI found. |

Earlier setups named these roles after airport jobs (Approach, Ground, Crew). On its first start a new towerd moves those files to the ids above once (`approach` → `task-claude`, `approach-chat`/`claude` → `task-claude-chat`, `crew` → `auto`, `ground` → `sub-opencode`, `ground-claude` → `sub-claude`, `ground-local` → `sub-local`), refreshes only names and missions that were still the seeded text, points `tower.toml`, `sources.toml`, `schedules.toml` and `state.json` at the new ids, and says so in Setup. The old ids keep working for agents already running. A role whose new file already exists is left where it is and named in Setup.

`max_concurrency` counts busy agents only. An agent still open on a landed flight doesn't block new launches.

**Keys in the Tower tab:**
- **Navigation:** `1`–`3` and `Tab` switch views (`4` is Team while the frequencies extra is on; other digits do nothing), `n` opens New flight, `s` opens New schedule, `Esc` closes a sheet or drawer, `r` refreshes (in Setup it also re-reads launchd and the crontab when that extra is on).
- **In text fields:** `Enter` submits a one-line field and adds a line in a multi-line one; `Ctrl+Enter` submits; `Tab` moves to the next field. IME composition is shown at the caret.

**Trust prompts:** in **New folder** flights, Claude starts without asking (see `pretrust_claude`). In **Here** or a worktree, Claude may ask whether to trust the folder the first time. Tower doesn't answer that for you: after 20 s the Inbox shows "… is waiting in its pane" with a **Focus pane** button.

## Local models and delegation

`models.toml` (or the Models view: **Add server**, **Add model**) lists the
model servers Tower knows and the models on them agents may hand work to:

```toml
[[endpoint]]
name = "company"
kind = "vllm"                     # ollama, lmstudio, llama.cpp, vllm, mlx, openai-compatible
base_url = "https://llm.example.internal/v1"
local = true                      # inside your network; unset = guessed from the URL
api_key_env = "COMPANY_LLM_KEY"   # read from the environment, never stored

[[model]]
id = "company/qwen3.8-27b"        # <endpoint>/<model name on that server>
label = "Qwen 3.8 27B"
size = "27B"
context = 131072
good_for = ["code edits", "docs", "tests"]
notes = "Strong on code; slow on very long files."
max_concurrency = 2
```

`local` matters: the guess only knows localhost, private addresses and
`.local`/`.lan`, so a company server on an internal hostname reads as cloud
until you say otherwise. `<endpoint>/<model>` ids work in any role's ladder
and in `[routing.engines]`; OpenCode agents get one OpenAI-compatible
provider per server, and on a local model every other provider, opencode's
`small_model` and sharing are switched off.

With at least one local `[[model]]`, Tower has a built-in **Local** role
(OpenCode over ACP on all of them; `roles/local.toml` replaces it). Agents
get three tools:

- `tower_models`: the local models, what each is good for, and how busy it is.
- `tower_delegate`: hand a self-contained subtask to one (`model = "auto"`
  picks the free one whose `good_for` best matches the task). It returns a
  handoff id at once; the agent keeps working.
- `tower_await`: wait (up to 45 s per call) for the result. When the repo
  has gate commands, they run on the result first and the agent gets
  pass/fail with it.

A model at its `max_concurrency` queues further packets; they start by
themselves when it is free. A local agent that ends its turn without
`tower_return` is reminded twice; after that it is stopped, its model is
free again, and the sender hears that the handoff failed.

**Agents you start yourself.** Models view, **Connect agents**: adds one
MCP server named `tower` to `~/.claude.json` and `~/.codex/config.toml`
(backed up once, nothing else touched). A `claude` or `codex` you then
start in any TGZTerminal pane gets the three tools above; outside
TGZTerminal the server lists none. Tower adopts such a session on its first
tool call and never types into its pane: results come only through
`tower_await`. **Disconnect** removes the entry. Those shims authenticate with their own token (`towerd.sock.token.pane`),
which towerd accepts for the delegation tools and nothing else. It does not
make a pane agent less trusted than you: it runs as you and could read the
master token file itself.

## Sensitive work (local models only)

Four ways to mark work that must never reach a cloud model:

- **Folders**: Policy view, **Local-only folders**, or
  `tower.toml [privacy] local_only_paths = ["~/src/payroll"]`.
- **A repo**: its own `.tower.toml` with `[privacy] local_only = true`.
- **A flight or schedule**: the "Sensitive: local models only" toggle.
- **An intake source**: `local_only = true` on a `[[jira]]`, `[[github]]`,
  `[[gitlab]]` or `[[scout]]`.

For such work Tower never starts a cloud model (`launch` refuses and says
why), routing only offers local engines, escalation stays local, triage
runs only on a local model or not at all, and handoffs inherit the mark. An
agent can mark a single subtask with `tower_delegate local_only = true`.

A cloud agent that delegated local-only work hears back only that it is
done and whether the checks passed, never the summary
(`[privacy] return = "summary"` changes that). The flight keeps the full
result for you.

**CLIs you start yourself** are kept out too, once you have clicked
**Connect agents**:

- Claude Code gets two user-wide hooks (`~/.claude/settings.json`,
  `tower guard prompt|tool`). In a local-only folder every prompt is blocked
  before anything is sent, and so is a prompt that names a local-only file
  with `@`; anywhere, a tool call (MCP tools included) that reaches into a
  local-only folder or a sensitive path is denied. If the guard cannot run
  at all, the hook blocks rather than letting everything through; a
  `tower.toml` that does not parse counts as "everything is local-only".
  The hooks and MCP entries run Tower's own copy of the binary (in its data
  folder, refreshed when towerd starts), so rebuilding the source tree does
  not break them.
- Codex has no such hook, but it starts Tower's MCP server before its first
  prompt: started in (or with `-C`) a local-only folder, the server ends it
  and says why on its terminal. Only a process that is recognisably Codex
  is ever stopped, never a shell or an IDE.
- The sidebar's agent launcher refuses to start any agent CLI in a
  local-only folder.
- `[privacy] allow_clis = ["opencode"]` (tower.toml) exempts a CLI you
  know runs local models.

A CLI that never starts Tower's MCP server (not connected, or started with
its MCP servers switched off) is still outside Tower's reach.

`[privacy] sensitive_paths = ["secrets/**"]` in a repo's `.tower.toml` is
the softer tool: cloud agents Tower starts there get Claude `Read`/`Edit`
deny rules for those paths, ACP permission requests naming them are
refused, and the prompt says so. A shell command can still get around
that, so it is best effort; for a hard rule, mark the folder or repo.

## Editing the setup in the GUI

Everything is in **Setup**: roles (**New role**, **Edit role**), model servers and
models, routing tiers (click the Routing card), policy levels (click a level),
local-only folders, schedules, sources and the extras switches.
towerd writes the files with `toml_edit`, so your comments and the settings
a form does not cover stay as they are; each write is atomic and keeps the
previous version as `<file>.bak` (a deleted role becomes
`roles/<id>.toml.bak`).

## Security model

- **Access to the daemon:**
  - The local socket is 0600, and on Windows the named pipe rejects remote clients.
  - The master token (0600 file) gives full access and is only for the Tower view and the CLI.
  - Each agent gets its own token, passed through its environment. It only allows that agent's hook events and tool calls, so an agent can't release its own handoffs or approve its own landing through Tower.
  - Agents with shell access run as your user and could still read the master token file. Real isolation would need separate OS users.
- **Untrusted text:** issue and handoff text is task data. Triage has no tools. Write-backs (comments, transitions, PRs) follow `[policy]`: by default each needs your click, and `auto` only runs while armed.
- **No bypass flags:** Tower never passes skip/bypass-permission flags, and never writes sign-in settings into an agent's config. Both are checked at launch (`compliance::lint_args`, `lint_settings`).
- **Vendor credentials:** Tower never reads, stores or forwards any vendor's tokens or credential files. Agents sign in through their own programs. See [`docs/VENDOR_TERMS.md`](docs/VENDOR_TERMS.md).
- **Redistributing a build:** Anthropic's [legal page](https://code.claude.com/docs/en/legal-and-compliance) says a product that runs Claude Code requires agreeing to the Commercial Terms of Service, with the binary unmodified, no sign-in method removed, and every user on their own credentials. Tower is built to those three conditions; if you ship a build of your own, you accept those terms as its distributor. Say "runs Claude Code" in plain text if you like, but keep "Claude Code" and "Anthropic" out of product, feature and company names and logos.
- **ACP and `acceptEdits`:** an ACP agent in `acceptEdits` mode applies edits without a permission request, so the deny-host and sensitive-path checks (which act on permission requests) do not see them. For a hard rule, mark the folder or repo local-only; `mode = "default"` in `acp_options` brings the requests back.
- **Audit:** `audit.jsonl` records every policy decision and external write.

## Debugging

- **Daemon:** `tower status` shows a summary, role warnings, pending terms notes and intake errors.
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
| `core/` | Pure rules: handoff lifecycle, gates → landed, ladder, mentions, tool summaries; `router.rs` (tier, floor, effort), `bandit.rs` (LinUCB), `work.rs` (work kinds, outcomes, host deny matching, timestamps), `cron.rs` (cron and intervals in civil time; also `#[path]`-included by `gui/`, so edition 2018 and no dependencies) |
| `towerd/` | See below |
| `gui/` | Compiled into wezterm-gui under `cfg(tgz_tower)`. Pure `forms`/`model`/`layout`/`chart`/`text`/`clip`/`theme`; `chart.rs` turns series into bar rectangles (the charts are only rectangles and text); `glue.rs` is the only file that touches `TermWindow`; `client.rs` runs the daemon's pane commands through the mux |

`towerd/` files:
- `engine.rs`: one task owns all state; `guard.rs` (a child module) is its side of policy, Armed, audit and vendor terms.
- `policy.rs`, `compliance.rs`, `audit.rs`: action levels and Armed, the vendor-terms matrix, the audit log (all pure, unit tested).
- `server.rs`: endpoint and auth.
- `transport.rs`: the blocking client.
- `drivers/{pane,acp,llm}.rs`: the three agent drivers.
- `shim.rs`: `tower hook` and `tower mcp`; `cli.rs`: `tower flight …` and `tower inbox …`.
- `jira.rs` and `mcp_http.rs`: Jira intake; `issues.rs`: the common issue/watch types and the GitHub/GitLab CLIs.
- `intake.rs` (a child module of `engine`): intake, kinds, project finding, worktrees, publishing; `followup.rs` (also a child module): decisions, artifacts, following issue activity; `projects.rs`: repo index, matching, gate detection.
- `usage.rs`: Claude transcripts.
- `schedule.rs` (`schedules.toml`, validation, placeholders) and `schedules.rs` (a child module of `engine`: firing, overlap, pause, Inbox asks); `clock.rs`: local time and DST for schedules and daily counters (`jiff`).
- `routines.rs`: the read-only launchd/crontab reader. `history.rs` (daily counters, backfill from `audit.jsonl`) and `activity.rs` (a child module of `engine`).
- `routing.rs`: engine availability, picking, effort/agent flags per driver.
- `delegate.rs` (a child module of `engine`): `tower_models` / `tower_delegate` / `tower_await`, the per-model queue, checking a local result; `panes.rs` (also a child module): CLIs in ordinary panes and "Connect agents"; `connect.rs`: the user-wide Claude/Codex MCP entry.
- `privacy.rs`: `[privacy]`, sensitive-path globs (pure); `sensitive.rs` (a child module of `engine`): local-only reasons, refusals, redaction.
- `config_write.rs`: `toml_edit` writes of `models.toml`, roles and `tower.toml` (pure plus one atomic write); `settings.rs` (a child module of `engine`): checks and reloads for those edits.
- `config.rs` (incl. the model registry and the built-in Local role), `gates.rs`, `store.rs`.

The tracked seam is about 39 lines, all cfg-gated:
- `wezterm-gui/build.rs`: `experimental_tab_views`
- `termwindow/mod.rs`: the `mod` declaration and the `EmitEvent` hook
- `render/pane.rs`: the first statement of `paint_pane`
- `mouseevent.rs`: the "Tower" row (painted in `sidebar.rs` behind `tower_available`)
