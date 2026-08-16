<div align="center">

# TGZTerminal

**A fork of [WezTerm](https://github.com/wezterm/wezterm), for people whose therapist has started asking "and how did that make you feel, relative to your `$SHELL`?"**

![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20(beta)-000000?logo=apple&logoColor=white)
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
- [Download & install](#download--install)
- [Staying up to date](#staying-up-to-date)
- [Build from source](#build-from-source)
- [Configuration](#configuration)
- [Build, test & format](#build-test--format)
- [Cutting a release](#cutting-a-release)
- [Branding](#branding)
- [Upstream & license](#upstream--license)

## Features

| | Feature | What it does | Why an admin would care |
|---|---|---|---|
| 🗂️ | **Vertical sidebar** | Docked, resizable replacement for the top tab bar — configurable width, position, density, title source and auto-hide, plus type-to-filter search. Labels step down to shorter forms when a column gets tight instead of clipping mid-word. | Twenty panes stop looking like twenty identical rectangles. |
| 🌲 | **Pane rows** | Split tabs get a chevron that expands into indented pane rows: click to focus, hover for a per-pane `×`, `(remote)` on anything running on another host. Which tabs are expanded survives a restart. | The tab list finally admits that a "tab" is three panes, one of which is the one you actually wanted. |
| 🤖 | **Agent awareness** | Vendor-neutral detection of running coding agents (Claude, Codex, Gemini, OpenCode, Copilot, Cursor, Amp, …). Every signal is ranked by strength — user vars, then process, then title, then the agent's own on-screen chrome — so a brand word in ordinary output can't badge a plain shell, and an established badge doesn't flip when the agent retitles itself. Surfaces status, model and token/cost metadata, with a status dot that pulses while the agent works. | You stop `Cmd+Tab`-ing between six windows to find out which agent is stuck waiting on you — and the badge tells the truth. |
| 🚀 | **Agent launcher** | Sidebar button that starts a fresh agent. The menu is *discovered*, not configured — an agent shows up only if its CLI actually resolves on `PATH` or in the usual install dirs (`~/.local/bin`, `~/.claude/local`, `~/.bun/bin`, Homebrew, …). Left-click launches the default, Alt-click flips split↔new-tab, right-click lists everything installed plus a sticky **Project root** toggle. | One click to put an agent beside the shell you were already in, in the directory you were already in — including from an SSH pane, where it runs *locally* by default because that's where the CLI and its credentials live. |
| 🔭 | **Agent insight pane** | A real split pane — not an overlay — listing every agent this terminal can see, grouped by project, with what each one is *doing right now* (`now: Bash cargo check`), an expandable log of its recent tool calls, and its subagents. `f` focuses an agent, `s` stops it, and the pane stays open while you do. | "Is it still working, or is it waiting on me?" answered without switching to the pane and squinting at the last twenty lines — and the `now:` label downgrades itself to `last:` rather than lie about a stale transcript. |
| 🧰 | **Agent toolbelt** | Per-pane strip with a live status dot plus context actions — Copy conversation · Stop · Attach · Resume · open logs · compose input. | The nuclear-launch-codes buttons (Stop/Resume/Attach) are locked behind a config flag and real evidence, not vibes — see below. |
| ➕ | **New-tab dropdown** | Chevron beside `+ New Tab` opens a grouped picker of what this machine actually has: discovered shells (`/etc/shells`, or PowerShell / PowerShell 7 / cmd / Git Bash on Windows), every registered domain including each WSL distro, and your own `launch_menu` entries. | Opening a tab in a specific distro or on a specific host stops being a `wezterm cli` invocation you have to remember. |
| 📁 | **File-browser pane** | Lightweight worktree browser that also works inside SSH sessions. | You can look at a file tree on a box three hops away without giving up your terminal identity. |
| ⌨️ | **Rich input composer** | On-demand bottom-anchored modal plus a persistent Warp-style docked input strip. Both insert plain text references only and submit visible text as ordinary bracketed-paste input. | Multi-line prompts stop looking like you're arguing with `readline`. |
| 🪟 | **WSL-aware paths** | Launching across a domain boundary carries the working directory with it: Windows→WSL hands the path to `wsl.exe --cd`, WSL→Windows becomes `C:\…` or a `\\wsl.localhost\…` UNC path, and a path with no meaning in the target is dropped instead of guessed. | The agent starts in the repo you were looking at, not in `/home/you` or `C:\Windows\System32`. |

> **Security model** — Control actions (Stop / Resume / Attach) are gated behind **both**
> an explicit config opt-in **and** trusted evidence (process name or explicit user
> variable) — never visible terminal text alone. A pane printing the word "claude" in a
> `cat`'d log file does not get to press your buttons. Copy actions are always
> user-initiated and need no trust gate — copying is not a privileged operation, it's
> just Tuesday. Both halves of the gate are required: trusted evidence on its own does
> not unlock anything, and neither does flipping the flag on a pane nobody can identify.
> The launcher is deliberately outside that gate for the same reason:
> its argv comes from config only, never from pane titles or visible text, and it
> only ever runs on a click. Starting a new process is not the same category of act
> as reaching into a session you merely *think* you detected.

## Download & install

Prebuilt binaries are attached to every [GitHub Release](../../releases). No package
manager ritual required, no `brew install` incantation to memorize and forget.

### macOS

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

Two artifacts, both published with every release plus a `.sha256` beside each. SmartScreen
will warn you once either way, because the build is unsigned — **More info → Run anyway**.

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

- **macOS** — open the dmg, drag onto **Applications**, replace the old copy.
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

Fork keys live alongside standard WezTerm config in `~/.config/wezterm/wezterm.lua` — no
new config file, no new format, no new place to lose track of. A minimal setup that gets
you most of the way there:

```lua
local wezterm = require 'wezterm'
local config = wezterm.config_builder()

-- Vertical sidebar instead of the top tab bar
config.sidebar_enabled = true
config.sidebar_position = 'Left'
config.sidebar_tab_metadata = { 'GitBranch', 'WorkingDirectory' }

-- Vertical sidebar instead of the top tab bar (width is 2x-calibrated and
-- scales on 1x displays; a drag-resize is per-session, not persisted)
config.sidebar_width_px = 400

-- Agent awareness + toolbelt
config.agent_ui = {
  enabled = true,
  show_sidebar_badges = true,
  show_pane_toolbelt = true,
  enable_control_actions = false, -- flip this on only once you trust the blast radius
  trust_visible_evidence = true,  -- let an agent's own on-screen chrome identify it

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

  -- The agent insight pane (dropdown entry, or bind ShowAgentHerd)
  insight = {
    side = 'Left',               -- 'Left' | 'Right' | 'Top' | 'Bottom'
    split_size_percent = 30,
    show_activity = true,        -- read transcripts for "what is it doing now"
    activity_history = 30,
  },
}

-- Grouped shell/domain picker on the sidebar's + New Tab chevron
config.new_tab_menu = { enabled = true, show_shells = true, show_domains = true }

-- Rich multiline input
config.rich_input = { enabled = true, docked = true }

return config
```

Built-in adapters (Claude, Codex, Gemini, OpenCode, Copilot, Cursor, Amp) work out of the
box and can be overridden or extended per-adapter — nobody's forcing you to run exactly
the agent roster we picked. See
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

Further reading, for when "read the code" isn't fast enough:
[`docs/TGZTERMINAL_REBUILD_SPEC.md`](docs/TGZTERMINAL_REBUILD_SPEC.md),
[`docs/AGENT_TOOLBELT_PLAN.md`](docs/AGENT_TOOLBELT_PLAN.md),
[`docs/RICH_INPUT_PLAN.md`](docs/RICH_INPUT_PLAN.md), and
[`docs/REPO_AUDIT_FIX_PLAN.md`](docs/REPO_AUDIT_FIX_PLAN.md) — the last one is the honest
list of what's still rough, kept up to date instead of pretending everything's fine.

## Cutting a release

Releases are driven by **`tgz-v*`** git tags — the same scheme the in-app updater
compares (`tgz-vYYYY.MM.PATCH`). Push a tag, walk away, come back to a release:

```sh
git tag tgz-v2026.07.2
git push origin tgz-v2026.07.2
```

- [`tgzterminal-release.yml`](.github/workflows/tgzterminal-release.yml) builds the
  macOS universal `.dmg`.
- [`tgzterminal-windows-release.yml`](.github/workflows/tgzterminal-windows-release.yml)
  builds the Windows per-user installer and the portable `.zip`, with a `.sha256` for
  each. Both are required: the run fails rather than publishing a partial release.

The macOS release signs with a real certificate when three repository secrets are set,
and falls back to ad-hoc signing when they aren't:

| Secret | What goes in it |
|---|---|
| `MACOS_CERT_P12` | base64 of a code-signing `.p12` (`base64 -i cert.p12 \| pbcopy`) |
| `MACOS_CERT_PASSWORD` | the `.p12` export password |
| `MACOS_SIGN_IDENTITY` | e.g. `Developer ID Application: You (TEAMID)` |

Worth doing: signing every release with the *same* certificate is the only thing that
lets users keep their folder-access grants across updates.

Both workflows bake the tag into the app's self-reported version (via a generated `.tag` file), so
a shipped build knows whether a later release supersedes it — no more "wait, which build
am I even running" archaeology.
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
