# TGZ Terminal Configuration

These options are specific to the `tgzterminal` rebuild and preserve upstream
WezTerm compatibility unless noted.

## Sidebar

```lua
config.sidebar_enabled = true
config.sidebar_width_px = 400
config.sidebar_collapsed_width_px = 48
config.sidebar_auto_hide = true
config.sidebar_position = "Left"
config.sidebar_tab_density = "Comfortable"
config.sidebar_tab_title_source = "Title"
config.sidebar_tab_metadata = { "GitBranch", "WorkingDirectory" }
config.sidebar_tab_hover_details = false
config.sidebar_scroll_bar = true
```

| Key | Type | Default | Accepted values / notes |
|---|---|---|---|
| `sidebar_enabled` | bool | `true` | Replaces the top tab strip with a vertical sidebar, which reserves horizontal space for the terminal grid. |
| `sidebar_width_px` | int px | `400` | Calibrated for 2x; scaled by display density (see below). Floored at `sidebar_collapsed_width_px`. |
| `sidebar_collapsed_width_px` | int px | `48` | Also the reserved width under `sidebar_auto_hide`, where it is forced to at least 48. |
| `sidebar_auto_hide` | bool | `true` | Reserves only the collapsed width and expands the sidebar as an overlay on hover. The in-app toggle persists to `tgz-ui-state.json` and then takes precedence over this key. |
| `sidebar_position` | enum | `"Left"` | `"Left"`, `"Right"` |
| `sidebar_tab_density` | enum | `"Comfortable"` | `"Comfortable"`, `"Compact"`. `Compact` also suppresses the metadata sub-line entirely. |
| `sidebar_tab_title_source` | enum | `"Title"` | `"Title"`, `"Command"`, `"WorkingDirectory"`, `"GitBranch"` |
| `sidebar_tab_metadata` | list of enum | `{ "GitBranch", "WorkingDirectory" }` | Elements: `"GitBranch"`, `"WorkingDirectory"` |
| `sidebar_tab_hover_details` | bool | `false` | Shows the metadata sub-line on the active/hovered row — **and makes every row two text lines tall** whether or not you hover, so fewer tabs fit on screen. |
| `sidebar_scroll_bar` | bool | `true` | Slim tab-list scrollbar when the list overflows. Reserves a 30px gutter, which narrows every row's text budget. |

The tab list scrolls with the mouse wheel when there are more tabs than visible
rows.

### Width, density and the resize grip

The two width keys are calibrated for a 2x (Retina) display and scaled by
display density, so the sidebar keeps a consistent physical size across
monitors. macOS reports DPI on a 72 base (2x ≈ 144, 1x ≈ 72); other platforms
use 96 (2x ≈ 192). The factor is `(dpi / base) / 2` clamped to `0.5..=1.25`, so a
2x display uses the configured values verbatim and a 1x display halves them.

Dragging the sidebar's inner edge resizes it between `max(collapsed_width, 140)`
and half the window width. A dragged width is kept verbatim (it is an explicit
physical-pixel choice) and is **not** persisted — restart returns to
`sidebar_width_px`.

Two thresholds change the *layout* rather than just the text:

- at 96px and below the search row and the auto-hide toggle are dropped;
- at 180px and below the Worktree / agent-launcher row is dropped, and the tab
  list reclaims the space.

Everything else adapts by measuring: each label picks the widest variant that
fits its box — `Worktree → Tree → Wt`, an adapter's `label → short_label`,
`+ New Tab → + Tab → +` — and falls back to its icon or dot alone rather than
rendering a clipped word. The default of 400 is the width at which both
`Worktree` and a full adapter label fit their halves of the shared bottom row at
a typical 14px cell, with headroom for wider monospace faces.

### Pane rows

A tab with more than one pane gets a chevron on its row. Clicking the chevron
shows the tab's panes as indented child rows; clicking it again hides them.
Tabs with a single pane have no chevron and are unchanged — their one pane is
what the tab row already describes.

- Clicking a pane row focuses that pane, switching tabs if needed.
- Hovering a pane row reveals a `×` that closes just that pane. A pane whose
  foreground process would normally prompt on close still gets the standard
  confirmation overlay, so an agent mid-task is never killed by a stray click.
  Closing a tab's last pane goes through the tab-close path instead of leaving
  an empty tab behind.
- A pane on another host is labelled `(remote)`, so a local agent split off an
  SSH shell is visibly distinct from the shell beside it.

The Worktree browser gets a pane row too, so it can be closed by hand if it
ever outlives its picker process.

Which tabs are expanded is remembered across restarts in `tgz-ui-state.json`.
Tab index is the only identity the row list has, so reordering or closing tabs
between sessions can shift which tab comes back expanded.

## Tab close context menu

```lua
config.tab_close_context_menu = true
```

Right-clicking the × (close button) of a tab opens a submenu offering to close
the surrounding tabs without touching the one you clicked. The menu appears on
both surfaces — the top tab bar and the sidebar — and labels itself accordingly:

| Entry (sidebar) | Entry (tab bar) | Effect |
|---|---|---|
| Close Tabs Above | Close Tabs to the Left | Close every tab with a lower index than the clicked one |
| Close Tabs Below | Close Tabs to the Right | Close every tab with a higher index than the clicked one |
| Close All Other Tabs | Close All Other Tabs | Close every tab except the clicked one |

The clicked tab is always preserved, so the window never empties. Batch closes
are silent — they bypass the per-tab confirmation overlay that a normal
left-click × would still show when a tab's foreground process needs prompting.
Setting `tab_close_context_menu = false` disables the submenu entirely and
restores right-click to its upstream behaviour (opening the tab navigator).

## Scrollbar

```lua
config.enable_scroll_bar = true
config.scroll_bar_auto_hide = false
```

`scroll_bar_auto_hide` hides the terminal scrollbar thumb until it is hovered,
dragged, or the pane is scrolled back.

## File Browser

```lua
config.file_browser = {
  editor_command = { "nvim" },
  list_command = { "find", ".", "-maxdepth", "3", "-type", "f" },
  split_size_percent = 30,
  reuse_editor_pane = true,
}
```

