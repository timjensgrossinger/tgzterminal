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
sidebar reserves horizontal space for the terminal grid. `sidebar_auto_hide`
uses the collapsed width as the reserved size and expands the sidebar as an
overlay when the collapsed strip is hovered. The tab list scrolls with the
mouse wheel when there are more tabs than visible rows. `sidebar_scroll_bar`
shows a slim sidebar tab-list scrollbar when the list overflows.

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
`resume_latest_command`, `attach_command`, and `detail_paths`. Pattern entries
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
