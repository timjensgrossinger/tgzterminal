# Plan: Windows/WSL agent issues — tab titles, restore, worktree, Lua knobs

Status: diagnosis complete, ready for implementation.
Test environment (this machine): Windows + WSL2 Ubuntu (running), opencode CLI v2.0.19
installed in Ubuntu (`~/.opencode/bin/opencode`, on PATH only via `~/.bashrc`),
`claude` in Ubuntu `~/.local/bin`, OpenCode **desktop app** running on Windows
(holds `opencode.db`), TGZTerminal installed at
`%LOCALAPPDATA%\Programs\TGZTerminal`, Git for Windows present, git present in
Ubuntu, fzf absent in Ubuntu.

---

## Diagnosed root causes

### 1. Tab name shows `wslhost.exe` for agent sessions in WSL

Chain of facts (all verified in code):

- `mux/src/localpane.rs:449-463` `LocalPane::get_title`: if the terminal title is
  still the default `"wezterm"`, fall back to the **Windows-side** foreground
  process. On Windows that is the youngest console descendant in the pane's
  process tree (`procinfo` snapshot) — and a WSL pane's tree is only
  `wezterm-gui.exe → wsl.exe → wslhost.exe`. Linux processes are invisible to the
  Windows snapshot. Upstream wezterm confirms this is inherent
  (wezterm/wezterm#3137: "essentially impossible" to divine WSL processes from the
  Windows side; official recommendation: OSC titles / shell-integration user
  vars).
- The agent launch goes through `wsl.exe --exec sh -lc 'exec "$0" "$@"' claude ...`
  (`mux/src/domain.rs:270-312` fixup + `wsl_paths.rs:207-226` wrapper), which
  bypasses rc files → the wezterm shell integration never loads → no
  `WEZTERM_PROG`/`agent.*` user vars are ever emitted from WSL panes.
- Agents emit OSC 2 titles only sporadically (claude titles its pane while
  working, not at the prompt), so the fallback wins and the tab reads
  `wslhost.exe`.
- When the agent is installed on Windows the pane's tree contains the real
  `claude`/`node` process, which is why a "normal claude session" titles
  correctly.

**Fix (both ends, cheap):**
1. *Launch-time*: the launcher already knows the agent label. Prefix the WSL
   wrapper with a title/user-var emit (same pattern as the worktree script's
   `printf '\033]1337;SetUserVar=tgzterminal.worktree=MQ==\007'`, mod.rs:3916):
   emit `agent.title` (or the adapter label) before `exec`. Optionally install
   the shell integration `PROG` emission in the wrapper as well.
2. *Fallback-time*: carry the `SpawnCommand.label` through to the pane (it is
   currently dropped — `spawn.rs:50-87` never maps it) and use it in
   `get_title`'s fallback when the foreground basename is a WSL host shim
   (`wslhost.exe`, `wslrelay.exe`). Also add `wslhost`/`wslrelay` to
   `is_generic_shell_title` (`sidebar.rs:2669-2698`) so the sidebar never
   displays them.

### 2. Restore/recent-session dropdown shows wrong names ("cmd")

- Herd row name falls back to the pane title
  (`agent_herd/mod.rs:719-724`: `session.name || pane.title || session_id`). On
  Windows the pane title for a shell is `cmd.exe` (the default prog), and that
  raw string is displayed (`sidebar.rs:13505`) and stored as the snapshot label
  (`sidebar.rs:4250`). The `is_generic_shell_title` blacklist that would reject
  `cmd`/`cmd.exe`/`wsl`/`bash` is applied only in the sidebar tab-label path,
  never in the herd-name path.
- **Additional root cause found live**: the opencode reader
  (`agent_herd/opencode.rs:128-254`) queries `FROM session` / `FROM part` — the
  opencode **v2 desktop/CLI schema dropped both tables**. The user's actual DB
  (verified by reading the sqlite DDL) has `session_v2`, `session_message`,
  `session_inbox`, `workspace`, `worktree`, `project` etc. The GUI log is full of
  `opencode database schema not recognized: no such table: session`. So opencode
  sessions get no titles at all, and herd rows fall back to junk names.

**Fix:**
1. Filter generic shell titles (`cmd`, `cmd.exe`, `wslhost.exe`, `wsl`, `bash`,
   …) in the herd-name fallback; fall through to `session_id`/adapter label
   instead of showing them.
2. Teach `opencode.rs` the v2 schema: `SELECT id, directory, title, … FROM
   session_v2 WHERE parent_id IS NULL …` with status/activity from
   `session_message` (`json_extract(data,'$.type')`), keeping the old
   `session`/`part` queries as fallback for older installs. (Schema captured in
   `session_v2`: id, project_id, directory, title, cost, tokens_input,
   tokens_output, time_created/time_updated, parent_id…; `session_message`:
   session_id, type, seq, time_created, time_updated, data.)

### 3. Opening an old session splits the tab (should be its own fullscreen tab)

