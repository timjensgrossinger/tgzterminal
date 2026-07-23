<div align="center">

# TGZTerminal

**A macOS-first fork of [WezTerm](https://github.com/wezterm/wezterm), tuned for working alongside terminal-based AI coding agents.**

![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20(beta)-000000?logo=apple&logoColor=white)
![Built on WezTerm](https://img.shields.io/badge/built%20on-WezTerm-4E49EE)
![Language](https://img.shields.io/badge/language-Rust-CE422B?logo=rust&logoColor=white)
![License](https://img.shields.io/badge/license-MIT-green)
![Additive fork](https://img.shields.io/badge/upstream-strictly%20additive-brightgreen)

</div>

---

TGZTerminal keeps the upstream WezTerm terminal engine, GPU renderer, multiplexer, and
Lua config model **fully intact**, and layers on a set of workflow-focused features for
driving coding agents like Claude, Codex, and Gemini from the terminal.

Everything the fork adds is **strictly additive**: no upstream WezTerm config key changes
behavior, and internal crate/binary names remain `wezterm` / `wezterm-gui`. The shipped
bundle is `dist/TGZTerminal.app`.

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

| | Feature | What it does |
|---|---|---|
| 🗂️ | **Vertical sidebar** | Docked, resizable replacement for the top tab bar — configurable density, title source, auto-hide, and search. |
| 🤖 | **Agent awareness** | Vendor-neutral detection of running coding agents (Claude, Codex, Gemini, OpenCode, Copilot, Cursor, Amp, …) from process, title, or explicit OSC user variables. Surfaces status, model, and token/cost metadata. Detection is cached per pane to stay off the render hot path. |
| 🧰 | **Agent toolbelt** | Per-pane strip with a live status dot plus context actions — Copy conversation · Stop · Attach · Resume · open logs · compose input. |
| 📁 | **File-browser pane** | Lightweight worktree browser that also works inside SSH sessions. |
| ⌨️ | **Rich input composer** | On-demand bottom-anchored modal plus a persistent Warp-style docked input strip. Both insert plain text references only and submit visible text as ordinary bracketed-paste input. |

> **Security model** — Control actions (Stop / Resume / Attach) are gated behind **both**
> an explicit config opt-in **and** trusted evidence (process name or explicit user
> variable) — never visible terminal text alone. Copy actions are always user-initiated
> and need no trust gate.

## Download & install

Prebuilt binaries are attached to every [GitHub Release](../../releases).

### macOS

1. Download `TGZTerminal.dmg` from the latest release.
2. Open it and drag **TGZTerminal** onto **Applications**.
3. First launch: right-click the app → **Open** (the build is ad-hoc signed, so
   Gatekeeper needs this once). If macOS still blocks it, run
   `xattr -dr com.apple.quarantine /Applications/TGZTerminal.app`.

Universal binary — runs natively on Apple Silicon and Intel.

### Windows (beta)

Download `TGZTerminal-windows-portable-<version>.zip` from the latest release,
extract anywhere, and run **`TGZTerminal.cmd`** (or `wezterm-gui.exe` directly).
No installation required. SmartScreen may warn on first run (unsigned build) —
choose **More info → Run anyway**.

> Windows support is new and built from the same additive fork; if you hit a
> platform issue, open an issue with the release version.

## Staying up to date

TGZTerminal reuses WezTerm's built-in update check, pointed at this repo. When a
newer release is published, the app shows an **"update available"** banner
linking to the release page — download the new `.dmg` / `.zip` and replace your
copy (on macOS, drag over the old app; on Windows, extract over the old folder).
Then **fully quit and relaunch** — a new window alone won't pick up a new binary.

Enable the check in `wezterm.lua` (off by default):

```lua
config.check_for_updates = true
config.check_for_updates_interval_seconds = 86400 -- once a day
```

The check only ever reads public GitHub release metadata; it never downloads or
installs anything automatically.

## Build from source

Build the macOS app bundle with the committed script (do **not** hand-assemble it):

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
`wezterm`, `wezterm-mux-server`, `strip-ansi-escapes`); the release workflow shows
the full packaging steps.

## Configuration

Fork keys live alongside standard WezTerm config in `~/.config/wezterm/wezterm.lua`.
A minimal setup:

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
  enable_control_actions = false, -- opt in to Stop/Resume/Attach
}

-- Rich multiline input
config.rich_input = { enabled = true, docked = true }

return config
```

Built-in adapters (Claude, Codex, Gemini, OpenCode, Copilot, Cursor, Amp) work out of the
box and can be overridden or extended per-adapter. See
**[`docs/TGZTERMINAL_CONFIG.md`](docs/TGZTERMINAL_CONFIG.md)** for the full reference of
every fork-specific key.

## Build, test & format

```sh
cargo check                                                  # fast type-check
cargo build -p wezterm -p wezterm-gui -p wezterm-mux-server  # main binaries
cargo build --release -p wezterm-gui                         # release GUI

make test                                                    # all tests (cargo-nextest)
cargo nextest run -p wezterm-escape-parser                   # no_std crate, run separately

cargo +nightly fmt --all -- --check                          # formatting (nightly required)
```

Further reading: [`docs/TGZTERMINAL_REBUILD_SPEC.md`](docs/TGZTERMINAL_REBUILD_SPEC.md),
[`docs/AGENT_TOOLBELT_PLAN.md`](docs/AGENT_TOOLBELT_PLAN.md),
[`docs/RICH_INPUT_PLAN.md`](docs/RICH_INPUT_PLAN.md), and
[`docs/REPO_AUDIT_FIX_PLAN.md`](docs/REPO_AUDIT_FIX_PLAN.md).

## Cutting a release

Releases are driven by **`tgz-v*`** git tags — the same scheme the in-app updater
compares (`tgz-vYYYY.MM.PATCH`). Pushing a tag builds and publishes both
platforms to one GitHub Release:

```sh
git tag tgz-v2026.07.1
git push origin tgz-v2026.07.1
```

- [`tgzterminal-release.yml`](.github/workflows/tgzterminal-release.yml) builds the
  macOS universal `.dmg`.
- [`tgzterminal-windows-release.yml`](.github/workflows/tgzterminal-windows-release.yml)
  builds the Windows portable `.zip` (and a best-effort installer).

Both bake the tag into the app's self-reported version (via a generated `.tag`
file), so a shipped build knows whether a later release supersedes it.
[`tgzterminal-build.yml`](.github/workflows/tgzterminal-build.yml) is CI only
(main / PRs) and publishes nothing.

## Branding

Product name and update/release repo are compile-time overridable via `BRAND_*` environment
variables (see `wezterm-gui/src/brand.rs` and the branding section of
`docs/TGZTERMINAL_CONFIG.md`). With no overrides set, the build resolves to the default
TGZTerminal values and upstream behavior is unchanged.

## Upstream & license

TGZTerminal tracks upstream WezTerm and preserves its attribution and license. See the
[WezTerm project](https://github.com/wezterm/wezterm) for the terminal engine, renderer, and
multiplexer this fork is built on. Licensed under the [MIT License](LICENSE.md), the same
terms as upstream WezTerm.
