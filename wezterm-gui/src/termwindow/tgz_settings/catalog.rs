//! What the Settings tab shows: every setting's place, wording, kind of
//! control and how to read its current value from the loaded config.
//!
//! Deliberately not here: `agent_ui.enable_control_actions` (a safety gate
//! that should be switched on by somebody reading its documentation, not by
//! a stray click) and settings the sidebar already toggles at runtime, whose
//! per-window override would mask what the file says (`sidebar_auto_hide`).

use super::lua_edit::LuaValue;
use config::{
    AgentLaunchTarget, AgentRingColors, Config, DiffPanelLongLines, DiffPanelMode,
    DiffPanelPosition, DiffPanelRaisedMode, FileBrowserEditorPlacement, FileBrowserUi,
    PaneToolbeltPosition, SidebarPosition, SidebarTabDensity, SidebarTabTitleSource, SidebarTheme,
};
use std::convert::TryFrom;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    Appearance,
    Sidebar,
    Agents,
    Worktree,
    Changes,
    Input,
    Privacy,
    /// Buttons of the user's own for the sidebar footer (`sidebar_buttons`).
    Buttons,
    /// The fork's key bindings, to read: they are changed with `config.keys`.
    Shortcuts,
}

impl Section {
    pub(crate) const ALL: [Section; 9] = [
        Section::Appearance,
        Section::Sidebar,
        Section::Agents,
        Section::Worktree,
        Section::Changes,
        Section::Input,
        Section::Privacy,
        Section::Buttons,
        Section::Shortcuts,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Sidebar => "Sidebar",
            Section::Agents => "Agents",
            Section::Worktree => "Worktree",
            Section::Changes => "Changes",
            Section::Input => "Input",
            Section::Privacy => "Privacy",
            Section::Buttons => "Buttons",
            Section::Shortcuts => "Shortcuts",
        }
    }
}

/// A setting's current value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Toggle,
    /// One of an enum's variants.
    Choice(fn() -> &'static [&'static str]),
    Int {
        min: i64,
        max: i64,
    },
    Float {
        min: f64,
        max: f64,
        step: f64,
    },
    /// Free text; empty means not set.
    Text,
    /// A font family, picked from the installed ones.
    Font,
    /// A colour scheme, picked from the known ones; empty means none.
    Scheme,
}

pub(crate) struct Setting {
    pub section: Section,
    /// The key as Lua spells it, outermost table first.
    pub path: &'static [&'static str],
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub get: fn(&Config) -> Value,
}

impl Setting {
    /// `value` clamped to what the setting accepts, as Lua will hold it.
    pub(crate) fn to_lua(&self, value: &Value) -> LuaValue {
        match (self.kind, value) {
            (Kind::Font, Value::Text(name)) => LuaValue::Font(name.clone()),
            (Kind::Int { min, max }, Value::Int(n)) => LuaValue::Int((*n).clamp(min, max)),
            (Kind::Float { min, max, .. }, Value::Float(f)) => {
                // Steps of 0.05 accumulate noise; two decimals is all a
                // font size or an opacity needs.
                LuaValue::Float(((*f).clamp(min, max) * 100.).round() / 100.)
            }
            (_, Value::Bool(b)) => LuaValue::Bool(*b),
            (_, Value::Int(n)) => LuaValue::Int(*n),
            (_, Value::Float(f)) => LuaValue::Float(*f),
            (_, Value::Text(s)) => LuaValue::Str(s.clone()),
        }
    }

    /// The value one step up (`+1`) or down (`-1`) from `value`; `None`
    /// when the setting does not step or is already at its end.
    pub(crate) fn stepped(&self, value: &Value, direction: i8) -> Option<Value> {
        let next = match (self.kind, value) {
            (Kind::Int { min, max }, Value::Int(n)) => {
                Value::Int((n + i64::from(direction)).clamp(min, max))
            }
            (Kind::Float { min, max, step }, Value::Float(f)) => {
                let stepped = (f + step * f64::from(direction)).clamp(min, max);
                Value::Float((stepped * 100.).round() / 100.)
            }
            (Kind::Choice(variants), Value::Text(current)) => {
                let variants = variants();
                let at = variants.iter().position(|v| v == current)?;
                let next = at as i64 + i64::from(direction);
                Value::Text(variants.get(usize::try_from(next).ok()?)?.to_string())
            }
            _ => return None,
        };
        (next != *value).then_some(next)
    }
}