- `mouse_event_sidebar_agent_menu_resume_session` (`mouseevent.rs:1134-1146`)
  → `resume_agent_session(index, None)` → `resume_agent_session_by_id(..., None)`
  (`sidebar.rs:6860-6874`) → `agent_launch_placement(false, None)` →
  `resolve_launch_target` returns `launcher.open_in`, whose default is
  **`SplitPane`** (`config.rs:917-933`). Hence the split.
- The batch restore already proves the desired pattern: it always spawns into new
  tabs (`agent_launch.rs:293-324`) and bypasses placement.

**Fix:** pass a target from the resume-row click handler through
`resume_agent_session` → `resume_agent_session_by_id` (the parameter already
exists). Introduce `AgentLaunchTarget::OwnTab` = "new tab, pane zoomed"
(fullscreen within its own tab), and make it the default for *resumed* sessions
while leaving fresh-launch placement (`open_in`) untouched. Also wire Alt-click
inversion for session rows (currently hardcoded `invert_target=false`).

### 4. CRITICAL: "Reopen last window" does nothing

Evidence on this machine: `C:\Users\gross\.local\share\wezterm` contains
`tgz-ui-state.json` but **no `tgz-last-session.json` at all**, despite long GUI
runs (60 MB, 135 MB logs) and `restore_last_window_sessions: 8` in the config
dump. The write path (`sidebar.rs:12882-12935`, sync close path
`12937-12954`) never produced the file.

Silent-failure modes identified (plan: fix all of them, then repro live):
1. Write-side gates that can permanently suppress writes:
   - `agent_herd_session_cache.is_none()` (first herd disk scan never landed —
     check why on Windows; kick the scan at window startup rather than lazily);
   - empty sets never persisted (deliberate, but combined with a hard kill the
     last good state can be lost);
   - agents without panes are never recorded (`window_agent_sessions` records
     only `pane_id.is_some()`); sessions living only in the OpenCode desktop app
     therefore never snapshot.
