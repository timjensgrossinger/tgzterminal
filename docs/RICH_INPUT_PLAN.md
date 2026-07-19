# Optional Rich Input Plan

> **Status (implemented):** Both surfaces ship and share one editing core
> (`ComposerBuffer`). The on-demand overlay composer is `ActivateComposer`. The
> Warp-style docked strip (`rich_input.docked`) is **agent-only** and **not**
> persistent: it appears only after activation via the agent-pane toolbelt
> **Input** button or the `ToggleDockedInput` action (default
> `Ctrl+Shift+Space`, a no-op on non-agent panes). When shown it reserves
> `dock_rows` rows from the pane; active-pane-only in V1. See
> `docs/TGZTERMINAL_CONFIG.md` → "Rich Input Composer". Key bindings use standard
> WezTerm key-assignments rather than the nested `key_bindings` table below.

## Goal

Add an optional multiline input composer for agent panes. The composer should make long prompts, pasted text, and file/path context easier to enter while still sending ordinary terminal input to the running CLI.

This is Track 3. It is optional and should follow the agent detection/toolbelt work.

## Non-Goals

- Do not replace normal shell input globally in V1.
- Do not add the deferred sidebar quick-prompt launcher in this track.
- Do not silently attach file contents or hidden context.
- Do not bypass any CLI permission, approval, or sandbox model.
- Do not build a full custom chat transcript UI yet.

## User Experience

When focused in an agent pane, the user can open a composer overlay. The overlay supports multiline editing and then sends the final text into the pane as if the user typed or pasted it.

V1 behavior:

- Open composer with a configurable key binding.
- Type or paste multiline text.
- Submit to the active agent pane.
- Escape closes without sending.
- Optional preview confirms exactly what will be sent.
- Composer is hidden for non-agent panes unless explicitly enabled later.

## Relationship To Agent Toolbelt

Rich input should depend on Track 1 detection. The composer should appear only when tgzterminal knows the active pane is an agent pane, unless the user explicitly enables it for all panes.

This avoids guessing terminal state independently and keeps the feature safer.

## Editing Features

V1:

- Multiline text editing.
- Soft wrapping.
- Cursor movement.
- Backspace/delete.
- Paste normalization.
- Submit/cancel.
- History recall for previous composer submissions.

V1.5:

- Select all.
- Word movement.
- Indentation helpers.
- Optional send preview.
- File/path insertion from Worktree selection.

Later:

- Prompt templates.
- Snippets.
- Drag-and-drop file path insertion.
- Markdown-style visual hints.
- Rich command palettes for agent-specific commands.

## Context Helpers

Context helpers must be explicit. V1 should insert references, not hidden content:

- Insert current working directory.
- Insert selected file path.
- Insert selected Worktree path.
- Insert selected terminal text.

Do not read and attach file contents automatically in V1. If file-content insertion is added later, require explicit user action and show what will be sent.

## Config

Suggested config shape:

```lua
rich_input = {
  enabled = false,
  agent_panes_only = true,
  show_send_preview = true,
  require_confirm_for_multiline = false,
  history_limit = 100,
  key_bindings = {
    open = "CTRL|SHIFT|Space",
    submit = "CTRL|Enter",
    cancel = "Escape",
  },
}
```

The default should remain off until the editing surface is stable.

## Rendering

The composer should feel like part of the terminal, not a modal dialog:

- Bottom overlay or pane-attached input strip.
- Same dark material/colors as the sidebar.
- Clear focus state.
- Stable height with max-height and internal scrolling.
- No overlap with scrollbars or sidebar controls.

## Sending Semantics

The composer should send normal terminal input:

1. Build final text.
2. Optionally show preview.
3. Send text to the active pane.
4. Send newline only when submit behavior requests it.

This keeps Claude, Codex, Gemini, and other CLIs in control of their own permission flows.

## Implementation Steps

1. Add config structs and defaults for `rich_input`.
2. Add composer state to `TermWindow`.
3. Add key handling for open/submit/cancel/edit.
4. Add renderer for bottom composer overlay.
5. Add paste handling and text normalization.
6. Add history ring.
7. Add context insertion for cwd, selected path, and selected text.
8. Gate the feature behind agent detection by default.
9. Document config and expected behavior.

## Testing

Focused checks:

- `cargo check -p wezterm-gui`
- `cargo test -p config`
- `git diff --check`

Runtime smoke:

- Composer opens in Claude/Codex panes when enabled.
- Composer does not open in normal shell panes by default.
- Escape closes without sending.
- Submit sends exactly the visible text.
- Multiline paste remains responsive.
- Existing terminal input still works when composer is closed.

## Acceptance Criteria

- Default off.
- Agent panes only by default.
- No hidden command execution.
- No hidden file-content injection.
- Normal terminal behavior is unchanged when the composer is closed.
- Large prompts remain responsive.
