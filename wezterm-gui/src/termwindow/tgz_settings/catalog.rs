//! What the Settings tab shows: every setting's place, wording, kind of
//! control and how to read its current value from the loaded config.
//!
//! Deliberately not here: `agent_ui.enable_control_actions` (a safety gate
//! that should be switched on by somebody reading its documentation, not by
//! a stray click) and settings the sidebar already toggles at runtime, whose
//! per-window override would mask what the file says (`sidebar_auto_hide`).

use super::lua_edit::LuaValue;
use config::{
    AgentCopyClick, AgentLaunchTarget, AgentRingColors, Config, DiffPanelLongLines, DiffPanelMode,
    DiffPanelPosition, DiffPanelRaisedMode, FileBrowserEditorPlacement, FileBrowserUi,
    NotificationHandling, PaneToolbeltPosition, ShellCopyClick, SidebarNewTabStyle,
    SidebarPosition, SidebarTabDensity, SidebarTabGrouping, SidebarTabTitleSource, SidebarTheme,
};
use std::convert::TryFrom;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    Appearance,
    /// `sidebar_colors`: every colour of the fork's chrome.
    Colors,
    Sidebar,
    Agents,
    /// Every system notification, with a master switch.
    Notifications,
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
    pub(crate) const ALL: [Section; 11] = [
        Section::Appearance,
        Section::Colors,
        Section::Sidebar,
        Section::Agents,
        Section::Notifications,
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
            Section::Colors => "Colours",
            Section::Sidebar => "Sidebar",
            Section::Agents => "Agents",
            Section::Notifications => "Notifications",
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
    /// A whole number, shown and stepped in a display unit: the config holds
    /// `value * scale` (milliseconds shown as minutes, say), `suffix` names
    /// the unit. `scale` is 1 and `suffix` empty for a plain number.
    Int {
        min: i64,
        max: i64,
        scale: i64,
        suffix: &'static str,
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
    /// A colour, typed as `#rrggbb` (or anything `colors` accepts); empty
    /// means the theme's own.
    Color,
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
            (
                Kind::Int {
                    min, max, scale, ..
                },
                Value::Int(n),
            ) => LuaValue::Int((*n).clamp(min, max).saturating_mul(scale)),
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
            (Kind::Int { min, max, .. }, Value::Int(n)) => {
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
            kind: Kind::Int { min: $min, max: $max, scale: 1, suffix: "" },
            get: |c| Value::Int(c.$($p).+ as i64),
        }
    };
}

