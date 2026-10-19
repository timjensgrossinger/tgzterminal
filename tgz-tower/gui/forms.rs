//! Text fields and the modal forms ("sheets"). Pure: no GPU, no IO.
//!
//! A field wraps the composer's `ComposerBuffer` (cursor, multi-line, paste)
//! so editing behaves like the rest of the terminal's input surfaces.

use super::text::{Font, Measure};
use crate::termwindow::composer::ComposerBuffer;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldId {
    Title,
    Brief,
    Task,
    Goal,
    Context,
    Constraints,
    Acceptance,
    Answer,
    Compose,
}

pub struct Field {
    pub buf: ComposerBuffer,
    pub multiline: bool,
    pub placeholder: &'static str,
}

impl Field {
    pub fn new(multiline: bool, placeholder: &'static str) -> Self {
        Self {
            buf: ComposerBuffer::new(),
            multiline,
            placeholder,
        }
    }

    pub fn with_text(mut self, text: &str) -> Self {
        self.buf.set_text(text);
        self
    }

    pub fn text(&self) -> String {
        self.buf.text()
    }

    pub fn is_empty(&self) -> bool {
        self.text().trim().is_empty()
    }

    /// Non-empty trimmed lines (for list fields like acceptance criteria).
    pub fn lines(&self) -> Vec<String> {
        self.text()
            .lines()
            .map(|l| {
                l.trim()
                    .trim_start_matches(|c| c == '-' || c == '*' || c == '•')
                    .trim()
                    .to_string()
            })
            .filter(|l| !l.is_empty())
            .collect()
    }

    pub fn insert(&mut self, text: &str) {
        if self.multiline {
            self.buf.insert_text(text);
        } else {
            let one_line: String = text
                .split(|c| c == '\n' || c == '\r')
                .collect::<Vec<_>>()
                .join(" ");
            self.buf.insert_text(&one_line);
        }
    }
}

/// A visual (wrapped) segment of one logical line: `[start, end)` in chars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// Soft-wrap logical lines to `max_w`, breaking after spaces when possible.
pub fn segments(m: &mut dyn Measure, lines: &[String], font: Font, max_w: f32) -> Vec<Segment> {
    let mut out = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            out.push(Segment {
                line: li,
                start: 0,
                end: 0,
            });
            continue;
        }
        let mut start = 0;
        while start < chars.len() {
            // Longest prefix from `start` that fits.
            let mut end = start + 1;
            let mut last_break = None;
            while end <= chars.len() {
                let s: String = chars[start..end].iter().collect();
                if m.width(&s, font) > max_w && end > start + 1 {
                    end -= 1;
                    break;
                }
                if chars[end - 1] == ' ' {
                    last_break = Some(end);
                }
                end += 1;
            }
            let end = end.min(chars.len());
            let cut = if end < chars.len() {
                last_break.filter(|b| *b > start).unwrap_or(end)
            } else {
                end
            };
            out.push(Segment {
                line: li,
                start,
                end: cut,
            });
            start = cut;
        }
    }
    out
}

/// Index of the segment holding the caret.
pub fn caret_segment(segs: &[Segment], line: usize, col: usize) -> usize {
    let mut best = 0;
    for (i, s) in segs.iter().enumerate() {
        if s.line == line && col >= s.start {
            best = i;
        }
    }
    best
}

/// Stands in for a role in the New flight picker: let triage choose.
pub const AUTO_ROLE: &str = "@auto";

/// Where a new flight's agents work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// A fresh folder of its own under the flights root.
    NewFolder,
    /// The folder Tower was opened from.
    Here,
    /// A git worktree of the folder Tower was opened from.
    Worktree,
}

pub struct NewFlightForm {
    pub title: Field,
    pub brief: Field,
    pub role: Option<String>,
    pub place: Place,
    /// `low` / `medium` / `high`; `None` lets Tower's router decide.
    pub tier: Option<String>,
}

