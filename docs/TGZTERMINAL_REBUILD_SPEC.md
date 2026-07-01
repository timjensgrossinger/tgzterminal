# TGZ Terminal Rebuild Spec

This document is the behavior-only source for the public `tgzterminal` rebuild.
It intentionally avoids code snippets, patches, assets, and private project
details from any prior prototype.

## Identity

- Public repository and package name: `tgzterminal`.
- Primary executable: `tgzterminal`.
- Human-facing app label: `TGs Terminal` where a display name is useful.
- Upstream compatibility is preserved unless a public artifact needs the
  `tgzterminal` identity.

## Sidebar Tabs

The terminal uses a vertical sidebar as the default tab surface. The sidebar
replaces the top tab strip when enabled.

Config surface:

- `sidebar_enabled = true`
- `sidebar_width_px = 280`
- `sidebar_collapsed_width_px = 36`
- `sidebar_auto_hide = true`
- `sidebar_position = "Left"` or `"Right"`
- `sidebar_tab_density = "Comfortable"` or `"Compact"`
- `sidebar_tab_title_source = "Title"`, `"Command"`,
  `"WorkingDirectory"`, or `"GitBranch"`
- `sidebar_tab_metadata = { "GitBranch", "WorkingDirectory" }`
- `sidebar_tab_hover_details = false`
- `sidebar_scroll_bar = true`
- `scroll_bar_auto_hide = false`

Behavior:

- Active tabs show a clear accent rail and stronger background treatment.
- Inactive tabs remain readable in a dense list.
- Hover states are subtle and should not shift row geometry.
- The tab list supports fast direct tab switching.
- The tab list supports wheel scrolling when there are more tabs than visible
  rows; a sidebar tab-list scrollbar is shown when tabs overflow.
- The sidebar can be positioned on either side.
- Auto-hide uses the collapsed width for the resting state and expands on
  hover without changing terminal cell geometry.
- Resizing is smooth and avoids terminal grid resize unless the effective cell
  geometry changes.
- The terminal scrollbar can be shown on the right and optionally hidden until
  hovered, dragged, or the pane is scrolled back.

Visual direction:

- Use restrained depth, subtle translucency/material effects where the platform
  supports them, and rounded active states.
- Keep the UI quiet and utility-focused. Avoid copying any third-party assets,
  source, or proprietary visual details.

## File Browser Pane

The rebuild includes a file browser pane controlled by public configuration:

- editor command
- list command
- split size
- reuse-editor-pane toggle

The browser opens files through the configured editor command, can reuse an
existing editor pane when requested, and keeps pane management compatible with
normal terminal splits.

## Agent Telemetry

Agent telemetry is vendor-neutral. Public code and docs use generic fields:

- `agent.kind`
- `agent.model`
- `agent.status`
- token counts
- estimated cost fields

Specific providers such as Claude, Codex, or Gemini may appear only as examples
in user configuration, never as hard-coded product assumptions.

## Performance Acceptance

Measured interactions:

- scroll a large terminal buffer
- drag-resize the sidebar repeatedly
- toggle or hover auto-hide

Acceptance criteria:

- no visible stalls during common scrolling and sidebar interactions
- no runaway CPU use during animations
- no full layout or terminal grid resize during pixel-level sidebar drag unless
  cell geometry actually changes
- repaint requests are coalesced where practical
- `max_fps` is not raised globally without measurement
