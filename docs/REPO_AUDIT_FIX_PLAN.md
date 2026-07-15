# TGZTerminal Repo Audit — Findings + Correction Plan

**Date:** 2026-07-03

## Context

Audit of the uncommitted TGZTerminal (wezterm fork) changes for logic/performance issues and wiring gaps. Three parallel explorations covered (a) sidebar/agent-UI + termwindow, (b) config + update checker, (c) CI workflows. The new agent-UI feature is broadly well-wired (all new `agent_ui.*` config options are consumed, control-action security gating is defense-in-depth, path-escape guards are tested), but there are 2 serious render-thread performance problems, a dead update checker, and 2 build-breaking CI bugs.

## Findings (ranked)

### Performance — sidebar agent detection (the big one)

- **P1 (HIGH)** `sidebar.rs:1754` `agent_supported_actions` does filesystem I/O (`command_exists_on_path` walks `$PATH` with `fs::metadata`; `resolve_agent_detail_path` does `canonicalize()`×2) on every detection-cache miss — and because `visible_fingerprint` is part of the cache key (`sidebar.rs:1892-1899`), a streaming agent pane misses the cache **every frame**. Blocking FS calls on the paint thread per repaint.
- **P2 (HIGH)** `detect_agent_pane` (`sidebar.rs:1810`) does its expensive work *before* the cache check at `:1901`: `process_agent_match`/`configured_agent_match` each call `merged_agent_adapters()` (`:1683`) which deep-clones all adapter configs (2–3 full clones per call), and `visible_agent_text` (`:1780`) loads up to 120 logical lines into a String. This runs per visible sidebar tab per paint, **including for ordinary non-agent panes** (with default `detect_processes=true`, `should_load_visible_agent_text(false, true, None)` is true → every shell pane's repaint loads 120 lines + regex-matches all `visible_patterns`).
- **P3 (MED)** Active pane detected 2–3× per paint (`paint_agent_toolbelt` via paint.rs:262, `sidebar_compact_tab_icon` :2661, `sidebar_agent_for_tab_idx` :2636) — cache returns same state but pre-cache work reruns each time.
- **P4 (MED)** `prune_agent_detection_cache` (`sidebar.rs:2037`) only called from `paint_sidebar` (`:3195`), which runs only when sidebar active. Toolbelt/copy handlers insert regardless → with sidebar disabled, `HashMap<PaneId,…>` grows unbounded over long sessions.
- **P5 (MED)** Redundant lowercasing: `visible_model_hint` (`:1403`) / `visible_agent_kind_hint` (`:1382`) lowercase the full visible text, then `agent_pattern_matches` (`:196`) lowercases it **again per pattern**. `contains_case_insensitive` (`:155`) allocates 2 Strings per call.
- **P6 (MED)** Toast notification (`wezterm_toast_notification::show`, `sidebar.rs:2015-2025`) fired inline inside `detect_agent_pane`, i.e. during paint — latent stall risk; delivery coupled to repaint cadence.
- **P7 (LOW)** Dead branch: `infer_agent_status_from_visible_text` (`:1410`) — both arms of final `if saw_content` return `AgentStatus::Unknown`.
- **P8 (LOW)** `process_agent_match` largely subsumed by `configured_agent_match` (both scan `process_names` over fresh `merged_agent_adapters()`); `get_dimensions()` called twice per detection.

### Logic — update checker (dead on arrival)

- **U1 (HIGH)** `update.rs:55-57` still queries upstream `api.github.com/repos/wezterm/wezterm/releases/latest`; banner/docs branded TGZTerminal but link goes to a wezterm release. Fork repo is `timjensgrossinger/tgzterminal` (git remote origin).
- **U2 (HIGH)** `update.rs:82,133,184` raw lexicographic tag comparison. Fork scheme `tgz-vYYYY.MM.patch`: `'t' > '2'` → current always "newer" than any upstream tag → update never reported; also `tgz-v2026.07.2 > tgz-v2026.07.10`. (Mitigating: `check_for_updates` defaults false.)
- **U3 (LOW)** `show_update_window.md` still describes a window; option is deprecated no-op (`config.rs:1243-1247`).
- **U4 (LOW)** `mod.rs:981` spawns update-checker thread even when disabled (idle sleeper — acceptable, skip unless trivial).

### Wiring consistency — sidebar adapters

- **W1 (LOW)** `AgentKind::from_hint` (`sidebar.rs:1055`) iterates `built_in_agent_adapters()` not merged — user-added adapters ignored in the explicit `agent.kind` user-var path.
- **W2 (LOW)** `agent_adapter_config` (`:1666`) reads raw config map without built-in merge — divergence risk vs `agent_adapter_config_by_id`; unify.

### CI — build-breaking

- **C1 (HIGH)** `tgzterminal-ci.yml:26,41` uses `actions/checkout@v7` — doesn't exist (latest v5). Both jobs fail at checkout.
- **C2 (HIGH)** `tgzterminal-ci.yml` checkouts lack `submodules: recursive`; repo needs `deps/harfbuzz`, `deps/freetype/*` to build `wezterm-gui`. All gen_*.yml use it.
- **C3 (HIGH, maintenance)** 21× `gen_*.yml` hand-edited (`if: github.repository == 'wezterm/wezterm'` on `build:` job) without updating generator `ci/generate-workflows.py` (build-job template ~lines 1064-1072; it already emits this `if` for `upload:` at :1088). Next regen wipes all 21 edits.
- **C4 (LOW)** `docs/RELEASE_UPDATE_PLAN.md`: `-p lua-api-crates` isn't a package (it's a dir of crates); line 22 claims upstream CI "mostly still works" but build jobs now gated off; references nonexistent `PROVENANCE.md`.

### Verified fine (no action)

