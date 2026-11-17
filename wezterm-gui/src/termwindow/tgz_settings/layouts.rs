//! The Layouts section of the Settings tab: the user's own `tgz_layouts`,
//! edited in a form and written back as one Lua table.
//!
//! Pure: a draft in, layouts and Lua text out. Each pane keeps the
//! `SpawnCommand` it came from, so fields the form does not show (`cwd`,
//! `domain`, `set_environment_variables`, `label`) survive an edit; the form
//! only changes its command line.

use config::keyassignment::{PaneLayout, SpawnCommand};
use config::{AgentLauncherCwd, TgzPaneLayout};
use wezterm_dynamic::{ToDynamic, Value};

/// The path the Layouts section writes.
pub(crate) const PATH: &[&str] = &["tgz_layouts"];

/// Where a layout's panes start, as the form offers it.
pub(crate) const START_IN: [AgentLauncherCwd; 2] =
    [AgentLauncherCwd::ActivePane, AgentLauncherCwd::ProjectRoot];

pub(crate) fn start_in_label(cwd: &AgentLauncherCwd) -> &'static str {
    match cwd {
        AgentLauncherCwd::ActivePane => "Active pane's folder",
        AgentLauncherCwd::ProjectRoot => "Project root",
    }
}

/// The text field of the form being typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DraftField {
    Name,
    Pane(usize),
}

/// A layout being edited (or made) in the form.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LayoutDraft {
    /// The name the layout had when the form opened; `None` for a new one.
    pub original: Option<String>,
    pub name: String,
    pub layout: PaneLayout,
    pub cwd: AgentLauncherCwd,
    /// Each pane's command line as typed, beside the pane it came from.
    pub panes: Vec<(String, SpawnCommand)>,
    pub typing: Option<DraftField>,
}

impl LayoutDraft {
    pub(crate) fn new_layout(existing: &[TgzPaneLayout]) -> Self {
        let name = (1..)
            .map(|n| {
                if n == 1 {
                    "New layout".to_string()
                } else {
                    format!("New layout {n}")
                }
            })
            .find(|name| !existing.iter().any(|l| &l.name == name))
            .unwrap_or_else(|| "New layout".to_string());
        Self {
            original: None,
            name,
            layout: PaneLayout::MainLeft,
            cwd: AgentLauncherCwd::default(),
            panes: vec![
                (String::new(), SpawnCommand::default()),
                (String::new(), SpawnCommand::default()),
            ],
            typing: Some(DraftField::Name),
        }
    }

    pub(crate) fn edit(layout: &TgzPaneLayout) -> Self {
        Self {
            original: Some(layout.name.clone()),
            name: layout.name.clone(),
            layout: layout.layout,
            cwd: layout.cwd.clone(),
            panes: layout
                .panes
                .iter()
                .map(|pane| (command_line(pane), pane.clone()))
                .collect(),
            typing: None,
        }
    }

    /// The text of `field`, for typing into.
    pub(crate) fn field_mut(&mut self, field: DraftField) -> Option<&mut String> {
        match field {
            DraftField::Name => Some(&mut self.name),
            DraftField::Pane(i) => self.panes.get_mut(i).map(|(line, _)| line),
        }
    }

    pub(crate) fn add_pane(&mut self) {
        self.panes.push((String::new(), SpawnCommand::default()));
        self.typing = Some(DraftField::Pane(self.panes.len() - 1));
    }

    pub(crate) fn remove_pane(&mut self, index: usize) {
        if index < self.panes.len() {
            self.panes.remove(index);
        }
        self.typing = None;
    }
}

