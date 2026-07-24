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

**Lawful basis and the only optional network egress.** The single case in which
data leaves your device is the update checker, which is **disabled by default**
and only runs if you explicitly set `check_for_updates = true`. When enabled, it
makes an HTTPS request to GitHub's public release API. That request necessarily
discloses your IP address and a TGZTerminal User-Agent string to **GitHub, Inc.
(a US-based processor/controller)**. The lawful basis is your consent, given by
opting in via that setting; withdraw it at any time by setting
`check_for_updates = false`. GitHub's handling of that request is governed by the
[GitHub Privacy Statement](https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement).

**International transfers.** No transfer occurs unless you enable the update
check. If you do, the request to GitHub may be processed in the United States;
those transfers rely on GitHub's own transfer safeguards (Standard Contractual
Clauses / Data Privacy Framework as applicable).

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

_Effective date: 2026-07-24. This notice is provided for transparency about how
the software behaves and is not legal advice. Parties who redistribute or deploy
TGZTerminal commercially should have their own counsel confirm their obligations._