| Key | Type | Default | Notes |
|---|---|---|---|
| `editor_command` | list of string | unset | Receives the selected file path as its final argument. |
| `list_command` | list of string | `{ "find", ".", "-maxdepth", "3", "-type", "f" }` | Accepted by the schema but **not currently read by any code**. |
| `split_size_percent` | int | `30` | Clamped to `5..=95`. |
| `reuse_editor_pane` | bool | `true` | Accepted by the schema but **not currently read by any code**. |

The file browser configuration is public schema for the browser pane behavior.
The editor command receives the selected file path as its final argument.
Use `wezterm.action.OpenFileBrowser` from a key binding or the command palette
to open the browser split.
When the target pane reports an SSH working directory, the browser runs locally
and lists the remote tree through `ssh` so selection can still control the
target pane through the local mux.

## Agent Telemetry

```lua
config.agent_ui = {
  enabled = true,
  show_sidebar_badges = true,
  show_pane_toolbelt = true,
  enable_control_actions = false,
  detect_processes = true,
  copy_scrollback_lines = 20000,
  waiting_notification = true,
  toolbelt_position = "Top", -- or "Bottom"
  visible_identity_signals = 2,
  trust_visible_evidence = true,
  pulse_working_dot = true,
  pulse_period_ms = 1600,
  show_stop = true,
  adapters = {
    claude = {
      enabled = true,
      label = "Claude",
      short_label = "Cl",
      color = "#db7a52",
      process_names = { "claude", "claude-code", "claude_code" },
      title_patterns = { "claude code", "claude" },
      visible_patterns = { "claude code", "claude team", "welcome to claude" },
      running_patterns = { "esc to interrupt" },
      chrome_patterns = { "? for shortcuts", "auto mode on (shift+tab" },
      strip_patterns = { "auto mode", "token usage" },
      model_patterns = { "sonnet", "opus", "haiku" },
      resume_command = { "claude", "--resume", "{session_id}" },
      resume_latest_command = { "claude", "--resume" },
      detail_paths = { "{home}/.claude/projects/{claude_project_path}" },
    },
    codex = {
      enabled = true,
      label = "Codex",
      short_label = "Cx",
      color = "#3da37a",
      process_names = { "codex" },
      title_patterns = { "codex" },
      visible_patterns = { "openai codex", "codex cli" },
      running_patterns = { "esc to interrupt" },
      chrome_patterns = { "send q or ctrl+c to exit" },
      strip_patterns = { "tokens", "context", "approval", "sandbox" },
      model_patterns = { "gpt-5", "gpt-4" },
      resume_command = { "codex", "resume", "{session_id}" },
      resume_latest_command = { "codex", "resume", "--last" },
      detail_paths = { "{home}/.codex/sessions", "{home}/.codex/log" },
    },
    gemini = { enabled = true },
    opencode = {
      enabled = true,
      resume_command = { "opencode", "-s", "{session_id}" },
      resume_latest_command = { "opencode", "-c" },
      attach_command = { "opencode", "attach", "{attach_url}" },
      detail_paths = {
        "{home}/.local/share/opencode/log",
        "{home}/.local/share/opencode/storage",
      },
    },
    copilot = {
      enabled = true,
      resume_command = { "copilot", "--resume={session_id}" },
      resume_latest_command = { "copilot", "--continue" },
      detail_paths = {
        "{home}/.copilot/session-state/{session_id}",
        "{home}/.copilot/session-state",
      },
    },
    antigravity = {
      enabled = true,
      launch_command = { "antigravity" },
      process_names = { "antigravity", "antigravity-cli", "agy" },
      title_patterns = { "antigravity", "antigravity cli", "agy" },
    },
    cursor = { enabled = true },
    amp = { enabled = true },
  },
  launcher = {
    enabled = true,
    default_adapter = "claude",
    cwd = "ActivePane",
    open_in = "SplitPane",
    split_direction = "Horizontal",
    split_size_percent = 50,
    tile = "SplitLargest",
    max_panes_per_tab = 4,
    remote_behavior = "ForceLocal",
    project_markers = { ".git", ".hg", ".svn", ".jj" },
    domain = nil,
    prefer_wsl = true, -- Windows default; false elsewhere
    resume_menu_sessions = 10,
    restore_last_window_sessions = 8, -- 0 hides the "Reopen last window" button
  },
}

config.new_tab_menu = {
  enabled = true,
  show_domains = true,
  show_shells = true,
  show_launch_menu = true,
}

config.agent_telemetry = {
  enabled = false,
  fields = {
    "Kind",
    "Model",
    "Status",
    "InputTokens",
    "OutputTokens",
    "EstimatedCost",
    -- "TotalTokens" is also accepted, but not on by default
  },
}
```

`agent_telemetry` is off by default. Its `fields` list accepts `"Kind"`,
`"Model"`, `"Status"`, `"InputTokens"`, `"OutputTokens"`, `"TotalTokens"` and
`"EstimatedCost"`; every one except `"TotalTokens"` is on by default.

`agent_ui` enables passive detection for known agent CLIs.
If a pane publishes generic agent metadata such as `agent.model` or
`agent.status` without `agent.kind`, tgzterminal treats it as an unknown
vendor-neutral agent.

| Key | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch for detection and every agent surface. |
| `show_sidebar_badges` | bool | `true` | |
| `show_pane_toolbelt` | bool | `true` | |
| `enable_control_actions` | bool | `false` | Opt-in half of the control-action gate; see below. |
| `show_stop` | bool | `true` | Show Stop in expanded herd rows when agent can be interrupted. |
| `detect_processes` | bool | `true` | When off, only user vars identify an agent — no process, title or visible-text detection, and therefore no inferred status. |
| `copy_scrollback_lines` | int | `20000` | Maximum **physical** rows a copy action reads, counted from the bottom of the pane buffer. Wrapped output costs several rows per logical line, which is why the previous `500` truncated real sessions. Clamped to 100000 rows per action. Lower it to capture less. |
| `waiting_notification` | bool | `true` | |
| `toolbelt_position` | enum | `"Top"` | `"Top"`, `"Bottom"` |
| `visible_identity_signals` | int | `2` | Distinct adapter-exclusive patterns that must agree before visible text names an agent. Clamped by how many the adapter declares. |
| `trust_visible_evidence` | bool | `true` | Whether multi-signal visible-text evidence counts as trusted for control actions. |
| `pulse_working_dot` | bool | `true` | Pulse the status dot while an agent is Running/Streaming. |
| `pulse_period_ms` | int | `1600` | One pulse cycle. Clamped to `400..=6000`. |
| `dock_badge` | bool | `true` | Show a count badge on the macOS dock icon for agents waiting for input while the app is unfocused. Ignored on non-macOS platforms. |
| `track_exited_unseen` | bool | `true` | Keep agents that finished while the window was unfocused in the waiting queue with a dimmed badge until seen. **Experimental**: relies on detecting the loss of agent identity, which is less reliable than the `WaitingForInput` signal. |

