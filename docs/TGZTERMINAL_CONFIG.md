# TGZ Terminal Configuration

These options are specific to the `tgzterminal` rebuild and preserve upstream
WezTerm compatibility unless noted.

## Sidebar

```lua
config.sidebar_enabled = true
config.sidebar_width_px = 280
config.sidebar_collapsed_width_px = 36
config.sidebar_auto_hide = true
config.sidebar_position = "Left"
config.sidebar_tab_density = "Comfortable"
config.sidebar_tab_title_source = "Title"
config.sidebar_tab_metadata = { "GitBranch", "WorkingDirectory" }
config.sidebar_tab_hover_details = false
config.sidebar_scroll_bar = true
```

`sidebar_enabled` replaces the top tab strip with a vertical sidebar. The
sidebar reserves horizontal space for the terminal grid. `sidebar_width_px` and
`sidebar_collapsed_width_px` are calibrated for a 2x (Retina) display and scale
down on lower-density displays so the sidebar keeps a consistent physical size
across monitors; a manual drag-resize overrides `sidebar_width_px` and is kept
verbatim. `sidebar_auto_hide`
uses the collapsed width as the reserved size and expands the sidebar as an
overlay when the collapsed strip is hovered. The tab list scrolls with the
mouse wheel when there are more tabs than visible rows. `sidebar_scroll_bar`
shows a slim sidebar tab-list scrollbar when the list overflows.

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
  copy_scrollback_lines = 500,
  waiting_notification = true,
  toolbelt_position = "Top",
  adapters = {
    claude = {
      enabled = true,
      label = "Claude",
      short_label = "Cl",
      color = "#db7a52",
      process_names = { "claude", "claude-code", "claude_code" },
      title_patterns = { "claude code", "claude" },
      visible_patterns = { "claude code", "claude team" },
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
      visible_patterns = { "codex", "openai" },
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
    remote_behavior = "ForceLocal",
    project_markers = { ".git", ".hg", ".svn", ".jj" },
    domain = nil,
    prefer_wsl = true, -- Windows default; false elsewhere
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
  },
}
```

`agent_ui` enables passive detection for known agent CLIs. Detection checks pane
user variables first, then the cached foreground process name and pane title,
then visible text only when earlier trusted signals are insufficient.
If a pane publishes generic agent metadata such as `agent.model` or
`agent.status` without `agent.kind`, tgzterminal treats it as an unknown
vendor-neutral agent.

Each adapter accepts `enabled`, `label`, `short_label`, `color`,
`process_names`, `title_patterns`, `visible_patterns`, `strip_patterns`, and
`model_patterns`. It may also accept action templates: `resume_command`,
`resume_latest_command`, `attach_command`, `detail_paths`,
`launch_command`, and `launch_domain`. Pattern entries
are literal case-insensitive fragments by default. Entries prefixed with `re:`
are treated as regexes, but long or invalid regexes are ignored to keep passive
detection bounded. Built-in detection defaults cover Claude, Codex, Gemini,
OpenCode, Copilot, Cursor, and Amp; partial adapter configs merge with those
defaults.

Action templates are argv/path arrays expanded only when the user clicks a
toolbelt action. Supported variables are `{session_id}`, `{cwd}`, `{home}`,
`{attach_url}`, and `{claude_project_path}`. TGZTerminal hides an action if a
required template value is missing, if the command is not found on `PATH`, or if
no configured details path exists. Cursor, Gemini, and Amp intentionally ship
without built-in resume/details actions; configure those fields explicitly if a
safe local CLI workflow exists on your machine.

When an agent pane is detected, the sidebar can show a compact badge and the
active pane can show a slim toolbelt. `Stop` and copy actions stay user
initiated. Copy actions read at most `copy_scrollback_lines` recent scrollback
lines and may include terminal output or secrets printed in that range.

`enable_control_actions` is `false` by default. Resume and log-opening controls
require trusted process/title/user-variable evidence or an explicit opt-in via
`agent_ui.enable_control_actions = true` or pane user variable
`agent.enable_control_actions=true`. Claude log directories are canonicalized
and must resolve under `~/.claude/projects`. Non-Claude local session or state
paths are shown as `Details` in the toolbelt.

`waiting_notification` enables a throttled local toast when an agent appears to
be waiting for input.

### Agent launcher

`agent_ui.launcher` adds a sidebar button that starts a fresh agent session. It
shares a row with the Worktree button (Worktree on the left, the agent on the
right) and, when the sidebar is collapsed, takes the icon rail slot directly
above `+`.

- Left-click launches the default agent.
- Alt-click launches it into the *other* target — see `open_in` below.
- Right-click opens a dropdown listing every installed agent, plus a sticky
  `Project root` toggle.

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

Holding **Alt** while clicking uses the other target for that launch only, so
switching between the two never needs a config edit.

`split_direction` is `"Horizontal"` (side by side, default) or `"Vertical"`
(stacked), and `split_size_percent` is the share given to the agent pane,
clamped to `5..=95`. Both are ignored when `open_in = "NewTab"`.

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
| `BRAND_PRODUCT_NAME` | `TGZTerminal` | Product name in the update User-Agent and "… Update Available" notifications |

`CFBundleExecutable` stays `wezterm-gui`, and the internal namespaces
(`tgzterminal.worktree` user var, `TGZTERMINAL_BIN`, `.cache/tgzterminal`) are
unaffected by branding.