pub struct LaunchForm {
    pub role_id: String,
    pub role_name: String,
    pub task: Field,
    pub place: Place,
}

pub struct PacketForm {
    pub flight_id: String,
    pub flight_key: String,
    pub to_role: Option<String>,
    pub goal: Field,
    pub context: Field,
    pub constraints: Field,
    pub acceptance: Field,
}

pub struct AnswerForm {
    pub item_id: String,
    pub title: String,
    pub question: String,
    pub text: Field,
}

pub enum Sheet {
    NewFlight(NewFlightForm),
    Launch(LaunchForm),
    Packet(PacketForm),
    Answer(AnswerForm),
}

impl Sheet {
    pub fn new_flight(role: Option<String>, place: Place) -> Self {
        Sheet::NewFlight(NewFlightForm {
            title: Field::new(false, "What should happen, in a few words"),
            brief: Field::new(true, "Details, links, files, how to verify… (optional)"),
            role,
            place,
            tier: None,
        })
    }

    pub fn launch(role_id: &str, role_name: &str, place: Place) -> Self {
        Sheet::Launch(LaunchForm {
            role_id: role_id.to_string(),
            role_name: role_name.to_string(),
            task: Field::new(
                true,
                "Describe the task. The first line becomes the flight's title.",
            ),
            place: if place == Place::Worktree {
                Place::Here
            } else {
                place
            },
        })
    }

    pub fn packet(flight_id: &str, key: &str, goal: &str) -> Self {
        Sheet::Packet(PacketForm {
            flight_id: flight_id.to_string(),
            flight_key: key.to_string(),
            to_role: None,
            goal: Field::new(true, "What the receiver should achieve").with_text(goal),
            context: Field::new(true, "One per line: files, functions, facts"),
            constraints: Field::new(true, "One per line: what not to touch"),
            acceptance: Field::new(true, "One per line: how to tell it's done (required)"),
        })
    }

    pub fn answer(item_id: &str, title: &str, question: &str) -> Self {
        Sheet::Answer(AnswerForm {
            item_id: item_id.to_string(),
            title: title.to_string(),
            question: question.to_string(),
            text: Field::new(true, "Your answer"),
        })
    }

    /// Fields in tab order.
    pub fn field_ids(&self) -> Vec<FieldId> {
        match self {
            Sheet::NewFlight(_) => vec![FieldId::Title, FieldId::Brief],
            Sheet::Launch(_) => vec![FieldId::Task],
            Sheet::Packet(_) => vec![
                FieldId::Goal,
                FieldId::Context,
                FieldId::Constraints,
                FieldId::Acceptance,
            ],
            Sheet::Answer(_) => vec![FieldId::Answer],
        }
    }

    pub fn field(&self, id: FieldId) -> Option<&Field> {
        match (self, id) {
            (Sheet::NewFlight(f), FieldId::Title) => Some(&f.title),
            (Sheet::NewFlight(f), FieldId::Brief) => Some(&f.brief),
            (Sheet::Launch(f), FieldId::Task) => Some(&f.task),
            (Sheet::Packet(f), FieldId::Goal) => Some(&f.goal),
            (Sheet::Packet(f), FieldId::Context) => Some(&f.context),
            (Sheet::Packet(f), FieldId::Constraints) => Some(&f.constraints),
            (Sheet::Packet(f), FieldId::Acceptance) => Some(&f.acceptance),
            (Sheet::Answer(f), FieldId::Answer) => Some(&f.text),
            _ => None,
        }
    }

    pub fn field_mut(&mut self, id: FieldId) -> Option<&mut Field> {
        match (self, id) {
            (Sheet::NewFlight(f), FieldId::Title) => Some(&mut f.title),
            (Sheet::NewFlight(f), FieldId::Brief) => Some(&mut f.brief),
            (Sheet::Launch(f), FieldId::Task) => Some(&mut f.task),
            (Sheet::Packet(f), FieldId::Goal) => Some(&mut f.goal),
            (Sheet::Packet(f), FieldId::Context) => Some(&mut f.context),
            (Sheet::Packet(f), FieldId::Constraints) => Some(&mut f.constraints),
            (Sheet::Packet(f), FieldId::Acceptance) => Some(&mut f.acceptance),
            (Sheet::Answer(f), FieldId::Answer) => Some(&mut f.text),
            _ => None,
        }
    }

