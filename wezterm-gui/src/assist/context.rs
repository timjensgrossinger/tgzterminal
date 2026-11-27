//! What the Ask panel sends (TGZTerminal).
//!
//! Recent command blocks (with exit codes), the pane's directory, the
//! selection or, on the alternate screen, the visible text, earlier turns of
//! this conversation, and the question. Everything is passed through the
//! secret masker first, and the whole is held under a byte budget: command
//! lines, the selection and the question are kept, and output is cut from
//! the oldest block first, keeping each block's last lines (where errors
//! are). The result also lists what was sent as short "chips" the card
//! shows, so nothing goes out that the user cannot see named.

use mux::tgz_blocks::BlockText;

/// One earlier question and its answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Turn {
    pub question: String,
    pub answer: String,
}

#[derive(Default)]
pub struct ContextInput<'a> {
    pub question: &'a str,
    pub cwd: Option<&'a str>,
    /// `None` for a local pane; otherwise where it runs (e.g. "ssh host").
    pub remote: Option<&'a str>,
    pub blocks: &'a [BlockText],
    pub selection: Option<&'a str>,
    /// Visible text, used instead of blocks on the alternate screen.
    pub screen: Option<&'a str>,
    pub history: &'a [Turn],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltPrompt {
    pub text: String,
    /// What was sent, for the card: "cwd ~/code/x", "3 commands", ...
    pub chips: Vec<String>,
    /// Secrets masked across everything sent.
    pub masked: usize,
}

/// The selection and the screen text are each capped at this many bytes.
const SELECTION_MAX: usize = 6000;
/// Earlier answers are capped at this many bytes each.
const HISTORY_ANSWER_MAX: usize = 3000;

pub fn build(
    input: &ContextInput,
    redact: &dyn Fn(&str) -> (String, usize),
    max_bytes: usize,
) -> BuiltPrompt {
    let mut masked = 0usize;
    let mut clean = |text: &str| {
        let (text, n) = redact(text);
        masked += n;
        text
    };

    let question = clean(input.question.trim());
    let selection = input
        .selection
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| tail_bytes(&clean(s), SELECTION_MAX));
    let screen = input
        .screen
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| tail_bytes(&clean(s), SELECTION_MAX));
    let commands: Vec<String> = input.blocks.iter().map(|b| clean(&b.command)).collect();
    let mut outputs: Vec<String> = input.blocks.iter().map(|b| clean(&b.output)).collect();
    let history: Vec<Turn> = input
        .history
        .iter()
        .map(|turn| Turn {
            question: clean(&turn.question),
            answer: tail_bytes(&clean(&turn.answer), HISTORY_ANSWER_MAX),
        })
        .collect();

    // Everything but block output is fixed; output shares what is left,
    // newest block first.
    let render = |outputs: &[String]| {
        render_prompt(
            input,
            &question,
            selection.as_deref(),
            screen.as_deref(),
            &commands,
            outputs,
            &history,
        )
    };
    let fixed = render(&vec![String::new(); outputs.len()]).len();
    // Room for the "[earlier output cut]" line and newline per block.
    let mut budget = max_bytes.saturating_sub(fixed + 24 * outputs.len());
    for idx in (0..outputs.len()).rev() {
        let len = outputs[idx].len();
        if len <= budget {
            budget -= len;
        } else {
            outputs[idx] = tail_bytes(&outputs[idx], budget);
            budget = budget.saturating_sub(outputs[idx].len());
        }
    }
    let text = render(&outputs);

    let mut chips = Vec::new();
    if let Some(cwd) = input.cwd {
        chips.push(format!("cwd {}", cwd));
    }
    if let Some(remote) = input.remote {
        chips.push(remote.to_string());
    }
    match input.blocks.len() {
        0 => {}
        1 => chips.push(format!("`{}`", ellipsize(&commands[0], 32))),
        n => chips.push(format!("{} commands", n)),
    }
    if let Some(selection) = &selection {
        chips.push(format!("selection {} chars", selection.chars().count()));
    }
    if screen.is_some() {
        chips.push("screen text".to_string());
    }
    if !history.is_empty() {
        chips.push(format!("{} earlier", history.len()));
    }
    if masked > 0 {
        chips.push(format!("{} masked", masked));
    }

    BuiltPrompt {
        text,
        chips,
        masked,
    }
}

