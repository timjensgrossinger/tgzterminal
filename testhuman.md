# Manual Agent UI Test Checklist

1. Badge disable
   - Set `agent_ui.show_sidebar_badges = false`.
   - Open Claude, Codex, or Gemini panes.
   - Confirm sidebar dots disappear.
   - Confirm compact tabs do not show `Cl`, `Cx`, or other agent labels.

2. Telemetry toggle
   - Set `agent_telemetry.enabled = false`.
   - Confirm sidebar/tab metadata rows disappear.
   - Confirm agent detection and badges still work when badges are enabled.

3. Hover details toggle
   - Set `sidebar_tab_hover_details = false`.
   - Confirm hover/comfortable metadata details hide.
   - Confirm telemetry is not globally disabled unless `agent_telemetry.enabled = false`.

4. Basic agent detection
   - Start panes with available agents: `claude`, `codex`, `gemini`, `opencode`, `copilot`, `cursor`, and `amp`.
   - Confirm labels, compact labels, and colors match the expected adapter.

5. Shell prompt false positives
   - In a normal shell, run commands that show prompts like `$ echo hi` and `# apt install foo`.
   - Confirm these are not detected as agent prompts or copied as agent messages.

6. Claude status
   - Start Claude and trigger a response.
   - Confirm spinner/status glyphs show as running.
   - Wait for Claude to finish.
   - Confirm the tab/sidebar changes to waiting state.

7. Waiting notification
   - Let a detected agent transition into waiting for input.
   - Confirm one toast notification appears.
   - Leave it waiting and repaint or switch tabs.
   - Confirm it does not repeatedly notify.

8. Copy menu
   - Use normal copy actions on a Claude or Codex conversation.
   - Confirm copied text removes chrome/status/footer noise.
   - Confirm normal content is not deleted.

9. Copy as Markdown
   - Copy a conversation with fenced code blocks.
   - Confirm code fences and line breaks are preserved verbatim.
   - Confirm content is cleaned but not rewrapped.

10. Toolbelt sizing
    - Shrink the sidebar/window width.
    - Confirm `Copy` stays available.
    - Confirm lower-priority actions disappear first.
    - Confirm buttons do not overlap or wrap badly.

11. Toolbelt actions
    - For any detected agent, confirm `Interrupt` and copy actions are available.
    - Confirm `Attach` is hidden.
    - For non-Claude agents, confirm `Resume` and `Logs` are hidden or disabled.

12. Claude Resume
    - In a Claude pane, click `Resume`.
    - Confirm a new tab opens in the same cwd.
    - Confirm it runs exactly `claude --resume`.

13. Claude Logs
    - In a Claude pane with resolvable cwd/session path, click `Logs`.
    - Confirm it opens the matching `~/.claude/projects/...` location.
    - Try a pane without resolvable logs.
    - Confirm a short toast appears instead of silently doing nothing.

14. Custom adapter config
    - Add a custom adapter under `agent_ui.adapters.<id>`.
    - Test process, title, and visible pattern detection.
    - Confirm it displays as configured.
    - Confirm adapter-specific actions are not enabled unless it is a known supported adapter.

15. Unknown metadata agent
    - Set agent user vars for an unknown kind/label.
    - Confirm it appears as `Unknown(<label>)`.
    - Confirm only generic actions are available.
