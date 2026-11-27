<div align="center">

# TGZTerminal

**A fork of [WezTerm](https://github.com/wezterm/wezterm), for people whose therapist has started asking "and how did that make you feel, relative to your `$SHELL`?"**

![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20(beta)%20%7C%20Debian-000000?logo=apple&logoColor=white)
![Built on WezTerm](https://img.shields.io/badge/built%20on-WezTerm-4E49EE)
![Language](https://img.shields.io/badge/language-Rust-CE422B?logo=rust&logoColor=white)
![License](https://img.shields.io/badge/license-MIT-green)
![Additive fork](https://img.shields.io/badge/upstream-strictly%20additive-brightgreen)
![Uptime](https://img.shields.io/badge/uptime-since%20last%20forced%20reboot-lightgrey)

</div>

---

## A note from whoever is on call

You ever notice how some people "use a terminal" and some people *live* in one? Three
panes open, one of them tailing a log nobody's looked at since the incident that named
it, a second one running `top` out of habit rather than need, and a third one where an
AI agent has been quietly grinding through a refactor for the last twenty minutes while
you pretend to also be doing something.

TGZTerminal is what happens when that second kind of person gets tired of squinting at
which tab is which, forgetting which pane the agent is actually in, and alt-tabbing to a
browser tab to remember what `git status` said five minutes ago.

It keeps the entire upstream WezTerm engine untouched — GPU renderer, multiplexer,
escape-sequence handling, Lua config, all of it, still exactly WezTerm under the hood —
and bolts on the parts a person who never leaves the terminal actually wants: a sidebar
that behaves like a real tab list, awareness of the coding agents running in your panes,
a one-click way to start a new agent right where you're already standing, a file browser
that doesn't require you to `cd` and pray, and an input box that isn't a single
unforgiving line.

Nothing here changes what upstream WezTerm keys do. If you've been running WezTerm
config since 2019, it still works exactly the same. We just added furniture to the room.

## Contents

- [Features](#features)
- [Knowing what your agents are doing](#knowing-what-your-agents-are-doing)
- [Settings without the Lua](#settings-without-the-lua)
- [From the keyboard](#from-the-keyboard)
- [Download & install](#download--install)
- [Staying up to date](#staying-up-to-date)
- [Build from source](#build-from-source)
- [Configuration](#configuration)
- [Build, test & format](#build-test--format)
- [Cutting a release](#cutting-a-release)
- [Branding](#branding)
- [Upstream & license](#upstream--license)

## Features

### The sidebar

| | Feature | What it does | Why you'd care |
|---|---|---|---|
| 🗂️ | **Vertical sidebar** | Docked, resizable replacement for the top tab bar: width, position, density, title source and auto-hide are all yours, plus type-to-filter search and a collapsed icon rail. Each tab row has two lines: what it is, then the agent and its state on the left (`Claude waiting`) and the git branch on the right. Labels step down to shorter forms when a column gets tight instead of clipping mid-word. | Twenty panes stop looking like twenty identical rectangles. |
| 📂 | **Tab groups** | New tabs file themselves under the project folder they start in, with a header that counts how many of its agents are waiting on you. Drag tabs between groups, collapse a group, or turn grouping off for one flat list. | Four repos open at once read as four projects, not one long list. |
| 🌲 | **Pane rows** | Split tabs get a chevron that expands into indented pane rows: click to focus, hover for a per-pane `×`, `(remote)` on anything running on another host. Which tabs are expanded survives a restart. | The tab list finally admits that a "tab" is three panes, one of which is the one you actually wanted. |
| 🎛️ | **A footer you arrange** | Three rows by default: view toggles (**Files**, **Diff**, **Tools**, **Lock**), a launch row (agent launcher, **SSH**, **Layouts**) and **+ New Tab** with its shortcut beside it. Drag buttons into the order you want, add more from a library, or define your own that run a command. New Tab is filled with the accent or drawn plain, your call. | The buttons you use are where your hand already is; the ones you don't are gone. |
| ➕ | **New-tab dropdown** | Right-clicking `+ New Tab` opens a grouped picker of what this machine actually has: discovered shells (`/etc/shells`, or PowerShell / PowerShell 7 / cmd / Git Bash on Windows), every registered domain including each WSL distro, your own `launch_menu` entries and your own pane layouts. | Opening a tab in a specific distro or on a specific host stops being a `wezterm cli` invocation you have to remember. |

### Agents

| | Feature | What it does | Why you'd care |
|---|---|---|---|
| 🤖 | **Agent awareness** | Vendor-neutral detection of running coding agents (Claude, Codex, Gemini, OpenCode, Copilot, Cursor, Amp, Antigravity). Every signal is ranked by strength (user vars, then process, then title, then the agent's own on-screen chrome), so a brand word in ordinary output can't badge a plain shell, and an established badge doesn't flip when the agent retitles itself. | You stop `Cmd+Tab`-ing between six windows to find out which agent is stuck waiting on you, and the badge tells the truth. |
| ✨ | **Status rings** | Every agent's tab carries a ring: an arc turning while it works, the whole ring in amber while it waits for you, one ripple when it finishes. It follows macOS **Reduce Motion**, holding still as a full ring in the running colour. Every animation has its own switch. See [below](#knowing-what-your-agents-are-doing). | Peripheral vision does the work. You know something finished without reading a word. |
| 🔔 | **Waiting queue** | Agents that stop and wait for you are tracked as a queue, not a vibe: a count badge on the macOS dock icon while the app is unfocused, a throttled notification, a chip on the collapsed rail, and `CycleWaitingAgent` to walk them. An agent is "acknowledged" simply by you activating its pane; there's no dismiss button to hunt for. | The thing that has been blocked on your input for eleven minutes stops being a thing you discover by accident. |
| 🚀 | **Agent launcher** | Sidebar button that starts a fresh agent. The menu is *discovered*, not configured: an agent shows up only if its CLI actually resolves on `PATH` or in the usual install dirs (`~/.local/bin`, `~/.claude/local`, `~/.bun/bin`, Homebrew, …). Left-click launches the default, Alt-click flips split↔new-tab, right-click lists everything installed plus a sticky **Project root** toggle and recent sessions to resume. | One click puts an agent beside the shell you were already in, in the directory you were already in, including from an SSH pane, where it runs *locally* by default because that's where the CLI and its credentials live. |
| 🔭 | **Agents section** | A section at the foot of the sidebar listing every agent this terminal can see, grouped by project, with what each one is *doing right now* (`now: Bash cargo check`), expandable rows carrying its recent tool calls, subagents and token/cost telemetry. Agents needing attention can sort to the top. Reads each vendor's own session store, including OpenCode's SQLite database. | "Is it still working, or is it waiting on me?" answered without switching to the pane, and the `now:` label downgrades itself to `last:` rather than lie about a stale transcript. |
| Σ | **Usage at a glance** | The **Σ** button shows how much of your plan's tightest window is used, as a thin meter that turns amber only as it runs out. Click it for the full picture in proper columns: Claude's 5-hour and weekly windows with their reset times, last session cost, OpenCode spend, tokens per open session, totals, and a tokens-per-day chart over 7, 14 or 30 days. | You find out you're at 97% of the 5-hour window before the agent does. |
| 🧰 | **Agent toolbelt** | Per-pane strip with a live status dot plus context actions: Copy conversation, Stop, Attach, Resume, open logs, compose input. Drag it where it doesn't cover your prompt. | The nuclear-launch-codes buttons (Stop/Resume/Attach) are locked behind a config flag and real evidence, not vibes. See below. |

### Panes, files and hosts

| | Feature | What it does | Why you'd care |
|---|---|---|---|
| 🧩 | **Pane layouts** | A **Layouts** footer button and `Cmd+Opt+L` open a menu of presets (Columns, Rows, Main left, Main top, Grid, Agent + shells) plus **Balance panes**. A preset rearranges the panes the tab already has, adds shells in the current directory when it needs more, and never closes anything. iTerm2/cmux-style keys on top of upstream's: `Cmd+D` / `Cmd+Shift+D` split right / down, `Cmd+Opt+Arrows` move focus, `Cmd+Ctrl+Arrows` resize, `Cmd+Shift+Return` zoom, `Cmd+Opt+=` balance, `Cmd+Opt+1…6` apply a preset. Your own bindings still win; `tgz_pane_keys = false` drops ours. Define layouts of your own with a command per pane (see [Pane layouts](#pane-layouts)), and drive it all from scripts with `cli apply-layout` / `cli balance-panes`. | "Agent on the left, a shell and a log on the right" is one click instead of three splits and a minute of dragging dividers. |
| ± | **Changes panel** | Diffs beside the pane, from git (or a folder of working copies, or no version control at all). In session mode it shows only the files this agent session touched. Docks left or right, raises above a growing prompt if you ask it to. | You read what the agent changed while it's still changing it, not after the commit. |
| 📁 | **Worktree browser** | A file tree beside your panes (or a fuzzy picker), with quick preview, open-in-editor and a right-click menu. Also works inside SSH sessions. | You can look at a file tree on a box three hops away without giving up your terminal identity. |
| 🔐 | **SSH quick-launch** | An SSH button that connects to your usual host in one click, a filterable host menu, and a **New SSH connection** form (password or key, SSH, Mosh or Eternal Terminal) that adds the `Host` block to `~/.ssh/config` for you. | `ssh prod` without remembering whether it was `prod`, `prod-1` or `prod.internal`. |
| ⌨️ | **Rich input composer** | On-demand bottom-anchored modal plus a persistent Warp-style docked input strip. Both insert plain text references only and submit visible text as ordinary bracketed-paste input. | Multi-line prompts stop looking like you're arguing with `readline`. |
| 🪟 | **WSL-aware paths** | Launching across a domain boundary carries the working directory with it: Windows→WSL hands the path to `wsl.exe --cd`, WSL→Windows becomes `C:\…` or a `\\wsl.localhost\…` UNC path, and a path with no meaning in the target is dropped instead of guessed. | The agent starts in the repo you were looking at, not in `/home/you` or `C:\Windows\System32`. |

### Safety

| | Feature | What it does | Why you'd care |
|---|---|---|---|
| 🙈 | **Secret masking** | Token-shaped strings (API keys, access tokens, passwords in URLs) are hidden on screen and left out of the copy menu's "(no secrets)" rows. On by default. | Screen-sharing a terminal stops being a small act of faith. |
| 🔒 | **Secure Keyboard Entry** (macOS) | The **Lock** button holds Secure Keyboard Entry while a TGZTerminal window has focus, and it switches on by itself at password prompts. Keyloggers and input monitors stop seeing what you type. | The same switch Terminal.app and iTerm2 have, without a trip to the menu bar. |

### Experimental: Assist

Off by default and still settling. Switch each part on under **Settings → Assist (experimental)**
or in Lua with `config.assist` (see [`docs/TGZTERMINAL_CONFIG.md`](docs/TGZTERMINAL_CONFIG.md#assist-experimental-ask-panel-command-blocks-terminal-tools-for-agents)).

| | Feature | What it does | Why you'd care |
|---|---|---|---|
| 🧪 | **Ask panel** (`assist.ask`) | `Cmd+I` on a shell pane opens a card that asks Claude (or Codex, or a CLI of your own) about what just happened. Enter on an empty field explains the last failed command. It sends the last few commands with their output and exit codes, the folder and your selection, with secrets masked, and shows you what it sends. Claude runs on your own login, read-only (Read/Grep/Glob in that folder, `.env` and `~/.ssh` refused), with your hooks and plugins left out. Suggested commands carry a label (**Only looks**, **Changes things**, **Deletes**): Enter pastes one at the prompt, `Cmd+Enter` runs it, a delete asks once more. | "Why did that fail?" without copying forty lines of output into another window. Nothing runs that you didn't press a key for. |
| 🧪 | **Command blocks** (`assist.command_blocks`, `assist.shell_integration`) | Shell integration injected into new zsh tabs (your own startup files still run; powerlevel10k marks its own prompt). Failed commands get a red bar beside them, `Cmd+Up` / `Cmd+Down` jump between prompts, `Cmd+Shift+C` copies the last output. | The command that failed three screens ago is one keystroke away. |
| 🧪 | **Terminal tools for agents** (`assist.mcp`) | `wezterm cli get-blocks` prints a pane's recent commands with output and exit codes; `wezterm cli mcp` serves the same to agents as an MCP server, secrets masked. *Connect terminal tools to Claude Code* in the palette registers it. Pasting a command into a pane is a separate opt-in and never presses Enter. | Claude in one pane can see what failed in the other without you pasting it. |

> **Security model**: Control actions (Stop / Resume / Attach) are gated behind **both**
> an explicit config opt-in **and** trusted evidence (process name or explicit user
> variable), never visible terminal text alone. A pane printing the word "claude" in a
> `cat`'d log file does not get to press your buttons. Copy actions are always
> user-initiated and need no trust gate: copying is not a privileged operation, it's
> just Tuesday. Both halves of the gate are required: trusted evidence on its own does
> not unlock anything, and neither does flipping the flag on a pane nobody can identify.
> The launcher is deliberately outside that gate for the same reason:
> its argv comes from config only, never from pane titles or visible text, and it
> only ever runs on a click. Starting a new process is not the same category of act
> as reaching into a session you merely *think* you detected.

## Knowing what your agents are doing

The sidebar tries to answer one question from across the room, without you reading
anything: **is it working, did it finish, or is it waiting on me?**

| State | What you see |
|---|---|
| **Working** | An arc turns around the ring on the agent's tab, in the running colour (the accent). It holds through the small pauses inside a single turn, so it doesn't stutter every time the agent redraws its prompt box mid-thought. |
| **Waiting for you** | The whole ring goes amber and breathes slowly; the second line says `Claude waiting` in amber and the group header counts it. It stays until you activate that pane. |
| **Just finished** | The arc stops and resolves: one ripple swells outward, fades, and leaves a faint trace on the row. |
| **Newly appeared** | The row slides in rather than popping into existence, so a new agent doesn't make the list jump under your cursor. |
| **Reduced motion** | With macOS Reduce Motion on (or animations off), nothing moves: a working agent shows a still, full ring in the running colour, a waiting one a still amber ring. Running and waiting still read apart by colour alone. |

Three behaviours worth knowing about because they're deliberate:

- **Only motion that carries state is on by default.** The turning arc, the waiting
  breath, the finish ripple and the slide-in are on. The status-dot pulse and the active
  rail's breathe move without saying anything new, so they're off unless you ask.
- **Motion continues while the window is unfocused.** The whole point is seeing agent
  state from another window. Turn it off with `animations.unfocused = false` if you'd
  rather a background window cost nothing.
- **The waiting outline is state, not motion.** It survives `animations.enabled = false`
  and Reduce Motion, because switching off animation should stop things moving, not
  blind you to which agent is blocked. It has its own `waiting_glow` toggle if you
  genuinely want it gone.

Every switch is in **Settings → Agents → Animations**, or in Lua:

```lua
config.agent_ui = {
  ring_colors = 'RedBlue',   -- RedBlue | OrangeCyan | TanSage | BrownSlate

  animations = {
    enabled              = true,  -- master switch for all sidebar motion
    follow_reduce_motion = true,  -- hold still while macOS asks for reduced motion
    unfocused            = true,  -- keep animating when the window isn't focused
    working_ring         = true,
    resolve_ripple       = true,
    waiting_glow         = true,
    agent_enter          = true,
    rail_breathe         = false,
    dot_pulse            = false,

    -- All optional. Anything unset falls back to the ring_colors preset.
    colors = {
      ring_a       = '#ff6b6b',
      ring_b       = '#58a6ff',
      resolve      = '#305b8c',
      waiting_glow = '#ff6b6b',
      dot_pulse    = '#ff6b6b',
    },
  },
}
```

Colour values accept hex, `rgb(…)` and CSS/X11 names, same as every other colour key.
Leave `resolve` unset and it stays derived from the cool end of your chosen pair at 55%
brightness, so a custom pair doesn't leave the "finished" trace looking like it came
from a different design.

> **Upgrading?** `agent_ui.pulse_working_dot` used to be the single kill switch for all
> of this. It still is, for as long as you have not written an `animations` table. The
> moment you add one (even an empty `animations = {}`, or a flip of any Animations
> switch in Settings), `animations.enabled` takes over as the master switch and
> `pulse_working_dot` goes back to meaning only what its name says. Nobody who already
> switched motion off gets it silently switched back on.

## Settings without the Lua

`Cmd+,` opens **Settings** as a tab: the fork's settings plus the basics (font, size,
line height, colour scheme, opacity), grouped as Appearance, Colours, Sidebar, Agents,
Worktree, Changes, Input, Privacy, Buttons and Shortcuts, where you click an action
and press the keys to rebind it. On/off
settings are switches, lists open a filterable picker, colours take any value `colors`
accepts and show a swatch.

Every change is written straight into your `wezterm.lua`, in place where the file
already sets that key and in a small managed block where it doesn't. Keys you set from
code (a function, a computed value) are shown as such and left alone, with a link to the
line. The Lua file stays the single source of truth; Settings is just a nicer pen.

**Colours** has one field per part of the chrome (`sidebar_colors`): the accent, the
attention amber, the keyboard focus ring (a soft lavender by default, never white, so it
doesn't glare at night), New Tab, the rings, every row, field and text tone.

## From the keyboard

Everything the sidebar's buttons do has a key assignment, each one is in the command
palette, and most have a default. On macOS they use `Cmd`; elsewhere the same letters
with `Super`. Your own `config.keys` entries win over all of these.

| Keys | Does |
|---|---|
| `Cmd+,` | Settings |
| `Cmd+Shift+E` | Put the keyboard on the sidebar: `Tab` / `Shift+Tab` walk the tabs and buttons, `Enter` acts, `Esc` goes back |
| `Cmd+Shift+F` | Search tabs (then `Tab` or `↓` moves on into the sidebar) |
| `Cmd+Shift+J` | Jump to the next agent that's waiting on you |
| `Ctrl+Shift+A` | Walk the Agents section with the arrow keys |
| `Cmd+Shift+A` / `Cmd+Shift+G` | Start the default agent / open the agent menu |
| `Cmd+Shift+U` | Usage popup |
| `Cmd+Shift+H` | SSH host menu, ready to type a filter |
| `Cmd+Shift+N` | The `+ New Tab` shell/domain menu |
| `Cmd+Opt+L` | Pane layouts |
| `Cmd+Shift+B` | Pin the sidebar open, or let it collapse to its rail |

Whatever has keyboard focus wears the same lavender ring, in the sidebar, in menus and
in Settings. The terminal never loses `Tab` unless you sent the keyboard to the sidebar
on purpose.

## Download & install

Prebuilt binaries are attached to every [GitHub Release](../../releases). Homebrew
(macOS) and winget (Windows) are optional and skip the "unidentified developer" prompts.

### macOS

With Homebrew (no Gatekeeper prompt, since the cask clears the quarantine flag):

```sh
brew trust timjensgrossinger/tap
brew install timjensgrossinger/tap/tgzterminal
```

`brew upgrade` picks up new releases. Or install by hand:

1. Download `TGZTerminal.dmg` from the latest release.
2. Open it, drag **TGZTerminal** onto **Applications**, feel a brief sense of
   accomplishment.
3. First launch: right-click the app → **Open** (the build is ad-hoc signed unless the
   release was built with a signing certificate, so Gatekeeper wants a manual blessing
   exactly once). If macOS is still being difficult:

   ```sh
   xattr -dr com.apple.quarantine /Applications/TGZTerminal.app
   ```

   Yes, this is the "turn it off and on again" of code signing. It works.
4. Also on first launch, macOS asks once for access to **Documents**, **Desktop** and
   **Downloads**. That's deliberate: the app touches each folder up front so the
   prompts land during setup instead of ambushing you twenty minutes into an incident,
   the first time a pane's cwd happens to be `~/Documents` and the sidebar looks for
   git state. Say no and nothing breaks — the sidebar just stays quiet about those
   directories.

Universal binary — runs natively on Apple Silicon and Intel, no Rosetta tax.

> **If a release re-asks for folder access after every update**, that release was
> signed ad-hoc. macOS pins privacy grants of an unsigned bundle to the binary's code
> hash, which changes on every single build. Nothing you can fix from the outside; the
> signing knobs are in [Cutting a release](#cutting-a-release).

### Windows (beta)

Two artifacts, both published with every release plus a `.sha256` beside each.

**Download warning.** Edge says the setup "is not commonly downloaded", and SmartScreen
may say "Windows protected your PC". Both come from the build being unsigned: Windows
judges unsigned files by how often *that exact file* has been downloaded, and every release
is a new file, so the warning comes back with each version. It is not a detection. To get
past it, choose **Keep** (Edge: **⋯ → Keep → Show more → Keep anyway**) and then
**More info → Run anyway**, or clear the mark before running it:

```powershell
Unblock-File .\TGZTerminal-Setup-<version>.exe
```

Installing with **winget** avoids the prompt altogether, because winget downloads outside
the browser (once the package is listed; see *Distribution* below):

```powershell
winget install TimGrossinger.TGZTerminal
```

**Installed** — `TGZTerminal-Setup-<version>.exe`. Installs **for your user only**, into
`%LOCALAPPDATA%\Programs\TGZTerminal`, with **no admin prompt**. You get a Start Menu
entry and *Open TGZTerminal here* on right-click in Explorer. Adding `tgzterminal` to your
PATH is an unchecked box in the installer — tick it if you want the CLI from other shells.
Upgrades replace the previous install in place.

**Portable** — `TGZTerminal-windows-portable-<version>.zip`. Extract anywhere and run
**`TGZTerminal.cmd`**. The zip contains a `.portable` marker file, which is what makes a
`wezterm.lua`, `colors\` or `wezterm_modules\` sitting next to the binaries take
precedence over your user config — handy on a thumb drive. Delete the marker to use only
your own config. The installed build deliberately has no marker and ignores its program
directory, so a file dropped there can't override anybody's config.

> **Upgrading from tgz-v2026.08.4 or earlier?** Those builds installed for *all users* and
> shared upstream WezTerm's application id, so Windows treated the two as one app. The new
> installer offers, once, to remove that old all-users copy (that step does need admin
> approval). If you had a `wezterm.lua` inside the old install folder it offers to copy it
> into your user profile first, since an installed build no longer reads that folder.

> **On a managed/corporate machine**, policy often only allows programs to run from
> `Program Files`, which can block a per-user install outright. Launch the setup as
> administrator and it offers an all-users install instead.

> Windows support is new and comes from the exact same additive fork. If it does
> something weird, open an issue with the release version — not a screenshot of your
> desktop wallpaper, the actual version.

**If it does not start**, it now says why in a dialog, and names its log file:
`%USERPROFILE%\.local\share\wezterm\wezterm-gui.exe-log-<pid>.txt`. Attach that file to
the issue. For more detail, start it from a terminal with logging turned up:

```powershell
$env:WEZTERM_LOG = "debug"
& "$env:LOCALAPPDATA\Programs\TGZTerminal\wezterm-gui.exe" start --always-new-process
```

On a machine without a working GPU driver (a VM, a basic display adapter) it falls back
from the system OpenGL to the bundled ANGLE renderer by itself. If even that fails,
`front_end = "Software"` in your config uses the bundled software renderer.

#### Distribution: signing and winget

Every Windows build is installed and launched on a real Windows runner before a release
is published — once as installed, once with a WSL default domain — and the release is held
back if it shows no window, an error dialog, a console window or a panic.

Two things are wired into `tgzterminal-windows-release.yml` and stay off until configured:

- **Code signing (SignPath Foundation).** Free for open-source projects. Apply at
  [signpath.org](https://signpath.org), publish the code-signing policy they ask for, and
  create artifact configurations named `binaries` (the four `.exe` files, uploaded flat)
  and `installer` (the Setup exe). Then set the repository variables
  `SIGNPATH_ORGANIZATION_ID`, `SIGNPATH_PROJECT_SLUG`, `SIGNPATH_SIGNING_POLICY_SLUG` and
  the secret `SIGNPATH_API_TOKEN`. Signed releases build SmartScreen reputation under one
  publisher, so the warning fades out and then stays away across versions — which
  unsigned builds can never do.
- **winget.** The first version has to be submitted once by hand: fork
  `microsoft/winget-pkgs`, then run `wingetcreate new <Setup-exe-URL>` with the
  identifier `TimGrossinger.TGZTerminal`, installer type `inno`, scope `user`. After it
  is merged, add a classic personal access token with `public_repo` scope as the
  secret `WINGET_TOKEN`, and every release opens the winget-pkgs update PR itself.

### Ubuntu and Debian

Download `tgzterminal_<version>_amd64.deb` from the latest release, then install it
with:

```sh
sudo apt install ./tgzterminal_<version>_amd64.deb
```

Package targets Ubuntu 22.04+ and Debian 12+ on amd64. It installs `tgzterminal`,
keeps `wezterm` as a compatibility command, registers a desktop entry, and replaces
upstream `wezterm` packages when present.

## Staying up to date

TGZTerminal reuses WezTerm's built-in update check, pointed at this repo instead. When a
newer release ships you get a notification naming the version; **clicking it downloads
the artifact for your platform** — `TGZTerminal.dmg` on macOS, and on Windows whichever
kind you are already running: the installer for an installed copy, the portable `.zip`
for a portable one. The banner in the first pane links the release page, for when you
want the notes first.

Install it the same way you installed the last one:

- **macOS** — `brew upgrade tgzterminal` if you installed with Homebrew; otherwise open
  the dmg, drag onto **Applications**, replace the old copy.
- **Windows** — run `TGZTerminal-Setup-<version>.exe`; it upgrades in place and closes a
  running instance for you. On the portable zip, extract over the old folder (keeping the
  `.portable` marker).
- **Ubuntu/Debian** — run `sudo apt install ./tgzterminal_<version>_amd64.deb`; apt upgrades
  the installed package in place.

Then **fully quit and relaunch** — an already-running window will not pick up a new
binary, no matter how hard you believe in it.

**Your settings survive.** Nothing user-owned lives inside the app bundle or the install
directory: `wezterm.lua`, the sidebar/UI state and everything else sit in your home
directory and are untouched by an upgrade.

Checking is **on by default**, once a day. It reads public GitHub release metadata over
HTTPS and nothing else — no telemetry, no phone-home, no automatic install. It asks "is
there a newer tag" and that's the entire conversation. Turn it off, or change the
cadence, in `wezterm.lua`:

```lua
config.check_for_updates = false
config.check_for_updates_interval_seconds = 86400 -- once a day, not once a heartbeat
```

Impatient? **Check for updates** in the command palette (or the Help menu) asks right
now, and — unlike the background check — tells you when you're already current.

## Build from source

Build the macOS app bundle with the committed script — do **not** hand-assemble the
bundle yourself, that way lies a broken `Info.plist` and a Saturday you won't get back:

```sh
# Fast local iteration — host arch only
ci/build-macos-bundle.sh --native

# Universal binary + dist/TGZTerminal.dmg
ci/build-macos-bundle.sh --universal --dmg
```

Then install and launch:

```sh
cp -R dist/TGZTerminal.app /Applications/
open /Applications/TGZTerminal.app
```

### Sign local builds once, stop re-approving permissions

macOS pins the privacy (TCC) grants of an **ad-hoc** signed bundle to the binary's code
directory hash, so every single rebuild silently revokes them and the app re-prompts for
Documents / Desktop / Downloads on the next launch. Rebuild ten times in an afternoon,
answer ten rounds of dialogs. Create one stable local identity instead — the build
script picks it up automatically (Developer ID if you have one, else this self-signed
cert, else ad-hoc with a loud warning):

```sh
ci/macos-signing-cert.sh create      # one-time; asks for your login password
ci/macos-signing-cert.sh status      # what builds will sign with
ci/macos-signing-cert.sh reset-tcc   # forget the grants so the app asks once more
```

The bundle is signed inside-out (nested Mach-O first, bundle last) rather than with
`--deep`, which Apple discourages and which does not reliably reseal nested code — an
unstable signature is an unstable TCC identity, which puts you right back in the
dialog loop.

On Windows, build the binaries with `cargo build --release -p wezterm-gui` (plus
`wezterm`, `wezterm-mux-server`, `strip-ansi-escapes`) — the release workflow has the
full packaging steps if `cargo build` alone leaves you a pile of loose `.exe` files.

## Configuration

Fork keys live alongside standard WezTerm config in `~/.config/wezterm/wezterm.lua`: no
new config file, no new format, no new place to lose track of. Everything below is also
in the Settings tab, and every key is optional. A setup that shows most of what there is:

```lua
local wezterm = require 'wezterm'
local config = wezterm.config_builder()

-- Vertical sidebar instead of the top tab bar
config.sidebar_enabled = true
config.sidebar_position = 'Left'
config.sidebar_tab_metadata = { 'GitBranch', 'WorkingDirectory' }
config.sidebar_tab_grouping = 'Auto'      -- 'Manual' or 'Off' for one flat list
config.sidebar_new_tab_style = 'Filled'   -- or 'Plain'

-- Width is 2x-calibrated and scales on 1x displays; a drag-resize is
-- per-session, not persisted
config.sidebar_width_px = 400

-- Chrome colours: set only what you want to change
config.sidebar_colors = {
  accent = '#58a6ff',
  focus_ring = '#a99bd6',
}

-- Agent awareness + toolbelt
config.agent_ui = {
  enabled = true,
  show_sidebar_badges = true,
  show_pane_toolbelt = true,
  enable_control_actions = false, -- flip this on only once you trust the blast radius
  trust_visible_evidence = true,  -- let an agent's own on-screen chrome identify it

  -- See "Knowing what your agents are doing" for the full set
  ring_colors = 'RedBlue',
  animations = { follow_reduce_motion = true },

  -- Agents that stop and wait for you
  waiting_notification = true,
  dock_badge = true,              -- macOS dock count while unfocused

  -- Sidebar button that starts a fresh agent session
  launcher = {
    enabled = true,
    default_adapter = 'claude',  -- nil/"" = first installed agent wins
    cwd = 'ActivePane',          -- or 'ProjectRoot' (walks up to .git/.hg/.svn/.jj)
    open_in = 'SplitPane',       -- or 'NewTab'; Alt-click uses the other one
    split_direction = 'Horizontal',
    split_size_percent = 50,
    remote_behavior = 'ForceLocal', -- SSH pane? run the agent here anyway
    -- domain = 'WSL:Ubuntu',    -- pin a distro if you have several
  },

  -- The agents section at the foot of the sidebar
  section = {
    enabled = true,
    refresh_ms = 1000,            -- clamped to 100..=10000
    show_activity = true,         -- read transcripts for "what is it doing now"
    show_tokens = true,           -- pane-reported token/cost telemetry
    show_quota = true,            -- plan limits on the Σ button and in its popup
    show_non_interactive = false, -- SDK harnesses and hook children stay hidden
    sort_attention_first = true,  -- blocked/waiting agents float to the top
  },
}

-- Grouped shell/domain picker behind a right-click on the sidebar's + New Tab
config.new_tab_menu = { enabled = true, show_shells = true, show_domains = true }

-- Rich multiline input
config.rich_input = { enabled = true, docked = true }

-- Diffs beside the pane
config.diff_panel = { enabled = true, position = 'Right', default_mode = 'Session' }

return config
```

### Pane layouts

Presets rearrange the tab you're in; layouts of your own open a new tab with a command
per pane. Both live in the **Layouts** footer button (`Cmd+Opt+L`), and your own also
appear in the right-click `+ New Tab` menu and the command palette.

```lua
config.tgz_layouts = {
  {
    name = 'Review',
    layout = 'AgentPlusShell',   -- any preset; the first pane takes the main slot
    cwd = 'ProjectRoot',         -- or 'ActivePane' (the default)
    panes = {                    -- each is a SpawnCommand; {} is a plain shell
      { args = { 'claude' } },
      { args = { 'lazygit' } },
      {},
    },
  },
}

-- Bind one to a key, or a preset to the tab you're in:
-- { key = 'r', mods = 'CMD|ALT', action = wezterm.action.OpenPaneLayout 'Review' }
-- { key = 'g', mods = 'CMD|ALT', action = wezterm.action.ApplyPaneLayout 'Grid' }
config.tgz_pane_keys = true      -- Cmd+D and friends; false keeps upstream's keys only
```

From a script or another tool:

```sh
tgzterminal cli apply-layout main-left   # the calling pane takes the main slot
tgzterminal cli apply-layout grid --no-fill   # don't add shells to fill the grid
tgzterminal cli balance-panes
```

Built-in adapters (Claude, Codex, Gemini, OpenCode, Copilot, Antigravity, Cursor, Amp)
work out of the box and can be overridden or extended per-adapter — nobody's forcing you
to run exactly the agent roster we picked. See
**[`docs/TGZTERMINAL_CONFIG.md`](docs/TGZTERMINAL_CONFIG.md)** for the full reference of
every fork-specific key, written for the version of you that's debugging this at 2am and
does not want prose, just the field name and the default. It ends with a
*What is not configurable* section listing the chrome that has no key at all — read that
before going looking for one.

## Build, test & format

```sh
cargo check                                                  # fast type-check for iteration

cargo build -p wezterm -p wezterm-gui -p wezterm-mux-server  # main binaries
cargo build --release -p wezterm-gui                         # release GUI

make test                                                    # all tests (cargo-nextest)
cargo nextest run -p wezterm-escape-parser                   # no_std crate, run separately

cargo +nightly fmt --all -- --check                          # formatting (nightly required)
```

If `cargo +nightly fmt` complains that stable rejects an option — that's expected,
`.rustfmt.toml` uses nightly-only settings on purpose. Install nightly, don't fight it.

Debugging agent detection is the one place where "read the code" is genuinely slower than
asking the binary. Both of these log ids, enum labels and booleans only — never pane
contents or transcript text:

```sh
WEZTERM_LOG=wezterm_gui::termwindow::render::sidebar=debug tgzterminal
```

`agent detect:` shows what each pane resolved to and why; `sidebar agent row:` shows
whether an animation actually fired and, if not, which gate stopped it.

Further reading, for when "read the code" isn't fast enough:
[`docs/TGZTERMINAL_REBUILD_SPEC.md`](docs/TGZTERMINAL_REBUILD_SPEC.md) for the
behaviour-level spec of every fork surface, and
[`docs/TGZTERMINAL_CONFIG.md`](docs/TGZTERMINAL_CONFIG.md) for the key-by-key reference.

## Cutting a release

Releases are driven by **`tgz-v*`** git tags — the same scheme the in-app updater
compares (`tgz-vYYYY.MM.PATCH`). Push a tag, walk away, come back to a release:

```sh
git tag tgz-v2026.08.10
git push origin tgz-v2026.08.10
```

- [`tgzterminal-release.yml`](.github/workflows/tgzterminal-release.yml) builds the
  macOS universal `.dmg`.
- [`tgzterminal-windows-release.yml`](.github/workflows/tgzterminal-windows-release.yml)
  builds the Windows per-user installer and the portable `.zip`, with a `.sha256` for
  each. Both are required: the run fails rather than publishing a partial release.
- [`tgzterminal-debian-release.yml`](.github/workflows/tgzterminal-debian-release.yml)
  builds the `.deb` for Ubuntu/Debian on amd64.

The macOS release signs with a real certificate when three repository secrets are set,
and falls back to ad-hoc signing when they aren't:

| Secret | What goes in it |
|---|---|
| `MACOS_CERT_P12` | base64 of a code-signing `.p12` (`base64 -i cert.p12 \| pbcopy`) |
| `MACOS_CERT_PASSWORD` | the `.p12` export password |
| `MACOS_SIGN_IDENTITY` | e.g. `Developer ID Application: You (TEAMID)` |

Worth doing: signing every release with the *same* certificate is the only thing that
lets users keep their folder-access grants across updates.

All release workflows bake the tag into the app's self-reported version (via a generated
`.tag` file), so a shipped build knows whether a later release supersedes it — no more
"wait, which build am I even running" archaeology.
[`tgzterminal-build.yml`](.github/workflows/tgzterminal-build.yml) is CI only
(main / PRs) and publishes nothing — it just tells you if you broke something before you
find out the hard way.

## Branding

Product name and update/release repo are compile-time overridable via `BRAND_*`
environment variables (see `wezterm-gui/src/brand.rs` and the branding section of
`docs/TGZTERMINAL_CONFIG.md`). With no overrides set, the build resolves to the default
TGZTerminal values and upstream behavior is unchanged — rebranding is a build flag, not a
fork of a fork.

## Upstream & license

TGZTerminal tracks upstream WezTerm and preserves its attribution and license in full. All
credit for the terminal engine, GPU renderer, and multiplexer goes to the
[WezTerm project](https://github.com/wezterm/wezterm) — this fork adds a room onto a
house someone else built well. Licensed under the [MIT License](LICENSE.md), same terms
as upstream.

See [`PRIVACY.md`](PRIVACY.md) for the privacy policy and EU/GDPR notice — short version:
nothing leaves your machine unless you explicitly opt into the update checker.

---

<div align="center">

*Built by people who have, at some point, debugged production over SSH from a phone.*

</div>
