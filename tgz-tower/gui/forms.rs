//! Text fields and the modal forms ("sheets"). Pure: no GPU, no IO.
//!
//! A field wraps the composer's `ComposerBuffer` (cursor, multi-line, paste)
//! so editing behaves like the rest of the terminal's input surfaces.

use super::cron::{self, CivilMinute, Cron};
use super::proto::{Endpoint, EndpointSpec, ModelInfo, ModelSpec, Role, RoleSpec, ScheduleSpec};
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
    SchedName,
    SchedWhen,
    SchedPath,
    EpName,
    EpUrl,
    EpKeyEnv,
    ModelName,
    ModelLabel,
    ModelSize,
    ModelContext,
    ModelGoodFor,
    ModelNotes,
    ModelAtOnce,
    RoleName,
    RoleCommand,
    RoleMission,
    RoleLadder,
    RoleAtOnce,
    RouteLow,
    RouteMedium,
    RouteHigh,
    LocalPaths,
}

pub struct Field {
    pub buf: ComposerBuffer,
    pub multiline: bool,
    pub placeholder: &'static str,
    /// Takes digits only (counts, sizes).
    pub digits: bool,
}

impl Field {
    pub fn new(multiline: bool, placeholder: &'static str) -> Self {
        Self {
            buf: ComposerBuffer::new(),
            multiline,
            placeholder,
            digits: false,
        }
    }

    /// A field that takes digits only.
    pub fn number(placeholder: &'static str) -> Self {
        Self {
            digits: true,
            ..Self::new(false, placeholder)
        }
    }

    /// The number typed, if any.
    pub fn number_value(&self) -> Option<u32> {
        self.text().trim().parse().ok()
    }

