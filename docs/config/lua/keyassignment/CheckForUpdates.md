# ``CheckForUpdates``

Asks GitHub for the latest release of TGZTerminal, right now, without waiting
for the next scheduled [`check_for_updates`](../config/check_for_updates.md)
poll.

Unlike the periodic check, this one always reports back:

* a newer release exists — a notification naming the version; clicking it
  downloads the release artifact for your platform (`TGZTerminal.dmg`,
  `TGZTerminal-Setup.exe`, or the portable `.zip`), falling back to the release
  page when the release publishes nothing for this platform
* you are already on the latest release — a short "up to date" notification
* this is a development build — development builds carry a git-derived version
  that cannot be compared against a release tag, so it reports the latest
  release and offers the download rather than guessing

The request runs on a background thread and never downloads or installs
anything by itself.

This action is also available from the command palette and the Help menu.

```lua
config.keys = {
  -- CTRL-SHIFT-U checks for updates
  { key = 'U', mods = 'CTRL|SHIFT', action = wezterm.action.CheckForUpdates },
}
```
