//! OSC 7501 program status, end to end through the terminal (TGZTerminal).
use super::*;
use k9::assert_equal as assert_eq;

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SharedWriter {
    /// The terminal hands its output to a writer thread, so a reply shows
    /// up asynchronously; wait (bounded) for it.
    fn wait_for_output(&self) -> String {
        for _ in 0..200 {
            let bytes = self.0.lock().unwrap().clone();
            if !bytes.is_empty() {
                return String::from_utf8(bytes).unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        String::new()
    }
}

fn terminal() -> (Terminal, SharedWriter) {
    let writer = SharedWriter::default();
    let term = Terminal::new(
        TerminalSize {
            rows: 5,
            cols: 20,
            pixel_width: 160,
            pixel_height: 80,
            dpi: 0,
        },
        Arc::new(TestTermConfig { scrollback: 0 }),
        "WezTerm",
        "O_o",
        Box::new(writer.clone()),
    );
    (term, writer)
}

#[test]
fn the_probe_is_answered_with_the_capability_reply() {
    let (mut term, writer) = terminal();
    term.advance_bytes(b"\x1b]7501;?\x1b\\");
    assert_eq!(
        writer.wait_for_output(),
        "\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\"
    );
    // BEL-terminated probe, as some programs send it.
    writer.0.lock().unwrap().clear();
    term.advance_bytes(b"\x1b]7501;?\x07");
    assert!(writer.wait_for_output().starts_with("\x1b]7501;?:"));
}

#[test]
fn reports_are_stored_and_a_prompt_or_reset_drops_them() {
    let (mut term, _writer) = terminal();
    term.advance_bytes(b"\x1b]7501;state=idle:app=claude-code\x1b\\");
    term.advance_bytes(b"\x1b]7501;state=working:id=agent1:title=RXhwbG9yZQ==\x1b\\");
    term.advance_bytes(b"\x1b]7501;state=done:id=agent2\x1b\\");
    let status = term.get_program_status();
    assert_eq!(status.root().unwrap().state, ProgramState::Idle);
    assert_eq!(
        status.get("agent1").unwrap().title.as_deref(),
        Some("Explore")
    );
    assert_eq!(status.app("agent1"), Some("claude-code"));

    // OSC 133 A: a new shell prompt drops working/blocked only.
    term.advance_bytes(b"\x1b]133;A\x1b\\");
    let status = term.get_program_status();
    assert!(status.get("agent1").is_none());
    assert!(status.get("agent2").is_some());
    assert!(status.root().is_some());

    // A clear for one id, then RIS for the rest.
    term.advance_bytes(b"\x1b]7501;state=clear:id=agent2\x1b\\");
    assert!(term.get_program_status().get("agent2").is_none());
    term.advance_bytes(b"\x1bc");
    assert!(term.get_program_status().is_empty());
}

#[test]
fn a_rejected_report_leaves_the_records_alone() {
    let (mut term, _writer) = terminal();
    term.advance_bytes(b"\x1b]7501;state=working\x1b\\");
    // Invalid base64 in title: the whole report is discarded.
    term.advance_bytes(b"\x1b]7501;state=idle:title=a\x1b\\");
    assert_eq!(
        term.get_program_status().root().unwrap().state,
        ProgramState::Working
    );
}
