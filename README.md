# TGZTerminal

TGZTerminal is a macOS-first fork of [WezTerm](https://github.com/wezterm/wezterm).
It keeps the upstream terminal engine, GPU renderer, multiplexer, and Lua config
model intact, and layers on a set of workflow-focused features aimed at working
alongside terminal-based AI coding agents.

## What the fork adds

- **Vertical sidebar tab surface** — a docked, resizable sidebar replacement for
  the top tab bar, with configurable density, title source, and auto-hide.
- **Vendor-neutral agent awareness** — detects running coding agents (Claude,
  Codex, Gemini, OpenCode, Copilot, Cursor, Amp, …) from process, title, or
  explicit OSC user variables, and surfaces status, model, and token/cost
  metadata. Detection is cached per pane to keep it off the render hot path.
- **Agent toolbelt** — a per-pane strip with a live status dot plus context
  actions (Copy conversation / Stop / Attach / Resume / open logs / compose
  input). Control actions are gated behind both an explicit config opt-in and
  trusted evidence — never visible text alone.
- **File-browser pane** — a lightweight worktree browser that also works inside
  SSH sessions.
- **Rich multiline input composer** — an on-demand bottom-anchored modal plus a
  persistent Warp-style docked input strip. Both insert plain text references
  only and submit visible text as ordinary bracketed-paste input.

Everything above is strictly additive: no upstream WezTerm config key changes
behavior, and internal crate/binary names remain `wezterm` / `wezterm-gui`.

## Build

```sh
cargo check                                                    # fast type-check
cargo build -p wezterm -p wezterm-gui -p wezterm-mux-server     # main binaries
cargo build --release -p wezterm-gui                            # release GUI
```

Build and package the macOS bundle with the committed script (do not hand-assemble it):

```sh
ci/build-macos-bundle.sh --native            # host arch only, fast local iteration
ci/build-macos-bundle.sh --universal --dmg   # both arches via lipo + dist/TGZTerminal.dmg
ci/build-macos-bundle.sh --no-build          # re-assemble dist/ from existing binaries
```

## Test & format

```sh
make test                                    # all tests (cargo-nextest)
cargo nextest run -p wezterm-escape-parser   # no_std crate, run separately
cargo +nightly fmt --all -- --check
```

## Documentation

- `docs/TGZTERMINAL_CONFIG.md` — full config reference for the fork-specific keys.
- `docs/TGZTERMINAL_REBUILD_SPEC.md`, `docs/AGENT_TOOLBELT_PLAN.md`,
  `docs/RICH_INPUT_PLAN.md` — feature specs and plans.

## Branding

Product name and update/release repo are compile-time overridable via `BRAND_*`
environment variables (see `wezterm-gui/src/brand.rs` and the branding section of
`docs/TGZTERMINAL_CONFIG.md`). With no overrides set, the build resolves to the
default TGZTerminal values and upstream behavior is unchanged.

## Upstream & license

TGZTerminal tracks upstream WezTerm and preserves its attribution and license.
See the WezTerm project for the terminal engine, renderer, and multiplexer this
fork is built on. Licensed under the same terms as upstream WezTerm.