    /// Why the form cannot be submitted yet, if it cannot.
    pub fn blocker(&self) -> Option<&'static str> {
        match self {
            Sheet::NewFlight(f) if f.title.is_empty() && f.brief.is_empty() => {
                Some("Give the flight a title")
            }
            Sheet::Launch(f) if f.task.is_empty() => Some("Say what the agent should do"),
            Sheet::Packet(f) if f.to_role.is_none() => Some("Pick who receives it"),
            Sheet::Packet(f) if f.goal.is_empty() => Some("Write the goal"),
            Sheet::Packet(f) if f.acceptance.lines().is_empty() => {
                Some("Add at least one acceptance criterion")
            }
            Sheet::Answer(f) if f.text.is_empty() => Some("Type an answer"),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::text::fake::FakeMeasure;
    use super::*;

    #[test]
    fn single_line_fields_flatten_pastes() {
        let mut f = Field::new(false, "");
        f.insert("one\ntwo\r\nthree");
        assert_eq!(f.text(), "one two  three");
        let mut m = Field::new(true, "");
        m.insert("a\nb");
        assert_eq!(m.text(), "a\nb");
    }

    #[test]
    fn list_fields_strip_bullets_and_blanks() {
        let f = Field::new(true, "").with_text("- tests pass\n\n * docs updated\n• no warnings");
        assert_eq!(f.lines(), vec!["tests pass", "docs updated", "no warnings"]);
    }

    #[test]
    fn segments_wrap_on_spaces_and_keep_empty_lines() {
        let mut m = FakeMeasure; // Body: 7 px per char
        let lines = vec![
            "hello world again".to_string(),
            String::new(),
            "x".to_string(),
        ];
        let segs = segments(&mut m, &lines, Font::Body, 7.0 * 12.0);
        assert_eq!(
            segs[0],
            Segment {
                line: 0,
                start: 0,
                end: 12
            },
            "breaks after 'hello world '"
        );
        assert_eq!(
            segs[1],
            Segment {
                line: 0,
                start: 12,
                end: 17
            }
        );
        assert_eq!(
            segs[2],
            Segment {
                line: 1,
                start: 0,
                end: 0
            }
        );
        assert_eq!(
            segs[3],
            Segment {
                line: 2,
                start: 0,
                end: 1
            }
        );
        assert_eq!(caret_segment(&segs, 0, 14), 1);
        assert_eq!(caret_segment(&segs, 0, 3), 0);
        assert_eq!(caret_segment(&segs, 2, 1), 3);
    }

    #[test]
    fn segments_hard_break_long_words() {
        let mut m = FakeMeasure;
        let lines = vec!["abcdefghij".to_string()];
        let segs = segments(&mut m, &lines, Font::Body, 7.0 * 4.0);
        assert_eq!(segs.len(), 3);
        assert_eq!((segs[0].start, segs[0].end), (0, 4));
        assert_eq!((segs[2].start, segs[2].end), (8, 10));
    }

    #[test]
    fn blockers_explain_what_is_missing() {
        let s = Sheet::new_flight(None, Place::NewFolder);
        assert_eq!(s.blocker(), Some("Give the flight a title"));
        let mut p = Sheet::packet("f", "K", "goal");
        assert_eq!(p.blocker(), Some("Pick who receives it"));
        if let Sheet::Packet(f) = &mut p {
            f.to_role = Some("ground".into());
        }
        assert_eq!(p.blocker(), Some("Add at least one acceptance criterion"));
        p.field_mut(FieldId::Acceptance)
            .unwrap()
            .insert("tests pass");
        assert_eq!(p.blocker(), None);
    }
}
