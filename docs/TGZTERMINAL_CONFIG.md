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

## Agent Telemetry

```lua
config.agent_ui = {
  enabled = true,
  show_sidebar_badges = true,
  show_pane_toolbelt = true,
  detect_processes = true,
  toolbelt_position = "Top",
  adapters = {
    claude = { enabled = true },
    codex = {
      enabled = true,
      process_names = { "codex" },
      title_patterns = { "codex" },
    },
    gemini = { enabled = true },
    opencode = { enabled = true },
    copilot = { enabled = true },
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
user variables first, then the cached foreground process name, then pane title
text, then configured adapter matchers. It does not spawn or drive any agent
process.
If a pane publishes generic agent metadata such as `agent.model` or
`agent.status` without `agent.kind`, tgzterminal treats it as an unknown
vendor-neutral agent.

Each adapter accepts optional `process_names` and `title_patterns` lists.
`process_names` are matched against the cached foreground process basename.
`title_patterns` are lowercase fragments matched against the pane title. Built-in
defaults cover Claude, Codex, Gemini, OpenCode, Copilot, Cursor, and Amp; custom
lists are for local wrappers or renamed CLIs.

When an agent pane is detected, the sidebar can show a compact badge and the
active pane can show a slim toolbelt. The initial toolbelt exposes safe generic
actions only: normal terminal interrupt (`^C`) and copy pane summary. Attach,
resume, and log/detail actions stay hidden unless a future adapter can expose a
safe implementation.

Telemetry field names are vendor-neutral. Provider names may be supplied by user
configuration or pane metadata, but the public UI contract is based on generic
agent fields rather than hard-coded provider products.
When enabled, the sidebar reads pane user variables named `agent.kind`,
`agent.model`, `agent.status`, `agent.input_tokens`, `agent.output_tokens`,
`agent.total_tokens`, `agent.cost`, and `agent.estimated_cost`. Underscore
forms such as `agent_model` are accepted as a compatibility fallback.