Each adapter accepts `enabled`, `label`, `short_label`, `color`,
`process_names`, `title_patterns`, `visible_patterns`, `running_patterns`,
`chrome_patterns`, `strip_patterns`, and
`model_patterns`. It may also accept action templates: `resume_command`,
`resume_latest_command`, `attach_command`, `detail_paths`,
`launch_command`, and `launch_domain`. Pattern entries
are literal case-insensitive fragments by default. Entries prefixed with `re:`
are treated as regexes, but long or invalid regexes are ignored to keep passive
detection bounded. Built-in detection defaults cover Claude, Codex, Gemini,
OpenCode, Copilot CLI, Antigravity CLI, Cursor, and Amp; partial adapter configs
merge with those defaults.

Action templates are argv/path arrays expanded only when the user clicks a
toolbelt action. Supported variables are `{session_id}`, `{cwd}`, `{home}`,
`{attach_url}`, and `{claude_project_path}`. TGZTerminal hides an action if a
required template value is missing, if the command is not found on `PATH`, or if
no configured details path exists. Cursor, Gemini, and Amp intentionally ship
without built-in resume/details actions; configure those fields explicitly if a
safe local CLI workflow exists on your machine.

When an agent pane is detected, the sidebar can show a compact badge and the
active pane can show a slim toolbelt. `Stop` and copy actions stay user
initiated. Copy actions read at most `copy_scrollback_lines` physical rows from
the **bottom of the pane buffer, regardless of the current scroll position**, so
scrolling up does not change what a copy returns. When older rows exist but fall
outside that window, the copied text starts with
`[… earlier scrollback not included …]`. If transcript cleanup ends up with
nothing, the raw pane text (or, failing that, the agent details) is copied
instead and the notification says which one it used. Copied text may include
terminal output or secrets printed in that range.

`enable_control_actions` is `false` by default. Resume, Attach and log-opening
controls require **both** an explicit opt-in — `agent_ui.enable_control_actions
= true` or the pane user variable `agent.enable_control_actions=true` — **and**
trusted identity evidence. Neither half is sufficient alone. Claude log
directories are canonicalized and must resolve under `~/.claude/projects`.
Non-Claude local session or state paths are shown as `Details` in the toolbelt.

`waiting_notification` enables a throttled local toast when an agent appears to
be waiting for input.

### Waiting-queue UX

Turning "which agent needs me?" into an inbox workflow. When a pane's inferred
status becomes `WaitingForInput`, it joins the waiting queue; an agent that
finished while the window was unfocused and never regained focus also joins
(with a dimmed badge) when `track_exited_unseen` is on.

Surfaces:

- **Collapsed rail chip** — a `● N` footer at the bottom of the sidebar rail
  while anything waits. Clicking it jumps to the oldest waiting pane. When
  nothing waits, the same chip shows a compact token total (`Σ 1.2M`) across
  this window's agent panes instead.
- **macOS dock badge** — `agent_ui.dock_badge` shows the waiting count on the
  dock icon while the app is unfocused. No-op elsewhere.
- **Rail badges** — waiting panes keep the amber attention dot; exited-unseen
  panes render dimmed.

Acknowledge is lazy: focusing a waiting pane (or clicking its rail row) drops it
from the queue immediately, even before the agent's status changes. Re-prompts
after you have looked restart the timer cleanly.

The default key assignment is:

```lua
config.keys = {
  { key = "j", mods = "CMD|SHIFT", action = "CycleWaitingAgent" },
}
```

`CycleWaitingAgent` jumps to the next waiting pane in the **current window**,
ordered oldest-wait-first and wrapping around. The command palette exposes the
same action under "Cycle to next waiting agent".

### How an agent is identified

Every signal produces a candidate tagged with the strength of its evidence, and
the strongest class wins. Within a class, the candidate with the most agreeing
patterns wins. Adapter table order never decides identity: a genuine tie between
two adapters yields no agent for the weak classes, and an unnamed generic agent
for the strong ones.

| Evidence | Source | Trusted for control actions |
|---|---|---|
| `UserVar` | `agent.kind` / `agent.adapter` pane user variables | yes |
| `Process` | foreground process basename matched `process_names` | yes |
| `TitlePhrase` | a multi-word (or `re:`) `title_patterns` entry matched the pane title | yes |
| `VisibleChrome` | the adapter's own on-screen chrome, with enough agreeing signals | only if `trust_visible_evidence` |
| `TitleToken` | a single bare brand word matched the pane title | yes |
| `Metadata` | generic `agent.*` telemetry with no identity of its own | no |

`TitleToken` deliberately ranks *below* `VisibleChrome`: a pane title is prose
the user or the agent typed ("fix the amp meter bug"), while an agent's TUI
furniture is not. This is also what lets a genuine Claude Code pane be
recognized at all — it runs as `node` and titles its pane with the current task,
so its own footer is the only durable signal it has.

Pattern conventions, which the built-in defaults follow:

- **`title_patterns`** may contain bare brand tokens. The haystack is one short
  string, and matching is word-boundary — `amp` no longer fires on `example`,
  `stamp`, `&amp;`, or `/opt/amp-tools`.
- **`visible_patterns`** must be phrases or long distinctive compounds, never a
  bare brand word: the haystack is a whole screen, where words like `cursor`,
  `codex` and `openai` appear in ordinary output.
- **`running_patterns`** are printed only while that adapter is working. They
  drive the Running status and count as identity when no other enabled adapter
  claims the same string.
- **`chrome_patterns`** are the adapter's permanent TUI furniture (footer, hint
  line). Identity only, never status.
- **`strip_patterns`** are never identity. They are deliberately short and
  generic (`tokens`, `ctx:`), which is exactly what must not badge a pane.