    /// Type one character, unless this field does not take it.
    pub fn insert_char(&mut self, c: char) {
        if !self.digits || c.is_ascii_digit() {
            self.buf.insert_char(c);
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
        if self.digits {
            let only: String = text.chars().filter(char::is_ascii_digit).collect();
            self.buf.insert_text(&only);
        } else if self.multiline {
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
    /// Sensitive: only models inside your network may work on it.
    pub local_only: bool,
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

/// New schedule / Edit schedule.
pub struct ScheduleForm {
    /// Empty for a new one.
    pub id: String,
    pub name: Field,
    /// A cron expression (`30 8 * * 1-5`, `@daily`) or an interval (`2h`).
    pub when: Field,
    pub role: Option<String>,
    pub place: Place,
    /// The folder for Here, the repo for Worktree.
    pub path: Field,
    pub title: Field,
    pub brief: Field,
    pub tier: Option<String>,
    /// Start even while the last run's flight is still open.
    pub overlap_run: bool,
    pub catch_up: bool,
    /// Its flights are sensitive: local models only.
    pub local_only: bool,
    /// What the form does not edit, kept as it was.
    pub keep: ScheduleSpec,
}

/// Add / Edit model server (`[[endpoint]]`).
pub struct EndpointForm {
    /// The name it had, when editing.
    pub old_name: Option<String>,
    pub name: Field,
    pub kind: String,
    pub url: Field,
    /// `None`: guess from the address.
    pub local: Option<bool>,
    pub key_env: Field,
}

/// The server kinds the form offers, with their usual address.
pub const ENDPOINT_KINDS: [(&str, &str); 6] = [
    ("ollama", "http://localhost:11434"),
    ("lmstudio", "http://localhost:1234"),
    ("llama.cpp", "http://localhost:8080"),
    ("vllm", "http://localhost:8000"),
    ("mlx", "http://localhost:8080"),
    ("openai-compatible", "https://"),
];

impl EndpointForm {
    pub fn spec(&self) -> EndpointSpec {
        EndpointSpec {
            name: self.name.text().trim().to_string(),
            kind: self.kind.clone(),
            base_url: self.url.text().trim().to_string(),
            local: self.local,
            api_key_env: Some(self.key_env.text().trim().to_string()).filter(|v| !v.is_empty()),
        }
    }
}

/// Add / Edit model (`[[model]]`).
pub struct ModelForm {
    /// The id it had, when editing.
    pub old_id: Option<String>,
    /// The server it runs on.
    pub endpoint: Option<String>,
    /// Its name on that server.
    pub name: Field,
    pub label: Field,
    pub size: Field,
    pub context: Field,
    /// One strength per line.
    pub good_for: Field,
    pub notes: Field,
    pub at_once: Field,
}

impl ModelForm {
    pub fn id(&self) -> String {
        format!(
            "{}/{}",
            self.endpoint.as_deref().unwrap_or(""),
            self.name.text().trim()
        )
    }

    pub fn spec(&self) -> ModelSpec {
        ModelSpec {
            id: self.id(),
            label: self.label.text().trim().to_string(),
            size: self.size.text().trim().to_string(),
            context: self.context.number_value().filter(|c| *c > 0),
            good_for: self
                .good_for
                .lines()
                .iter()
                .flat_map(|l| l.split(','))
                .map(|g| g.trim().to_string())
                .filter(|g| !g.is_empty())
                .collect(),
            notes: self.notes.text().trim().to_string(),
            max_concurrency: self.at_once.number_value().unwrap_or(1),
        }
    }
}

/// New role / Edit role.
pub struct RoleForm {
    /// The id it had, when editing a role file.
    pub old_id: Option<String>,
    /// The file name; empty for a new role (made from the name).
    pub id: String,
    pub name: Field,
    /// `#rrggbb`.
    pub color: String,
    /// `pane`, `acp`, `llm` or `routed`.
    pub driver: String,
    pub command: Field,
    pub mission: Field,
    /// One model per line, cheapest first.
    pub ladder: Field,
    pub at_once: Field,
    /// Its TOML file, for settings the form does not cover.
    pub file: Option<String>,
}

/// Role colours the form offers: (name, `#rrggbb`).
pub const ROLE_COLORS: [(&str, &str); 8] = [
    ("blue", "#7aa2f7"),
    ("green", "#9ece6a"),
    ("violet", "#bb9af7"),
    ("amber", "#e0af68"),
    ("sky", "#7dcfff"),
    ("red", "#f7768e"),
    ("teal", "#73daca"),
    ("orange", "#ff9e64"),
];

/// `#rrggbb` -> RGB.
pub fn hex_rgb(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// How a role runs, with what the form says about it.
pub const DRIVERS: [(&str, &str); 4] = [
    (
        "pane",
        "An interactive CLI in a terminal pane next to Tower",
    ),
    (
        "acp",
        "An Agent Client Protocol agent Tower talks to directly",
    ),
    ("llm", "One model call, no tools (triage)"),
    (
        "routed",
        "Tower picks the engine from [routing] by task size",
    ),
];

impl RoleForm {
    pub fn spec(&self) -> RoleSpec {
        RoleSpec {
            id: self.id.clone(),
            name: self.name.text().trim().to_string(),
            color: self.color.clone(),
            driver: self.driver.clone(),
            command: if matches!(self.driver.as_str(), "routed" | "llm") {
                vec![]
            } else {
                self.command
                    .text()
                    .split_whitespace()
                    .map(str::to_string)
                    .collect()
            },
            mission: self.mission.text().trim().to_string(),
            ladder: if self.driver == "routed" {
                vec![]
            } else {
                self.ladder.lines()
            },
            max_concurrency: self.at_once.number_value().unwrap_or(1),
        }
    }

    /// Add a model to the end of the ladder (once).
    pub fn add_model(&mut self, id: &str) {
        let mut lines = self.ladder.lines();
        if !lines.iter().any(|l| l == id) {
            lines.push(id.to_string());
            self.ladder.buf.set_text(&lines.join("\n"));
        }
    }
}

/// Routing: which engines run each task size.
pub struct RoutingForm {
    pub low: Field,
    pub medium: Field,
    pub high: Field,
}

impl RoutingForm {
    /// `(tier, engines)` for each tier.
    pub fn tiers(&self) -> Vec<(String, Vec<String>)> {
        let split = |f: &Field| -> Vec<String> {
            f.text()
                .split(|c: char| c == ',' || c.is_whitespace())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        };
        vec![
            ("low".to_string(), split(&self.low)),
            ("medium".to_string(), split(&self.medium)),
            ("high".to_string(), split(&self.high)),
        ]
    }
}

/// Toggles on the schedule form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetFlag {
    Overlap,
    CatchUp,
    /// New flight and schedule: sensitive, local models only.
    LocalOnly,
}

/// Starting points for a new schedule: name, when, title, brief.
pub const TEMPLATES: [(&str, &str, &str, &str); 4] = [
    (
        "Morning PR review",
        "30 8 * * 1-5",
        "PR review {date}",
        "Review the open pull requests in this repo that changed since {since}. For each: what it does, risks, and what to ask the author. Write the summary to review.md.",
    ),
    (
        "Nightly dependency audit",
        "0 2 * * *",
        "Dependency audit {date}",
        "Check this repo's dependencies for known vulnerabilities and outdated major versions. Propose the upgrades worth doing, with the risk of each. Do not push anything.",
    ),
    (
        "Weekly stale-branch report",
        "0 9 * * 1",
        "Stale branches {date}",
        "List branches with no commits in 30 days, whether each is merged, and which look safe to delete. Report only; delete nothing.",
    ),
    (
        "Daily activity digest",
        "0 18 * * 1-5",
        "Digest {weekday} {date}",
        "Summarise what changed in this repo since {since}: merged work, open questions, anything that looks broken. Keep it to one screen.",
    ),
];

impl ScheduleForm {
    /// The spec to save.
    pub fn spec(&self) -> ScheduleSpec {
        let (cron, every) = split_when(&self.when.text());
        let (place, path) = match self.place {
            Place::NewFolder => ("new_folder", None),
            Place::Here => ("dir", Some(self.path.text().trim().to_string())),
            Place::Worktree => ("worktree", Some(self.path.text().trim().to_string())),
        };
        ScheduleSpec {
            id: self.id.clone(),
            name: self.name.text().trim().to_string(),
            cron,
            every,
            role: match self.role.as_deref() {
                Some(AUTO_ROLE) => "auto".to_string(),
                Some(r) => r.to_string(),
                None => String::new(),
            },
            tier: self.tier.clone(),
            title: self.title.text().trim().to_string(),
            brief: self.brief.text().trim().to_string(),
            place: place.to_string(),
            path,
            overlap: if self.overlap_run { "run" } else { "skip" }.to_string(),
            catch_up: self.catch_up,
            local_only: self.local_only,
            ..self.keep.clone()
        }
    }

    pub fn apply_template(&mut self, i: usize) {
        if let Some((name, when, title, brief)) = TEMPLATES.get(i) {
            self.name = Field::new(false, NAME_HINT).with_text(name);
            self.when = Field::new(false, WHEN_HINT).with_text(when);
            self.title = Field::new(false, TITLE_HINT).with_text(title);
            self.brief = Field::new(true, BRIEF_HINT).with_text(brief);
        }
    }
}

const NAME_HINT: &str = "Morning PR review";
const WHEN_HINT: &str = "30 8 * * 1-5   or   every 2h";
const TITLE_HINT: &str = "Flight title; {date} {weekday} are filled in";
const BRIEF_HINT: &str = "What the agent should do each run; {since} is the last good run";
const PATH_HINT: &str = "~/src/project";

/// `30 8 * * 1-5` -> cron; `every 2h`, `2h`, `45m` -> interval.
pub fn split_when(text: &str) -> (Option<String>, Option<String>) {
    let t = text.trim();
    if t.is_empty() {
        return (None, None);
    }
    let lower = t.to_ascii_lowercase();
    let rest = lower.strip_prefix("every").map(str::trim);
    let looks_cron = t.starts_with('@') || t.split_whitespace().count() == 5;
    match rest {
        Some(r) if !r.is_empty() => (None, Some(r.to_string())),
        _ if looks_cron => (Some(t.to_string()), None),
        _ => (None, Some(t.to_string())),
    }
}

/// Local wall-clock minute of `unix`.
pub fn local_civil(unix: u64) -> CivilMinute {
    use chrono::{Datelike, Local, TimeZone, Timelike};
    match Local.timestamp_opt(unix as i64, 0).single() {
        Some(t) => CivilMinute::new(
            t.year(),
            t.month() as u8,
            t.day() as u8,
            t.hour() as u8,
            t.minute() as u8,
        ),
        None => CivilMinute::new(1970, 1, 1, 0, 0),
    }
}

/// `Mon 08:30`.
pub fn civil_label(c: &CivilMinute) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    format!(
        "{} {:02}:{:02}",
        DAYS[c.weekday() as usize % 7],
        c.hour,
        c.minute
    )
}

/// What the When field means, for the hint under it: "weekdays 08:30 ·
/// next Mon 08:30", or why it cannot be read.
pub fn when_preview(text: &str, now_unix: u64) -> Result<String, String> {
    match split_when(text) {
        (Some(c), _) => {
            let cron = Cron::parse(&c)?;
            let next = cron
                .next_after(&local_civil(now_unix))
                .map(|n| format!(" · next {}", civil_label(&n)))
                .unwrap_or_else(|| " · never fires".to_string());
            Ok(format!("{}{}", cron.describe(), next))
        }
        (None, Some(e)) => {
            let secs = cron::parse_every(&e)?;
            Ok(format!(
                "{} · counted from when it is saved",
                cron::describe_every(secs)
            ))
        }
        (None, None) => Err("Say when it runs".to_string()),
    }
}

pub enum Sheet {
    NewFlight(NewFlightForm),
    Launch(LaunchForm),
    Packet(PacketForm),
    Answer(AnswerForm),
    Schedule(ScheduleForm),
    Endpoint(EndpointForm),
    Model(ModelForm),
    /// Confirm adding (`true`) or removing Tower's entry in the user-wide
    /// Claude Code and Codex configs.
    Connect(bool),
    Role(RoleForm),
    Routing(RoutingForm),
    /// `[privacy] local_only_paths`, one folder per line.
    LocalOnly(Field),
}

impl Sheet {
    pub fn new_flight(role: Option<String>, place: Place) -> Self {
        Sheet::NewFlight(NewFlightForm {
            title: Field::new(false, "What should happen, in a few words"),
            brief: Field::new(true, "Details, links, files, how to verify… (optional)"),
            role,
            place,
            tier: None,
            local_only: false,
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

    /// A new schedule (`spec` = None) or an existing one to edit.
    pub fn schedule(spec: Option<&ScheduleSpec>, default_path: Option<&str>) -> Self {
        let s = spec.cloned().unwrap_or_default();
        let place = match s.place.as_str() {
            "dir" => Place::Here,
            "worktree" => Place::Worktree,
            _ => Place::NewFolder,
        };
        let when = s.cron.clone().unwrap_or_else(|| {
            s.every
                .as_deref()
                .map(|e| format!("every {}", e))
                .unwrap_or_default()
        });
        let path = s
            .path
            .clone()
            .or_else(|| default_path.map(str::to_string))
            .unwrap_or_default();
        Sheet::Schedule(ScheduleForm {
            id: s.id.clone(),
            name: Field::new(false, NAME_HINT).with_text(&s.name),
            when: Field::new(false, WHEN_HINT).with_text(&when),
            role: match s.role.as_str() {
                "" => None,
                "auto" => Some(AUTO_ROLE.to_string()),
                r => Some(r.to_string()),
            },
            place,
            path: Field::new(false, PATH_HINT).with_text(&path),
            title: Field::new(false, TITLE_HINT).with_text(&s.title),
            brief: Field::new(true, BRIEF_HINT).with_text(&s.brief),
            tier: s.tier.clone(),
            overlap_run: s.overlap == "run",
            catch_up: s.catch_up,
            local_only: s.local_only,
            keep: s,
        })
    }

    /// Add a model server (`ep` = None) or edit one.
    pub fn endpoint(ep: Option<&Endpoint>) -> Self {
        Sheet::Endpoint(EndpointForm {
            old_name: ep.map(|e| e.name.clone()),
            name: Field::new(false, "company").with_text(ep.map(|e| e.name.as_str()).unwrap_or("")),
            kind: ep
                .map(|e| e.kind.clone())
                .unwrap_or_else(|| "ollama".to_string()),
            url: Field::new(false, "http://localhost:11434")
                .with_text(ep.map(|e| e.base_url.as_str()).unwrap_or("")),
            local: ep.and_then(|e| e.local_set),
            key_env: Field::new(false, "COMPANY_LLM_KEY (optional)")
                .with_text(ep.and_then(|e| e.api_key_env.as_deref()).unwrap_or("")),
        })
    }

    /// Add a model (`m` = None, optionally starting from a served model's
    /// `<endpoint>/<name>`) or edit one.
    pub fn model(m: Option<&ModelInfo>, prefill: Option<&str>) -> Self {
        let id = m.map(|m| m.id.as_str()).or(prefill).unwrap_or("");
        let (endpoint, name) = match id.split_once('/') {
            Some((e, n)) => (Some(e.to_string()), n),
            None => (None, ""),
        };
        let num = |v: Option<u32>| v.map(|n| n.to_string()).unwrap_or_default();
        Sheet::Model(ModelForm {
            old_id: m.map(|m| m.id.clone()),
            endpoint: m.map(|m| m.endpoint.clone()).or(endpoint),
            name: Field::new(false, "qwen3.8-27b").with_text(name),
            label: Field::new(false, "Qwen 3.8 27B").with_text(
                m.map(|m| m.label.as_str())
                    .filter(|l| *l != name)
                    .unwrap_or(""),
            ),
            size: Field::new(false, "27B").with_text(m.map(|m| m.size.as_str()).unwrap_or("")),
            context: Field::number("131072").with_text(&num(m.and_then(|m| m.context))),
            good_for: Field::new(true, "One per line: code edits, docs, tests…")
                .with_text(&m.map(|m| m.good_for.join("\n")).unwrap_or_default()),
            notes: Field::new(true, "What an agent should know before handing it work")
                .with_text(m.map(|m| m.notes.as_str()).unwrap_or("")),
            at_once: Field::number("1").with_text(&num(m.map(|m| m.max_concurrency).or(Some(1)))),
        })
    }

    /// New role (`r` = None) or edit one.
    pub fn role(r: Option<&Role>) -> Self {
        let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        let driver = match r {
            Some(r) if r.routed => "routed".to_string(),
            Some(r) => r.driver.label().to_ascii_lowercase(),
            None => "acp".to_string(),
        };
        let ladder: Vec<String> = r
            .map(|r| r.ladder.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default();
        Sheet::Role(RoleForm {
            old_id: r.filter(|r| !r.builtin).map(|r| r.id.clone()),
            id: r.map(|r| r.id.clone()).unwrap_or_default(),
            name: Field::new(false, "Review").with_text(r.map(|r| r.name.as_str()).unwrap_or("")),
            color: r
                .map(|r| hex(r.color))
                .unwrap_or_else(|| ROLE_COLORS[0].1.to_string()),
            driver,
            command: Field::new(false, "opencode acp")
                .with_text(&r.map(|r| r.command.join(" ")).unwrap_or_default()),
            mission: Field::new(true, "What this role does, and how it reports back")
                .with_text(r.map(|r| r.mission.as_str()).unwrap_or("")),
            ladder: Field::new(true, "One model per line, cheapest first")
                .with_text(&ladder.join("\n")),
            at_once: Field::number("1")
                .with_text(&r.map(|r| r.max_concurrency).unwrap_or(1).to_string()),
            file: r.and_then(|r| r.file.clone()),
        })
    }

    pub fn routing(low: &[String], medium: &[String], high: &[String]) -> Self {
        Sheet::Routing(RoutingForm {
            low: Field::new(false, "qwen-local, claude-haiku").with_text(&low.join(", ")),
            medium: Field::new(false, "claude-sonnet").with_text(&medium.join(", ")),
            high: Field::new(false, "claude-opus").with_text(&high.join(", ")),
        })
    }

    pub fn local_only(paths: &[String]) -> Self {
        Sheet::LocalOnly(
            Field::new(true, "~/src/payroll\n/Volumes/customers").with_text(&paths.join("\n")),
        )
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
            Sheet::Schedule(f) => {
                let mut v = vec![FieldId::SchedName, FieldId::SchedWhen];
                if f.place != Place::NewFolder {
                    v.push(FieldId::SchedPath);
                }
                v.push(FieldId::Title);
                v.push(FieldId::Brief);
                v
            }
            Sheet::Endpoint(_) => vec![FieldId::EpName, FieldId::EpUrl, FieldId::EpKeyEnv],
            Sheet::Connect(_) => vec![],
            Sheet::Role(f) => {
                let mut v = vec![FieldId::RoleName];
                if !matches!(f.driver.as_str(), "routed" | "llm") {
                    v.push(FieldId::RoleCommand);
                }
                v.push(FieldId::RoleMission);
                if f.driver != "routed" {
                    v.push(FieldId::RoleLadder);
                }
                v.push(FieldId::RoleAtOnce);
                v
            }
            Sheet::Routing(_) => vec![FieldId::RouteLow, FieldId::RouteMedium, FieldId::RouteHigh],
            Sheet::LocalOnly(_) => vec![FieldId::LocalPaths],
            Sheet::Model(_) => vec![
                FieldId::ModelName,
                FieldId::ModelLabel,
                FieldId::ModelSize,
                FieldId::ModelContext,
                FieldId::ModelGoodFor,
                FieldId::ModelNotes,
                FieldId::ModelAtOnce,
            ],
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
            (Sheet::Schedule(f), FieldId::SchedName) => Some(&f.name),
            (Sheet::Schedule(f), FieldId::SchedWhen) => Some(&f.when),
            (Sheet::Schedule(f), FieldId::SchedPath) => Some(&f.path),
            (Sheet::Schedule(f), FieldId::Title) => Some(&f.title),
            (Sheet::Schedule(f), FieldId::Brief) => Some(&f.brief),
            (Sheet::Endpoint(f), FieldId::EpName) => Some(&f.name),
            (Sheet::Endpoint(f), FieldId::EpUrl) => Some(&f.url),
            (Sheet::Endpoint(f), FieldId::EpKeyEnv) => Some(&f.key_env),
            (Sheet::Model(f), FieldId::ModelName) => Some(&f.name),
            (Sheet::Model(f), FieldId::ModelLabel) => Some(&f.label),
            (Sheet::Model(f), FieldId::ModelSize) => Some(&f.size),
            (Sheet::Model(f), FieldId::ModelContext) => Some(&f.context),
            (Sheet::Model(f), FieldId::ModelGoodFor) => Some(&f.good_for),
            (Sheet::Model(f), FieldId::ModelNotes) => Some(&f.notes),
            (Sheet::Model(f), FieldId::ModelAtOnce) => Some(&f.at_once),
            (Sheet::Role(f), FieldId::RoleName) => Some(&f.name),
            (Sheet::Role(f), FieldId::RoleCommand) => Some(&f.command),
            (Sheet::Role(f), FieldId::RoleMission) => Some(&f.mission),
            (Sheet::Role(f), FieldId::RoleLadder) => Some(&f.ladder),
            (Sheet::Role(f), FieldId::RoleAtOnce) => Some(&f.at_once),
            (Sheet::Routing(f), FieldId::RouteLow) => Some(&f.low),
            (Sheet::Routing(f), FieldId::RouteMedium) => Some(&f.medium),
            (Sheet::Routing(f), FieldId::RouteHigh) => Some(&f.high),
            (Sheet::LocalOnly(f), FieldId::LocalPaths) => Some(f),
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
            (Sheet::Schedule(f), FieldId::SchedName) => Some(&mut f.name),
            (Sheet::Schedule(f), FieldId::SchedWhen) => Some(&mut f.when),
            (Sheet::Schedule(f), FieldId::SchedPath) => Some(&mut f.path),
            (Sheet::Schedule(f), FieldId::Title) => Some(&mut f.title),
            (Sheet::Schedule(f), FieldId::Brief) => Some(&mut f.brief),
            (Sheet::Endpoint(f), FieldId::EpName) => Some(&mut f.name),
            (Sheet::Endpoint(f), FieldId::EpUrl) => Some(&mut f.url),
            (Sheet::Endpoint(f), FieldId::EpKeyEnv) => Some(&mut f.key_env),
            (Sheet::Model(f), FieldId::ModelName) => Some(&mut f.name),
            (Sheet::Model(f), FieldId::ModelLabel) => Some(&mut f.label),
            (Sheet::Model(f), FieldId::ModelSize) => Some(&mut f.size),
            (Sheet::Model(f), FieldId::ModelContext) => Some(&mut f.context),
            (Sheet::Model(f), FieldId::ModelGoodFor) => Some(&mut f.good_for),
            (Sheet::Model(f), FieldId::ModelNotes) => Some(&mut f.notes),
            (Sheet::Model(f), FieldId::ModelAtOnce) => Some(&mut f.at_once),
            (Sheet::Role(f), FieldId::RoleName) => Some(&mut f.name),
            (Sheet::Role(f), FieldId::RoleCommand) => Some(&mut f.command),
            (Sheet::Role(f), FieldId::RoleMission) => Some(&mut f.mission),
            (Sheet::Role(f), FieldId::RoleLadder) => Some(&mut f.ladder),
            (Sheet::Role(f), FieldId::RoleAtOnce) => Some(&mut f.at_once),
            (Sheet::Routing(f), FieldId::RouteLow) => Some(&mut f.low),
            (Sheet::Routing(f), FieldId::RouteMedium) => Some(&mut f.medium),
            (Sheet::Routing(f), FieldId::RouteHigh) => Some(&mut f.high),
            (Sheet::LocalOnly(f), FieldId::LocalPaths) => Some(f),
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
            Sheet::Schedule(f) if f.name.is_empty() => Some("Give the schedule a name"),
            Sheet::Schedule(f) if when_preview(&f.when.text(), 0).is_err() => {
                Some("Fix when it runs: see the hint under it")
            }
            Sheet::Schedule(f) if f.role.is_none() => Some("Pick a role, or Auto"),
            Sheet::Schedule(f) if f.title.is_empty() && f.brief.is_empty() => {
                Some("Say what the agent should do")
            }
            Sheet::Schedule(f) if f.place != Place::NewFolder && f.path.is_empty() => {
                Some("Say which folder it runs in")
            }
            Sheet::Endpoint(f) if f.name.is_empty() => Some("Name the server"),
            Sheet::Endpoint(f)
                if !f
                    .name
                    .text()
                    .trim()
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) =>
            {
                Some("Name: letters, digits, '-', '_' and '.' only")
            }
            Sheet::Endpoint(f) if !url_ok(&f.url.text()) => {
                Some("Its address starts with http:// or https://")
            }
            Sheet::Model(f) if f.endpoint.is_none() => Some("Pick the server it runs on"),
            Sheet::Model(f) if f.name.is_empty() => Some("Name the model as its server knows it"),
            Sheet::Model(f) if f.name.text().trim().contains(char::is_whitespace) => {
                Some("The model name has no spaces")
            }
            Sheet::Model(f) if !matches!(f.at_once.number_value(), Some(1..=64)) => {
                Some("At once: a number from 1 to 64")
            }
            Sheet::Model(f) if !f.context.is_empty() && f.context.number_value().is_none() => {
                Some("Context: a number of tokens (up to 4294967295)")
            }
            Sheet::Role(f) if f.name.is_empty() => Some("Give the role a name"),
            Sheet::Role(f)
                if matches!(f.driver.as_str(), "pane" | "acp") && f.command.is_empty() =>
            {
                Some("Say which program runs it")
            }
            Sheet::Role(f) if f.driver == "llm" && f.ladder.lines().is_empty() => {
                Some("An llm role needs a model")
            }
            Sheet::Role(f) if f.mission.is_empty() => Some("Write its mission"),
            Sheet::Role(f) if !matches!(f.at_once.number_value(), Some(1..=64)) => {
                Some("At once: a number from 1 to 64")
            }
            Sheet::LocalOnly(f)
                if f.lines()
                    .iter()
                    .any(|p| !(p.starts_with('/') || p.starts_with('~') || p.contains(":\\"))) =>
            {
                Some("Give full folder paths (/… or ~/…)")
            }
            _ => None,
        }
    }
}

/// Does this address look like it is on this machine or the local network?
/// The same guess towerd makes when a server does not say (`is_local_url`).
pub fn looks_local(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let authority = rest
        .split(|c| c == '/' || c == '?' || c == '#')
        .next()
        .unwrap_or(rest);
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let h = host.split(':').next().unwrap_or("").to_lowercase();
    if let Ok(ip) = h.parse::<std::net::Ipv4Addr>() {
        return ip.is_loopback() || ip.is_private();
    }
    let name_ok = !h.is_empty()
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    name_ok
        && (h == "localhost"
            || h.ends_with(".localhost")
            || h.ends_with(".local")
            || h.ends_with(".lan")
            || h.ends_with(".home.arpa"))
}

fn url_ok(u: &str) -> bool {
    let u = u.trim();
    ["http://", "https://"]
        .iter()
        .any(|s| u.len() > s.len() && u.starts_with(s))
        && !u.contains(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::super::text::fake::FakeMeasure;
    use super::*;

    #[test]
    fn role_form_round_trips_and_knows_what_each_driver_needs() {
        let role = Role {
            id: "ground".into(),
            name: "Ground".into(),
            mission: "Execute packets.".into(),
            color: [0x9e, 0xce, 0x6a],
            driver: super::super::proto::DriverKind::Acp,
            command: vec!["opencode".into(), "acp".into()],
            ladder: vec![super::super::proto::ModelRef {
                id: "opencode-go/kimi".into(),
                ..Default::default()
            }],
            max_concurrency: 3,
            ..Default::default()
        };
        let mut sheet = Sheet::role(Some(&role));
        let spec = match &sheet {
            Sheet::Role(f) => f.spec(),
            _ => unreachable!(),
        };
        assert_eq!(spec.id, "ground");
        assert_eq!(spec.color, "#9ece6a");
        assert_eq!(spec.driver, "acp");
        assert_eq!(spec.command, vec!["opencode", "acp"]);
        assert_eq!(spec.ladder, vec!["opencode-go/kimi"]);
        assert_eq!(spec.max_concurrency, 3);
        assert_eq!(sheet.blocker(), None);
        if let Sheet::Role(f) = &mut sheet {
            f.add_model("localhost/qwen3.6:27b");
            f.add_model("localhost/qwen3.6:27b");
            assert_eq!(f.spec().ladder.len(), 2, "added once, at the end");
            f.driver = "routed".into();
            assert!(f.spec().command.is_empty() && f.spec().ladder.is_empty());
            f.driver = "pane".into();
            f.command.buf.clear();
        }
        assert_eq!(sheet.blocker(), Some("Say which program runs it"));
        let built_in = Role {
            builtin: true,
            ..role
        };
        match Sheet::role(Some(&built_in)) {
            Sheet::Role(f) => assert_eq!(f.old_id, None, "saving writes a new file"),
            _ => unreachable!(),
        }
        assert_eq!(Sheet::role(None).blocker(), Some("Give the role a name"));
        assert_eq!(hex_rgb("#ff9e64"), Some([0xff, 0x9e, 0x64]));
        assert_eq!(hex_rgb("nope"), None);
    }

    #[test]
    fn routing_and_folder_forms_split_their_lists() {
        let s = Sheet::routing(&["a".into()], &[], &["c".into(), "d".into()]);
        if let Sheet::Routing(f) = &s {
            assert_eq!(f.tiers()[0], ("low".to_string(), vec!["a".to_string()]));
            assert_eq!(f.tiers()[2].1, vec!["c", "d"]);
        }
        let mut s = Sheet::local_only(&["~/src/payroll".into()]);
        assert_eq!(s.blocker(), None);
        s.field_mut(FieldId::LocalPaths)
            .unwrap()
            .insert("\nrelative/dir");
        assert_eq!(s.blocker(), Some("Give full folder paths (/… or ~/…)"));
    }

    #[test]
    fn endpoint_form_round_trips_and_checks_the_address() {
        let ep = Endpoint {
            name: "company".into(),
            kind: "vllm".into(),
            base_url: "https://llm.example.internal/v1".into(),
            local: true,
            local_set: Some(true),
            api_key_env: Some("COMPANY_LLM_KEY".into()),
            ..Default::default()
        };
        let sheet = Sheet::endpoint(Some(&ep));
        let f = match &sheet {
            Sheet::Endpoint(f) => f,
            _ => unreachable!(),
        };
        assert_eq!(f.old_name.as_deref(), Some("company"));
        let spec = f.spec();
        assert_eq!(spec.local, Some(true));
        assert_eq!(spec.api_key_env.as_deref(), Some("COMPANY_LLM_KEY"));
        assert_eq!(sheet.blocker(), None);

        let mut new = Sheet::endpoint(None);
        assert_eq!(new.blocker(), Some("Name the server"));
        new.field_mut(FieldId::EpName).unwrap().insert("gpu box");
        assert!(new.blocker().unwrap().starts_with("Name:"));
        new.field_mut(FieldId::EpName).unwrap().buf.clear();
        new.field_mut(FieldId::EpName).unwrap().insert("gpu-box");
        assert_eq!(
            new.blocker(),
            Some("Its address starts with http:// or https://")
        );
        new.field_mut(FieldId::EpUrl)
            .unwrap()
            .insert("http://gpu-box.lan:8000");
        assert_eq!(new.blocker(), None);
        if let Sheet::Endpoint(f) = &new {
            assert_eq!(f.spec().api_key_env, None, "blank key = none");
        }
    }

    #[test]
    fn model_form_round_trips_and_splits_strengths() {
        let info = ModelInfo {
            id: "company/qwen3.8-27b".into(),
            label: "Qwen 3.8 27B".into(),
            endpoint: "company".into(),
            size: "27B".into(),
            context: Some(131_072),
            good_for: vec!["code edits".into(), "docs".into()],
            notes: "fast".into(),
            max_concurrency: 2,
            ..Default::default()
        };
        let sheet = Sheet::model(Some(&info), None);
        let f = match &sheet {
            Sheet::Model(f) => f,
            _ => unreachable!(),
        };
        let spec = f.spec();
        assert_eq!(spec.id, info.id);
        assert_eq!(spec.good_for, info.good_for);
        assert_eq!(spec.context, Some(131_072));
        assert_eq!(spec.max_concurrency, 2);
        assert_eq!(f.old_id.as_deref(), Some("company/qwen3.8-27b"));

        let mut new = Sheet::model(None, Some("localhost/qwen3.6:27b"));
        assert_eq!(new.blocker(), None, "a served model is enough to add");
        new.field_mut(FieldId::ModelGoodFor)
            .unwrap()
            .insert("tests, docs\n- refactors");
        new.field_mut(FieldId::ModelAtOnce).unwrap().insert("x");
        if let Sheet::Model(f) = &new {
            assert_eq!(f.spec().good_for, vec!["tests", "docs", "refactors"]);
            assert_eq!(f.at_once.text(), "1", "digits only");
            assert_eq!(f.old_id, None);
        }
        assert_eq!(
            Sheet::model(None, None).blocker(),
            Some("Pick the server it runs on")
        );
    }

    #[test]
    fn when_is_a_cron_or_an_interval() {
        assert_eq!(
            split_when("30 8 * * 1-5"),
            (Some("30 8 * * 1-5".into()), None)
        );
        assert_eq!(split_when("@daily"), (Some("@daily".into()), None));
        assert_eq!(split_when("every 2h"), (None, Some("2h".into())));
        assert_eq!(split_when("45m"), (None, Some("45m".into())));
        assert_eq!(split_when("  "), (None, None));
        assert!(when_preview("0 25 * * *", 0).unwrap_err().contains("hour"));
        assert!(when_preview("every 2h", 0).unwrap().starts_with("every 2h"));
        assert!(when_preview("0 7 * * 1-5", 0)
            .unwrap()
            .starts_with("weekdays 07:00 · next"));
    }

    #[test]
    fn schedule_form_round_trips_a_spec() {
        let spec = ScheduleSpec {
            id: "deps".into(),
            name: "Deps".into(),
            every: Some("2h".into()),
            role: "auto".into(),
            brief: "check".into(),
            place: "worktree".into(),
            path: Some("~/src/a".into()),
            pause_after_failures: 5,
            policy: Some("confirm".into()),
            ..Default::default()
        };
        let sheet = Sheet::schedule(Some(&spec), None);
        let form = match &sheet {
            Sheet::Schedule(f) => f,
            _ => unreachable!(),
        };
        assert_eq!(form.role.as_deref(), Some(AUTO_ROLE));
        assert_eq!(form.spec(), spec);
        assert!(sheet.field_ids().contains(&FieldId::SchedPath));
        assert_eq!(sheet.blocker(), None);

        let mut new = Sheet::schedule(None, Some("/w"));
        assert_eq!(new.blocker(), Some("Give the schedule a name"));
        if let Sheet::Schedule(f) = &mut new {
            f.apply_template(0);
            f.role = Some("review".into());
            assert_eq!(f.spec().cron.as_deref(), Some("30 8 * * 1-5"));
            assert_eq!(f.spec().place, "new_folder");
        }
        assert_eq!(new.blocker(), None);
    }

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
