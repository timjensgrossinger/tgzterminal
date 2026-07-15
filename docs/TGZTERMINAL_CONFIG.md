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
