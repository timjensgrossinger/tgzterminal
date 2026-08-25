# Agent Waiting-Queue UX Plan

## Goal

Turn "which agent needs me?" from alt-tab roulette into an inbox workflow.
The terminal shows how many agents are waiting for input, lets the user jump
between them with one action, and makes waiting panes visible at a glance in
the collapsed icon rail.

This is Track 5. It depends on reliable status inference: `WaitingForInput`
must actually fire for **every** supported vendor's panes, not just the ones
that happen to be installed on the developer's machine. It pairs with Track 4
events, but does not require Track 4 to ship.

Vendors that print a prompt glyph (`>`, `❯`, `›`, or the boxed `│ >` form) are
covered by the built-in generic scan. Vendors that signal idleness some other
way — Gemini prints `type your message` — supply their own
`agent_ui.adapters.<id>.waiting_patterns`.

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
  (focus = acknowledged), even before the agent status flips, and does not
  come back when focus moves on.

Default key binding suggestion (not forced): `CMD|SHIFT-j`.

## Queue Semantics

- Queue membership: panes whose inferred status is `WaitingForInput`, plus
  panes that transitioned to `Exited` while unfocused and have not been
  focused since ("finished behind your back").
- Order: time entered queue, oldest first.
- Acknowledgement: focusing the pane removes it from the queue, and *stays*
  removed after focus moves elsewhere. If the agent prompts again later, that
  is a new episode and it re-enters.
- The queue lives on the same per-pane `AgentRuntimeState` cache introduced
  in Track 4 step 1 (entered-queue timestamp + acknowledged flag).

Implemented as `AgentDetectionCacheEntry.waiting_since` +
`acknowledged_at` in `render/sidebar.rs`. The acknowledgement is stamped by the
detection pass whenever a waiting pane is the focused-active pane, which covers
every route to focus (sidebar row, herd row, rail chip, `CycleWaitingAgent`,
keyboard tab switch, a click in the terminal); `activate_sidebar_pane` also
calls `acknowledge_waiting_pane` directly so a click clears the glow on the same
frame. It is reset when `waiting_since` goes `None -> Some`, which is what
re-arms the glow on a re-prompt. `build_waiting_queue` filters on it, so the row
glow, the rail chip and the dock badge cannot disagree.

Note the earlier implementation excluded the focused-active pane at *query
time* only. That looked equivalent but was not: nothing was recorded, so the
glow returned as soon as focus moved away from a pane the user had already
handled.

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
  *Done for the herd section:* `SidebarPalette::attention` now carries that
  colour (seeded from `agent_herd::ATTENTION_RGB` for the unbranded themes,
  the brand's own under `sidebar_theme = "Brand"`), and the status dot, its pip,
  the keyboard-selection bar and the attention detail line all read it. The
  compact-rail badge and `subagent_status_color` are still literal.
- Pulse animation: reuse the existing sidebar easing helpers; must render
  nothing new when the queue is empty (zero cost in the common case).
- Counter is a small pill in the rail using existing pill-fill helpers; it is
  a UIItem so it is clickable.
- Respect `show_sidebar_badges = false`: no badges, no counter (fix the
  currently-dead option as part of this track if not already done).

## Implementation Steps

1. ~~Add queue state (entered_at, acknowledged) to the per-pane agent runtime
   cache; update on status transitions and on focus events.~~ Done.
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

- Two agent panes prompting → counter shows 2, cycle key visits both
  oldest-first, focusing each clears it. Run this for each vendor CLI installed,
  not only for Claude: a vendor whose prompt shape the generic scan misses is
  exactly the regression this check exists to catch.
- Exited agent in background tab appears dimmed-attention until visited.
- Empty queue renders nothing and costs nothing.
- Dock badge appears only while app unfocused and count > 0.

## Acceptance Criteria

- No focus ever changes without explicit user action.
- Queue is accurate: no stale waiting badges after the user has responded.
- Collapsed rail communicates waiting state without expanding.
- Zero visual or perf change when no agent panes exist.