Visible text only names an agent when at least one `running`/`chrome` pattern
matched **and** at least `visible_identity_signals` distinct adapter-exclusive
patterns matched. A pattern claimed by more than one enabled adapter (such as
`esc to interrupt`) still drives status but carries no identity weight. This is
why displaying a log, a diff — or this project's own adapter table — does not
badge the pane.

Once established, a badge only changes on stronger evidence or after two
consecutive detections agree on a different adapter, so a pane that retitles
itself every turn does not flicker between agents. An identity is reused when a
frame finds no fresh evidence, scoped to what earned it: process and user-var
identities last as long as that anchor is unchanged, title-derived identities die
with the title they came from, and visible-text identities expire after 30
seconds and must be re-earned.

Status is inferred from the whole visible region — not a fixed tail — and a
`Running` reading is held for a 3 second grace period, because agents repaint
their spinner asynchronously and a frame caught between repaints shows neither a
marker nor a prompt.

### Agent launcher

`agent_ui.launcher` adds a sidebar button that starts a fresh agent session. It
shares a row with the Worktree button (Worktree on the left, the agent on the
right) and, when the sidebar is collapsed, takes the icon rail slot directly
above `+`.

- Left-click launches the default agent.
- Alt-click launches it into the *other* target — see `open_in` below.
- Right-click opens a dropdown listing every installed agent, plus a sticky
  `Project root` toggle. Clicking an agent row expands it into a submenu of
  **Split pane** / **Fullscreen** / **New tab**, launching that one agent at
  the explicit target regardless of `open_in` or Alt.
- Clicking the launcher button repeatedly tiles agents into the current tab
  — see "Repeat launches" below — instead of splitting the same pane over
  and over.

Which agents appear is discovered, not configured: an adapter is offered only if
it is `enabled` and the first element of its `launch_command` resolves to an
executable. Lookup uses `PATH` first and then a fixed list of user install
directories — `~/.local/bin`, `~/bin`, `~/.claude/local`, `~/.bun/bin`,
`~/.cargo/bin`, `~/.volta/bin`, `~/.npm-global/bin`, `~/.yarn/bin`,
`/opt/homebrew/bin`, `/usr/local/bin` — because an app bundle launched from
Finder or the Dock inherits launchd's minimal `PATH`, not the login shell's, and
would otherwise miss `~/.local/bin/claude`. The resolved absolute path is what
gets spawned, for the same reason. Nothing installed means no button at all, and
the Worktree button keeps its original full width. `launch_command` is a plain
argv with no `{...}` substitutions — the working directory is supplied by the
launcher, not baked into the command. Built-in launch commands are `claude`,
`codex`, `gemini`, `opencode`, `copilot`, `cursor-agent`, and `amp`.

`default_adapter` names the adapter used by a plain click and defaults to
`"claude"` (Claude Code). When it is set to `nil`/`""` or names an agent that is
not installed, the first installed adapter wins, which is Claude in the built-in
ordering.

`cwd` is the *initial* working-directory rule:

- `"ActivePane"` (default) — the active pane's current directory, the same OSC 7
  value a new tab would inherit.
- `"ProjectRoot"` — walk up from that directory to the nearest one containing an
  entry from `project_markers`, falling back to the pane directory when the pane
  is not inside a project.

The `Project root` row in the dropdown toggles the same setting at runtime and
persists it to `tgz-ui-state.json` in the data directory. Once you have used the
toggle, the persisted value takes precedence over the `cwd` config key — the
same behavior as the sidebar auto-hide toggle. `project_markers` entries must be
plain directory names; path-shaped entries such as `../.git` are ignored.

#### Where the agent opens

`open_in` decides the launch target:

- `"SplitPane"` (default) — split the active pane and put the agent in the new
  half, so the agent sits beside the shell it was started from. The shell keeps
  its position; the agent takes the second half.
- `"NewTab"` — open the agent in its own tab, the pre-`open_in` behavior.
- `"Zoomed"` — split in like `"SplitPane"`, then zoom the new agent pane so it
  fills the tab regardless of how many panes were already open. Un-zooming
  (`Ctrl-Shift-Z`, or `SetPaneZoomState(false)`) restores every pane that was
  there before, agents included — nothing is closed, only hidden.

Holding **Alt** while clicking inverts `"SplitPane"`/`"NewTab"` for that launch
only, so switching between the two never needs a config edit. Alt-clicking a
`"Zoomed"` launcher also falls back to `"NewTab"` — `"Zoomed"` has no
independent "other target" to invert into. The dropdown's per-agent submenu
(above) always launches at the target you click, ignoring both `open_in` and
Alt.

`split_direction` is `"Horizontal"` (side by side, default) or `"Vertical"`
(stacked), and `split_size_percent` is the share given to the agent pane,
clamped to `5..=95`. Both apply only to the *first* agent launched into a tab;
both are ignored when `open_in = "NewTab"`.

#### Repeat launches: tiling into one tab

