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

return config
