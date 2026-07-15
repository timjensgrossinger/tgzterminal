---
tags:
  - updates
---
# `check_for_updates` & `check_for_updates_interval_seconds`

TGZTerminal disables update checking by default. This private preview expects
manual rebuild, sign, and reinstall updates for the macOS bundle.

If you explicitly enable `check_for_updates`, TGZTerminal checks regularly for a
new stable release on GitHub and shows a TGZTerminal-branded notification when a
newer release is found.

NOTE that it doesn't automatically download or install the release. No
TGZTerminal usage data are collected as part of this.

Keep `check_for_updates` as `false` to disable this completely or set
`check_for_updates_interval_seconds` for an alternative update interval after
opting in.

```lua
config.check_for_updates = false
config.check_for_updates_interval_seconds = 86400
```