Clicking the launcher (or a submenu's Split/Fullscreen row) a second time in
the same tab does not re-split the agent you just launched. Instead it splits
the **largest** eligible pane along its longer axis, so three clicks give an
even-ish grid rather than three ever-thinner slivers stacked on top of the
first agent. Eligible panes exclude the worktree/file-browser pane, which is
never split into.

- `tile` controls this: `"SplitLargest"` (default) picks the largest pane as
  above; `"ActivePane"` restores the pre-tiling behavior of always splitting
  whichever pane is currently focused.
- `max_panes_per_tab` (default `4`, `0` = unlimited) caps how many eligible
  panes one tab may hold. A launch that would exceed the cap opens a new tab
  instead of adding another pane. A pane whose resulting half would be too
  small to use (under roughly 40 columns or 12 rows) also falls back to a new
  tab rather than producing a sliver.
- `"Zoomed"` composes with tiling: the new agent is tiled in first, then
  zoomed. Un-zooming reveals the full tiled grid, not just the pane that was
  there before the most recent launch.

This applies no new layout engine — it is the same `SplitPane` your other key
bindings use, aimed at the largest pane instead of the active one. WezTerm's
existing pane zoom, splits, and sidebar pane rows are what make the tiled
agents navigable; nothing new is introduced to maintain them.

#### Launching from an SSH pane

`remote_behavior` decides what happens when the active pane is a session on
another machine:

- `"ForceLocal"` (default) — run the agent on **this** machine regardless. The
  remote pane's working directory names a path over there and means nothing
  here, so the agent starts in the most recently active *local* pane's
  directory instead, falling back to your home directory when the window has no
  local pane. With `open_in = "SplitPane"` the local agent still splits in
  beside the SSH pane, the same way the Worktree browser does.
- `"FollowPane"` — launch into the active pane's domain, starting the agent on
  the remote host. Use this when the agent CLI is genuinely installed there.

`ForceLocal` is the default because the agent CLI and its credentials normally
live on the workstation; following an SSH pane usually finds no binary at all.
It is checked last, so an explicitly configured `domain` (or `prefer_wsl` on
Windows) still wins — both of those already name a local target.

A pane is treated as remote when any of these hold: an `ssh://` working-
directory URL, an `ssh`/`mosh` foreground process, an ssh-shaped argv, a pane
belonging to a WezTerm SSH domain, or an OSC 7 `file://` URL whose host is not
this machine.

Unlike Resume, Attach, and Details, the launcher is **not** governed by
`enable_control_actions`. That gate exists so that evidence derived from
*detection* can never authorize acting on a session. Launching is a different
category: the argv comes only from config, never from pane titles or visible
text, and it always follows an explicit click.

#### Resuming a past session

The launcher dropdown's **Resume session** row expands into the most recently
used agent sessions found on disk, newest first, mixing every vendor into one
list. Clicking one starts that agent with its resume command
(`claude --resume <id>`, `codex resume <id>`, …) **in the directory the session
originally ran in** — the project-root toggle deliberately does not apply, since
a resumed session whose relative paths have moved is not much use. Placement
otherwise follows `open_in`, `tile`, and the rest of the launcher config, exactly
like a fresh launch.

Each row reads `project · description`, prefixed with `[branch]` when the session
was not on `main`/`master`. The description is Claude Code's own generated
session title where one exists, and otherwise the first thing the user actually
asked, trimmed to ten words.

- `resume_menu_sessions` (default `10`, max `25`, `0` hides the row) caps how
  many sessions are offered.
- `restore_last_window_sessions` (default `8`, max `25`, `0` hides the button)
  caps how many sessions one "Reopen last window" click may bring back.

Only Claude Code and Codex are listed. The other adapters declare resume
commands, but none of them documents a session store this terminal could
enumerate; when one does, it slots into the same list. The scan runs on a worker
thread and is cached for ten seconds, so opening the submenu never blocks
rendering — a first open may show `Scanning…` briefly.

Session ids come from the filesystem rather than from pane output, and are
charset-checked (and refused if they begin with `-`) before reaching argv, so a
file dropped into an agent's state directory cannot turn into a command-line
flag. Because the argv still comes only from config and the row must be clicked,
this action is not gated by `enable_control_actions` either — unlike the
toolbelt's Resume button, whose session id *does* come from pane text.

#### Launching into another domain (WSL)

On Windows, agent CLIs are usually installed inside a WSL distro rather than on
the host, so `prefer_wsl` defaults to `true` there and to `false` everywhere
else. The domain for a launch is resolved in this order:

1. the adapter's own `launch_domain`,
2. `agent_ui.launcher.domain`,
3. `prefer_wsl`, if the active pane is not already inside a distro,
4. the active pane's domain.

A configured domain name that is not registered logs a warning and falls
through to the next rule instead of failing the click. `prefer_wsl` picks the
**first registered** WSL domain — WSL reports distributions in its own order and
the "default distro" flag is not carried into the domain list, so pin a specific
one with `domain = "WSL:Ubuntu"` if you have several.

```lua
agent_ui = {
  launcher = { domain = 'WSL:Ubuntu' },
  adapters = {
    -- keep one agent on the Windows side
    codex = { launch_domain = 'local' },
  },
}
```

The working directory follows you across the domain change:

- **Windows → WSL** needs no translation. The spawn becomes
  `wsl.exe --distribution <distro> --cd <path> --exec <argv>`, and `wsl.exe --cd`
  accepts a Windows path and translates it, so `C:\Users\tim\proj` lands at
  `/mnt/c/Users/tim/proj`.
- **WSL → Windows** is translated here: `/mnt/c/foo` becomes `C:\foo`, and a
  path inside the distro becomes its UNC form,
  `\\wsl.localhost\Ubuntu\home\tim\proj`.
- **One distro → a different distro**, or between two unrelated non-WSL domains
  (local → SSH), drops the directory and lets the target domain use its own
  default, because the path has no meaning there.

Project-root mode works from a WSL pane too: because Windows cannot stat a Linux
path directly, the marker walk runs against the `\\wsl.localhost` view of the
distro and the result is mapped back before launching.

### Agent herd section

The sidebar has a collapsible **Agents** section listing agents TGZTerminal
can see, headed `Agents · N`. It merges two sources: agents detected live in
this window's own tabs/panes, and agents found by scanning each vendor's
on-disk session files (so an agent in a pane outside the current window can
still show up). The two sources are **joined**, not concatenated: a session is
bound to the pane running it by process tree (falling back to a *unique* cwd
match), so an agent visible both ways is one row, not two. A session that binds
to no pane is still listed, but it cannot be focused.

Both sources are filtered before display:

- **Liveness**: a vendor session file is only shown while its process is
  still alive. A session whose process has exited is a stale leftover and is
  dropped silently — it does not linger as a phantom row.
- **Interactive only**: vendors write the same session files for processes no
  human is typing into — SDK harnesses, one-shot `-p` runs, hook children.
  These are hidden unless `show_non_interactive = true`. For Claude the
  discriminator is the session's `entrypoint` (`cli` is interactive, `sdk-cli`
  is not); vendors whose store does not record this are always treated as
  interactive.
- **Project scope**: only agents belonging to the active pane's project (the
  repo root of its working directory, or a directory nested under it) are
  shown. If the active pane's project can't be determined, the section falls
  back to showing everything rather than going blank.
- **Per-adapter opt-out**: a vendor with `agent_ui.adapters.<id>.enabled =
  false` is excluded from the disk-scanned source (live pane detection is
  already gated by the same adapter config).

