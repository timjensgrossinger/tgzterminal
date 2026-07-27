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
a file browser that doesn't require you to `cd` and pray, and an input box that isn't a
single unforgiving line.

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
| 🗂️ | **Vertical sidebar** | Docked, resizable replacement for the top tab bar — configurable density, title source, auto-hide, and search. | Twenty panes stop looking like twenty identical rectangles. |
| 🤖 | **Agent awareness** | Vendor-neutral detection of running coding agents (Claude, Codex, Gemini, OpenCode, Copilot, Cursor, Amp, …) from process, title, or explicit OSC user variables. Surfaces status, model, and token/cost metadata. Detection is cached per pane to stay off the render hot path. | You stop `Cmd+Tab`-ing between six windows to find out which agent is stuck waiting on you. |
| 🧰 | **Agent toolbelt** | Per-pane strip with a live status dot plus context actions — Copy conversation · Stop · Attach · Resume · open logs · compose input. | The nuclear-launch-codes buttons (Stop/Resume/Attach) are locked behind a config flag and real evidence, not vibes — see below. |
| 📁 | **File-browser pane** | Lightweight worktree browser that also works inside SSH sessions. | You can look at a file tree on a box three hops away without giving up your terminal identity. |
| ⌨️ | **Rich input composer** | On-demand bottom-anchored modal plus a persistent Warp-style docked input strip. Both insert plain text references only and submit visible text as ordinary bracketed-paste input. | Multi-line prompts stop looking like you're arguing with `readline`. |

> **Security model** — Control actions (Stop / Resume / Attach) are gated behind **both**
> an explicit config opt-in **and** trusted evidence (process name or explicit user
> variable) — never visible terminal text alone. A pane printing the word "claude" in a
> `cat`'d log file does not get to press your buttons. Copy actions are always
> user-initiated and need no trust gate — copying is not a privileged operation, it's
> just Tuesday.

## Download & install

Prebuilt binaries are attached to every [GitHub Release](../../releases). No package
manager ritual required, no `brew install` incantation to memorize and forget.

### macOS

1. Download `TGZTerminal.dmg` from the latest release.
2. Open it, drag **TGZTerminal** onto **Applications**, feel a brief sense of
   accomplishment.
3. First launch: right-click the app → **Open** (the build is ad-hoc signed, so
   Gatekeeper wants a manual blessing exactly once). If macOS is still being difficult:

   ```sh
   xattr -dr com.apple.quarantine /Applications/TGZTerminal.app
   ```

   Yes, this is the "turn it off and on again" of code signing. It works.

Universal binary — runs natively on Apple Silicon and Intel, no Rosetta tax.

### Windows (beta)

Download `TGZTerminal-windows-portable-<version>.zip` from the latest release, extract
anywhere, run **`TGZTerminal.cmd`** (or `wezterm-gui.exe` directly if you enjoy typing
extensions). No installer, no registry entries you'll regret later. SmartScreen will warn
you once because the build is unsigned — **More info → Run anyway**.

> Windows support is new and comes from the exact same additive fork. If it does
> something weird, open an issue with the release version — not a screenshot of your
> desktop wallpaper, the actual version.

## Staying up to date

TGZTerminal reuses WezTerm's built-in update check, pointed at this repo instead. When a
newer release ships, the app shows an **"update available"** banner linking to the
release page. Download the new `.dmg` / `.zip`, replace your copy (drag over the old app
on macOS, extract over the old folder on Windows), then **fully quit and relaunch** — a
new window will not magically pick up a new binary, no matter how hard you believe in it.

It's **off by default**, because nobody who lives in a terminal wants a surprise popup
mid-incident. Turn it on in `wezterm.lua` if you want it:

```lua
config.check_for_updates = true
config.check_for_updates_interval_seconds = 86400 -- once a day, not once a heartbeat
```

The check only ever reads public GitHub release metadata over HTTPS — no telemetry, no
phone-home, no automatic install. It asks "is there a newer tag" and that's the entire
conversation.

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

-- Agent awareness + toolbelt
config.agent_ui = {
  enabled = true,
  show_sidebar_badges = true,
  show_pane_toolbelt = true,
  enable_control_actions = false, -- flip this on only once you trust the blast radius
}

-- Rich multiline input
config.rich_input = { enabled = true, docked = true }

return config
```

Built-in adapters (Claude, Codex, Gemini, OpenCode, Copilot, Cursor, Amp) work out of the
box and can be overridden or extended per-adapter — nobody's forcing you to run exactly
the agent roster we picked. See
**[`docs/TGZTERMINAL_CONFIG.md`](docs/TGZTERMINAL_CONFIG.md)** for the full reference of
every fork-specific key, written for the version of you that's debugging this at 2am and
does not want prose, just the field name and the default.

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
  builds the Windows portable `.zip` (and a best-effort installer).

Both bake the tag into the app's self-reported version (via a generated `.tag` file), so
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
