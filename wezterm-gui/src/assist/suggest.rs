//! Suggested commands in an answer (TGZTerminal Ask panel).
//!
//! The system prompt asks for every runnable command in its own fenced
//! `sh` block. Each non-comment line of such a block is one suggestion; a
//! line ending in `\` continues on the next. A `console` block's `$ `
//! prompts are stripped. Other fences (code listings, diffs) are ignored.

/// At most this many suggestions are offered.
pub const MAX_SUGGESTIONS: usize = 6;

const SHELL_LANGS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "shell",
    "console",
    "fish",
    "shellsession",
];

pub fn extract(answer: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_block: Option<String> = None; // language of the open fence
    let mut pending = String::new();
    for raw in answer.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            if in_block.is_some() {
                flush(&mut pending, &mut out);
                in_block = None;
            } else {
                in_block = Some(rest.trim().to_ascii_lowercase());
            }
            continue;
        }
        let Some(lang) = in_block.as_deref() else {
            continue;
        };
        if !SHELL_LANGS.contains(&lang) {
            continue;
        }
        let mut text = trimmed;
        if lang == "console" || lang == "shellsession" {
            match text.strip_prefix("$ ").or_else(|| text.strip_prefix("% ")) {
                Some(command) => text = command,
                // Output lines in a console transcript.
                None if pending.is_empty() => continue,
                None => {}
            }
        }
        if pending.is_empty() && (text.is_empty() || text.starts_with('#')) {
            continue;
        }
        if let Some(head) = text.strip_suffix('\\') {
            pending.push_str(head.trim_end());
            pending.push(' ');
            continue;
        }
        pending.push_str(text);
        flush(&mut pending, &mut out);
    }
    flush(&mut pending, &mut out);
    out.truncate(MAX_SUGGESTIONS);
    out
}

fn flush(pending: &mut String, out: &mut Vec<String>) {
    let command = pending.trim().to_string();
    pending.clear();
    if !command.is_empty() && !out.contains(&command) {
        out.push(command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_fences_become_suggestions() {
        let answer = "Move the check.\n\n```sh\nnpm test -- --test-name-pattern=304\n```\n\
                      Or:\n```bash\n# comment\ngit log -3\ngit status\n```\n";
        assert_eq!(
            extract(answer),
            vec![
                "npm test -- --test-name-pattern=304",
                "git log -3",
                "git status"
            ]
        );
    }

    #[test]
    fn code_listings_are_not_commands() {
        let answer = "```js\nconst etag = hash(tile);\n```\n```\nplain\n```";
        assert!(extract(answer).is_empty());
    }

    #[test]
    fn continuations_join_and_console_prompts_strip() {
        let answer = "```sh\ndocker run \\\n  -it ubuntu\n```\n```console\n$ ls\nfile\n$ pwd\n```";
        assert_eq!(extract(answer), vec!["docker run -it ubuntu", "ls", "pwd"]);
    }

    #[test]
    fn duplicates_and_overflow_are_dropped() {
        let many: String = (0..10)
            .map(|i| format!("```sh\necho {i}\necho 0\n```\n"))
            .collect();
        let got = extract(&many);
        assert_eq!(got.len(), MAX_SUGGESTIONS);
        assert_eq!(got[0], "echo 0");
    }

    #[test]
    fn an_unclosed_fence_still_yields_what_streamed_so_far() {
        assert_eq!(extract("```sh\ncargo test"), vec!["cargo test"]);
    }
}