```lua
 agent_ui = {
   section = {
     enabled = true,      -- show the Agents section in the sidebar at all
     refresh_ms = 500,     -- how often the disk-scanned source re-reads, clamped 100..=10000
     show_non_interactive = false, -- also list SDK/headless/hook agent processes
     show_activity = true, -- transcript headline, attention, branch, subagent summary + tree
     show_tokens = true, -- pane-reported token/cost telemetry
     sort_attention_first = true, -- surface blocked/waiting agents at the top of the list
   },
 }
 ```

The section header reads `Agents · N` (or `Agents · N · M⚠` when `M` agents
need attention). Scroll the list with the mouse wheel when it is taller than
the section. **Left-click** the header collapses it; **right-click** toggles
between the current-project view and `· all` (every project's agents). Click a
row to focus its pane, or the chevron to expand it.

Expanded rows show status, project root, the latest activity headline, a flat
indented **subagent tree**, and action buttons. Available actions: `Focus`,
`Resume` (detached sessions), `Attach`, `Logs`, `Stop`, `Log` (full-screen
activity log overlay — press `r` to refresh, `q`/Esc to close), `Copy Id`, and
`Transcript` (reveal the session directory in the file manager).

**Keyboard navigation:** bind `ActivateAgentSection` (e.g.
`CTRL|SHIFT|a`) to enter navigation mode; `↑`/`↓` move the cursor,
`Enter`/`←` focus the selected agent, `Space`/`→` expand it, `Esc` exits.

### New-tab dropdown

`new_tab_menu` controls the chevron beside the sidebar's `+ New Tab` button,
which opens a small picker of the shells and domains available on this machine.
Clicking the `+` label itself still opens a tab as before, and right-clicking it
still opens WezTerm's full launcher overlay.

The list is grouped, with a divider between groups:

- **Shells** (`show_shells`) — discovered, not configured. On Windows:
  PowerShell, PowerShell 7, Command Prompt, and Git Bash, each shown only if
  present. On macOS and Linux: the entries in `/etc/shells` that exist and are
  executable.
- **Domains** (`show_domains`) — every registered spawnable domain, including
  each WSL distro and any SSH or mux domain. A `WSL:Ubuntu` domain is listed as
  `Ubuntu`.
- **`launch_menu`** (`show_launch_menu`) — your own entries. Entries with no
  `args` are skipped, since the plain `+` button already covers the default
  shell.

Picking a row carries the current directory into the new tab using the same
translation rules as the agent launcher above. The chevron is hidden entirely
when the list would be empty.

Telemetry field names are vendor-neutral. Provider names may be supplied by user
configuration or pane metadata, but the public UI contract is based on generic
agent fields rather than hard-coded provider products.
When enabled, the sidebar reads pane user variables named `agent.kind`,
`agent.model`, `agent.status`, `agent.input_tokens`, `agent.output_tokens`,
`agent.total_tokens`, `agent.cost`, and `agent.estimated_cost`. Underscore
forms such as `agent_model` are accepted as a compatibility fallback.

## Rich Input Composer

```lua
config.rich_input = {
  enabled = false,
  agent_panes_only = true,
  show_send_preview = true,
  require_confirm_for_multiline = false,
  history_limit = 100,
  docked = false,
  dock_rows = 3,
}
```

`rich_input` adds an optional multiline input composer that opens as a
bottom-anchored overlay. It is intended for composing long prompts, pasted
text, and path references before sending them to an agent CLI. It is `false`
by default and adds no behavior until enabled.

| Key | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Master switch for the composer. |
| `agent_panes_only` | `true` | Only open when the active pane is a detected agent pane. |
| `show_send_preview` | `true` | Show a `[N lines, M chars]` summary of what will be sent. |
| `require_confirm_for_multiline` | `false` | Require a second submit press to send multiline content. |
| `history_limit` | `100` | Maximum previous submissions kept for recall. |
| `docked` | `false` | Enable the Warp-style docked input strip, activated per agent pane via the toolbelt **Input** button or `ToggleDockedInput` (see below). |
| `dock_rows` | `3` | Visible content rows reserved for the docked strip when shown (clamped 1–12). |

The composer has no default key binding. Bind the `ActivateComposer` action to
open it, for example:

```lua
config.keys = {
  { key = 'Space', mods = 'CTRL|SHIFT', action = wezterm.action.ActivateComposer },
}
```

When `rich_input.enabled = true`, the agent pane toolbelt also shows a **Compose**
button (next to Copy). Clicking it toggles the composer open/closed for that pane,
so no key binding is required. The button honors the same gating as
`ActivateComposer` (`agent_panes_only`), and the toolbelt only appears on detected
agent panes.

Composer key handling while the overlay is open:

| Key | Action |
| --- | --- |
| `Ctrl+Enter` | Send the buffer to the active pane, then submit (sends `Enter`). |
| `Enter` / `Shift+Enter` | Insert a newline. |
| `Esc` | Close without sending. |
| `Backspace` / `Delete` | Edit the buffer. |
| Arrows / `Home` / `End` | Move the cursor. |
| `Alt+Up` / `Alt+Down` | Recall previous / next submission from history. |
| `Ctrl+U` | Clear the buffer. |
| `Alt+D` | Insert the active pane's working directory as a plain path. |
| `Alt+S` | Insert the current terminal selection. |
| Paste | Pasted text is normalized (CRLF → LF) and inserted into the buffer. |

### Sending semantics and safety

On submit the composer sends the buffer as a bracketed paste followed by a
single carriage return, so multiline content and embedded control characters
are delivered safely and the CLI's own permission and approval flows stay in
control. The composer never attaches hidden file contents: `Alt+D` and `Alt+S`
insert plain text references (a path or the current selection) that remain
visible and editable before you send. When the composer is closed, terminal
input is unchanged.

Context helpers for inserting a selected file path or Worktree path from the
sidebar are planned for a later version; V1 covers working directory and
terminal selection insertion.

### Docked input strip (Warp-style)

Set `rich_input.docked = true` (with `enabled = true`) to make a Warp-style
multiline input strip available on **agent CLI panes**. The strip is **not**
persistent chrome and is **not** shown by default: it appears only after you
activate it, and only on detected agent panes.

Activate it in one of two ways, both agent-only:

- Click the **Input** button in the agent pane toolbelt (it replaces the
  "Compose" button when `docked = true`). The toolbelt only appears on detected
  agent panes, so the button — and therefore the strip — is unavailable
  elsewhere.
- Press `Ctrl+Shift+Space` (the `ToggleDockedInput` action, bound by default)
  while an agent pane is active. On a non-agent pane the key does nothing.

Toggling activates the strip for that pane and focuses it; toggling again hides
it. When shown, it reserves `dock_rows` rows (plus a header and hint line) from
that pane's viewport, so the terminal becomes that many rows shorter. The
reservation is fixed (it does not grow while you type); content beyond
`dock_rows` scrolls internally.

While the strip is shown:

- Keystrokes edit it: `Ctrl+Enter` sends the buffer to the pane (bracketed paste
  + `Enter`) and keeps focus for the next prompt, `Enter` inserts a newline,
  `Esc` releases focus back to the terminal (the strip stays shown). Arrows,
  `Home`, `End`, `Backspace`, `Delete`, `Ctrl+U` (clear), `Alt+Up`/`Alt+Down`
  (history), `Alt+D` (cwd) and `Alt+S` (selection) behave as in the overlay
  composer.
- Click inside the strip to focus it; click the terminal above it to release
  focus. While unfocused, terminal input is completely unchanged.

The toolbelt requires `agent_ui.enabled` and `agent_ui.show_pane_toolbelt`
(both on by default). Split panes: V1 shows a single strip that follows the
active pane; independent per-pane strips inside splits are not yet supported.

Note: because bound key assignments are handled before the strip, a bound
clipboard-paste shortcut still pastes into the pane rather than the strip while
the strip is focused; use the overlay composer if you need paste-into-buffer.

## SSH Quick-Launch (mosh / Eternal Terminal)

TGZTerminal adds a standalone sidebar button (below the agent launcher)
that opens a dropdown of pre-registered SSH connections. Each row spawns
into a new tab. The list comes from `config.ssh_domains` — the same key
upstream WezTerm uses — plus two new per-domain fields:

| Key (on `SshDomain`) | Type | Default | Meaning |
|---|---|---|---|
| `transport` | enum | `"WezTerm"` | `"WezTerm"` (native mux, default for `SSHMUX:`), `"Ssh"` (plain ssh, default for `SSH:`), `"Mosh"` (requires `mosh` on `PATH`), `"Et"` (requires `et` on `PATH`), or `"Custom"` (run an arbitrary argv you supply — see `custom_command`). |
| `extra_args` | list of string | unset | Appended to the spawn argv for `Mosh`/`Et`/`Custom` only. Ignored for `WezTerm`/`Ssh` (use `ssh_option` for those). |
| `custom_command` | list of string | unset | Literal argv run when `transport = "Custom"`. Use a wrapper script, an autossh invocation, a Secretive-mediated `ssh user@secretive_alias`, or anything the built-in transports cannot name. First element is probed on `PATH`; missing binary hides the row. |

```lua
config.ssh_domains = {
  {
    name = "prod",
    remote_address = "prod.example.com",
    username = "tim",
    transport = "Mosh",        -- mosh user@prod.example.com
    extra_args = { "--predict=adaptive" },
  },
  {
    name = "jumpy",
    remote_address = "jumpy.example.com:2022",
    username = "tim",
    transport = "Et",           -- et tim@jumpy.example.com:2022
  },
  -- Secretive (macOS): a regular `ssh` invocation whose Host alias is
  -- resolved by ~/.ssh/config to the actual host, while the key is served
  -- by the Secretive ssh-agent (SSH_AUTH_SOCK). A plain `Ssh`/`WezTerm`
  -- transport already works with Secretive because wezterm-ssh forwards
  -- the agent — use `Custom` only when you want a wrapper command, e.g.
  -- autossh or a host alias that bypasses ~/.ssh/config.
  {
    name = "secretive-ansible",
    remote_address = "ignored",  -- required by SshDomain schema, unused by Custom
    transport = "Custom",
    custom_command = {
      "ssh",                     -- probed on PATH first
      "ansible_direct@ansible_secretive",
    },
  },
}
```

Discovery and behavior:

- `WezTerm`/`Ssh` rows spawn through `SpawnTabDomain::DomainName("<name>")`
  and reuse the wezterm-native SSH path (auth, ssh_config, optional mux).
  An SSH agent — including macOS Secretive, which exposes keys via
  `SSH_AUTH_SOCK` — is forwarded automatically because
  `mux_enable_ssh_agent` defaults to `true`. No `Custom` config needed for
  plain Secretive: just declare the host with `transport = "Ssh"` (or put
  an `Host ansible_secretive` block in `~/.ssh/config` and let the
  auto-generated domain pick it up).
- `Mosh` rows spawn `mosh <user@host>` as a plain shell command in the
  **local** domain — mosh owns its own reconnect and bypasses the wezterm
  mux entirely. The row is hidden when `mosh` is not on `PATH`.
- `Et` rows similarly spawn `et <user@host[:port]>`. Hidden when `et` is
  not on `PATH`.
- `Custom` rows run `custom_command` verbatim (plus `extra_args`) as a
  plain shell command in the local domain. Use this for autossh
  (`custom_command = { "autossh", "-M", "0", "user@host" }`), a wrapper
  script, or any program that brandishes its own connection lifecycle. An
  empty `custom_command` or a non-executable first element silently hides
  the row, matching the mosh/et behavior.
- Auto-generated entries from `wezterm.default_ssh_domains()` always use
  `transport = "WezTerm"`, matching previous behavior; only your own
  `ssh_domains` entries opt into mosh/et/custom.
- Binary lookup uses `PATH` and then `fallback_command_dirs`
  (`~/.local/bin`, `~/bin`, `/opt/homebrew/bin`, …) for the same
  Finder/Dock launchd-PATH reason as the agent launcher. The resolved
  absolute path is what gets spawned.
- Reconnect, roaming, and UDP/TCP behavior for mosh/et are owned by
  those transports — TGZTerminal only pre-registers and launches them.

The dropdown is hidden entirely when no row is usable (no `ssh_domains`
and no sidecar binaries installed).

## Update Checking

These are upstream WezTerm keys whose fork behavior differs; they are documented
in full in `docs/config/lua/config/check_for_updates.md`.

| Key | Default | Meaning |
|---|---|---|
| `check_for_updates` | `true` | Periodically ask GitHub whether a newer release of this fork exists. Upstream WezTerm also defaults this to true; earlier TGZTerminal builds defaulted it to `false`. |
| `check_for_updates_interval_seconds` | `86400` | Seconds between checks. |
| `show_update_window` | `false` | Deprecated no-op, kept for config compatibility. |

When a newer release is found, the notification's click target is the release
artifact for the running platform — `TGZTerminal.dmg`, `TGZTerminal-Setup.exe`,
or the portable `TGZTerminal-windows-portable-<tag>.zip` — falling back to the
release page when the release publishes nothing for this platform. The mux
banner always links the release page. Nothing is downloaded or installed
without the user acting on the notification.

The `CheckForUpdates` key assignment runs the same check on demand and always
reports a result, including "up to date". It is registered in the command
palette and the Help menu.

```lua
config.keys = {
  { key = 'U', mods = 'CTRL|SHIFT', action = wezterm.action.CheckForUpdates },
}
```

Asset names are derived from `BRAND_PRODUCT_NAME` (see below), so a rebranded
overlay fork resolves its own artifacts without patching the updater.

## Branding (build-time)

These are compile/package-time environment variables, not Lua config keys. Each
defaults to the standard TGZTerminal value, so a build with none of them set is
identical to the default TGZTerminal build. They exist so an overlay fork can
rebrand without patching source.

`ci/build-macos-bundle.sh` reads:

| Env var | Default | Controls |
|---|---|---|
| `BRAND_APP_NAME` | `TGZTerminal` | `.app` and `.dmg` names, `CFBundleName`, `CFBundleDisplayName`, DMG volume name |
| `BRAND_BUNDLE_ID` | `com.tgzterminal.app` | `CFBundleIdentifier` |
| `BRAND_CLI_BIN` | `tgzterminal` | CLI binary name in `Contents/MacOS` (the `wezterm` compatibility symlink is always kept) |
| `BRAND_ICON` | _(unset)_ | Path to a `.icns` copied over `Contents/Resources/terminal.icns`; the build fails if set but missing. `CFBundleIconFile` stays `terminal.icns`. |

`wezterm-gui` reads these at compile time (via `option_env!`, resolved in
`wezterm-gui/src/brand.rs`):

| Env var | Default | Controls |
|---|---|---|
| `BRAND_GITHUB_REPO` | `timjensgrossinger/tgzterminal` | `owner/repo` used for update/release queries |
| `BRAND_PRODUCT_NAME` | `TGZTerminal` | Product name in the update User-Agent and update notifications, and the prefix used to match release assets (`<name>.dmg`, `<name>-Setup*.exe`, `<name>-windows-portable-*.zip`) |

`CFBundleExecutable` stays `wezterm-gui`, and the internal namespaces
(`tgzterminal.worktree` user var, `TGZTERMINAL_BIN`, `.cache/tgzterminal`) are
unaffected by branding.

## Portable mode (Windows)

On Windows only, configuration precedence depends on a file rather than a config
key. When a file named `.portable` sits next to the executables, these three
locations take precedence over their counterparts in your user profile:

| In the program folder | Instead of |
|---|---|
| `wezterm.lua` | `%USERPROFILE%\.wezterm.lua`, `%USERPROFILE%\.config\wezterm\wezterm.lua` |
| `colors\` | the `colors` directory of each config dir |
| `wezterm_modules\` | later entries on the Lua `package.path` |

The portable `.zip` ships that marker; the installer never does, and actively
deletes one it finds. So an extracted zip behaves like a self-contained tool on a
thumb drive, while an installed build reads only your own configuration — which
matters because the program folder belongs to the installer, and a file dropped
there would otherwise outrank every user's config on the machine.

- `TGZTERMINAL_PORTABLE=1` forces portable mode on, `=0` forces it off, for a
  layout the marker does not describe.
- `--config-file` and `WEZTERM_CONFIG_FILE` outrank all of the above in both
  modes.
- A `wezterm.lua` found next to the executable without a marker is ignored, and
  logs a warning once saying so.

Nothing is portable about *state*: UI state, caches and sockets always live under
the user profile (`%APPDATA%\wezterm`, `%LOCALAPPDATA%\wezterm`,
`%USERPROFILE%\.local\share\wezterm`), in either mode.

## What is not configurable

Some sidebar and toolbelt chrome has no config key at all. It is listed here so
you are not left hunting for one.

| Chrome | What governs it |
|---|---|
| Search box | Appears when the sidebar is wider than 96px. The `Search tabs...` placeholder is hardcoded, and steps down to `Search...` / `Search` when the row is narrow. |
| Auto-hide toggle button | Always drawn alongside the search row. Clicking it persists a value that then overrides `sidebar_auto_hide`. |
| Worktree button | Appears when the sidebar is wider than 180px; its label ladder and its half of the shared row are computed, not configured. |
| `+ New Tab` button and label | Always drawn. `new_tab_menu.enabled` controls only the chevron beside it, not the button. |
| Collapsed icon-rail composition | Derived from the auto-hide state and whether an agent CLI was discovered. Two-character badges come from an adapter's `short_label`. |
| Toolbelt button labels, sizes and drop order | Hardcoded. When the strip is too narrow buttons are dropped in a fixed order (Input/Compose, then Details, Attach, Resume), and Stop and Copy are the last two standing. |
| Per-toolbelt-button visibility | Toolbelt visibility is derived. Herd-row `Stop` is governed by `agent_ui.show_stop`; it appears when the agent can be interrupted. `Copy` whenever an agent is detected; `Attach` / `Resume` / `Details` need their action templates *and* the control-action gate; `Input` / `Compose` follow `rich_input.enabled` and `rich_input.docked`. If a button is missing, it is a detection or a gate question — see *How an agent is identified*. |
| Sidebar spacing, radii and row geometry | Compile-time constants. |
| Sidebar colors | Derived from the active color scheme. |
| Worktree picker behavior | No config surface. |

Two keys are accepted by the schema but currently read by no code:
`file_browser.list_command` and `file_browser.reuse_editor_pane`.
