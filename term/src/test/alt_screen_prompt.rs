//! A shell prompt drawn on the alternate screen (TGZTerminal): the program
//! that switched to it is gone, even when it ran behind ssh/et/mosh.
use super::*;

const PROMPT: &str = "\x1b]133;A\x1b\\";

#[test]
fn a_prompt_mark_on_the_alt_screen_is_noted() {
    let mut term = TestTerm::new(5, 20, 0);
    term.print("\x1b[?1049h");
    assert!(!term.prompt_on_alt_screen());
    term.print(PROMPT);
    assert!(term.prompt_on_alt_screen());
}

#[test]
fn a_prompt_mark_on_the_primary_screen_is_not() {
    let mut term = TestTerm::new(5, 20, 0);
    term.print(PROMPT);
    assert!(!term.prompt_on_alt_screen());
    // Entering afterwards does not inherit it.
    term.print("\x1b[?1049h");
    assert!(!term.prompt_on_alt_screen());
}

#[test]
fn leaving_and_reentering_starts_clean() {
    let mut term = TestTerm::new(5, 20, 0);
    term.print("\x1b[?1049h");
    term.print(PROMPT);
    term.print("\x1b[?1049l");
    assert!(!term.prompt_on_alt_screen());
    term.print("\x1b[?1049h");
    assert!(!term.prompt_on_alt_screen());
}

#[test]
fn every_alt_screen_mode_and_prompt_mark_counts() {
    for (enter, leave) in [("\x1b[?47h", "\x1b[?47l"), ("\x1b[?1047h", "\x1b[?1047l")] {
        for mark in [PROMPT, "\x1b]133;P\x1b\\", "\x1b]133;N\x1b\\"] {
            let mut term = TestTerm::new(5, 20, 0);
            term.print(enter);
            term.print(mark);
            assert!(term.prompt_on_alt_screen(), "{enter:?} then {mark:?}");
            term.print(leave);
            assert!(!term.prompt_on_alt_screen(), "{leave:?} after {mark:?}");
        }
    }
}

#[test]
fn a_full_reset_clears_it() {
    let mut term = TestTerm::new(5, 20, 0);
    term.print("\x1b[?1049h");
    term.print(PROMPT);
    term.print("\x1bc");
    assert!(!term.prompt_on_alt_screen());
}
