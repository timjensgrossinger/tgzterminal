# Privacy Policy for TGZTerminal

No data about your device(s) or TGZTerminal usage leave your device by default.

## Data Maintained by TGZTerminal

TGZTerminal maintains some historical data, such as recent searches or action
usage, in some of its overlays such as the debug overlay and character
selector, in order to make your usage more convenient. It is used only
by the local process, and care is taken to limit access for the associated
files on disk to only your local user identity.

TGZTerminal tracks the output from the commands that you have executed in
a scrollback buffer.  At the time of writing, that scrollback buffer
is an in-memory structure that is not visible to other users of the machine.
In the future, if wezterm expands to offload scrollback information to
your local disk, it will do so in such a way that other users on the
same system will not be able to inspect it.

## macOS and Data permissions

On macOS, when a GUI application that has a "bundle" launches child processes
(eg: TGZTerminal, running your shell, and your shell running the programs which you
direct it to run), any permissioned resource access that may be attempted by
those child processes will be reported as though TGZTerminal is attempting to
access those resources.

The result is that from time to time you may see a dialog about TGZTerminal
accessing your Contacts if run a `find` command that happens to step through
the portion of your filesystem where the contacts are stored.  Or perhaps you
are running a utility that accesses your camera; it will appear as though
TGZTerminal is accessing those resources, but it is not: there is no logic within
TGZTerminal to attempt to access your contacts, camera or any other sensitive
information.

## Agent UI and Clipboard Actions

Agent detection is passive and local. Copy actions are user-initiated and copy a
bounded number of recent scrollback lines; copied text may include any terminal
output visible in that range, including secrets printed by shell commands or
agent tools.

Resume, attach, and details-opening controls are disabled unless TGZTerminal
sees trusted agent evidence or you explicitly enable control actions. Adapter
detail paths are opened only after a user click; Claude log paths are
canonicalized and must resolve under `~/.claude/projects` before they are
opened.

## Update Checking

TGZTerminal disables automatic update checking by default. While this private
preview is manually installed, updates are expected to come from rebuilding,
signing, and reinstalling the local macOS bundle.

If you explicitly set `check_for_updates = true`, the compatibility update
checker may make an HTTP request to GitHub's release API. The request uses a
TGZTerminal User-Agent string and release notifications use TGZTerminal-branded
text.

## Third-Party Builds

The above is true of this TGZTerminal source tree and local builds produced from
it.

If you obtained a pre-built TGZTerminal binary from some other source be aware that
the person(s) building those versions may have modified them to behave
differently from the source version.
