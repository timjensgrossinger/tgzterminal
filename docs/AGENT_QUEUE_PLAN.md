# Agent Waiting-Queue UX Plan

## Goal

Turn "which agent needs me?" from alt-tab roulette into an inbox workflow.
The terminal shows how many agents are waiting for input, lets the user jump
between them with one action, and makes waiting panes visible at a glance in
the collapsed icon rail.

This is Track 5. It depends on reliable status inference
(`WaitingForInput` must actually fire for Claude/Codex panes) and pairs with
Track 4 events, but does not require Track 4 to ship.

## Non-Goals

- Do not auto-answer or auto-approve anything on behalf of the user.
- Do not steal focus automatically; focus changes only on explicit user
  action (key binding, click) unless the user opts in via Lua.
- Do not build OS-level notification preferences UI — Track 4 Lua events
  cover custom notification behavior.
- Do not add sounds in V1.

## User Experience

Passive surfaces:

- Sidebar icon rail: waiting agent tabs get a distinct badge state
  (orange dot / subtle pulse), visible at collapsed width. Running = neutral,
  waiting = attention, exited-with-output-unseen = dimmed attention.
- Rail header (or footer) shows a compact counter when anything waits:
  `● 2 waiting`. Hidden when zero.
- macOS dock badge shows the waiting count when the app is unfocused
  (config-gated, default on).

Active surfaces:

- `CycleWaitingAgent` key assignment: jump to the next waiting agent pane,
  ordered oldest-wait-first. Repeated presses cycle through the queue.
- Clicking the rail counter jumps to the oldest waiting pane.
- After the user focuses a waiting pane, it leaves the queue immediately
  (focus = acknowledged), even before the agent status flips.

Default key binding suggestion (not forced): `CMD|SHIFT-j`.

## Queue Semantics

- Queue membership: panes whose inferred status is `WaitingForInput`, plus
  panes that transitioned to `Exited` while unfocused and have not been
  focused since ("finished behind your back").
- Order: time entered queue, oldest first.
- Acknowledgement: focusing the pane removes it from the queue. If the agent
  prompts again later, it re-enters.
- The queue lives on the same per-pane `AgentRuntimeState` cache introduced
  in Track 4 step 1 (entered-queue timestamp + acknowledged flag).

## Config

```lua
agent_ui = {
  -- existing options unchanged, plus:
  waiting_badge = true,
  waiting_counter = true,
  waiting_include_exited = true,
  dock_badge = true,           -- macOS only, count shown when unfocused
}
```

New key assignment: `wezterm.action.CycleWaitingAgent` (and
`ActivateOldestWaitingAgent` for the click/jump-to-first behavior).

## Rendering Notes

- Badge states render in the existing compact tab icon path
  (`sidebar_compact_tab_icon`); waiting state overrides the per-agent color
  with the attention color from the palette, not a hardcoded orange.
- Pulse animation: reuse the existing sidebar easing helpers; must render
  nothing new when the queue is empty (zero cost in the common case).
- Counter is a small pill in the rail using existing pill-fill helpers; it is
  a UIItem so it is clickable.
- Respect `show_sidebar_badges = false`: no badges, no counter (fix the
  currently-dead option as part of this track if not already done).

## Implementation Steps

1. Add queue state (entered_at, acknowledged) to the per-pane agent runtime
   cache; update on status transitions and on focus events.
2. Render waiting badge state in compact tab icons.
3. Render rail counter pill + UIItem + click dispatch.
4. Add `CycleWaitingAgent` / `ActivateOldestWaitingAgent` key assignments in
   the config crate and wire to mux pane activation.
5. macOS dock badge: set/clear application badge label from queue count on
   queue change + window focus events.
6. Docs: one page, GIF of the flow, config reference.

## Testing

Focused checks:

- `cargo check -p wezterm-gui -p config`
- `cargo test -p config`
- Unit tests: queue ordering, acknowledge-on-focus, exited-unseen inclusion,
  re-entry after re-prompt.

Runtime smoke:

- Two Claude panes prompting → counter shows 2, cycle key visits both
  oldest-first, focusing each clears it.
- Exited agent in background tab appears dimmed-attention until visited.
- Empty queue renders nothing and costs nothing.
- Dock badge appears only while app unfocused and count > 0.

## Acceptance Criteria

- No focus ever changes without explicit user action.
- Queue is accurate: no stale waiting badges after the user has responded.
- Collapsed rail communicates waiting state without expanding.
- Zero visual or perf change when no agent panes exist.
