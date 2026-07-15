# Lua Agent Events and API Plan

## Goal

Expose the agent awareness layer to Lua so users can script their own
notifications, automations, and adapters. The terminal surfaces primitives;
the community builds workflows on top.

This is Track 4. It depends on Track 1 (detection/toolbelt) and on status
inference being reliable.

## Non-Goals

- Do not execute agent commands on behalf of Lua without an explicit user
  action or config opt-in.
- Do not build any specific notification integration (Slack, email) into the
  core — that is exactly what user Lua is for.
- Do not expose raw scrollback scraping internals as stable API in V1.
- Do not break existing `wezterm.on` event semantics or user configs.

## User Experience

A user can write:

```lua
wezterm.on('agent-status-changed', function(window, pane, old_status, new_status)
  if new_status == 'WaitingForInput' then
    window:toast_notification('TGZTerminal', 'Agent waiting: ' .. pane:get_title(), nil, 4000)
  end
end)
```

and get exactly the behavior they want, without the core shipping a
notification feature.

## Events

Emitted on the window event loop, debounced per pane:

- `agent-detected` — pane newly classified as an agent pane.
  Args: `window, pane, agent_info`.
- `agent-status-changed` — inferred or user-var status transition.
  Args: `window, pane, old_status, new_status`.
- `agent-waiting` — convenience event, fired once per transition into
  `WaitingForInput`. Args: `window, pane, agent_info`.
- `agent-finished` — transition into `Exited` or process gone.
  Args: `window, pane, agent_info`.
- `agent-gone` — pane no longer classified as agent (process exited or
  detection lost). Args: `window, pane`.

Debounce rules:

- No event storms from flapping detection: a status must be stable for a
  configurable settle interval (default 500 ms) before an event fires.
- Events fire at most once per (pane, transition).

## `wezterm.agent` API

Read surface (V1):

```lua
local agents = wezterm.agent.list()  -- all detected agent panes
-- each entry:
-- {
--   pane_id, window_id, tab_id,
--   kind = 'Claude' | 'Codex' | ... | 'Unknown',
--   status = 'Unknown' | 'Idle' | 'Running' | 'WaitingForInput' | 'Streaming' | 'Exited',
--   model, session_id, cwd,
--   input_tokens, output_tokens, cost,
--   idle_seconds,
-- }

wezterm.agent.info(pane_id)     -- one entry or nil
wezterm.agent.waiting()         -- shortcut: entries with status WaitingForInput
```

Action surface (V1, gated):

```lua
wezterm.agent.interrupt(pane_id)      -- same path as toolbelt Stop button
wezterm.agent.send_text(pane_id, s)   -- ordinary terminal input, only if
                                      -- config.agent_ui.lua_send_text = true
```

`send_text` is off by default. When disabled, the call returns `nil, "disabled"`.

## Lua-Defined Adapters

Move adapter matching from the fixed Rust struct to data the config can
extend (requires the adapter-registry refactor from the cleanup plan):

```lua
config.agent_ui.custom_adapters = {
  aider = {
    process_names = { 'aider' },
    title_patterns = { 'aider' },
    strip_patterns = { '^%s*Tokens:', '^%s*aider v' },  -- transcript chrome
    symbol = 'ai',
    color = '#22c55e',
  },
}
```

Built-in adapters (claude, codex, gemini, opencode, copilot, cursor, amp)
remain defaults; `custom_adapters` merges over them. A new agent CLI becomes
a shareable 10-line snippet instead of a fork patch. This also lets the
community maintain chrome strip-patterns as vendors change their TUIs.

## Config

```lua
agent_ui = {
  -- existing options unchanged, plus:
  emit_events = true,
  event_settle_ms = 500,
  lua_send_text = false,
  custom_adapters = {},
}
```

## Implementation Steps

1. Introduce a per-pane `AgentRuntimeState` cache (detection result + status
   + last transition timestamp) owned by the mux/window, updated on the
   existing periodic tick — this is also the caching layer that removes
   per-frame scraping.
2. Add transition detection with settle-interval debounce.
3. Emit events through the existing `wezterm.on` emit path
   (`emit_event` / window event scheduling, same as `user-var-changed`).
4. Register `wezterm.agent` module in the lua-api-crates layer; `list`/`info`
   read the runtime cache, never scrape directly.
5. Gate `send_text` behind config; route `interrupt` through the existing
   toolbelt interrupt code path.
6. Merge `custom_adapters` into the adapter registry at config-reload time.
7. Docs page with copy-paste recipes: toast on waiting, auto-focus waiting
   pane, log cost per session to a file.

## Testing

Focused checks:

- `cargo check -p wezterm-gui -p config -p lua-api-crates`
- `cargo test -p config`
- Unit tests: transition debounce (flapping status produces one event),
  event once-per-transition, custom adapter merge precedence.

Runtime smoke:

- Recipe config receives `agent-waiting` when Claude prompts for approval.
- `wezterm.agent.list()` returns entries matching visible badges.
- `send_text` returns disabled error by default.
- Config reload picks up new custom adapter without restart.

## Acceptance Criteria

- No event fires for non-agent panes.
- Flapping detection never fires more than one event per real transition.
- `send_text` is opt-in and never silently enabled.
- A new agent CLI can be supported with config only, no rebuild.
- Existing configs without `agent_ui` changes behave identically.
