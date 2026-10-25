//! Getting a pane off an alternate screen nobody is going to leave.
//!
//! A full-screen program switches to the alternate screen (`CSI ?1049h`) and
//! switches back on exit. If it dies first -- killed, or its ssh connection
//! dropped -- the pane stays there with a shell prompt on it: the alternate
//! screen has no scrollback, so the scrollbar disappears, and the wheel is sent
//! as arrow keys (xterm's alternateScroll), which a shell reads as history.

use termwiz::escape::csi::{DecPrivateMode, DecPrivateModeCode, Mode, CSI};
use termwiz::escape::Action;

/// What a program leaving the alternate screen would have sent: back to the
/// primary screen, mouse reporting off, cursor visible.
///
/// Fed to the terminal model, never to the pty. Deliberately not a full reset
/// (`ResetTerminal`), which would erase the screen and scrollback the user is
/// trying to get back to.
pub fn leave_alt_screen_actions() -> Vec<Action> {
    let mode = |mode: fn(DecPrivateMode) -> Mode, code| {
        Action::CSI(CSI::Mode(mode(DecPrivateMode::Code(code))))
    };
    let mut actions: Vec<Action> = [
        // Term's reset handler leaves whichever alternate screen is active
        // (47 and 1047 share it) and restores the cursor saved on entry.
        DecPrivateModeCode::ClearAndEnableAlternateScreen,
        DecPrivateModeCode::MouseTracking,
        DecPrivateModeCode::ButtonEventMouse,
        DecPrivateModeCode::AnyEventMouse,
        DecPrivateModeCode::SGRMouse,
        DecPrivateModeCode::SGRPixelsMouse,
        DecPrivateModeCode::Utf8Mouse,
    ]
    .iter()
    .cloned()
    .map(|code| mode(Mode::ResetDecPrivateMode, code))
    .collect();
    actions.push(mode(
        Mode::SetDecPrivateMode,
        DecPrivateModeCode::ShowCursor,
    ));
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use termwiz::escape::{Esc, EscCode};
    use wezterm_term::{Terminal, TerminalSize};

    fn terminal() -> Terminal {
        Terminal::new(
            TerminalSize {
                rows: 5,
                cols: 20,
                ..Default::default()
            },
            Arc::new(config::TermConfig::new()),
            "test",
            "0",
            Box::new(Vec::new()),
        )
    }

    #[test]
    fn leaving_resets_the_alt_screen_and_mouse_but_is_no_full_reset() {
        let actions = leave_alt_screen_actions();
        let reset = |code| {
            Action::CSI(CSI::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                code,
            ))))
        };
        assert!(actions.contains(&reset(DecPrivateModeCode::ClearAndEnableAlternateScreen)));
        assert!(actions.contains(&reset(DecPrivateModeCode::SGRMouse)));
        assert!(actions.contains(&reset(DecPrivateModeCode::MouseTracking)));
        assert!(!actions.contains(&Action::Esc(Esc::Code(EscCode::FullReset))));
    }

    #[test]
    fn a_stranded_pane_gets_its_primary_screen_and_scrollback_back() {
        let mut term = terminal();
        // Enough output to push lines into the primary screen's scrollback.
        for line in 0..12 {
            term.advance_bytes(format!("line {line}\r\n"));
        }
        let scrollback = term.screen().scrollback_rows();
        // A program enters the alternate screen with mouse reporting, then dies.
        term.advance_bytes("\x1b[?1049h\x1b[?1000h\x1b[?1006h");
        assert!(term.is_alt_screen_active());
        assert!(term.is_mouse_grabbed());

        term.perform_actions(leave_alt_screen_actions());

        assert!(!term.is_alt_screen_active());
        assert!(!term.is_mouse_grabbed());
        assert_eq!(term.screen().scrollback_rows(), scrollback);
    }

    #[test]
    fn a_remote_prompt_on_the_alt_screen_is_cleared_by_leaving() {
        let mut term = terminal();
        // tmux behind et dies; the remote shell's next prompt carries OSC 133.
        term.advance_bytes("\x1b[?1049h\x1b[?1000h");
        term.advance_bytes("\x1b]133;A\x1b\\$ ");
        assert!(term.prompt_on_alt_screen());

        term.perform_actions(leave_alt_screen_actions());

        assert!(!term.is_alt_screen_active());
        assert!(!term.prompt_on_alt_screen());
    }

    #[test]
    fn leaving_the_primary_screen_is_a_no_op() {
        let mut term = terminal();
        term.advance_bytes("hello\r\n");
        let before = term.screen().scrollback_rows();
        term.perform_actions(leave_alt_screen_actions());
        assert!(!term.is_alt_screen_active());
        assert_eq!(term.screen().scrollback_rows(), before);
    }
}
