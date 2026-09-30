-- TGZTerminal default configuration.
--
-- Every setting below is commented out and shows its built-in default, so this
-- file changes nothing until you edit it. Uncomment a line to change that
-- setting. The installer writes this file only when you have no config yet,
-- and never overwrites or removes it, so your edits survive upgrades.
--
-- TGZTerminal is WezTerm under the hood, so everything documented at
-- https://wezterm.org/config/files.html applies here too (key assignments,
-- panes, multiplexing, the lot).

local config = {}

-- ---------------------------------------------------------------------------
-- Sidebar
-- ---------------------------------------------------------------------------

-- Which side hosts the tab sidebar: 'Left' or 'Right'.
-- config.sidebar_position = 'Left'

-- Show the sidebar. Setting this false restores the classic top tab bar.
-- config.sidebar_enabled = true

-- Collapsed rail width when the sidebar auto-hides, in pixels.
-- (Calibrated for a 2x display; a 1x display scales it down.)
-- config.sidebar_collapsed_width_px = 48

-- Expanded sidebar width, in pixels. Dragging the sidebar's edge overrides it.
-- config.sidebar_width_px = 400

-- Keep the sidebar collapsed until the mouse hovers it. The panel slides
-- open over the terminal and tucks itself away again when the mouse leaves.
-- The toggle button in the sidebar flips this and remembers your choice.
-- config.sidebar_auto_hide = true

-- Where the sidebar chrome takes its colours from:
--   'Auto'              the colour scheme's tab_bar colours, else its window
--                       background/foreground, else TGZTerminal's dark look
--   'FollowColorScheme' the same as 'Auto', spelled out
--   'Modern'            always TGZTerminal's dark look
--   'Brand'             the palette a branded build was compiled with
-- config.sidebar_theme = 'Auto'

-- ---------------------------------------------------------------------------
-- Agents
-- ---------------------------------------------------------------------------

-- Where a resumed session (picked from the launcher's "Resume session" list)
-- opens: 'NewTab' gives it the whole tab, 'SplitPane' splits the active pane,
-- 'Zoomed' splits in and zooms. Fresh launches follow open_in.
-- config.agent_ui.launcher.resume_open_in = 'NewTab'

-- How far back the "Resume session" list and the sessions dropdown reach, in
-- days (0 = no age limit), and a safety cap on how many rows they may list
-- (0 hides them). Both dropdowns scroll once the list outgrows them.
-- config.agent_ui.launcher.resume_menu_max_age_days = 30
-- config.agent_ui.launcher.resume_menu_sessions = 1000

-- Pin the WSL distro agents launch into (and the worktree picker's WSL
-- fallback uses) instead of the first registered one, e.g. 'Ubuntu'.
-- config.agent_ui.launcher.wsl_distro = 'Ubuntu'

-- Name the tab an adapter's panes get, e.g. for agents running inside WSL
-- where the pane's process would otherwise read as the wslhost shim.
-- config.agent_ui.adapters.claude.tab_title = 'Claude'

-- ---------------------------------------------------------------------------
-- File browser (worktree)
-- ---------------------------------------------------------------------------

-- Where the worktree picker runs: 'Auto' prefers the target pane's own
-- distro then Git Bash, 'Wsl' always runs it inside WSL, 'GitBash' demands
-- Git for Windows.
-- config.file_browser.shell = 'Auto'

-- Distro for the picker when the target pane is not a WSL pane; falls back
-- to agent_ui.launcher.wsl_distro.
-- config.file_browser.wsl_distro = 'Ubuntu'

-- ---------------------------------------------------------------------------

return config
