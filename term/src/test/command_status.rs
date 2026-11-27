//! OSC 133;D exit statuses are kept per row (TGZTerminal): upstream parses
//! and drops them, and failed-command marks need them.
use super::*;

/// One shell round trip as `wezterm.sh` writes it: prompt, input, output,
/// then the status of the command that just finished.
fn run(term: &mut TestTerm, command: &str, output: &str, status: i32) {
    term.print("\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\");
    term.print(command);
    term.print("\r\n\x1b]133;C\x1b\\");
    term.print(output);
    term.print(&format!("\x1b]133;D;{status};aid=1\x1b\\"));
}

#[test]
fn the_status_is_recorded_at_the_row_after_the_output() {
    let mut term = TestTerm::new(10, 20, 0);
    run(&mut term, "echo hi", "hi\r\n", 0);
    run(&mut term, "false", "", 1);
    // Row 0 prompt, row 1 output "hi", row 2 is where the shell reported 0
    // and the next prompt began; `false` printed nothing, so its status
    // lands on row 3.
    std::assert_eq!(term.command_statuses(0..100), vec![(2, 0), (3, 1)]);
}

#[test]
fn a_range_filters_and_an_empty_range_is_empty() {
    let mut term = TestTerm::new(10, 20, 0);
    run(&mut term, "a", "x\r\n", 0);
    run(&mut term, "b", "y\r\n", 2);
    std::assert_eq!(term.command_statuses(3..5), vec![(4, 2)]);
    assert!(term.command_statuses(4..4).is_empty());
    assert!(term.command_statuses(5..2).is_empty());
}

#[test]
fn the_alternate_screen_records_nothing() {
    let mut term = TestTerm::new(10, 20, 0);
    term.print("\x1b[?1049h");
    term.print("\x1b]133;D;1\x1b\\");
    term.print("\x1b[?1049l");
    assert!(term.command_statuses(0..100).is_empty());
}

#[test]
fn a_full_reset_forgets_them() {
    let mut term = TestTerm::new(10, 20, 0);
    run(&mut term, "false", "", 1);
    term.print("\x1bc");
    assert!(term.command_statuses(0..100).is_empty());
}

#[test]
fn rows_that_left_the_scrollback_are_pruned() {
    // Two rows of screen and two of scrollback: older rows fall off.
    let mut term = TestTerm::new(2, 20, 2);
    run(&mut term, "false", "", 1);
    for _ in 0..10 {
        term.print("line\r\n");
    }
    run(&mut term, "true", "", 0);
    let statuses = term.command_statuses(0..1000);
    std::assert_eq!(statuses.len(), 1, "{statuses:?}");
    std::assert_eq!(statuses[0].1, 0);
}
