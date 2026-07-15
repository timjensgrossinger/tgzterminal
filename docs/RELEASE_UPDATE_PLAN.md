# Release Engineering and Auto-Update Plan

## Goal

Make TGZTerminal installable, updatable, and rebasable so people other than
the author can run it daily. Signed macOS builds, an update channel, and a
scripted upstream-rebase workflow that keeps the fork tax bounded.

This is Track 6. It has no dependency on the agent tracks and can run in
parallel with them.

## Non-Goals

- No Windows or Linux release polish in V1 (macOS-first stays).
- No Mac App Store distribution.
- No silent background installs; updates always require user confirmation.
- No telemetry/phone-home beyond the update check itself.

## Current Starting Point

- Private preview branch; update story intentionally manual.
- Upstream WezTerm CI exists in `.github/workflows`; the build jobs are gated behind `if: github.repository == 'wezterm/wezterm'` and do not run in this fork.
- Fork-specific commits are few and concentrated (sidebar, agent UI, config).

## Distribution Targets

1. GitHub Releases: signed + notarized universal2 `.app` in a `.dmg`,
   per tag `tgz-vYYYY.MM.patch`.
2. Homebrew cask in a personal tap (`brew install --cask tgrossinger/tap/tgzterminal`).
3. In-app update check (Sparkle) against an appcast fed from GitHub Releases.

## Release Pipeline

CI job on tag push:

1. Build universal2 (aarch64 + x86_64) release bundle, reusing the upstream
   macOS packaging steps (`ci/deploy.sh` path).
2. Codesign with Developer ID Application cert; hardened runtime; entitlements
   as upstream ships them.
3. Notarize (`notarytool submit --wait`) and staple.
4. Produce `.dmg`, generate Sparkle EdDSA signature, upload release assets.
5. Regenerate `appcast.xml` (GitHub Pages branch or release asset) and bump
   the cask in the tap via automated PR.

Secrets required in CI: signing cert (p12), notary API key, Sparkle private
key. Document rotation in `ci/RELEASING.md`.

## Auto-Update (Sparkle)

- Integrate Sparkle 2 via the existing macOS window/app layer
  (`window` crate app delegate), feed it the appcast URL.
- Behavior: check on launch at most once per 24 h; menu item
  "Check for Updates…"; user confirms download + install; relaunch prompt.
- Config: `check_for_updates = true|false` — reuse/repurpose the existing
  upstream option so it gates Sparkle instead of the old upstream check.
- Preserve running sessions: update installs on quit/relaunch; combined with
  mux resume this should feel lossless.

Versioning: `CFBundleVersion` monotonic build number, human version from the
tag. Keep upstream WezTerm version visible in About for provenance.

## Upstream Rebase Strategy

The fork tax is the long-term existential risk. Bound it:

1. Keep all TGZTerminal commits as a linear stack on top of upstream `main`
   (already the case). Never merge upstream in; always rebase the stack.
2. Add `ci/rebase-upstream.sh` (or `just rebase-upstream`):
   - fetch upstream, create `rebase/YYYYMMDD` branch,
   - rebase the TGZ stack, stopping on conflict with a summary of which
     TGZ commit conflicts against which upstream change,
   - on success run the smoke suite below.
3. Smoke suite (scripted, must pass before the rebase branch lands):
   - `cargo check -p wezterm-gui -p config -p wezterm-mux -p wezterm-term`
   - `cargo test -p config -p wezterm-gui`
   - launch, open shell pane, open Claude pane, verify badge + toolbelt
     render (manual checklist in `ci/SMOKE.md` until automated).
4. Cadence: rebase monthly or before each release, whichever comes first.
5. Anything generic (not sidebar/agent related) keeps going upstream as PRs —
   every upstreamed fix shrinks the stack.

## Repo Hygiene for Public Launch

- `PROVENANCE.md` stays; expand with "how this fork tracks upstream".
- README: install section (dmg + brew), 5-minute quickstart
  (install → open Claude Code → see badge), GIF of the waiting-queue flow.
- Delete stray local artifacts (`review_*.json`) and keep them ignored.
- Issue template distinguishing "upstream WezTerm bug" from "TGZ feature".

## Implementation Steps

1. Manual signed+notarized release once, by hand, documenting every step in
   `ci/RELEASING.md` (proves cert/notary setup before automating).
2. Automate as tag-triggered workflow.
3. Add Sparkle integration + appcast generation.
4. Create Homebrew tap + cask automation.
5. Write `rebase-upstream` script + smoke checklist; do one real rebase
   against current upstream to validate.
6. README/quickstart/provenance pass.

## Testing

- Release workflow dry-run on a test tag produces installable, notarized dmg
  (verify with `spctl -a -vv` and Gatekeeper on a clean machine/VM).
- Sparkle update path: install N-1, publish N, confirm prompt + update +
  session resume.
- Cask installs and launches on a machine without dev tools.
- Rebase script correctly stops and reports on a synthetic conflict.

## Acceptance Criteria

- A stranger with a Mac can install and stay current without cloning the repo.
- Updates never install without user confirmation.
- One command starts an upstream rebase and tells you exactly what broke.
- Fork-specific diff stays a reviewable linear stack after each rebase.