2. Restore-side filters that can drop every candidate silently
   (`plan_session_restore`, `sidebar.rs:4288-4311`): already-live anywhere on the
   machine (the desktop app's sessions are *always* live → always filtered!),
   disabled adapter, cwd deleted, cap.
3. When 0 spawns succeed, `notify_restore_outcome` returns early on (0,0): no
   toast, no message — the click looks dead.

**Fix:**
- Kick the herd scan at window startup so the cache is present before the first
  snapshot decision.
- Make "already live" filter only sessions that are live **in a pane** of
  TGZTerminal (a session in the OpenCode desktop app or another terminal is a
  legitimate restore candidate — arguably the main use case).
- Always toast the restore outcome with counts and skip reasons
  (already-live/dead-cwd/failed-resume), never (0,0)-silently.
- On successful restore, spend the offer per window-set (already done) but keep
  the snapshot file intact for other windows/runs.
- Add diagnostics: log every gate decision behind a debug flag; validate live.

### 5. Worktree does not work (WSL projects / "should show the project with WSL tools")

Two concrete, live-reproduced root causes:

**5a. Windows CLI socket discovery is broken (upstream wezterm bug, still in
upstream main).** `wezterm-client/src/discovery.rs:189-194` publishes only
`path.file_name()` (e.g. `gui-sock-8460`) into the shared-memory NameHolder;
`resolve()` returns that bare name, and any client then tries to connect to a
*relative* path and fails instantly. Reproduced: `tgzterminal.exe cli list`
fails with `failed to connect to Socket("gui-sock-8460")`, while setting
`WEZTERM_UNIX_SOCKET` to the full path works. Consequences:
- The worktree picker's `$TGZTERMINAL_BIN cli split-pane / send-text /
  kill-pane` calls do nothing (script swallows errors) — the picker opens but
  selecting a file/cd/close silently fails. This is very likely the whole
  "worktree doesn't work".
- Single-instance handoff (`Publish::try_spawn`, main.rs:543-556) also
  connects to the bare name → every launch spawns a fresh GUI process (two GUIs
  are running right now).

**Fix:** store the full path when publishing *and* make `resolve()` join
`RUNTIME_DIR` when the stored value is bare (covers stale publishers). Add a
regression test + verify `cli list` with no env connects.

**5b. The WSL worktree route never forwards `WEZTERM_UNIX_SOCKET`.**
`mod.rs:4801-4829` forwards only `TGZTERMINAL_*` vars via `env KEY=VAL…`
(because spawn env stops at `wsl.exe` — Microsoft documents that only `WSLENV`-
listed vars bridge the boundary; the code's `env` approach is correct, the var
list is just incomplete). So inside Ubuntu every CLI call fails even after 5a is
fixed. **Fix:** forward `WEZTERM_UNIX_SOCKET` in that list (Windows path is fine
— the CLI is the Windows `.exe` invoked through interop).

**5c. WSL-first shell selection (the "normally ubuntu is used" ask).**
`file_browser_shell` (`mod.rs:236-273`) uses the WSL route only when the target
pane's own domain is a WSL domain, demands Git Bash otherwise, and offers no
configuration. **Fix:** `config.file_browser.shell = "Auto" | "Wsl" |
"GitBash"`, with `"Wsl"` (or `"Auto"` when a distro pane exists) preferring the
pane's distro → `agent_ui.launcher.wsl_distro` (new) → default distro via
`wsl.exe -d … --exec sh -lc` (spawned in the local domain, like the agent
fallback path). This makes the picker work for WSL projects even from a
PowerShell tab.

Also worth doing: document that fzf is optional (Ubuntu here has none; the
numbered-prompt fallback is what the user currently sees and may read as
"broken").

### 6. Lua configuration surface ("a person should be able to set these things in the lua")

Existing knobs already cover some of it (`agent_ui.launcher.open_in`,
`prefer_wsl`, `domain`, `restore_last_window_sessions`, per-adapter
`launch_domain`). Add:

| New key | Purpose | Default |
| --- | --- | --- |
| `agent_ui.launcher.resume_open_in` | placement of *resumed* sessions (`OwnTab` / `NewTab` / `SplitPane` / `Zoomed`) | `"OwnTab"` (per request: fullscreen own tab) |
| `agent_ui.launcher.wsl_distro` | preferred distro for `prefer_wsl`, resume domain and worktree fallback | first registered WSL domain |
| `agent_ui.adapters.<id>.tab_title` | template for the tab title injected at agent launch (e.g. `"{agent}"`, `"{agent} · {project}"`) | adapter label |
| `config.file_browser.shell` | worktree picker shell: `"Auto"`, `"Wsl"`, `"GitBash"` | `"Auto"` |
| `config.file_browser.wsl_domain` / `distro` | which distro runs the picker when the pane isn't a WSL pane | `wsl_distro` |

Document all of them in `docs/TGZTERMINAL_CONFIG.md` (agent launcher + file
browser sections; also remove the "no config surface" row for the worktree
picker) and mirror in `ci/wezterm-default.lua`.

---

## Sources used (official)

- WezTerm official docs: pane `get_title` (OSC 0/1/2 precedence; Windows
  fallback = "most recently spawned descendant"), `get_foreground_process_name`
  (Windows heuristic; unavailable for WSL/SSH panes), `format-tab-title`,
  shell-integration user vars.
- wezterm issue #3137 — `get_foreground_process_name()` returns `wslhost.exe`
  for WSL panes; maintainer: "essentially impossible"; recommended approach is
  OSC/user-vars.
- Microsoft Learn — "Working across file systems" (\\wsl.localhost, interop,
  Windows tools from WSL keep their Windows identity) and "Advanced settings
  configuration in WSL" (wsl.conf interop: `enabled=false` blocks Windows
  binaries; `appendWindowsPath=false` blocks PATH bridging; WSLENV is the only
  env-var bridge — hence the fork's `env KEY=VAL` forward, which just missed one
  var).
- opencode v2 database schema — read directly from the live `opencode.db`
  (`session_v2`, `session_message`, …), which supersedes the `session`/`part`
  schema the fork encodes.

---

## Suggested implementation order

1. **Socket discovery fix** (discovery.rs publish/resolve) — unblocks CLI
   everywhere incl. worktree; tiny change, big payoff. Regression test:
   `cli list` with clean env.
2. **Worktree WSL route env forwarding** (`WEZTERM_UNIX_SOCKET`) + shell
   selection (5c) + fzf note.
3. **Reopen-last-window** (critical): startup herd scan, already-live semantics
   (pane-live vs app-live), outcome toast with reasons; live repro with an
   opencode-in-Ubuntu session started from TGZTerminal.
4. **Resume rows → OwnTab** (new target + config key + Alt-click wiring).
5. **Tab titles**: launch-time title injection + `wslhost` fallback mapping +
   generic-title filter in herd names.
6. **opencode v2 schema** migration in `opencode.rs`.
7. **Lua knobs + docs** (table above), update `ci/wezterm-default.lua`.

## Live test checklist (machine is ready)

- [ ] `wsl.exe -l -v` shows Ubuntu running; opencode/claude found via the
      fork's two-stage probe (verified: claude via `sh -lc`, opencode via
      `bash -lic` — the probe's design holds on this machine).
- [ ] Launch built GUI → start opencode in Ubuntu via launcher → tab title shows
      `opencode` (not `wslhost.exe`).
- [ ] Sessions dropdown rows show transcript titles (`slug`/`title` from
      `session_v2`), not `cmd`/`wslhost.exe`.
- [ ] Clicking an old session opens a zoomed pane in its own tab.
- [ ] Reopen-last-window: start agent panes, close window cleanly, relaunch,
      click → sessions reopen in new tabs + toast; second click is a no-op.
- [ ] Worktree button inside an Ubuntu pane: repo listing appears (git present
      in Ubuntu), selecting a file opens a split via CLI (socket fix), cd works,
      close works.
- [ ] `tgzterminal.exe cli list` (no env) connects after the discovery fix.