/// A millisecond key shown in a coarser unit (`scale` ms each, `suffix` after
/// the number). The config value is rounded to the nearest unit for display.
macro_rules! duration {
    ($section:ident, $label:expr, $help:expr, $min:expr, $max:expr, $scale:expr, $suffix:expr, $($p:ident).+) => {
        Setting {
            section: Section::$section,
            path: path!($($p).+),
            label: $label,
            help: $help,
            kind: Kind::Int { min: $min, max: $max, scale: $scale, suffix: $suffix },
            get: |c| Value::Int(((c.$($p).+ as i64) + $scale / 2) / $scale),
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

/// One `sidebar_colors` entry, shown as `#rrggbb`.
macro_rules! chrome_color {
    ($label:expr, $help:expr, $field:ident) => {
        Setting {
            section: Section::Colors,
            path: &["sidebar_colors", stringify!($field)],
            label: $label,
            help: $help,
            kind: Kind::Color,
            get: |c| Value::Text(color_hex(c.sidebar_colors.$field)),
        }
    };
}

/// `#rrggbb` (or `#rrggbbaa` when not opaque); empty when unset.
pub(crate) fn color_hex(color: Option<config::RgbaColor>) -> String {
    let Some(color) = color else {
        return String::new();
    };
    let (r, g, b, a) = color.to_srgb_u8();
    if a == 0xff {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

/// Whether `text` is a colour the config would accept.
pub(crate) fn is_color(text: &str) -> bool {
    config::RgbaColor::try_from(text.trim().to_string()).is_ok()
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

/// One `agent_ui.animations` switch. The table is optional in the config, so
/// the value is read through `resolved_animations` rather than a field path.
macro_rules! animation {
    ($label:expr, $help:expr, $field:ident) => {
        Setting {
            section: Section::Agents,
            path: &["agent_ui", "animations", stringify!($field)],
            label: $label,
            help: $help,
            kind: Kind::Toggle,
            get: |c| Value::Bool(c.agent_ui.resolved_animations().$field),
        }
    };
}

/// Small headings that split a long section into groups: each is drawn
/// above the setting with that path.
const GROUP_HEADINGS: &[(&[&str], &str)] = &[
    (&["font"], "Text"),
    (&["color_scheme"], "Window"),
    (&["sidebar_colors", "accent"], "Chrome"),
    (&["sidebar_new_tab_style"], "New Tab button"),
    (&["sidebar_colors", "ring_running"], "Rings"),
    (&["sidebar_colors", "background"], "Surfaces and text"),
    (&["agent_ui", "enabled"], "Detection"),
    (&["agent_ui", "show_pane_toolbelt"], "Toolbelt"),
    (&["agent_ui", "section", "enabled"], "Agents section"),
    (&["agent_ui", "launcher", "open_in"], "Launcher"),
    (&["agent_ui", "animations", "enabled"], "Animations"),
];

/// The heading drawn above `setting`, if a group starts there.
pub(crate) fn group_heading(setting: &Setting) -> Option<&'static str> {
    GROUP_HEADINGS
        .iter()
        .find(|(path, _)| *path == setting.path)
        .map(|(_, heading)| *heading)
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
    toggle!(
        Appearance,
        "App name as window title",
        "Off: the active tab's title, as upstream WezTerm shows it.",
        window_title_app_name
    ),
    // Colours
    chrome_color!("Accent", "Focus, selections, the active rail. Default for New Tab and the running ring.", accent),
    chrome_color!("Attention", "An agent waiting for you. Default for the waiting ring.", attention),
    chrome_color!("Focus ring", "Around whatever has keyboard focus. Default: a soft lavender, never white.", focus_ring),
    choice!(
        Colors,
        "New Tab style",
        "Filled uses the colour below; Plain matches the other sidebar buttons.",
        SidebarNewTabStyle,
        sidebar_new_tab_style
    ),
    chrome_color!("New Tab button", "Fill of + New Tab. Default: the accent.", new_tab),
    chrome_color!("New Tab label", "Default: black or white, whichever reads.", new_tab_text),
    chrome_color!("Ring: running", "The arc turning while an agent works. Default: the accent.", ring_running),
    chrome_color!("Ring: waiting", "The ring while an agent waits. Default: attention.", ring_waiting),
    chrome_color!("Ring: track", "The unfilled part of a context ring.", ring_track),
    chrome_color!("Background", "Sidebar and panels.", background),
    chrome_color!("Row", "Resting fill of rows and buttons.", row),
    chrome_color!("Row: active", "The active tab's row.", row_active),
    chrome_color!("Row: hover", "Under the pointer.", row_hover),
    chrome_color!("Row: pressed", "While pressed.", row_pressed),
    chrome_color!("Row: working", "Behind a row whose agent works.", row_working),
    chrome_color!("Field", "Search and text fields.", field),
    chrome_color!("Field: typing", "A field being typed in.", field_focused),
    chrome_color!("Divider", "Between the sidebar and the terminal, and between sections.", divider),
    chrome_color!("Menu edge", "Edge of menus and floating cards.", menu_border),
    chrome_color!("Text", "Titles and the active tab.", text),
    chrome_color!("Text: idle", "Rows that are not active.", text_idle),
    chrome_color!("Text: meta", "Second lines, hints and counts.", text_meta),
    // Sidebar
    float!(
        Sidebar,
        "Text size",
        "Sidebar text in points; the sizes below start from it.",
        8.,
        30.,
        0.5,
        sidebar_font_size
    ),
    Setting {
        section: Section::Sidebar,
        path: &["sidebar_tab_font_size"],
        label: "Tab text size",
        help: "Tab rows, group headers, pane and subagent rows.",
        kind: Kind::Float {
            min: 8.,
            max: 30.,
            step: 0.5,
        },
        // Unset follows the sidebar text size, so show that.
        get: |c| Value::Float(c.sidebar_tab_font_size.unwrap_or(c.sidebar_font_size)),
    },
    Setting {
        section: Section::Sidebar,
        path: &["sidebar_agents_font_size"],
        label: "Agents text size",
        help: "The Agents section.",
        kind: Kind::Float {
            min: 8.,
            max: 30.,
            step: 0.5,
        },
        // Unset follows the sidebar text size, so show that.
        get: |c| Value::Float(c.sidebar_agents_font_size.unwrap_or(c.sidebar_font_size)),
    },
    Setting {
        section: Section::Sidebar,
        path: &["sidebar_button_font_size"],
        label: "Button text size",
        help: "The buttons at the bottom and the search field.",
        kind: Kind::Float {
            min: 8.,
            max: 30.,
            step: 0.5,
        },
        // Unset follows the sidebar text size, so show that.
        get: |c| Value::Float(c.sidebar_button_font_size.unwrap_or(c.sidebar_font_size)),
    },
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
        "Tab groups",
        "Auto puts a new tab under its project folder. Manual groups only by hand. Off is one flat list.",
        SidebarTabGrouping,
        sidebar_tab_grouping
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
        "Grey once seen",
        "A waiting agent's tab turns from amber to a grey \u{201c}ready\u{201d} once you have looked at it.",
        agent_ui.waiting_settles_when_seen
    ),
    duration!(
        Agents,
        "Counts as seen after",
        "How long you have to stay on a waiting agent's pane before it counts as seen. 0 = as soon as you look.",
        0,
        60,
        1000,
        " s",
        agent_ui.waiting_ack_ms
    ),
    duration!(
        Agents,
        "Grey after",
        "A waiting agent nobody visited turns grey after this long. 0 keeps it amber until you answer.",
        0,
        1440,
        60_000,
        " min",
        agent_ui.waiting_settle_after_ms
    ),
    duration!(
        Agents,
        "Cold cache after",
        "Mark a tab with a snowflake and the idle time once its session has made no request for this long. 0 turns it off.",
        0,
        1440,
        60_000,
        " min",
        agent_ui.cache_cold_after_ms
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
    duration!(
        Agents,
        "Keep finished helpers",
        "How long a finished subagent stays listed in the Agents section. 0 keeps all.",
        0,
        10_080,
        60_000,
        " min",
        agent_ui.section.finished_subagents_keep_ms
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
    Setting {
        section: Section::Agents,
        path: &["agent_ui", "animations", "enabled"],
        label: "Animations",
        help: "Off: rings and dots hold still and show state by colour alone.",
        kind: Kind::Toggle,
        // The older `pulse_working_dot = false` still stops everything until
        // the table is written; show what is in effect.
        get: |c| {
            Value::Bool(
                c.agent_ui.resolved_animations().enabled
                    && !c.agent_ui.legacy_animation_kill_switch(),
            )
        },
    },
    animation!(
        "Follow Reduce Motion",
        "While macOS Reduce Motion is on, hold everything still.",
        follow_reduce_motion
    ),
    animation!(
        "Running ring",
        "The arc turning while an agent works. Off: a still ring.",
        working_ring
    ),
    animation!(
        "Waiting glow",
        "The outline and slow breathing on a tab whose agent needs you.",
        waiting_glow
    ),
    animation!(
        "Finish ripple",
        "One ripple when an agent finishes.",
        resolve_ripple
    ),
    animation!(
        "New agent fade-in",
        "A new agent's row slides in.",
        agent_enter
    ),
    animation!(
        "Pulse while working",
        "The status dot pulses while its agent works.",
        dot_pulse
    ),
    animation!(
        "Rail breathe",
        "The active tab's rail slowly breathes.",
        rail_breathe
    ),
    animation!(
        "Animate in background windows",
        "Off saves battery while TGZTerminal is not focused.",
        unfocused
    ),
    // Worktree
    toggle!(
        Notifications,
        "Notifications",
        "Master switch: off silences every notification TGZTerminal shows and clears the dock badge.",
        tgz_notifications.enabled
    ),
    toggle!(
        Notifications,
        "Agent needs you",
        "A notification when an agent is waiting for you while you are elsewhere.",
        agent_ui.waiting_notification
    ),
    choice!(
        Notifications,
        "From programs",
        "Notifications programs send (OSC 9/777), e.g. a CLI saying it is done.",
        NotificationHandling,
        notification_handling
    ),
    toggle!(
        Notifications,
        "Updates",
        "Check for TGZTerminal updates and say when one is out.",
        check_for_updates
    ),
    toggle!(
        Notifications,
        "Copy confirmations",
        "A \u{201c}Copied\u{201d} message after copying from a pane or the sidebar.",
        tgz_notifications.copy
    ),
    toggle!(
        Notifications,
        "Status and errors",
        "Feedback from agent, layout, Worktree and sidebar actions, including when they fail.",
        tgz_notifications.status
    ),
    toggle!(
        Notifications,
        "Dock badge",
        "Number of waiting agents on the dock icon while the app is in the background (macOS).",
        agent_ui.dock_badge
    ),
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
    toggle!(
        Input,
        "iTerm2-style pane keys",
        "Cmd+D splits, Cmd+Opt+Arrows move focus, Cmd+Opt+1…6 apply a layout. \
         Your own key bindings still win.",
        tgz_pane_keys
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
        "Every Copy row leaves secrets out, so the (no secrets) rows are not needed.",
        secret_masking.redact_copy_actions
    ),
    choice!(
        Privacy,
        "Copy icon on a shell",
        "What a click on the Copy icon copies. The others are on right click.",
        ShellCopyClick,
        pane_toolbelt.shell_copy_click
    ),
    choice!(
        Privacy,
        "Copy icon on an agent",
        "What a click copies. An agent known only by its title is always redacted.",
        AgentCopyClick,
        pane_toolbelt.agent_copy_click
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
    fn colour_fields_show_hex_and_accept_only_colours() {
        use std::convert::TryFrom;
        assert_eq!(color_hex(None), "");
        let blue = config::RgbaColor::try_from("#58A6FF".to_string()).unwrap();
        assert_eq!(color_hex(Some(blue)), "#58a6ff");
        assert!(is_color("#58a6ff"));
        assert!(is_color(" #58a6ff "));
        assert!(!is_color("blueish"));
        assert!(!is_color(""));
        let colours: Vec<&Setting> = CATALOG
            .iter()
            .filter(|setting| matches!(setting.kind, Kind::Color))
            .collect();
        assert_eq!(colours.len(), 21);
        assert!(colours.iter().all(
            |setting| setting.section == Section::Colors && setting.path[0] == "sidebar_colors"
        ));
    }

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
                    | (
                        Kind::Text | Kind::Font | Kind::Scheme | Kind::Color,
                        Value::Text(_)
                    )
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

    /// Millisecond keys show and step in minutes or seconds, and write
    /// milliseconds back: the file never learns about the display unit.
    #[test]
    fn a_duration_setting_shows_its_unit_and_writes_milliseconds() {
        let find = |path: &[&str]| CATALOG.iter().find(|s| s.path == path).unwrap();
        let config = Config::default_config();

        let grey = find(&["agent_ui", "waiting_settle_after_ms"]);
        assert_eq!((grey.get)(&config), Value::Int(10), "600000 ms = 10 min");
        assert!(matches!(
            grey.to_lua(&Value::Int(15)),
            LuaValue::Int(900_000)
        ));
        assert_eq!(grey.stepped(&Value::Int(10), 1), Some(Value::Int(11)));
        assert_eq!(grey.stepped(&Value::Int(0), -1), None, "0 is the floor");

        let seen = find(&["agent_ui", "waiting_ack_ms"]);
        assert_eq!((seen.get)(&config), Value::Int(5), "5000 ms = 5 s");
        assert!(matches!(seen.to_lua(&Value::Int(2)), LuaValue::Int(2000)));

        let cold = find(&["agent_ui", "cache_cold_after_ms"]);
        assert_eq!((cold.get)(&config), Value::Int(60));
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
    fn every_group_heading_sits_above_a_setting() {
        for (path, heading) in GROUP_HEADINGS {
            assert!(
                CATALOG.iter().any(|s| s.path == *path),
                "{heading}: no setting at {path:?}"
            );
        }
        let animations: Vec<_> = in_section(Section::Agents)
            .into_iter()
            .map(|index| &CATALOG[index])
            .skip_while(|s| group_heading(s) != Some("Animations"))
            .collect();
        // The group holds every switch of the table, and nothing else.
        assert_eq!(animations.len(), 9);
        assert!(animations
            .iter()
            .all(|s| s.path.starts_with(&["agent_ui", "animations"])));
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
