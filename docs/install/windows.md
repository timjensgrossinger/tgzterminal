## Installing on Windows

64-bit Windows 10.0.17763 (1809) or later is required, because the terminal
depends on [ConPTY, first released in
10.0.17763](https://devblogs.microsoft.com/commandline/windows-command-line-introducing-the-windows-pseudo-console-conpty/).
Builds are x64; an arm64 machine runs them under x64 emulation, since the
bundled ANGLE, ConPTY, Mesa and fzf binaries are x64-only.

Every release publishes two Windows artifacts and a `.sha256` beside each:

| Artifact | What it is |
|---|---|
| `TGZTerminal-Setup-<version>.exe` | Installer. Per-user, no admin prompt. |
| `TGZTerminal-windows-portable-<version>.zip` | Extract and run; nothing is registered. |

Both are unsigned, so SmartScreen warns the first time: **More info → Run
anyway**. Verify a download against its sidecar first if you like:

```powershell
Get-FileHash .\TGZTerminal-Setup-<version>.exe -Algorithm SHA256
type .\TGZTerminal-Setup-<version>.exe.sha256
```

### The installer

Installs **for the current user only**, into
`%LOCALAPPDATA%\Programs\TGZTerminal`, and therefore never shows a UAC prompt.
It adds:

- a Start Menu entry (which is also what makes Windows toast notifications work,
  since a toast has to be traced back to a shortcut);
- *Open TGZTerminal here* on the right-click menu for drives, folders, and folder
  backgrounds;
- optionally, and **off by default**, `%LOCALAPPDATA%\Programs\TGZTerminal` on
  your `PATH`, so the `tgzterminal` CLI resolves in any shell. This is a
  checkbox on the *Select Additional Tasks* page and writes only your own
  environment, never the machine's.

Re-running a newer installer upgrades in place. A running TGZTerminal is closed
and reopened for you rather than the install failing on locked files.

Uninstalling removes the program directory, the shortcuts, the context-menu
entries and the `PATH` entry. It does not touch your configuration or state (see
*Where your files live*).

**On a managed machine**, AppLocker or WDAC commonly permits execution only from
`Program Files` and `Windows`, which can block a per-user install outright. Run
the setup **as administrator** and it offers an all-users install instead.

**Upgrading from tgz-v2026.08.4 or earlier.** Those builds installed for all
users and reused upstream WezTerm's application id, so Windows could not tell
TGZTerminal and WezTerm apart — one Apps & Features entry, one install
directory. The current installer has its own id, and offers once to remove that
old all-users copy (which does require admin approval). Decline and nothing
breaks; you simply have two entries until you remove the old one by hand. A
genuine WezTerm install is detected and left completely alone.

### The portable zip

Extract anywhere and run `TGZTerminal.cmd` (or `wezterm-gui.exe` directly).
Nothing is written outside the folder except your own configuration and state.

The zip contains a `.portable` marker file. While that file sits next to the
executables, a `wezterm.lua`, a `colors\` directory and a `wezterm_modules\`
directory **in that same folder** take precedence over the ones in your user
profile — which is the point on a thumb drive shared between machines. Delete
the marker to ignore them and use only your own configuration.

The installer never ships that marker, so an installed build always reads your
configuration and never the program directory. `TGZTERMINAL_PORTABLE=1` forces
portable mode on and `TGZTERMINAL_PORTABLE=0` forces it off, for a layout the
marker does not describe.

Note that `--config-file` and `WEZTERM_CONFIG_FILE` still outrank everything
above, in both modes.

### Where your files live

None of this is inside the install directory, so upgrading and uninstalling
leave it alone:

| What | Where |
|---|---|
| Configuration | `%USERPROFILE%\.wezterm.lua` or `%USERPROFILE%\.config\wezterm\wezterm.lua` |
| UI state, recent items, update cache | `%APPDATA%\wezterm` |
| Cache | `%LOCALAPPDATA%\wezterm` |
| Sockets and logs | `%USERPROFILE%\.local\share\wezterm` |

### Which executable is which

| File | Role |
|---|---|
| `wezterm-gui.exe` | The terminal itself. What the shortcut and the context menu launch. |
| `tgzterminal.exe` | The CLI (`tgzterminal cli ...`, `tgzterminal start ...`). |
| `wezterm-mux-server.exe` | Headless multiplexer server. |
| `TGZTerminal.cmd` | Portable-zip convenience launcher for `wezterm-gui.exe`. |

The GUI and mux server keep their upstream names because other parts of the
program locate each other by those names.
