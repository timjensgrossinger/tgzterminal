# TGZTerminal

TGZTerminal is a focused, macOS-first rebuild of WezTerm with a quieter tab
surface, better agent visibility, and defaults tuned for long-running AI and
developer sessions.

It keeps the upstream WezTerm terminal engine, GPU renderer, multiplexer, and
configuration model, then layers TGZTerminal-specific workflow improvements on
top.

## What Makes It Different

- Vertical sidebar tabs replace the traditional top tab strip.
- Auto-hide sidebar mode keeps a compact icon rail visible without resizing the
  terminal grid.
- The sidebar has meaningful compact tab badges for terminals, Claude, Codex,
  Gemini, Copilot, Cursor, and other agent panes.
- Agent panes get a slim in-pane toolbelt for safe actions such as interrupting
  the process and copying useful conversation text.
- The terminal scrollbar can stay visible or auto-hide independently from the
  sidebar.
- A worktree/file-browser pane can open project files through your configured
  editor command.
- All agent metadata is vendor-neutral and driven by pane title, process name,
  or user variables.

## Current Status

This is an active private preview branch. It is usable locally, but the feature
surface is still settling and the app update story is intentionally manual:
build, sign, and reinstall the macOS bundle.

## Highlights

### Sidebar Tabs

The sidebar is designed for repeated terminal work rather than a landing-page
style UI. Active tabs have a clear rail, inactive tabs stay dense and readable,
and overflow is handled with wheel scrolling plus a slim sidebar scrollbar.

Auto-hide mode reserves only the collapsed rail width and expands as an overlay
when the rail is hovered. The hover trigger is limited to the visible rail so
terminal text selection near the edge still works normally.

### Agent Toolbelt

Detected agent panes can show a small toolbelt in the active pane. The current
safe actions are:

- `Stop`: sends `Ctrl-C` to a running or streaming agent pane.
- `Copy conversation`: copies recent pane scrollback and visible output.
- `Copy last message`: copies a best-effort latest visible assistant response.
- `Copy agent details`: copies the agent metadata summary.

Agent detection is passive. TGZTerminal does not drive or spawn agent CLIs for
the toolbelt.

### Vendor-Neutral Agent Metadata

Panes can publish metadata with user variables such as:

```text
agent.kind
agent.model
agent.status
agent.input_tokens
agent.output_tokens
agent.total_tokens
agent.cost
agent.estimated_cost
```

Detection also checks foreground process names and pane titles for common local
agent CLIs.

## Configuration

TGZTerminal remains compatible with WezTerm-style Lua configuration. Useful
options include:

```lua
config.sidebar_enabled = true
config.sidebar_auto_hide = true
config.sidebar_position = "Left"
config.sidebar_width_px = 280
config.sidebar_collapsed_width_px = 48
config.sidebar_tab_density = "Comfortable"
config.sidebar_scroll_bar = true

config.enable_scroll_bar = true
config.scroll_bar_auto_hide = false

config.agent_ui = {
  enabled = true,
  show_sidebar_badges = true,
  show_pane_toolbelt = true,
  detect_processes = true,
  toolbelt_position = "Top",
}
```

See [docs/TGZTERMINAL_CONFIG.md](docs/TGZTERMINAL_CONFIG.md) for the fuller
configuration reference.

## Build

Prerequisites are the same broad Rust/macOS toolchain expectations as upstream
WezTerm.

```sh
cargo build -p wezterm -p wezterm-gui -p wezterm-mux-server
```

For a release binary:

```sh
cargo build --release -p wezterm-gui
```

## Local macOS Bundle Flow

The local bundle is assembled under `dist/TGZTerminal.app` and installed
manually while this preview is private.

The currently used runtime inside the app bundle is:

```text
dist/TGZTerminal.app/Contents/MacOS/wezterm-gui
```

After replacing the runtime binary, sign and verify the bundle:

```sh
codesign --force --deep --sign - dist/TGZTerminal.app
codesign --verify --deep --strict --verbose=2 dist/TGZTerminal.app
```

Then copy it into `/Applications` or `~/Applications`.

## Documentation

- [TGZTerminal rebuild spec](docs/TGZTERMINAL_REBUILD_SPEC.md)
- [TGZTerminal config](docs/TGZTERMINAL_CONFIG.md)
- [Agent toolbelt plan](docs/AGENT_TOOLBELT_PLAN.md)
- [Rich input plan](docs/RICH_INPUT_PLAN.md)
- [Provenance notes](docs/PROVENANCE.md)

## Upstream

TGZTerminal is based on [WezTerm](https://github.com/wezterm/wezterm). The
terminal engine, renderer, mux, and much of the configuration/runtime model come
from upstream WezTerm. The TGZTerminal-specific work is focused on local
workflow, sidebar UX, agent visibility, and macOS bundle identity.

Please keep upstream attribution intact when carrying this work forward.