macro_rules! path {
    ($($p:ident).+) => { &[$(stringify!($p)),+] };
}

macro_rules! toggle {
    ($section:ident, $label:expr, $help:expr, $($p:ident).+) => {
        Setting {
            section: Section::$section,
            path: path!($($p).+),
            label: $label,
            help: $help,
            kind: Kind::Toggle,
            get: |c| Value::Bool(c.$($p).+),
        }
    };
}

macro_rules! choice {
    ($section:ident, $label:expr, $help:expr, $ty:ty, $($p:ident).+) => {
        Setting {
            section: Section::$section,
            path: path!($($p).+),
            label: $label,
            help: $help,
            kind: Kind::Choice(<$ty>::variants),
            get: |c| Value::Text(format!("{:?}", c.$($p).+)),
        }
    };
}

macro_rules! int {
    ($section:ident, $label:expr, $help:expr, $min:expr, $max:expr, $($p:ident).+) => {
        Setting {
            section: Section::$section,
            path: path!($($p).+),
            label: $label,
            help: $help,
            kind: Kind::Int { min: $min, max: $max },
            get: |c| Value::Int(c.$($p).+ as i64),
        }
    };
}

macro_rules! float {
    ($section:ident, $label:expr, $help:expr, $min:expr, $max:expr, $step:expr, $($p:ident).+) => {
        Setting {
            section: Section::$section,
            path: path!($($p).+),
            label: $label,
            help: $help,
            kind: Kind::Float { min: $min, max: $max, step: $step },
            get: |c| Value::Float(f64::from(c.$($p).+)),
        }
    };
}

macro_rules! text {
    ($section:ident, $label:expr, $help:expr, $($p:ident).+) => {
        Setting {
            section: Section::$section,
            path: path!($($p).+),
            label: $label,
            help: $help,
            kind: Kind::Text,
            get: |c| Value::Text(c.$($p).+.clone().unwrap_or_default()),
        }
    };
}

/// One field of a button of the user's own. The Settings tab has room for
/// four of them, `custom1` to `custom4`; more, other names and buttons that
/// run a key assignment are written in the file.
macro_rules! custom_button {
    ($id:literal, $field:literal, $label:expr, $help:expr, $get:expr) => {
        Setting {
            section: Section::Buttons,
            path: &["sidebar_buttons", $id, $field],
            label: $label,
            help: $help,
            kind: Kind::Text,
            get: |c| {
                let get: fn(&config::SidebarButtonConfig) -> String = $get;
                Value::Text(c.sidebar_buttons.get($id).map(get).unwrap_or_default())
            },
        }
    };
}

macro_rules! custom_button_label {
    ($id:literal, $n:literal) => {
        custom_button!(
            $id,
            "label",
            concat!("Button ", $n, ": label"),
            "Text on the button.",
            |b| b.label.clone()
        )
    };
}

macro_rules! custom_button_command {
    ($id:literal, $n:literal) => {
        custom_button!(
            $id,
            "command",
            concat!("Button ", $n, ": command"),
            "Runs in a new tab. Then add the button: Customize buttons, +.",
            |b| b.command.clone().unwrap_or_default()
        )
    };
}

macro_rules! custom_button_icon {
    ($id:literal, $n:literal) => {
        custom_button!(
            $id,
            "icon",
            concat!("Button ", $n, ": icon"),
            "One glyph before the label; optional.",
            |b| b.icon.clone().unwrap_or_default()
        )
    };
}