All `agent_ui.*` options defined AND consumed; `check_for_updates=false` default matches PRIVACY.md/docs; update check runs on background thread, banner via `spawn_into_main_thread`; `percent-encoding` dep used (mod.rs:55); Claude log-path canonicalize + `starts_with` escape guard tested (:729, :963, test :4750); control actions never trusted from visible-text evidence, re-checked at exec; regex cache bounded; toolbelt/copy/mouse dispatch fully wired; remote file browser coherent; no RefCell nested-borrow risk in detection; 13 unit tests in sidebar, `agent_ui_tests` in config.

## Fix Plan

### Phase 1 — CI (small, unblocks everything)

1. `tgzterminal-ci.yml`: `actions/checkout@v7` → `@v5` (both jobs); add `with: submodules: recursive` to both checkouts. (C1, C2)
2. `ci/generate-workflows.py`: add `if: github.repository == 'wezterm/wezterm'` to the `build:` job template (~line 1064-1072, mirroring the existing upload-job line :1088); rerun `python3 ci/generate-workflows.py` and diff — output must reproduce the 21 hand-edits exactly. (C3)
3. `docs/RELEASE_UPDATE_PLAN.md`: replace `-p lua-api-crates` with real member packages; reword line 22; drop/fix `PROVENANCE.md` reference. (C4)

### Phase 2 — sidebar detection performance (core)

File: `wezterm-gui/src/termwindow/render/sidebar.rs` (+ `mod.rs` for cache entry struct if it lives there).

1. **Time-throttled cache re-check (fixes P1+P2+P3 together):** add `detected_at: Instant` to `AgentDetectionCacheEntry`. In `detect_agent_pane`, build the *cheap* key first (process, title, relevant vars, viewport — no fingerprint). If cheap fields match the cached key AND `detected_at.elapsed() < ~500ms`, return cached state before any adapter merging, visible-text load, or FS probing. Full re-detect (incl. fingerprint + `agent_supported_actions`) runs at most ~2×/sec/pane instead of every frame. Keep fingerprint in the stored key so a full re-check still short-circuits identical content.
2. **Adapter snapshot cache (P2):** cache `merged_agent_adapters()` result in a `RefCell<Option<(config_generation, Arc<Vec<(String, AgentAdapterConfig)>>)>>` on TermWindow; rebuild only when `self.config.generation()` changes. All callers (`process_agent_match`, `configured_agent_match`, `visible_agent_match`, `from_hint` path) take the Arc — kills the per-call deep clones.
3. **Fold `process_agent_match` into `configured_agent_match`** (identical process_names scan) and pass `dims` down to `visible_agent_text` (P8).
4. **Lowercase once (P5):** add `agent_pattern_matches_pre_lowered(haystack_lower, pattern)`; callers that already lowered the text use it. Keep the public fn for un-lowered callers/tests.
5. **Toast off the paint path (P6):** wrap `wezterm_toast_notification::show` in `promise::spawn::spawn_into_main_thread(async move { … }).detach()` (pattern already used in update.rs:136).
6. **Prune everywhere (P4):** call `prune_agent_detection_cache` from `paint_agent_toolbelt` (or unconditionally in the paint pass) so toolbelt-only setups prune too. It's cheap (iterates live panes).
7. **Dead branch (P7):** collapse `if saw_content { Unknown } else { Unknown }` to `AgentStatus::Unknown` (or implement intended distinction if one was meant — check test expectations first).

### Phase 3 — update checker

1. `update.rs`: retarget `get_latest_release_info` (and nightly variant) to `https://api.github.com/repos/timjensgrossinger/tgzterminal/releases/latest`. (U1)
   - Fallback: if the fork repo turns out private/unreleased, stub the checker (early return) until Sparkle per RELEASE_UPDATE_PLAN — decide at implementation time.
2. Add `fn release_tag_is_newer(latest: &str, current: &str) -> bool`: strip optional `tgz-v`/`v` prefixes, split on `.`/`-`, compare numeric components; non-parsable → fall back to string inequality but never flag upstream-format tags as updates for `tgz-` builds. Replace all three comparison sites (`:82`, `:133`, `:184`). Unit-test: `tgz-v2026.07.2 < tgz-v2026.07.10`, upstream `20240203-…` vs `tgz-v…` → no update, equal → no update. (U2)
3. `docs/config/lua/config/show_update_window.md`: state the option is deprecated/no-op. (U3)

### Phase 4 — adapter lookup consistency (small)

1. `AgentKind::from_hint` path (`sidebar.rs:1055`): resolve via merged adapters (use the Phase-2 snapshot). (W1)
2. `agent_adapter_config` (`:1666`): route through `agent_adapter_config_by_id` merge logic; delete raw variant. (W2)

### Ordering

Phases independent; do 1 first (tiny), then 2 (biggest win), 3, 4.

## Verification

1. `cargo build -p wezterm-gui -p config` and `cargo +nightly fmt --all -- --check`.
2. `cargo test -p wezterm-gui termwindow::render::sidebar::tests` (13 existing tests must pass; add tests for throttle-entry reuse, `release_tag_is_newer`, pre-lowered matcher).
3. `cargo test -p config agent_ui_tests`.
4. Regenerate workflows: `python3 ci/generate-workflows.py && git diff --stat .github/workflows` → must be empty (proves C3 fix reproduces hand edits).
5. Manual perf check: run built wezterm-gui, open pane running `claude`, stream output; confirm no per-frame `$PATH` walk (e.g. `sudo fs_usage -f filesys | grep wezterm` shows no metadata storm) and toolbelt still updates within ~0.5s.
6. Update check smoke: `WEZTERM_ALWAYS_SHOW_UPDATE_UI=1` + `check_for_updates=true` → banner links to fork release page.
