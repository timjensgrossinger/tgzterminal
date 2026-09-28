-- TGZTerminal default configuration.
--
-- This file is installed only if it does not already exist, so feel free to
-- edit it: TGZTerminal follows these settings on every launch and your edits
-- survive upgrades.
--
-- TGZTerminal is WezTerm under the hood, so everything documented at
-- https://wezterm.org/config/files.html applies here too (key assignments,
-- panes, multiplexing, the lot).
--
-- Local shorthand used below:
--   local config = {}
--   return config

local config = {}

-- ---------------------------------------------------------------------------
-- Sidebar
-- ---------------------------------------------------------------------------

-- Which side hosts the tab sidebar: "Left" or "Right".
config.sidebar_position = 'Left'

-- Show the sidebar. Setting this false restores the classic top tab bar.
config.sidebar_enabled = true

-- Collapsed rail width when the sidebar auto-hides, in pixels.
-- (Calibrated for a 2x display; a 1x display scales it down.)
-- config.sidebar_collapsed_width_px = 48

-- Expanded sidebar width, in pixels.
-- config.sidebar_width_px = 400

-- Keep the sidebar collapsed until the mouse hovers it. The panel slides
-- open over the terminal and tucks itself away again when the mouse leaves.
config.sidebar_auto_hide = true

-- Theme of the sidebar chrome: "Auto" (follow dark/light), "Light", "Dark"
-- or "Brand" (the TGZTerminal accent palette).
-- config.sidebar_theme = 'Auto'

-- ---------------------------------------------------------------------------

return config