pub(crate) static CATALOG: &[Setting] = &[
    // Appearance
    Setting {
        section: Section::Appearance,
        path: &["font"],
        label: "Font",
        help: "Font family for the terminal and the sidebar.",
        kind: Kind::Font,
        get: |c| {
            Value::Text(
                c.font
                    .font
                    .first()
                    .map(|f| f.family.clone())
                    .unwrap_or_default(),
            )
        },
    },
    float!(
        Appearance,
        "Font size",
        "In points.",
        6.,
        40.,
        0.5,
        font_size
    ),
    float!(
        Appearance,
        "Line height",
        "1.0 is the font's own spacing.",
        0.8,
        2.,
        0.05,
        line_height
    ),
    Setting {
        section: Section::Appearance,
        path: &["color_scheme"],
        label: "Colour scheme",
        help: "A built-in or configured scheme. Without one, your own colours apply.",
        kind: Kind::Scheme,
        get: |c| Value::Text(c.color_scheme.clone().unwrap_or_default()),
    },
    float!(
        Appearance,
        "Window opacity",
        "1.0 is opaque.",
        0.3,
        1.,
        0.05,
        window_background_opacity
    ),
    toggle!(
        Appearance,
        "Scroll bar",
        "Show a scroll bar in terminal panes.",
        enable_scroll_bar
    ),
    toggle!(
        Appearance,
        "Hide scroll bar when idle",
        "Show it only while scrolling or hovering.",
        scroll_bar_auto_hide
    ),
    // Sidebar
    toggle!(
        Sidebar,
        "Sidebar",
        "Vertical tab list. Off shows the classic tab bar.",
        sidebar_enabled
    ),
    choice!(
        Sidebar,
        "Position",
        "Which window edge the sidebar sits on.",
        SidebarPosition,
        sidebar_position
    ),
    choice!(
        Sidebar,
        "Theme",
        "Auto follows your colours when you set any.",
        SidebarTheme,
        sidebar_theme
    ),
    choice!(
        Sidebar,
        "Tab density",
        "How much room each tab row takes.",
        SidebarTabDensity,
        sidebar_tab_density
    ),
    choice!(
        Sidebar,
        "Tab title",
        "What a tab row is named after.",
        SidebarTabTitleSource,
        sidebar_tab_title_source
    ),
    toggle!(
        Sidebar,
        "Hover details",
        "Show a tab's details while the pointer rests on it.",
        sidebar_tab_hover_details
    ),
    toggle!(
        Sidebar,
        "Tab list scroll bar",
        "Show a scroll bar when the tabs do not fit.",
        sidebar_scroll_bar
    ),
    int!(
        Sidebar,
        "Buttons per row",
        "Most buttons one footer row holds.",
        1,
        7,
        sidebar_footer_max_per_row
    ),
    text!(
        Sidebar,
        "SSH default host",
        "Host the SSH button connects to. Empty opens the host list.",
        sidebar_ssh_default_host
    ),
    // Agents
    toggle!(
        Agents,
        "Agent awareness",
        "Detect coding agents in panes.",
        agent_ui.enabled
    ),
    toggle!(
        Agents,
        "Status rings on tabs",
        "Show each agent's state on its tab row.",
        agent_ui.show_sidebar_badges
    ),
    toggle!(
        Agents,
        "Toolbelt",
        "Floating controls on agent panes.",
        agent_ui.show_pane_toolbelt
    ),
    choice!(
        Agents,
        "Toolbelt position",
        "Where the toolbelt starts before you drag it.",
        PaneToolbeltPosition,
        agent_ui.toolbelt_position
    ),
    toggle!(
        Agents,
        "Notify when waiting",
        "Send a notification when an agent needs you.",
        agent_ui.waiting_notification
    ),
    toggle!(
        Agents,
        "Pulse while working",
        "Animate the ring of a working agent.",
        agent_ui.pulse_working_dot
    ),
    choice!(
        Agents,
        "Ring colours",
        "Colour set for agent states.",
        AgentRingColors,
        agent_ui.ring_colors
    ),
    toggle!(
        Agents,
        "Agents section",
        "List every agent at the bottom of the sidebar.",
        agent_ui.section.enabled
    ),
    toggle!(
        Agents,
        "Show activity",
        "What each agent is doing right now.",
        agent_ui.section.show_activity
    ),
    toggle!(
        Agents,
        "Show tokens",
        "Token use per agent.",
        agent_ui.section.show_tokens
    ),
    toggle!(
        Agents,
        "Show plan limits",
        "Subscription usage on the \u{03a3} button.",
        agent_ui.section.show_quota
    ),
    toggle!(
        Agents,
        "Claude usage relay",
        "Read Claude's limits through its status line command.",
        agent_ui.section.claude_usage_relay
    ),
    toggle!(
        Agents,
        "Waiting agents first",
        "Sort agents that need you to the top.",
        agent_ui.section.sort_attention_first
    ),
    choice!(
        Agents,
        "Open new agents in",
        "Where the agent button puts a new agent.",
        AgentLaunchTarget,
        agent_ui.launcher.open_in
    ),
    int!(
        Agents,
        "Agents per tab",
        "Most panes the launcher tiles into one tab.",
        1,
        8,
        agent_ui.launcher.max_panes_per_tab
    ),
    text!(
        Agents,
        "Default agent",
        "Adapter the agent button starts, e.g. claude. Empty uses the first installed.",
        agent_ui.launcher.default_adapter
    ),
    // Worktree
    choice!(
        Worktree,
        "Interface",
        "Panel docks a tree beside the panes; Picker opens a fuzzy finder.",
        FileBrowserUi,
        file_browser.ui
    ),
    choice!(
        Worktree,
        "Open files in",
        "Where the editor opens.",
        FileBrowserEditorPlacement,
        file_browser.editor_placement
    ),
    toggle!(
        Worktree,
        "Hidden files",
        "List files whose name starts with a dot.",
        file_browser.show_hidden
    ),
    toggle!(
        Worktree,
        "Respect .gitignore",
        "Leave ignored files out.",
        file_browser.respect_gitignore
    ),
    toggle!(
        Worktree,
        "Reuse editor pane",
        "Open the next file in the editor pane that is already there.",
        file_browser.reuse_editor_pane
    ),
    // Changes
    toggle!(
        Changes,
        "Changes panel",
        "Diff of what changed, per tab.",
        diff_panel.enabled
    ),
    choice!(
        Changes,
        "Position",
        "Which side the panel opens on.",
        DiffPanelPosition,
        diff_panel.position
    ),
    choice!(
        Changes,
        "Shows",
        "Session: what this tab's agents touched. All: the whole working tree.",
        DiffPanelMode,
        diff_panel.default_mode
    ),
    choice!(
        Changes,
        "When raised",
        "Float over the pane or reserve room beside it.",
        DiffPanelRaisedMode,
        diff_panel.raised_mode
    ),
    choice!(
        Changes,
        "Long lines",
        "Wrap, scroll sideways or cut off.",
        DiffPanelLongLines,
        diff_panel.long_lines
    ),
    toggle!(
        Changes,
        "Keep clear of the prompt",
        "A raised panel stops above the pane's input line.",
        diff_panel.avoid_input
    ),
    // Input
    toggle!(
        Input,
        "Rich input",
        "Multi-line composer for prompts.",
        rich_input.enabled
    ),
    toggle!(
        Input,
        "Agent panes only",
        "Offer the composer only where an agent runs.",
        rich_input.agent_panes_only
    ),
    toggle!(
        Input,
        "Docked input strip",
        "Keep an input strip at the bottom of the active pane.",
        rich_input.docked
    ),
    int!(
        Input,
        "Strip height",
        "Rows the docked strip reserves.",
        1,
        12,
        rich_input.dock_rows
    ),
    toggle!(
        Input,
        "Preview before sending",
        "Show what will be sent.",
        rich_input.show_send_preview
    ),
    toggle!(
        Input,
        "Confirm multi-line sends",
        "Ask before sending more than one line.",
        rich_input.require_confirm_for_multiline
    ),
    // Privacy
    toggle!(
        Privacy,
        "Secret masking",
        "Recognise tokens and passwords in output.",
        secret_masking.enabled
    ),
    toggle!(
        Privacy,
        "Mask on screen",
        "Paint recognised secrets as bullets.",
        secret_masking.mask_on_screen
    ),
    toggle!(
        Privacy,
        "Reveal on hover",
        "Show a masked secret while the pointer is on it.",
        secret_masking.reveal_on_hover
    ),
    toggle!(
        Privacy,
        "Redact when copying",
        "Copy actions leave recognised secrets out.",
        secret_masking.redact_copy_actions
    ),
    toggle!(
        Privacy,
        "Secure Keyboard Entry at password prompts",
        "macOS: turn it on by itself while a password is typed.",
        macos_secure_keyboard_entry_auto
    ),
    toggle!(
        Privacy,
        "Heal a stuck full-screen view",
        "Leave the alternate screen a crashed program left behind.",
        heal_stranded_alt_screen
    ),
    // Buttons
    custom_button_label!("custom1", "1"),
    custom_button_command!("custom1", "1"),
    custom_button_icon!("custom1", "1"),
    custom_button_label!("custom2", "2"),
    custom_button_command!("custom2", "2"),
    custom_button_icon!("custom2", "2"),
    custom_button_label!("custom3", "3"),
    custom_button_command!("custom3", "3"),
    custom_button_icon!("custom3", "3"),
    custom_button_label!("custom4", "4"),
    custom_button_command!("custom4", "4"),
    custom_button_icon!("custom4", "4"),
];

