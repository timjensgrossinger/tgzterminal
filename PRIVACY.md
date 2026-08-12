# Privacy Policy for TGZTerminal

No data about your device(s) or TGZTerminal usage leave your device, with one
exception: the update check described under _Update Checking_, which is enabled
by default and can be turned off.

## Data Maintained by TGZTerminal

TGZTerminal maintains some historical data, such as recent searches or action
usage, in some of its overlays such as the debug overlay and character
selector, in order to make your usage more convenient. It is used only
by the local process, and care is taken to limit access for the associated
files on disk to only your local user identity.

When the agent launcher's "Reopen last window" button is enabled (it is by
default, `agent_ui.launcher.restore_last_window_sessions`), TGZTerminal records
which agent sessions a window had open — the adapter id, the vendor's session id
and the working directory — in a `tgz-last-session.json` file in its local data
directory, so those agents can be offered back after a restart. It contains no
conversation content, is written with owner-only permissions, never leaves your
device, and can be deleted at any time; setting the option to `0` stops the file
being written or read at all.

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

TGZTerminal checks for new releases by default, so that you find out about
security and bug fixes without having to watch the repository. Set
`check_for_updates = false` in your configuration to switch it off.

**What is sent.** Periodically (once per day by default, configurable via
`check_for_updates_interval_seconds`) TGZTerminal makes an HTTPS GET request to
GitHub's public release API for this project's repository. The request carries a
User-Agent of `TGZTerminal/<version>` and nothing else: no identifier, no
configuration, no usage data. Like any HTTP request it necessarily discloses
your IP address to GitHub.

**What is stored.** The release metadata GitHub returns is cached in a
`check_update` file in TGZTerminal's local data directory
(`~/Library/Application Support/wezterm` on macOS). Its modification time is how
TGZTerminal knows when it last checked. Delete the file at any time.

**What is not done.** TGZTerminal never downloads or installs an update on its
own. When a newer release exists it shows a notification; acting on that
notification opens a normal browser download of the release artifact, which you
then install yourself. Nothing is executed without you asking for it.

A manual **Check for updates** command (command palette, Help menu, or the
`CheckForUpdates` key assignment) performs the same single request on demand.

## EU / GDPR Notice (Regulation (EU) 2016/679)

This section explains how TGZTerminal relates to the EU General Data Protection
Regulation (GDPR) and related EU/EEA data-protection law. It is written for
transparency and does not, by itself, collect or transmit any personal data.

**Data controller.** For the TGZTerminal source tree and builds produced from it
by the project maintainer, the controller is the project maintainer (see
_Contact_ below). If you obtain a build from a third party, that distributor is
the relevant controller for any changes they introduce (see _Third-Party
Builds_).

**Personal data we process: none, by default.** TGZTerminal is a local terminal
application. It does not collect, store, or transmit personal data to the
project or to any server. There are **no analytics, no telemetry uploads, no
advertising, no cookies, no tracking identifiers, no profiling, and no automated
decision-making**. The `agent_telemetry` setting only controls what is *displayed
locally* in the app's own UI surfaces; it does not send anything anywhere.

**Data that stays on your device.** Terminal scrollback, recent searches, and
similar convenience state may contain personal data, but it remains local (see
_Data Maintained by TGZTerminal_) and under your sole control. You are the
controller of that local content. Delete it at any time by clearing the relevant
buffers or removing the app's data directory and configuration files.

**Lawful basis and the only network egress.** The single case in which data
leaves your device is the update checker, which is **enabled by default**. It
makes a periodic HTTPS request to GitHub's public release API. That request
necessarily discloses your IP address and a TGZTerminal User-Agent string to
**GitHub, Inc. (a US-based processor/controller)**. The lawful basis is the
legitimate interest (Art. 6(1)(f) GDPR) in informing users of security and
correctness fixes to software they are running; the processing is limited to
what a plain HTTP request to a public API entails, no profile is built, and no
identifier is transmitted. You may object at any time, with immediate effect and
no loss of functionality, by setting `check_for_updates = false`. GitHub's
handling of that request is governed by the
[GitHub Privacy Statement](https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement).

**International transfers.** Unless you disable the update check, the request to
GitHub may be processed in the United States; those transfers rely on GitHub's
own transfer safeguards (Standard Contractual Clauses / Data Privacy Framework
as applicable). Setting `check_for_updates = false` stops any transfer.

**Retention.** The project retains no personal data about you. Local data is
retained on your device until you delete it.

**Your rights (Arts. 15–22 GDPR).** You have the rights of access,
rectification, erasure, restriction, portability, and objection. Because the
project holds **no** personal data about you, there is nothing on our side to
export, correct, or erase; all such data is local and directly under your
control. You may also lodge a complaint with your national data-protection
supervisory authority.

**Children.** TGZTerminal is a developer tool and is not directed at children,
and it collects no data from anyone.

**Changes.** Material changes to this notice will be reflected in this file's git
history and the _Effective date_ below.

## Contact

Data-protection inquiries: `[set a contact email or URL here before distribution]`.
For source builds, issues can also be raised at the project's GitHub repository.

## Third-Party Builds

The above is true of this TGZTerminal source tree and local builds produced from
it.

If you obtained a pre-built TGZTerminal binary from some other source be aware that
the person(s) building those versions may have modified them to behave
differently from the source version.

---

_Effective date: 2026-07-31. This notice is provided for transparency about how
the software behaves and is not legal advice. Parties who redistribute or deploy
TGZTerminal commercially should have their own counsel confirm their obligations._
