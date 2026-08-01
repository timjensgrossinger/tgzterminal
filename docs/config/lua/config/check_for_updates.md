---
tags:
  - updates
---
# `check_for_updates` & `check_for_updates_interval_seconds`

TGZTerminal checks for a new stable release on GitHub by default and shows a
TGZTerminal-branded notification when a newer release is found. Clicking the
notification downloads the release artifact for your platform:
`TGZTerminal.dmg` on macOS, `TGZTerminal-Setup.exe` (or the portable
`TGZTerminal-windows-portable-<tag>.zip`, if a release did not produce an
installer) on Windows. On platforms with no published artifact the click opens
the release page instead.

The accompanying banner in the first pane always links the release page, so the
release notes stay one click away.

NOTE that TGZTerminal never downloads or installs anything on its own — you
install the downloaded artifact the usual way. Your configuration and UI state
live outside the application bundle and install directory, so they survive an
upgrade unchanged. No TGZTerminal usage data are collected as part of the
check; see [PRIVACY.md](https://github.com/timjensgrossinger/tgzterminal/blob/main/PRIVACY.md).

Set `check_for_updates` to `false` to disable this completely, or set
`check_for_updates_interval_seconds` for a different interval between checks
(the default is once per day).

```lua
config.check_for_updates = true
config.check_for_updates_interval_seconds = 86400
```

You can also check on demand without waiting for the interval: run
**Check for updates** from the command palette or the Help menu, or bind the
`CheckForUpdates` action. Unlike the periodic check, it always reports back —
including when you are already up to date.

```lua
config.keys = {
  { key = 'U', mods = 'CTRL|SHIFT', action = wezterm.action.CheckForUpdates },
}
```