/// A pane's command as one line: its arguments, quoted where a shell would
/// need it. Empty for the default shell.
pub(crate) fn command_line(pane: &SpawnCommand) -> String {
    match &pane.args {
        None => String::new(),
        Some(args) => args
            .iter()
            .map(|arg| {
                shlex::try_quote(arg)
                    .map(|quoted| quoted.into_owned())
                    .unwrap_or_else(|_| arg.clone())
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// A typed command line as arguments: `Some(None)` for an empty line (the
/// default shell), `None` when its quoting does not close.
pub(crate) fn parse_command_line(line: &str) -> Option<Option<Vec<String>>> {
    if line.trim().is_empty() {
        return Some(None);
    }
    shlex::split(line).map(Some)
}

/// The layouts with `draft` saved into them: replacing the layout it was
/// opened from, else added at the end. `Err` says what is wrong with it.
pub(crate) fn apply_draft(
    existing: &[TgzPaneLayout],
    draft: &LayoutDraft,
) -> Result<Vec<TgzPaneLayout>, String> {
    let name = draft.name.trim();
    if name.is_empty() {
        return Err("A layout needs a name.".to_string());
    }
    if existing
        .iter()
        .any(|l| l.name == name && draft.original.as_deref() != Some(name))
    {
        return Err(format!("There is already a layout called \"{name}\"."));
    }
    let mut panes = Vec::with_capacity(draft.panes.len());
    for (index, (line, pane)) in draft.panes.iter().enumerate() {
        let args = parse_command_line(line)
            .ok_or_else(|| format!("Pane {}: a quote is not closed.", index + 1))?;
        panes.push(SpawnCommand {
            args,
            ..pane.clone()
        });
    }
    let layout = TgzPaneLayout {
        name: name.to_string(),
        layout: draft.layout,
        panes,
        cwd: draft.cwd.clone(),
    };
    let mut out = existing.to_vec();
    match draft
        .original
        .as_ref()
        .and_then(|original| out.iter().position(|l| &l.name == original))
    {
        Some(at) => out[at] = layout,
        None => out.push(layout),
    }
    Ok(out)
}

/// `src` with the key bindings Settings recorded for layout `old` moved to
/// `new` (renamed), or dropped (`None`, deleted).
pub(crate) fn move_layout_keys(
    src: &str,
    old: &str,
    new: Option<&str>,
) -> Result<String, super::lua_edit::EditError> {
    use super::lua_edit::{key_lines, set_key_lines};
    use config::keyassignment::KeyAssignment;
    let old_action = KeyAssignment::OpenPaneLayout(old.to_string());
    let old_tag = super::pane::shortcut_tag(&old_action);
    let lines: Vec<String> = key_lines(src)?
        .into_iter()
        .filter(|line| line.tag == old_tag)
        .map(|line| line.entry)
        .collect();
    if lines.is_empty() {
        return Ok(src.to_string());
    }
    let out = set_key_lines(src, &old_tag, &[])?;
    let Some(new) = new else {
        return Ok(out);
    };
    let new_action = KeyAssignment::OpenPaneLayout(new.to_string());
    let (from, to) = (
        crate::inputmap::lua_action(&old_action),
        crate::inputmap::lua_action(&new_action),
    );
    let moved: Vec<String> = lines
        .iter()
        .map(|entry| entry.replace(&from, &to))
        .collect();
    set_key_lines(&out, &super::pane::shortcut_tag(&new_action), &moved)
}

/// The layouts as one line of Lua, for `config.tgz_layouts = …`.
pub(crate) fn layouts_to_lua(layouts: &[TgzPaneLayout]) -> String {
    lua_literal(&Value::Array(
        layouts.iter().map(|layout| layout.to_dynamic()).collect(),
    ))
}

/// A dynamic value as a Lua literal on one line. Nulls and empty maps are
/// left out (every field that can be either has a default); an enum variant
/// with data is `{ Variant = value }`, which is how wezterm reads one.
pub(crate) fn lua_literal(value: &Value) -> String {
    match value {
        Value::Null => "nil".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::String(s) => quote(s),
        Value::U64(n) => n.to_string(),
        Value::I64(n) => n.to_string(),
        Value::F64(n) => {
            let n = n.into_inner();
            if n.is_finite() {
                format!("{n:?}")
            } else {
                "0".to_string()
            }
        }
        Value::Array(items) => {
            if items.is_empty() {
                return "{}".to_string();
            }
            let items: Vec<String> = items.iter().map(lua_literal).collect();
            format!("{{ {} }}", items.join(", "))
        }
        Value::Object(fields) => {
            let mut out = Vec::new();
            for (key, field) in fields.iter() {
                if matches!(field, Value::Null) || matches!(field, Value::Object(o) if o.is_empty())
                {
                    continue;
                }
                let key = match key {
                    Value::String(key) if is_identifier(key) => key.clone(),
                    Value::String(key) => format!("[{}]", quote(key)),
                    other => format!("[{}]", lua_literal(other)),
                };
                out.push(format!("{key} = {}", lua_literal(field)));
            }
            if out.is_empty() {
                "{}".to_string()
            } else {
                format!("{{ {} }}", out.join(", "))
            }
        }
    }
}

fn is_identifier(s: &str) -> bool {
    const KEYWORDS: [&str; 22] = [
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if",
        "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ];
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !KEYWORDS.contains(&s)
}

/// A Lua string literal that stays on one line.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\{:03}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wezterm_dynamic::FromDynamic;

    fn layout(name: &str, panes: Vec<SpawnCommand>) -> TgzPaneLayout {
        TgzPaneLayout {
            name: name.to_string(),
            layout: PaneLayout::AgentPlusShell,
            panes,
            cwd: AgentLauncherCwd::ProjectRoot,
        }
    }

    fn cmd(args: &[&str]) -> SpawnCommand {
        SpawnCommand {
            args: Some(args.iter().map(|s| s.to_string()).collect()),
            ..SpawnCommand::default()
        }
    }

    /// The Lua text, evaluated, is the layouts again.
    fn round_trip(layouts: &[TgzPaneLayout]) -> Vec<TgzPaneLayout> {
        let text = layouts_to_lua(layouts);
        assert!(!text.contains('\n'), "{text}");
        let lua = mlua::Lua::new();
        let value: mlua::Value = lua.load(&format!("return {text}")).eval().unwrap();
        let dynamic = luahelper::lua_value_to_dynamic(value).unwrap();
        Vec::<TgzPaneLayout>::from_dynamic(&dynamic, Default::default()).unwrap()
    }

    #[test]
    fn layouts_survive_the_trip_through_lua() {
        let mut odd = cmd(&["sh", "-c", "echo 'hi' \\ \"there\"\nnext"]);
        odd.cwd = Some("/tmp/a b".into());
        odd.label = Some("Log".into());
        odd.set_environment_variables
            .insert("FOO-BAR".to_string(), "1".to_string());
        let layouts = vec![
            layout(
                "Review",
                vec![cmd(&["claude"]), SpawnCommand::default(), odd],
            ),
            layout("end", vec![]),
        ];
        assert_eq!(round_trip(&layouts), layouts);
    }

    #[test]
    fn a_command_line_round_trips_through_its_text() {
        for args in [
            vec!["claude"],
            vec!["sh", "-c", "tail -f log.txt"],
            vec!["it's", ""],
        ] {
            let line = command_line(&cmd(&args));
            let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            assert_eq!(parse_command_line(&line), Some(Some(owned)), "{line}");
        }
        assert_eq!(command_line(&SpawnCommand::default()), "");
        assert_eq!(parse_command_line("  "), Some(None));
        assert_eq!(parse_command_line("echo 'open"), None);
    }

    #[test]
    fn saving_a_draft_replaces_its_layout_and_keeps_hidden_pane_fields() {
        let mut shell = SpawnCommand::default();
        shell.cwd = Some("/srv".into());
        let existing = vec![layout("A", vec![shell]), layout("B", vec![])];
        let mut draft = LayoutDraft::edit(&existing[0]);
        draft.name = "Renamed".to_string();
        draft.panes[0].0 = "htop".to_string();
        let saved = apply_draft(&existing, &draft).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].name, "Renamed");
        assert_eq!(saved[0].panes[0].args, Some(vec!["htop".to_string()]));
        assert_eq!(saved[0].panes[0].cwd, Some("/srv".into()));
        assert_eq!(saved[1], existing[1]);
    }

    #[test]
    fn a_draft_needs_a_free_name_and_closed_quotes() {
        let existing = vec![layout("A", vec![]), layout("B", vec![])];
        let mut draft = LayoutDraft::edit(&existing[0]);
        draft.name = "B".to_string();
        assert!(apply_draft(&existing, &draft).is_err());
        draft.name = "  ".to_string();
        assert!(apply_draft(&existing, &draft).is_err());
        draft.name = "A".to_string();
        draft
            .panes
            .push(("echo 'x".to_string(), SpawnCommand::default()));
        assert_eq!(
            apply_draft(&existing, &draft),
            Err("Pane 1: a quote is not closed.".to_string())
        );
    }

    #[test]
    fn renaming_a_layout_moves_its_key_and_deleting_drops_it() {
        use super::super::lua_edit::{key_lines, new_file, set_key_lines};
        use config::keyassignment::KeyAssignment;
        let entry = crate::inputmap::lua_key_entry(
            &::window::KeyCode::Char('r'),
            ::window::Modifiers::SUPER | ::window::Modifiers::ALT,
            &KeyAssignment::OpenPaneLayout("Review".into()),
        );
        let tag = super::super::pane::shortcut_tag(&KeyAssignment::OpenPaneLayout("Review".into()));
        let src = set_key_lines(&new_file(), &tag, &[entry]).unwrap();

        let renamed = move_layout_keys(&src, "Review", Some("Code review")).unwrap();
        let lines = key_lines(&renamed).unwrap();
        assert_eq!(lines.len(), 1, "{renamed}");
        assert!(lines[0].tag.contains("Code review"), "{renamed}");
        let to = crate::inputmap::lua_action(&KeyAssignment::OpenPaneLayout("Code review".into()));
        assert!(lines[0].entry.contains(&to), "{}", renamed);

        let deleted = move_layout_keys(&renamed, "Code review", None).unwrap();
        assert!(key_lines(&deleted).unwrap().is_empty(), "{deleted}");
        // A layout with no keys leaves the file alone.
        assert_eq!(move_layout_keys(&src, "Other", None).unwrap(), src);
    }

    #[test]
    fn a_new_layout_is_added_under_a_name_not_taken() {
        let existing = vec![layout("New layout", vec![])];
        let draft = LayoutDraft::new_layout(&existing);
        assert_eq!(draft.name, "New layout 2");
        let saved = apply_draft(&existing, &draft).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[1].panes.len(), 2);
        assert_eq!(saved[1].panes[0].args, None);
    }
}