fn render_prompt(
    input: &ContextInput,
    question: &str,
    selection: Option<&str>,
    screen: Option<&str>,
    commands: &[String],
    outputs: &[String],
    history: &[Turn],
) -> String {
    let mut s = String::from("<terminal>\n");
    if let Some(cwd) = input.cwd {
        s.push_str(&format!("cwd: {}\n", cwd));
    }
    match input.remote {
        Some(remote) => s.push_str(&format!(
            "pane: {} (remote; its files are not readable from here)\n",
            remote
        )),
        None => s.push_str("pane: local shell\n"),
    }
    for (idx, block) in input.blocks.iter().enumerate() {
        let status = match block.exit_status {
            Some(code) => format!(" exit=\"{}\"", code),
            None if block.guessed => " exit=\"unknown, no shell integration\"".to_string(),
            None => String::new(),
        };
        s.push_str(&format!("<command{}>\n$ {}\n", status, commands[idx]));
        let output = &outputs[idx];
        if !output.is_empty() {
            if output.len() < block.output.len() {
                s.push_str("[earlier output cut]\n");
            }
            s.push_str(output);
            s.push('\n');
        }
        s.push_str("</command>\n");
    }
    if let Some(screen) = screen {
        s.push_str("<screen>\n");
        s.push_str(screen);
        s.push_str("\n</screen>\n");
    }
    if let Some(selection) = selection {
        s.push_str("<selection>\n");
        s.push_str(selection);
        s.push_str("\n</selection>\n");
    }
    s.push_str("</terminal>\n");
    for turn in history {
        s.push_str(&format!(
            "<earlier_question>{}</earlier_question>\n<earlier_answer>\n{}\n</earlier_answer>\n",
            turn.question, turn.answer
        ));
    }
    s.push_str("\nQuestion: ");
    s.push_str(question);
    s.push('\n');
    s
}

/// The last `max` bytes of `text`, cut at a line start when one is near,
/// never inside a character.
pub fn tail_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let tail = &text[start..];
    match tail.find('\n') {
        Some(nl) if nl < 200 && nl + 1 < tail.len() => tail[nl + 1..].to_string(),
        _ => tail.to_string(),
    }
}

fn ellipsize(text: &str, max_chars: usize) -> String {
    let first_line = text.lines().next().unwrap_or("");
    if first_line.chars().count() <= max_chars && !text.contains('\n') {
        return first_line.to_string();
    }
    let cut: String = first_line
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect();
    format!("{}…", cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(command: &str, output: &str, exit: Option<i32>) -> BlockText {
        BlockText {
            command: command.into(),
            output: output.into(),
            prompt_row: 0,
            output_end: 0,
            exit_status: exit,
            guessed: false,
            head_clipped: false,
        }
    }

    fn no_redact(text: &str) -> (String, usize) {
        (text.to_string(), 0)
    }

    #[test]
    fn blocks_cwd_and_question_are_sent() {
        let blocks = [block("npm test", "1 failed", Some(1))];
        let built = build(
            &ContextInput {
                question: "why?",
                cwd: Some("/code/atlas"),
                blocks: &blocks,
                ..Default::default()
            },
            &no_redact,
            10_000,
        );
        assert!(built.text.contains("cwd: /code/atlas"));
        assert!(built
            .text
            .contains("<command exit=\"1\">\n$ npm test\n1 failed\n</command>"));
        assert!(built.text.ends_with("Question: why?\n"));
        assert_eq!(built.chips, vec!["cwd /code/atlas", "`npm test`"]);
    }

    #[test]
    fn secrets_are_masked_everywhere_and_counted() {
        let blocks = [block("echo TOKEN", "TOKEN", Some(0))];
        let redact = |text: &str| {
            let n = text.matches("TOKEN").count();
            (text.replace("TOKEN", "[REDACTED]"), n)
        };
        let built = build(
            &ContextInput {
                question: "what is TOKEN",
                selection: Some("TOKEN"),
                blocks: &blocks,
                ..Default::default()
            },
            &redact,
            10_000,
        );
        assert!(!built.text.contains("TOKEN"));
        assert_eq!(built.masked, 4);
        assert!(built.chips.contains(&"4 masked".to_string()));
    }

    #[test]
    fn output_is_cut_oldest_first_keeping_the_tail() {
        let old = "old line\n".repeat(400);
        let new = format!("{}error: the real problem\n", "noise\n".repeat(50));
        let blocks = [
            block("make old", &old, Some(0)),
            block("make", &new, Some(2)),
        ];
        let built = build(
            &ContextInput {
                question: "q",
                blocks: &blocks,
                ..Default::default()
            },
            &no_redact,
            1200,
        );
        assert!(built.text.len() <= 1200, "{}", built.text.len());
        assert!(built.text.contains("error: the real problem"));
        assert!(built.text.contains("$ make old"), "commands survive");
        assert!(built.text.contains("[earlier output cut]"));
    }

    #[test]
    fn tail_bytes_never_splits_a_character() {
        let text = "äöü".repeat(100);
        for max in 0..20 {
            let tail = tail_bytes(&text, max);
            assert!(tail.len() <= max);
        }
    }

    #[test]
    fn a_remote_pane_says_so() {
        let built = build(
            &ContextInput {
                question: "q",
                remote: Some("ssh prod"),
                ..Default::default()
            },
            &no_redact,
            1000,
        );
        assert!(built.text.contains("pane: ssh prod (remote"));
        assert!(built.chips.contains(&"ssh prod".to_string()));
    }
}