/// Indices into [`CATALOG`] of the settings in `section`, in order.
pub(crate) fn in_section(section: Section) -> Vec<usize> {
    CATALOG
        .iter()
        .enumerate()
        .filter(|(_, s)| s.section == section)
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_reads_a_value_of_its_kind_from_the_default_config() {
        let config = Config::default_config();
        for setting in CATALOG {
            let value = (setting.get)(&config);
            let fits = matches!(
                (setting.kind, &value),
                (Kind::Toggle, Value::Bool(_))
                    | (Kind::Choice(_), Value::Text(_))
                    | (Kind::Int { .. }, Value::Int(_))
                    | (Kind::Float { .. }, Value::Float(_))
                    | (Kind::Text | Kind::Font | Kind::Scheme, Value::Text(_))
            );
            assert!(fits, "{}: {value:?}", setting.label);
            if let (Kind::Choice(variants), Value::Text(current)) = (setting.kind, &value) {
                assert!(
                    variants().contains(&current.as_str()),
                    "{}: {current} is not one of {:?}",
                    setting.label,
                    variants()
                );
            }
        }
    }

    #[test]
    fn paths_are_unique_and_every_section_has_settings() {
        let mut paths: Vec<_> = CATALOG.iter().map(|s| s.path).collect();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), CATALOG.len());
        for section in Section::ALL {
            // Shortcuts lists key bindings, not settings.
            assert_eq!(
                in_section(section).is_empty(),
                section == Section::Shortcuts,
                "{:?}",
                section
            );
        }
    }

    #[test]
    fn stepping_stays_in_range_and_walks_enum_variants() {
        let size = CATALOG.iter().find(|s| s.path == ["font_size"]).unwrap();
        assert_eq!(
            size.stepped(&Value::Float(12.), 1),
            Some(Value::Float(12.5))
        );
        assert_eq!(size.stepped(&Value::Float(40.), 1), None);
        assert_eq!(size.to_lua(&Value::Float(99.)), LuaValue::Float(40.));

        let position = CATALOG
            .iter()
            .find(|s| s.path == ["sidebar_position"])
            .unwrap();
        let Kind::Choice(variants) = position.kind else {
            panic!("position is a choice");
        };
        let first = Value::Text(variants()[0].to_string());
        assert_eq!(position.stepped(&first, -1), None);
        assert_eq!(
            position.stepped(&first, 1),
            Some(Value::Text(variants()[1].to_string()))
        );
    }
}
