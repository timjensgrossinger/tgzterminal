# Agent Detection and Toolbelt Plan

## Goal

Add a vendor-neutral agent awareness layer to tgzterminal. The terminal should detect panes running AI agent CLIs, show useful state in the sidebar, and provide small pane-level controls without replacing the agent's own terminal UI.

This is Track 1. It is the next implementation target.

## Non-Goals

- Do not build a full custom chat UI in this track.
- Do not add a sidebar quick-prompt launcher yet.
- Do not execute hidden agent commands.
- Do not make Claude, Codex, Gemini, or any other product a hard dependency.
- Do not copy code or assets from another terminal project.

## Current Starting Point

The sidebar already supports metadata rows and vendor-neutral `agent_telemetry` fields from pane user vars such as:

- `agent.kind`
- `agent.model`
- `agent.status`
- `agent.input_tokens`
- `agent.output_tokens`
- `agent.cost`

The first implementation should reuse this surface and add a real detection/action layer behind it.

## User Experience

When a pane is running an agent CLI:

- The sidebar row shows a compact agent badge or status dot.
- The sidebar title remains the best available session title, not just the process name.
- Metadata can show agent kind, model, status, token usage, or cost when available.
- A slim toolbelt appears near the pane edge or top/bottom gutter when enabled.
- The toolbelt is optional and can be hidden without disabling agent detection.

Toolbelt V1 controls:

- Agent kind/model label.
- Status indicator.
- Interrupt/stop button.
- Copy pane/session summary button.
- Attach/resume button only for adapters that can do this safely.
- Open logs/details button only for adapters that expose a known local path or command.

## Data Model

Add a small internal model that is intentionally generic:

```rust
enum AgentKind {
    Claude,
    Codex,
    Gemini,
    OpenCode,
    Copilot,
    Cursor,
    Amp,
    Unknown(String),
}

enum AgentStatus {
    Unknown,
    Idle,
    Running,
    WaitingForInput,
    Streaming,
    Exited,
}

struct AgentPaneState {
    kind: AgentKind,
    status: AgentStatus,
    model: Option<String>,
    session_id: Option<String>,
    cwd: Option<PathBuf>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cost: Option<String>,
}
```

The concrete type names can change during implementation, but the public behavior should stay vendor-neutral.

## Detection Strategy

Detection should merge signals in priority order:

1. Explicit pane user vars, when present.
2. Foreground process basename.
3. Pane title patterns.
4. Configured process/title matchers.
5. Passive fallback to `Unknown` only when there is enough evidence that the pane is agent-like.

Initial process names:

- `claude`
- `codex`
- `gemini`
- `opencode`
- `copilot`
- `cursor`
- `amp`

Detection must be cheap. It should use existing cached pane APIs where possible and avoid polling subprocesses.

## Adapter Layer

Create an adapter abstraction with passive detection first:

```rust
trait AgentAdapter {
    fn detect(&self, pane: &dyn Pane) -> Option<AgentPaneState>;
    fn supported_actions(&self) -> AgentActions;
}
```

Actions should be opt-in per adapter:

- `interrupt`
- `attach`
- `resume`
- `open_logs`
- `copy_summary`

If an adapter cannot perform an action safely, the UI should hide or disable that action.

## Config

Suggested config shape:

```lua
agent_ui = {
  enabled = true,
  show_sidebar_badges = true,
  show_pane_toolbelt = true,
  detect_processes = true,
  toolbelt_position = "Top",
  adapters = {
    claude = { enabled = true },
    codex = { enabled = true },
    gemini = { enabled = true },
    opencode = { enabled = true },
    copilot = { enabled = true },
    cursor = { enabled = true },
    amp = { enabled = true },
  },
}
```

Keep the existing `agent_telemetry` config intact for metadata rendering. If possible, make `agent_ui` feed that surface rather than replacing it.

## Implementation Steps

1. Add config structs and defaults for `agent_ui`.
2. Add internal agent state types and adapter registry.
3. Implement passive process/title/user-var detection.
4. Connect detected state to sidebar badges and metadata.
5. Add a minimal pane toolbelt renderer.
6. Wire pointer hit testing for toolbelt buttons.
7. Implement safe actions:
   - interrupt via normal terminal interrupt path
   - copy summary from known pane state
   - adapter-specific attach/resume only after detection is reliable
8. Add docs for config and behavior.

## Testing

Focused checks:

- `cargo check -p wezterm-gui`
- `cargo test -p config`
- `git diff --check`

Runtime smoke:

- Normal shell pane shows no agent UI.
- Claude Code pane shows agent badge and keeps its session title.
- Codex pane shows generic agent badge/state.
- Toolbelt can be hidden without disabling sidebar metadata.
- Interrupt button sends a normal interrupt and does not kill the whole app.
- Unsupported actions are hidden or disabled.

## Acceptance Criteria

- Agent detection is passive by default.
- No hidden agent execution.
- Sidebar remains usable with many tabs.
- Agent pane titles do not regress.
- Non-agent panes are visually unchanged except for existing sidebar behavior.
- Claude/Codex/Gemini are examples, not architectural dependencies.
