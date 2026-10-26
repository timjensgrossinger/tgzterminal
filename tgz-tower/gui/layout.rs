//! Pure layout: model + measurer + theme -> display list, hit map, scroll regions.
//!
//! Everything is computed in window pixels inside `area`, the pane's content
//! rectangle. Nothing here touches the GPU; `render.rs` replays the list.

use super::chart::{
    bar_chart, compact, compact_money, share_bar, spark_bars, BarChartSpec, Column,
};
use super::clip::Rect;
use super::forms::{
    caret_segment, civil_label, hex_rgb, local_civil, looks_local, segments, when_preview, Field,
    FieldId, Place, Sheet, SheetFlag, AUTO_ROLE, DRIVERS, ENDPOINT_KINDS, ROLE_COLORS, TEMPLATES,
};
use super::model::{Action, Conn, Drawer, Extra, Hit, Model, ScrollId, ScrollRegion, View};
use super::proto::{
    Agent, AgentStatus, ChatKind, DayStats, DriverKind, Flight, GateState, HandDirection,
    HandoffPacket, HandoffState, Health, InboxItem, InboxKind, IssueInfo, Locality, ModelInfo,
    ModelRef, NoticeLevel, Role, RouteInfo, RoutineRow, RunResult, ScheduleRow, SlaClock,
    SnapshotOrigin, SourceStatus, Stage, TimelineBody, TimelineEntry,
};
use super::text::{age, ellipsize, usage_label, wrap, Font, Measure};
use super::theme::{Color, Theme};

#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Rect {
        r: Rect,
        color: Color,
        radius: f32,
        clip: Rect,
    },
    /// `y` is the top of the line box.
    Text {
        x: f32,
        y: f32,
        text: String,
        font: Font,
        color: Color,
        clip: Rect,
    },
}

#[derive(Default)]
pub struct Scene {
    pub cmds: Vec<Cmd>,
    pub hits: Vec<(Rect, Hit)>,
    pub scrolls: Vec<ScrollRegion>,
    /// Pixels per wheel notch.
    pub wheel_step: f32,
    /// Window-pixel top-left of the focused field's caret, for the IME.
    pub caret: Option<(f32, f32)>,
}

pub fn layout(
    model: &Model,
    theme: &Theme,
    m: &mut dyn Measure,
    area: Rect,
    now_unix: u64,
) -> Scene {
    let lh = m.line_height(Font::Body);
    let u = (lh / 5.0).round().max(2.0);
    let mut l = L {
        m,
        t: theme,
        md: model,
        s: Scene::default(),
        clip: area,
        u,
        lh,
        now: now_unix,
    };
    l.s.wheel_step = (lh * 2.0).round();
    l.root(area);
    l.s
}

struct L<'a> {
    m: &'a mut dyn Measure,
    t: &'a Theme,
    md: &'a Model,
    s: Scene,
    clip: Rect,
    /// Spacing unit (about a fifth of a body line).
    u: f32,
    /// Body line height.
    lh: f32,
    now: u64,
}

const EMPTY: Rect = Rect::new(0.0, 0.0, 0.0, 0.0);

/// The parts of Setup, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Notices,
    Terms,
    Roles,
    Models,
    Routing,
    Connect,
    Policy,
    LocalOnly,
    Schedules,
    Sources,
    Machine,
    Extras,
}

impl<'a> L<'a> {
    // -- primitives ---------------------------------------------------------

    fn fill(&mut self, r: Rect, color: Color, radius: f32) {
        if color.a <= 0.0 || r.intersect(&self.clip).is_none() {
            return;
        }
        self.s.cmds.push(Cmd::Rect {
            r,
            color,
            radius,
            clip: self.clip,
        });
    }

    fn text(&mut self, x: f32, y: f32, text: &str, font: Font, color: Color) -> f32 {
        let w = self.m.width(text, font);
        let h = self.m.line_height(font);
        if !text.is_empty() && Rect::new(x, y, w, h).intersect(&self.clip).is_some() {
            self.s.cmds.push(Cmd::Text {
                x,
                y,
                text: text.to_string(),
                font,
                color,
                clip: self.clip,
            });
        }
        w
    }

    /// Draw `text` ellipsized to `max_w`; returns the drawn width.
    fn text_fit(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        font: Font,
        color: Color,
        max_w: f32,
    ) -> f32 {
        let s = ellipsize(self.m, text, font, max_w);
        self.text(x, y, &s, font, color)
    }

    /// Wrapped paragraph; returns the height used.
    fn para(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        font: Font,
        color: Color,
        max_w: f32,
        max_lines: usize,
    ) -> f32 {
        let lh = self.lh_of(font);
        let mut h = 0.0;
        for raw in text.lines() {
            if h / lh >= max_lines as f32 {
                break;
            }
            let left = max_lines - (h / lh) as usize;
            if raw.trim().is_empty() {
                h += lh * 0.5;
                continue;
            }
            for line in wrap(self.m, raw, font, max_w, left) {
                self.text(x, y + h, &line, font, color);
                h += lh;
            }
        }
        h
    }

    /// Right-aligned text ending at `right`.
    fn text_right(&mut self, right: f32, y: f32, text: &str, font: Font, color: Color) -> f32 {
        let w = self.m.width(text, font);
        self.text(right - w, y, text, font, color)
    }

    fn hit(&mut self, r: Rect, hit: Hit) {
        if let Some(v) = r.intersect(&self.clip) {
            self.s.hits.push((v, hit));
        }
    }

    fn hovered(&self, hit: &Hit) -> bool {
        self.md.hover.as_ref() == Some(hit)
    }

    fn push_clip(&mut self, r: Rect) -> Rect {
        let old = self.clip;
        self.clip = old.intersect(&r).unwrap_or(EMPTY);
        old
    }

    fn pop_clip(&mut self, old: Rect) {
        self.clip = old;
    }

    fn lh_of(&self, font: Font) -> f32 {
        self.m.line_height(font)
    }

    /// Register a scroll region and return its offset from the top, clamped.
    fn scroll_region(
        &mut self,
        id: ScrollId,
        viewport: Rect,
        content: f32,
        horizontal: bool,
        from_bottom: bool,
    ) -> f32 {
        let region = ScrollRegion {
            id,
            viewport,
            content,
            horizontal,
            from_bottom,
        };
        let max = region.max_offset();
        let stored = self.md.scroll_of(id).max(0.0).min(max);
        self.s.scrolls.push(region);
        if from_bottom {
            max - stored
        } else {
            stored
        }
    }

    /// A thin scrollbar thumb when content overflows.
    fn scrollbar(&mut self, viewport: Rect, content: f32, off: f32) {
        if content <= viewport.h + 0.5 {
            return;
        }
        let w = (self.u * 0.8).max(2.0);
        let h = (viewport.h * viewport.h / content).max(self.lh);
        let y = viewport.y + (viewport.h - h) * (off / (content - viewport.h));
        let c = self.t.muted.alpha(0.35);
        self.fill(
            Rect::new(viewport.right() - w - self.u * 0.5, y, w, h),
            c,
            w / 2.0,
        );
    }

    fn shadow(&mut self, r: Rect, radius: f32) {
        let s = self.t.shadow;
        self.fill(
            r.translate(0.0, self.u * 0.5).inset(-self.u * 0.25),
            s.alpha(s.a * 0.35),
            radius + self.u * 0.25,
        );
        self.fill(r.translate(0.0, self.u * 0.25), s.alpha(s.a * 0.6), radius);
    }

    fn dot(&mut self, cx: f32, cy: f32, d: f32, color: Color) {
        self.fill(Rect::new(cx - d / 2.0, cy - d / 2.0, d, d), color, d / 2.0);
    }

    fn chip_h(&self) -> f32 {
        self.lh_of(Font::SmallStrong) + self.u * 0.6
    }

    /// Horizontal text inset of a pill: clear of the rounded ends.
    fn chip_pad(&self) -> f32 {
        (self.chip_h() * 0.45).max(self.u * 1.6)
    }

    fn chip_w(&mut self, label: &str) -> f32 {
        self.m.width(label, Font::SmallStrong) + self.chip_pad() * 2.0
    }

    /// A pill with text; returns its rect.
    fn chip(&mut self, x: f32, y: f32, label: &str, fg: Color, bg: Color) -> Rect {
        let font = Font::SmallStrong;
        let th = self.lh_of(font);
        let pad = self.chip_pad();
        let w = self.m.width(label, font) + pad * 2.0;
        let h = self.chip_h();
        let r = Rect::new(x, y, w, h);
        self.fill(r, bg, h / 2.0);
        self.text(x + pad, y + (h - th) / 2.0, label, font, fg);
        r
    }

    /// A chip led by a coloured dot.
    fn dot_chip(&mut self, x: f32, y: f32, label: &str, dot: Color, fg: Color, bg: Color) -> Rect {
        let font = Font::SmallStrong;
        let th = self.lh_of(font);
        let pad = self.chip_pad();
        let d = self.u * 1.4;
        let w = pad + d + self.u * 1.2 + self.m.width(label, font) + pad;
        let h = self.chip_h();
        let r = Rect::new(x, y, w, h);
        self.fill(r, bg, h / 2.0);
        self.dot(x + pad + d / 2.0, y + h / 2.0, d, dot);
        self.text(
            x + pad + d + self.u * 1.2,
            y + (h - th) / 2.0,
            label,
            font,
            fg,
        );
        r
    }

    /// A selectable chip (role pickers); returns its rect.
    fn pick_chip(
        &mut self,
        x: f32,
        y: f32,
        label: &str,
        color: Color,
        selected: bool,
        hit: Hit,
    ) -> Rect {
        let hover = self.hovered(&hit);
        let (fg, bg) = if selected {
            (self.t.accent_text, color)
        } else if hover {
            (self.t.text, self.t.tint(color).alpha(0.35))
        } else {
            (self.t.readable(color), self.t.tint(color))
        };
        let font = Font::Strong;
        let th = self.lh_of(font);
        let h = th + self.u * 2.0;
        let pad = h * 0.5;
        let w = self.m.width(label, font) + pad * 2.0;
        let r = Rect::new(x, y, w, h);
        self.fill(r, bg, h / 2.0);
        self.text(x + pad, y + (h - th) / 2.0, label, font, fg);
        self.hit(r, hit);
        r
    }

    fn button_size(&mut self, label: &str) -> (f32, f32) {
        let th = self.lh_of(Font::Strong);
        (
            self.m.width(label, Font::Strong) + self.u * 6.0,
            th + self.u * 2.4,
        )
    }

    fn button(&mut self, x: f32, y: f32, label: &str, primary: bool, hit: Hit) -> Rect {
        let (w, h) = self.button_size(label);
        let r = Rect::new(x, y, w, h);
        let hover = self.hovered(&hit);
        let (bg, fg) = if primary {
            let bg = if hover {
                self.t.accent.mix(self.t.text, 0.15)
            } else {
                self.t.accent
            };
            (bg, self.t.accent_text)
        } else {
            let bg = if hover {
                self.t.card_hover
            } else {
                self.t.card
            };
            (bg, self.t.text)
        };
        if !primary {
            self.fill(r.inset(-1.0), self.t.border, h / 2.0 + 1.0);
        }
        self.fill(r, bg, h / 2.0);
        let th = self.lh_of(Font::Strong);
        self.text(
            x + self.u * 3.0,
            y + (h - th) / 2.0,
            label,
            Font::Strong,
            fg,
        );
        self.hit(r, hit);
        r
    }

    /// A row of buttons right-aligned at `right`; returns the leftmost x.
    fn buttons_right(&mut self, right: f32, y: f32, items: Vec<(String, bool, Hit)>) -> f32 {
        let mut x = right;
        for (label, primary, hit) in items.into_iter().rev() {
            let (w, _) = self.button_size(&label);
            x -= w;
            self.button(x, y, &label, primary, hit);
            x -= self.u * 2.0;
        }
        x
    }

    fn section_label(&mut self, x: f32, y: f32, label: &str) -> f32 {
        self.text(x, y, label, Font::SmallStrong, self.t.faint);
        self.lh_of(Font::SmallStrong) + self.u
    }

    /// A banner line (warnings, errors) with a coloured edge; returns height.
    fn banner(&mut self, x: f32, y: f32, w: f32, text: &str, color: Color) -> f32 {
        let pad = self.u * 2.4;
        let measure = self.push_clip(EMPTY);
        let th = self.para(
            0.0,
            0.0,
            text,
            Font::Small,
            color,
            w - pad * 2.0 - self.u,
            4,
        );
        self.pop_clip(measure);
        let h = th + pad * 2.0;
        let r = Rect::new(x, y, w, h);
        self.fill(r, self.t.tint(color), self.u * 1.6);
        let old = self.push_clip(r);
        self.fill(Rect::new(x, y, self.u * 0.8, h), color, 0.0);
        self.pop_clip(old);
        let fg = self.t.readable(color);
        self.para(
            x + pad + self.u,
            y + pad,
            text,
            Font::Small,
            fg,
            w - pad * 2.0 - self.u,
            4,
        );
        h
    }

    // -- text fields --------------------------------------------------------

    fn field_height(&self, rows: usize) -> f32 {
        rows as f32 * self.lh_of(Font::Body) + self.u * 4.0
    }

    /// The caret at `(x, y)` (top of the line), with any uncommitted IME
    /// composition drawn underlined in front of it.
    fn caret_at(&mut self, x: f32, y: f32) {
        let lh = self.lh_of(Font::Body);
        let mut x = x;
        if let Some(pre) = self.md.preedit.clone() {
            let w = self.text(x, y, &pre, Font::Body, self.t.accent);
            self.fill(
                Rect::new(x, y + lh * 0.92, w, (self.u * 0.3).max(1.0)),
                self.t.accent,
                0.0,
            );
            x += w;
        }
        self.fill(
            Rect::new(
                x.round(),
                y + lh * 0.12,
                (self.u * 0.45).max(1.5),
                lh * 0.76,
            ),
            self.t.accent,
            0.0,
        );
        self.s.caret = Some((x, y));
    }

    fn field(&mut self, r: Rect, id: FieldId, f: &Field, rows: usize) {
        let focused = self.md.focus == Some(id);
        let hit = Hit::Field(id);
        let hover = self.hovered(&hit);
        let radius = self.u * 1.6;
        let border = if focused {
            self.t.accent
        } else if hover {
            self.t.muted.alpha(0.6)
        } else {
            self.t.border
        };
        self.fill(r.inset(-1.0), border, radius + 1.0);
        self.fill(r, self.t.bg, radius);
        self.hit(r, hit);
        let pad = self.u * 2.0;
        let inner = r.inset(pad);
        let lh = self.lh_of(Font::Body);
        let old = self.push_clip(r.inset_xy(pad * 0.5, self.u));
        if f.is_empty() && !focused {
            self.text(inner.x, inner.y, f.placeholder, Font::Body, self.t.faint);
        }
        let segs = segments(self.m, &f.buf.lines, Font::Body, inner.w.max(1.0));
        let caret = caret_segment(&segs, f.buf.cursor_line, f.buf.cursor_col);
        let first = if focused {
            (caret + 1).saturating_sub(rows)
        } else {
            0
        };
        for (row, seg) in segs.iter().enumerate().skip(first).take(rows) {
            let line: Vec<char> = f.buf.lines[seg.line].chars().collect();
            let s: String = line[seg.start.min(line.len())..seg.end.min(line.len())]
                .iter()
                .collect();
            let y = inner.y + (row - first) as f32 * lh;
            self.text(inner.x, y, &s, Font::Body, self.t.text);
            if focused && row == caret {
                let upto: String = line
                    [seg.start.min(line.len())..f.buf.cursor_col.min(line.len()).max(seg.start)]
                    .iter()
                    .collect();
                let cx = inner.x + self.m.width(&upto, Font::Body);
                self.caret_at(cx, y);
            }
        }
        if focused && segs.is_empty() {
            self.caret_at(inner.x, inner.y);
        }
        self.pop_clip(old);
    }

    // -- colours ------------------------------------------------------------

    fn stage_color(&self, stage: Stage) -> Color {
        match stage {
            Stage::Holding => self.t.muted,
            Stage::Cleared => self.t.info,
            Stage::InFlight => self.t.violet,
            Stage::Gates => self.t.warn,
            Stage::NeedsYou => self.t.bad,
            Stage::Landed => self.t.ok,
        }
    }

    fn agent_color(&self, status: AgentStatus) -> Color {
        match status {
            AgentStatus::Starting => self.t.info,
            AgentStatus::Working => self.t.ok,
            AgentStatus::Holding => self.t.muted,
            AgentStatus::NeedsYou => self.t.warn,
            AgentStatus::Offline => self.t.faint,
        }
    }

    fn role_color(&self, role: Option<&Role>) -> Color {
        role.map(|r| Color::from_u8(r.color))
            .unwrap_or(self.t.muted)
    }

    fn model_chip(&mut self, x: f32, y: f32, model: &ModelRef) -> Rect {
        let dot = match model.locality {
            Locality::Local => self.t.ok,
            Locality::Cloud => self.t.info,
        };
        let fg = self.t.muted;
        let bg = self.t.tint(self.t.muted);
        let label = model.label.clone();
        self.dot_chip(x, y, &label, dot, fg, bg)
    }

    fn tier_color(&self, tier: &str) -> Color {
        match tier {
            "low" => self.t.ok,
            "high" => self.t.violet,
            _ => self.t.info,
        }
    }

    fn tier_chip(&mut self, x: f32, y: f32, tier: &str) -> Rect {
        let c = self.tier_color(tier);
        let fg = self.t.readable(c);
        let bg = self.t.tint(c);
        self.chip(x, y, tier, fg, bg)
    }

    fn role_chip(&mut self, x: f32, y: f32, role: &Role) -> Rect {
        let c = Color::from_u8(role.color);
        let fg = self.t.readable(c);
        let bg = self.t.tint(c);
        self.chip(x, y, &role.name, fg, bg)
    }

    fn status_chip(&mut self, x: f32, y: f32, status: AgentStatus) -> Rect {
        let c = self.agent_color(status);
        let fg = self.t.readable(c);
        let bg = self.t.tint(c);
        self.dot_chip(x, y, status.label(), c, fg, bg)
    }

    // -- root ---------------------------------------------------------------

    fn root(&mut self, area: Rect) {
        self.fill(area, self.t.bg, 0.0);
        self.hit(area, Hit::Surface);
        let rail_w = (self.lh * 6.2).round();
        let (rail, main) = area.take_left(rail_w);
        self.rail(rail);
        let header_h = (self.lh_of(Font::Heading) + self.lh_of(Font::Small) + self.u * 7.0).round();
        let (header, content) = main.take_top(header_h);
        self.header(header);
        let content = Rect::new(
            content.x + self.u * 4.0,
            content.y,
            (content.w - self.u * 8.0).max(0.0),
            (content.h - self.u * 4.0).max(0.0),
        );
        let md = self.md;
        // Nothing to show until the first snapshot: a live daemon with no
        // roles is a first run, and the Board explains what to do.
        let offline = md.snap.now_unix == 0
            || (!md.live() && md.snap.roles.is_empty() && md.snap.flights.is_empty());
        if offline {
            self.offline_state(content);
        } else {
            match md.view {
                View::Board => self.board(content),
                View::Setup => self.setup(content),
                View::History => self.history(content),
                View::Team => self.team(content),
            }
        }
        match &md.drawer {
            Some(Drawer::Flight(id)) => {
                if let Some(f) = md.snap.flight(id) {
                    self.flight_drawer(main, f);
                }
            }
            Some(Drawer::Agent(id)) => {
                if let Some(a) = md.snap.agent(id) {
                    self.agent_drawer(main, a);
                }
            }
            None => {}
        }
        if let Some(sheet) = &md.sheet {
            self.sheet(area, sheet);
        }
        self.toast(area);
    }

    fn offline_state(&mut self, area: Rect) {
        let (title, detail) = match &self.md.conn {
            Conn::Connecting => (
                "Connecting to towerd…",
                "Tower starts its daemon in the background.",
            ),
            Conn::Offline(why) => ("towerd is offline", why.as_str()),
            Conn::Live { .. } => ("Waiting for towerd…", "The first snapshot is on its way."),
        };
        let detail = detail.to_string();
        self.empty_state(area, title, &detail, None);
    }

    fn rail(&mut self, r: Rect) {
        self.fill(r, self.t.rail, 0.0);
        self.fill(
            Rect::new(r.right() - 1.0, r.y, 1.0, r.h),
            self.t.border.alpha(0.6),
            0.0,
        );
        let x = r.x + self.u * 3.0;
        let mut y = r.y + self.u * 4.0;
        // Wordmark: a small radar blip and the name.
        let d = self.lh_of(Font::Strong);
        self.fill(Rect::new(x, y, d, d), self.t.accent.alpha(0.18), d / 2.0);
        self.dot(x + d / 2.0, y + d / 2.0, d * 0.45, self.t.accent);
        self.text(x + d + self.u * 2.0, y, "Tower", Font::Strong, self.t.text);
        y += d + self.u * 6.0;

        let inbox = self.md.snap.inbox.len();
        // Setup wants a look: a config error, or a terms note not yet accepted.
        let attention = self
            .md
            .snap
            .notices
            .iter()
            .any(|n| n.level == NoticeLevel::Error)
            || !self.md.terms_due().is_empty();
        let item_h = (self.lh * 1.7).round();
        let views = self.md.views();
        for view in views.iter() {
            let row = Rect::new(r.x + self.u * 1.5, y, r.w - self.u * 3.0, item_h);
            let hit = Hit::Nav(*view);
            let active = self.md.view == *view;
            let hover = self.hovered(&hit);
            if active {
                self.fill(row, self.t.accent.alpha(0.16), self.u * 1.6);
                self.fill(
                    Rect::new(row.x, row.y + item_h * 0.25, self.u * 0.7, item_h * 0.5),
                    self.t.accent,
                    self.u * 0.35,
                );
            } else if hover {
                self.fill(row, self.t.card.alpha(0.7), self.u * 1.6);
            }
            let color = if active { self.t.text } else { self.t.muted };
            let font = if active { Font::Strong } else { Font::Body };
            let ty = row.y + (item_h - self.lh_of(font)) / 2.0;
            self.text(row.x + self.u * 2.5, ty, view.label(), font, color);
            if *view == View::Setup && attention {
                let d = self.u * 1.6;
                self.dot(
                    row.right() - self.u * 3.0,
                    row.y + item_h / 2.0,
                    d,
                    self.t.warn,
                );
            }
            if *view == View::Board && inbox > 0 {
                let label = inbox.to_string();
                let cw = self.chip_w(&label);
                let ch = self.chip_h();
                let cr = Rect::new(
                    row.right() - cw - self.u * 2.0,
                    row.y + (item_h - ch) / 2.0,
                    cw,
                    ch,
                );
                self.fill(cr, self.t.bad, ch / 2.0);
                let fg = self.t.accent_text;
                let th = self.lh_of(Font::SmallStrong);
                self.text(
                    cr.x + self.chip_pad(),
                    cr.y + (ch - th) / 2.0,
                    &label,
                    Font::SmallStrong,
                    fg,
                );
            }
            self.hit(row, hit);
            y += item_h + self.u * 0.5;
        }

        // Connection status at the bottom.
        let (dot, label, detail) = match (&self.md.conn, self.md.snap.origin) {
            (Conn::Live { pid }, SnapshotOrigin::Live) => {
                (self.t.ok, "Live".to_string(), format!("towerd {}", pid))
            }
            (Conn::Live { pid }, SnapshotOrigin::Demo) => (
                self.t.warn,
                "Demo data".to_string(),
                format!("towerd {}", pid),
            ),
            (Conn::Connecting, _) => (self.t.muted, "Connecting".to_string(), String::new()),
            (Conn::Offline(_), _) => (self.t.bad, "Offline".to_string(), "retrying…".to_string()),
        };
        let sh = self.lh_of(Font::Small);
        let by = r.bottom() - self.u * 4.0 - sh * 2.0;
        if let Some(left) = self.armed_left() {
            // Armed is loud on purpose: things may happen without a click.
            let ay = by - sh * 2.0 - self.u * 3.0;
            let row = Rect::new(
                r.x + self.u * 1.5,
                ay,
                r.w - self.u * 3.0,
                sh * 2.0 + self.u,
            );
            let hit = Hit::Nav(View::Setup);
            self.fill(row, self.t.tint(self.t.bad), self.u * 1.6);
            self.dot(
                x + self.u,
                ay + self.u * 0.5 + sh / 2.0,
                self.u * 1.4,
                self.t.bad,
            );
            self.text(
                x + self.u * 3.0,
                ay + self.u * 0.5,
                "Armed",
                Font::SmallStrong,
                self.t.bad,
            );
            let max_w = r.w - self.u * 6.0;
            self.text_fit(
                x,
                ay + self.u * 0.5 + sh,
                &format!("{} left", duration(left)),
                Font::Small,
                self.t.muted,
                max_w,
            );
            self.hit(row, hit);
        }
        self.dot(x + self.u, by + sh / 2.0, self.u * 1.4, dot);
        self.text(
            x + self.u * 3.0,
            by,
            &label,
            Font::SmallStrong,
            self.t.muted,
        );
        let max_w = r.w - self.u * 6.0;
        self.text_fit(x, by + sh, &detail, Font::Small, self.t.faint, max_w);
    }

    fn header(&mut self, r: Rect) {
        let x = r.x + self.u * 4.0;
        let y = r.y + self.u * 4.0;
        let md = self.md;
        let snap = &md.snap;
        let airborne = snap
            .agents
            .iter()
            .filter(|a| matches!(a.status, AgentStatus::Working | AgentStatus::Starting))
            .count();
        let needs = snap.inbox.len();
        let subtitle = match md.view {
            View::Board => format!(
                "{} flights · {} agents working · {} waiting on you",
                snap.flights.len(),
                airborne,
                needs
            ),
            View::Setup => {
                let up = snap
                    .endpoints
                    .iter()
                    .filter(|e| e.health == Health::Up)
                    .count();
                let armed = match self.armed_left() {
                    Some(left) => format!("Armed for {}", duration(left)),
                    None => "Disarmed".to_string(),
                };
                format!(
                    "{} of {} roles can run · {}/{} servers up · {}",
                    snap.setup.roles_usable,
                    snap.roles.len(),
                    up,
                    snap.endpoints.len(),
                    armed
                )
            }
            View::History if snap.extras.activity => {
                let days = self.activity_days();
                let auto: u32 = days.iter().map(|d| d.auto.values().sum::<u32>()).sum();
                let asked: u32 = days.iter().map(|d| d.asked).sum();
                format!(
                    "Last {} days · {} ran by itself · {} asked you",
                    md.activity_days, auto, asked
                )
            }
            View::History => "What Tower did without a click".to_string(),
            View::Team => "Talk to the team · @role reaches its agents".to_string(),
        };
        self.text(x, y, md.view.label(), Font::Heading, self.t.text);
        let hy = y + self.lh_of(Font::Heading);
        let sub_w = r.w * 0.5;
        self.text_fit(x, hy, &subtitle, Font::Small, self.t.muted, sub_w);

        let (_, bh) = self.button_size("New flight");
        let by = r.y + (r.h - bh) / 2.0;
        let right = r.right() - self.u * 4.0;
        let cfg = snap.config_dir.clone();
        let terminal = (
            "Open terminal".to_string(),
            false,
            Hit::Button(Action::OpenTerminal { cwd: None }),
        );
        let items = match md.view {
            View::Board => vec![
                terminal,
                (
                    "New flight".to_string(),
                    true,
                    Hit::Button(Action::NewFlight),
                ),
            ],
            View::Setup => {
                let armed = self.armed_left().is_some();
                vec![(
                    if armed { "Disarm" } else { "Arm" }.to_string(),
                    !armed,
                    Hit::Button(Action::SetArmed(!armed)),
                )]
            }
            View::History if snap.extras.activity => [7usize, 30, 90]
                .iter()
                .map(|n| {
                    (
                        format!("{} days", n),
                        md.activity_days == *n,
                        Hit::Button(Action::ActivityDays(*n)),
                    )
                })
                .collect(),
            View::History => vec![],
            View::Team => vec![terminal],
        };
        let _ = cfg;
        self.buttons_right(right, by, items);
        self.fill(
            Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0),
            self.t.border.alpha(0.5),
            0.0,
        );
    }

    // -- board --------------------------------------------------------------

    fn board(&mut self, area: Rect) {
        let md = self.md;
        let gap = self.u * 3.0;
        let strip_w = area.w.min(self.lh * 60.0);
        let mut y = area.y + self.u * 2.0;
        // Callouts first: a config error, a terms note waiting for an OK.
        let errors: Vec<String> = md
            .snap
            .notices
            .iter()
            .filter(|n| n.level == NoticeLevel::Error)
            .map(|n| n.text.clone())
            .collect();
        for text in errors.iter().rev().take(3) {
            let c = self.t.bad;
            y += self.callout(
                area.x,
                y,
                strip_w,
                text,
                c,
                Some(("Open Setup".to_string(), Hit::Nav(View::Setup))),
            ) + gap;
        }
        for t in md.terms_due() {
            let text = format!(
                "{}: needs your OK before {} agents start",
                t.title, t.vendor
            );
            let c = self.t.warn;
            y += self.callout(
                area.x,
                y,
                strip_w,
                &text,
                c,
                Some(("Review".to_string(), Hit::Nav(View::Setup))),
            ) + gap;
        }
        // What is waiting on you, as a strip above the flights.
        let items: Vec<&InboxItem> = md.snap.inbox.iter().rev().collect();
        if !items.is_empty() {
            y += self.section_label(area.x, y, &format!("WAITING ON YOU · {}", items.len()));
            let measure = self.push_clip(EMPTY);
            let heights: Vec<f32> = items
                .iter()
                .map(|i| self.inbox_row(i, area.x, 0.0, strip_w))
                .collect();
            self.pop_clip(measure);
            let content = heights.iter().sum::<f32>() + gap * heights.len() as f32;
            let first = heights[0] + gap;
            let max_h = (area.h * 0.4).max(first.min(area.h * 0.6));
            let vh = content.min(max_h).max(0.0);
            let viewport = Rect::new(area.x, y, strip_w, vh);
            let off = self.scroll_region(ScrollId::Inbox, viewport, content, false, false);
            let old = self.push_clip(viewport);
            let mut ry = viewport.y - off;
            for (item, h) in items.iter().zip(heights.iter()) {
                let r = Rect::new(area.x, ry, strip_w, *h);
                let row_hit = item.flight_id.clone().map(Hit::InboxRow);
                let hover = row_hit.as_ref().map(|h| self.hovered(h)).unwrap_or(false);
                let radius = self.u * 2.4;
                self.shadow(r, radius);
                self.fill(
                    r,
                    if hover {
                        self.t.card_hover
                    } else {
                        self.t.card
                    },
                    radius,
                );
                if let Some(h) = row_hit {
                    self.hit(r, h);
                }
                self.inbox_row(item, area.x, ry, strip_w);
                ry += h + gap;
            }
            self.pop_clip(old);
            self.scrollbar(viewport, content, off);
            y += vh + gap;
        }
        let used = y - area.y - self.u * 2.0;
        let area = if used > 0.0 {
            Rect::new(area.x, area.y + used, area.w, (area.h - used).max(0.0))
        } else {
            area
        };
        // Then the flights.
        let first_run = md.snap.roles.is_empty()
            || (md.snap.flights.is_empty() && md.snap.setup.roles_usable == 0);
        if first_run {
            self.first_run_state(area);
            return;
        }
        if md.snap.flights.is_empty() {
            self.empty_state(
                area,
                "No flights yet",
                "A flight is a piece of work. Create one, give it a role, and an agent takes off.",
                Some(("New flight", Hit::Button(Action::NewFlight))),
            );
            return;
        }
        let n = Stage::ALL.len() as f32;
        let min_col = (self.lh * 11.0).round();
        let col_w = ((area.w - gap * (n - 1.0)) / n).max(min_col).floor();
        let total = col_w * n + gap * (n - 1.0);
        let area = area.translate(0.0, self.u * 4.0);
        let area = Rect::new(area.x, area.y, area.w, (area.h - self.u * 4.0).max(0.0));
        let off_x = self.scroll_region(ScrollId::BoardX, area, total, true, false);
        let old = self.push_clip(area);
        for (i, stage) in Stage::ALL.iter().enumerate() {
            let x = area.x - off_x + i as f32 * (col_w + gap);
            self.column(Rect::new(x, area.y, col_w, area.h), *stage);
        }
        self.pop_clip(old);
    }

    /// A fresh setup: the one thing to do next, in order of what blocks the
    /// first flight.
    fn first_run_state(&mut self, area: Rect) {
        let md = self.md;
        let setup = &md.snap.setup;
        if setup.installed_clis.is_empty() {
            self.empty_state(
                area,
                "No agent CLI found",
                "Tower runs the agent programs you already have: claude, codex, opencode, agy or gemini. Install one and Tower seeds a role for it.",
                Some(("Open terminal", Hit::Button(Action::OpenTerminal { cwd: None }))),
            );
        } else if !md.terms_due().is_empty() {
            self.empty_state(
                area,
                "One note before agents start",
                "Setup shows the vendor's terms note for the agent you have. Accept it once and the first flight can take off.",
                Some(("Review", Hit::Nav(View::Setup))),
            );
        } else if md.snap.roles.is_empty() {
            self.empty_state(
                area,
                "No roles yet",
                "A role is an agent with a mission. Tower seeds one for each installed CLI; add your own in Setup.",
                Some(("New role", Hit::Button(Action::NewRole))),
            );
        } else {
            let why = md
                .snap
                .roles
                .iter()
                .find_map(|r| r.warning.clone())
                .unwrap_or_default();
            let detail = format!("No role can run yet. {}", why);
            self.empty_state(
                area,
                "Nothing can take off yet",
                detail.trim(),
                Some(("Open Setup", Hit::Nav(View::Setup))),
            );
        }
    }

    fn column(&mut self, r: Rect, stage: Stage) {
        if r.intersect(&self.clip).is_none() {
            return;
        }
        let radius = self.u * 2.4;
        self.fill(r, self.t.surface, radius);
        let pad = self.u * 2.4;
        let head_h = self.lh_of(Font::Strong) + pad * 2.0;
        let md = self.md;
        let flights: Vec<&Flight> = md.snap.flights_in(stage).collect();
        let color = self.stage_color(stage);
        let d = self.u * 1.6;
        let hy = r.y + pad;
        self.dot(
            r.x + pad + d / 2.0,
            hy + self.lh_of(Font::Strong) / 2.0,
            d,
            color,
        );
        let lx = r.x + pad + d + self.u * 2.0;
        let w = self.text(lx, hy, stage.label(), Font::Strong, self.t.text);
        let count = flights.len().to_string();
        let ch = self.chip_h();
        let fg = self.t.muted;
        let bg = self.t.tint(self.t.muted);
        self.chip(
            lx + w + self.u * 2.0,
            hy + (self.lh_of(Font::Strong) - ch) / 2.0,
            &count,
            fg,
            bg,
        );

        let body = Rect::new(r.x, r.y + head_h, r.w, (r.h - head_h).max(0.0));
        let card_w = r.w - pad * 2.0;
        let heights: Vec<f32> = flights
            .iter()
            .map(|f| self.card_height(f, card_w))
            .collect();
        let gap = self.u * 2.0;
        let content: f32 =
            heights.iter().sum::<f32>() + gap * heights.len().saturating_sub(1) as f32 + pad;
        let off = self.scroll_region(ScrollId::Column(stage), body, content, false, false);
        let old = self.push_clip(body);
        let mut y = body.y - off;
        if flights.is_empty() {
            let msg = match stage {
                Stage::Holding => "Nothing holding",
                Stage::NeedsYou => "All clear",
                _ => "Empty",
            };
            self.text(r.x + pad, y + self.u, msg, Font::Small, self.t.faint);
        }
        for (f, h) in flights.iter().zip(heights.iter()) {
            if y > body.bottom() {
                break;
            }
            if y + h >= body.y {
                self.card(Rect::new(r.x + pad, y, card_w, *h), f);
            }
            y += h + gap;
        }
        self.pop_clip(old);
        self.scrollbar(body, content, off);
    }

    fn card_title_lines(&mut self, f: &Flight, w: f32) -> Vec<String> {
        let inner = w - self.u * 6.0;
        wrap(self.m, &f.title, Font::Body, inner, 3)
    }

    fn lead_agent(&self, f: &Flight) -> Option<&'a Agent> {
        let md = self.md;
        f.agent_id.as_ref().and_then(|id| md.snap.agent(id))
    }

    fn card_height(&mut self, f: &Flight, w: f32) -> f32 {
        let lines = self.card_title_lines(f, w).len().max(1) as f32;
        let mut h = self.u * 2.5
            + self.lh_of(Font::SmallStrong)
            + self.u
            + lines * self.lh_of(Font::Body)
            + self.u * 2.0
            + self.chip_h()
            + self.u * 2.5;
        if self
            .lead_agent(f)
            .map(|a| a.status.is_live() && a.activity.is_some())
            .unwrap_or(false)
        {
            h += self.lh_of(Font::Small) + self.u;
        }
        if !f.lineage.is_empty() {
            h += self.lh_of(Font::Small) + self.u;
        }
        h.round()
    }

    fn card(&mut self, r: Rect, f: &Flight) {
        let hit = Hit::Card(f.id.clone());
        let selected = self.md.drawer == Some(Drawer::Flight(f.id.clone()));
        let hover = self.hovered(&hit);
        let radius = self.u * 2.0;
        self.shadow(r, radius);
        if selected {
            self.fill(r.inset(-self.u * 0.5), self.t.accent, radius + self.u * 0.5);
        }
        let bg = if hover || selected {
            self.t.card_hover
        } else {
            self.t.card
        };
        self.fill(r, bg, radius);
        let md = self.md;
        let role = f.role_id.as_ref().and_then(|id| md.snap.role(id));
        // Flight-strip edge in the role's colour.
        let edge = self.role_color(role);
        let old = self.push_clip(r);
        self.fill(Rect::new(r.x, r.y, self.u * 0.9, r.h), edge, 0.0);
        self.pop_clip(old);

        let x = r.x + self.u * 3.2;
        let right = r.right() - self.u * 2.6;
        let mut y = r.y + self.u * 2.5;
        let key_w = self.text(x, y, &f.key, Font::SmallStrong, self.t.muted);
        let source_label = match f.source.as_deref() {
            Some("jira") => Some("Jira"),
            Some("github") => Some("GitHub"),
            Some("gitlab") => Some("GitLab"),
            _ => None,
        };
        match source_label {
            Some(label) => {
                self.text(x + key_w + self.u * 1.5, y, label, Font::Small, self.t.info);
            }
            _ if f.branch.is_some() => {
                self.text(
                    x + key_w + self.u * 1.5,
                    y,
                    "worktree",
                    Font::Small,
                    self.t.violet,
                );
            }
            _ => {}
        }
        // A running response clock outranks the age: it is what you act on.
        match most_urgent(&f.sla) {
            Some(c) => {
                let (text, tone) = sla_status(c, self.now);
                let color = self.tone_color(tone);
                self.text_right(right, y, &text, Font::SmallStrong, color);
            }
            None => {
                let age = age(self.now, f.updated_unix);
                self.text_right(right, y, &age, Font::Small, self.t.faint);
            }
        }
        y += self.lh_of(Font::SmallStrong) + self.u;

        for line in self.card_title_lines(f, r.w) {
            self.text(x, y, &line, Font::Body, self.t.text);
            y += self.lh_of(Font::Body);
        }
        y += self.u * 2.0;

        let mut cx = x;
        if let Some(role) = role {
            let c = self.role_chip(cx, y, role);
            cx = c.right() + self.u * 1.5;
        }
        if let Some(route) = &f.route {
            if cx + self.chip_w(&route.tier) + self.u * 10.0 < right {
                let c = self.tier_chip(cx, y, &route.tier);
                cx = c.right() + self.u * 1.5;
            }
        }
        if f.local_only && cx + self.chip_w("local only") + self.u * 10.0 < right {
            let c = self.local_only_chip(cx, y);
            cx = c.right() + self.u * 1.5;
        }
        if let Some(model) = &f.model {
            if cx + self.u * 10.0 < right {
                let old = self.push_clip(Rect::new(r.x, r.y, right - r.x, r.h));
                self.model_chip(cx, y, model);
                self.pop_clip(old);
            }
        } else if role.is_none() {
            let fg = self.t.muted;
            let bg = self.t.tint(self.t.muted);
            self.chip(cx, y, "unassigned", fg, bg);
        }
        let spent = usage_label(
            f.usage.input_tokens,
            f.usage.output_tokens,
            f.usage.cost_usd,
        );
        if !spent.is_empty() && f.stage != Stage::Gates {
            let w = self.m.width(&spent, Font::Small);
            if right - w > cx + self.u * 2.0 {
                let ty = y + (self.chip_h() - self.lh_of(Font::Small)) / 2.0;
                self.text(right - w, ty, &spent, Font::Small, self.t.faint);
            }
        }
        if f.stage == Stage::Gates && !f.gates.is_empty() {
            let passed = f
                .gates
                .iter()
                .filter(|g| g.state == GateState::Passed)
                .count();
            let label = format!("{}/{}", passed, f.gates.len());
            let w = self.chip_w(&label);
            let fg = self.t.warn;
            let bg = self.t.tint(self.t.warn);
            self.chip(right - w, y, &label, fg, bg);
        }
        y += self.chip_h() + self.u;

        if let Some(a) = self.lead_agent(f).filter(|a| a.status.is_live()) {
            if let Some(act) = &a.activity {
                let c = self.agent_color(a.status);
                let d = self.u * 1.2;
                self.dot(x + d / 2.0, y + self.lh_of(Font::Small) / 2.0, d, c);
                self.text_fit(
                    x + d + self.u * 1.5,
                    y,
                    act,
                    Font::Small,
                    self.t.muted,
                    right - x - d - self.u * 1.5,
                );
                y += self.lh_of(Font::Small) + self.u;
            }
        }

        if let Some(step) = f.lineage.last() {
            let from = md
                .snap
                .role(&step.from_role)
                .map(|r| r.name.clone())
                .unwrap_or_else(|| step.from_role.clone());
            let (arrow, verb, color) = match step.direction {
                HandDirection::Down => ("↘", "handed down from", self.t.ok),
                HandDirection::Up => ("↗", "escalated from", self.t.warn),
                HandDirection::Across => ("→", "passed from", self.t.muted),
            };
            let aw = self.text(x, y, arrow, Font::SmallStrong, color);
            let max_w = right - x - aw - self.u;
            let label = format!("{} {}", verb, from);
            self.text_fit(x + aw + self.u, y, &label, Font::Small, self.t.faint, max_w);
        }
        self.hit(r, hit);
    }

    // -- drawers ------------------------------------------------------------

    /// Scrim plus panel; returns the panel rect.
    fn drawer_panel(&mut self, main: Rect) -> Rect {
        self.fill(main, self.t.scrim, 0.0);
        self.hit(main, Hit::Scrim);
        let w = (main.w * 0.46)
            .max(self.lh * 20.0)
            .min(main.w - self.lh * 2.0)
            .max(0.0)
            .round();
        let (_, panel) = main.take_right(w);
        self.fill(
            Rect::new(panel.x - self.u * 1.5, panel.y, self.u * 1.5, panel.h),
            self.t.shadow.alpha(0.12),
            0.0,
        );
        self.fill(panel, self.t.surface, 0.0);
        self.fill(
            Rect::new(panel.x, panel.y, 1.0, panel.h),
            self.t.border,
            0.0,
        );
        self.hit(panel, Hit::Surface);
        panel
    }

    fn close_button(&mut self, right: f32, y: f32) -> f32 {
        let close = Hit::CloseDrawer;
        let cs = self.lh_of(Font::Strong) + self.u * 2.0;
        let cr = Rect::new(right - cs, y - self.u, cs, cs);
        if self.hovered(&close) {
            self.fill(cr, self.t.card_hover, cs / 2.0);
        }
        let xw = self.m.width("×", Font::Strong);
        self.text(
            cr.x + (cs - xw) / 2.0,
            cr.y + self.u,
            "×",
            Font::Strong,
            self.t.muted,
        );
        self.hit(cr, close);
        cs
    }

    fn flight_drawer(&mut self, main: Rect, f: &Flight) {
        let panel = self.drawer_panel(main);
        let pad = self.u * 5.0;
        let x = panel.x + pad;
        let right = panel.right() - pad;
        let inner_w = right - x;
        let md = self.md;
        let role = f.role_id.as_ref().and_then(|id| md.snap.role(id));
        let lead = self.lead_agent(f);
        let live_lead = lead.filter(|a| a.status.is_live());

        // Fixed header.
        let mut y = panel.y + pad;
        let mut key_line = f.key.clone();
        if let Some(src) = &f.source {
            key_line.push_str(&format!("  ·  {}", src));
        }
        if let Some(b) = &f.branch {
            key_line.push_str(&format!("  ·  {}", b));
        }
        let cs = self.close_button(right, y);
        self.text_fit(
            x,
            y,
            &key_line,
            Font::SmallStrong,
            self.t.muted,
            inner_w - cs,
        );
        y += self.lh_of(Font::SmallStrong) + self.u * 1.5;
        for line in wrap(self.m, &f.title, Font::Heading, inner_w - cs, 3) {
            self.text(x, y, &line, Font::Heading, self.t.text);
            y += self.lh_of(Font::Heading);
        }
        y += self.u * 2.0;
        let mut cx = x;
        let sc = self.stage_color(f.stage);
        let stage_bg = self.t.tint(sc);
        let stage_fg = self.t.readable(sc);
        let c = self.dot_chip(cx, y, f.stage.label(), sc, stage_fg, stage_bg);
        cx = c.right() + self.u * 1.5;
        if let Some(role) = role {
            let c = self.role_chip(cx, y, role);
            cx = c.right() + self.u * 1.5;
        }
        if let Some(route) = &f.route {
            let c = self.tier_chip(cx, y, &route.tier);
            cx = c.right() + self.u * 1.5;
        }
        if f.local_only {
            let c = self.local_only_chip(cx, y);
            cx = c.right() + self.u * 1.5;
        }
        if let Some(model) = &f.model {
            let c = self.model_chip(cx, y, model);
            cx = c.right() + self.u * 1.5;
        }
        let spent = usage_label(
            f.usage.input_tokens,
            f.usage.output_tokens,
            f.usage.cost_usd,
        );
        if !spent.is_empty() && cx + self.chip_w(&spent) < right {
            let fg = self.t.muted;
            let bg = self.t.tint(self.t.muted);
            self.chip(cx, y, &spent, fg, bg);
        }
        y += self.chip_h() + self.u * 3.0;

        // Footer actions depend on where the flight is.
        let (_, bh) = self.button_size("Launch");
        let footer_h = bh + pad * 2.0;
        let footer = Rect::new(panel.x, panel.bottom() - footer_h, panel.w, footer_h);
        self.fill(
            Rect::new(footer.x, footer.y, footer.w, 1.0),
            self.t.border.alpha(0.6),
            0.0,
        );
        let mut items: Vec<(String, bool, Hit)> = Vec::new();
        match live_lead {
            Some(a) if a.driver == DriverKind::Pane && a.pane_id.is_some() => items.push((
                "Focus pane".into(),
                true,
                Hit::Button(Action::FocusPane {
                    pane_id: a.pane_id.unwrap_or(0),
                }),
            )),
            Some(a) => items.push((
                "Open agent".into(),
                true,
                Hit::Button(Action::OpenAgent {
                    agent_id: a.id.clone(),
                }),
            )),
            None if f.stage != Stage::Landed => {
                if let Some(role) = role {
                    items.push((
                        format!("Launch {}", role.name),
                        true,
                        Hit::Button(Action::LaunchFlight {
                            flight_id: f.id.clone(),
                            role_id: role.id.clone(),
                        }),
                    ));
                }
            }
            None => {}
        }
        let can_hand_down = f.stage != Stage::Landed && md.snap.roles.len() > 1;
        if can_hand_down {
            items.push((
                "Hand down".into(),
                false,
                Hit::Button(Action::OpenPacket {
                    flight_id: f.id.clone(),
                }),
            ));
        }
        let rung_up = role
            .map(|r| {
                let cur = f
                    .model
                    .as_ref()
                    .and_then(|m| r.ladder.iter().position(|l| l.id == m.id))
                    .unwrap_or(0);
                cur + 1 < r.ladder.len()
            })
            .unwrap_or(false);
        if rung_up && f.stage != Stage::Landed && f.parent_id.is_none() {
            items.push((
                "Escalate".into(),
                false,
                Hit::Button(Action::Escalate {
                    flight_id: f.id.clone(),
                }),
            ));
        }
        if live_lead.is_none() {
            items.push((
                "Delete".into(),
                false,
                Hit::Button(Action::DeleteFlight {
                    flight_id: f.id.clone(),
                }),
            ));
        }
        let mut bx = x;
        for (label, primary, hit) in items {
            let (w, _) = self.button_size(&label);
            if bx + w > right + 1.0 {
                break;
            }
            let b = self.button(bx, footer.y + pad, &label, primary, hit);
            bx = b.right() + self.u * 2.0;
        }

        // Scrolling body between header and footer.
        let body = Rect::new(panel.x, y, panel.w, (footer.y - y).max(0.0));
        let measure = self.push_clip(EMPTY);
        let content = self.flight_body(f, x, inner_w, 0.0) + self.u * 4.0;
        self.pop_clip(measure);
        let off = self.scroll_region(ScrollId::Drawer, body, content, false, false);
        let old = self.push_clip(body);
        self.flight_body(f, x, inner_w, body.y - off);
        self.pop_clip(old);
        self.scrollbar(body, content, off);
    }

    /// The flight drawer's scrolling body at `y0`; returns its height.
    fn flight_body(&mut self, f: &Flight, x: f32, w: f32, y0: f32) -> f32 {
        let md = self.md;
        let mut y = y0;
        // Unassigned: pick a role right here.
        if f.role_id.is_none() && f.stage != Stage::Landed {
            y += self.section_label(x, y, "LAUNCH WITH");
            let mut cx = x;
            for r in md.snap.roles.iter() {
                let label = r.name.clone();
                let (cw, ch) = {
                    let th = self.lh_of(Font::Strong);
                    let h = th + self.u * 2.0;
                    (self.m.width(&label, Font::Strong) + h, h)
                };
                if cx + cw > x + w {
                    cx = x;
                    y += ch + self.u * 1.5;
                }
                let c = Color::from_u8(r.color);
                let rr = self.pick_chip(
                    cx,
                    y,
                    &label,
                    c,
                    false,
                    Hit::Button(Action::LaunchFlight {
                        flight_id: f.id.clone(),
                        role_id: r.id.clone(),
                    }),
                );
                cx = rr.right() + self.u * 1.5;
            }
            y += self.lh_of(Font::Strong) + self.u * 2.0 + self.u * 4.0;
        }
        if let Some(agent) = self.lead_agent(f) {
            y += self.section_label(x, y, "AGENT");
            let row_h = self.lh_of(Font::Strong) + self.lh_of(Font::Small) + self.u * 3.0;
            let r = Rect::new(x - self.u * 1.5, y - self.u, w + self.u * 3.0, row_h);
            let hit = Hit::Button(Action::OpenAgent {
                agent_id: agent.id.clone(),
            });
            if self.hovered(&hit) {
                self.fill(r, self.t.card_hover, self.u * 1.6);
            }
            self.hit(r, hit);
            let c = self.agent_color(agent.status);
            let d = self.u * 1.6;
            self.dot(x + d / 2.0, y + self.lh_of(Font::Strong) / 2.0, d, c);
            let nw = self.text(
                x + d + self.u * 2.0,
                y,
                &agent.id,
                Font::Strong,
                self.t.text,
            );
            let mut status = agent.status.label().to_string();
            if agent.queued > 0 {
                status.push_str(&format!(" · {} queued", agent.queued));
            }
            self.text(x + d + self.u * 4.0 + nw, y, &status, Font::Small, c);
            self.text_right(x + w, y, "Open ›", Font::Small, self.t.faint);
            y += self.lh_of(Font::Strong);
            let detail = agent
                .error
                .clone()
                .or_else(|| agent.activity.clone())
                .unwrap_or_else(|| agent.model.label.clone());
            self.text_fit(
                x + d + self.u * 2.0,
                y,
                &detail,
                Font::Small,
                self.t.muted,
                w - d - self.u * 2.0,
            );
            y += self.lh_of(Font::Small) + self.u * 5.0;
        }
        if f.issue.is_some() || !f.sla.is_empty() {
            y += self.section_label(x, y, "TRACKER");
            if let Some(line) = f.issue.as_ref().map(tracker_line).filter(|l| !l.is_empty()) {
                self.text_fit(x, y, &line, Font::Small, self.t.muted, w);
                y += self.lh_of(Font::Small) + self.u;
            }
            for c in &f.sla {
                let (text, tone) = sla_status(c, self.now);
                let color = self.tone_color(tone);
                let line = format!("{} · {}", c.name, text);
                self.text_fit(x, y, &line, Font::Small, color, w);
                y += self.lh_of(Font::Small) + self.u;
            }
            y += self.u * 3.0;
        }
        if f.kind.is_some() || f.outcome.is_some() {
            y += self.section_label(x, y, "WORK");
            let mut cx = x;
            if let Some(k) = &f.kind {
                let c = self.t.info;
                let (fg, bg) = (self.t.readable(c), self.t.tint(c));
                let r = self.chip(cx, y, k, fg, bg);
                cx = r.right() + self.u * 1.5;
            }
            if let Some(o) = &f.outcome {
                let c = match o.as_str() {
                    "resolved" => self.t.ok,
                    "proposed" => self.t.warn,
                    _ => self.t.bad,
                };
                let (fg, bg) = (self.t.readable(c), self.t.tint(c));
                self.chip(cx, y, o, fg, bg);
            }
            y += self.chip_h() + self.u * 4.0;
        }
        if !f.artifacts.is_empty() {
            y += self.section_label(x, y, "ARTIFACTS");
            for a in &f.artifacts {
                let name = std::path::Path::new(a)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| a.clone());
                let hit = Hit::Button(Action::OpenPath { path: a.clone() });
                let (bw, bh) = self.button_size("Open");
                self.button(x + w - bw, y, "Open", false, hit);
                let ty = y + (bh - self.lh_of(Font::Body)) / 2.0;
                self.text_fit(x, ty, &name, Font::Body, self.t.text, w - bw - self.u * 2.0);
                y += bh + self.u * 1.5;
            }
            y += self.u * 3.0;
        }
        if let Some(route) = &f.route {
            y += self.section_label(x, y, "ROUTE");
            y += self.route_summary(route, x, y, w);
            y += self.u * 4.0;
        }
        if !f.brief.trim().is_empty() && f.brief.trim() != f.title.trim() {
            y += self.section_label(x, y, "BRIEF");
            y += self.para(x, y, &f.brief, Font::Body, self.t.text, w, 14);
            y += self.u * 4.0;
        }
        if let Some(dir) = &f.workdir {
            y += self.section_label(x, y, "WORKS IN");
            self.text_fit(x, y, dir, Font::Small, self.t.muted, w);
            y += self.lh_of(Font::Small) + self.u * 4.0;
        }
        if !f.gates.is_empty() {
            y += self.section_label(x, y, "GATES");
            for g in &f.gates {
                let (mark, c) = match g.state {
                    GateState::Passed => ("✓", self.t.ok),
                    GateState::Failed => ("✕", self.t.bad),
                    GateState::Running => ("•", self.t.warn),
                    GateState::Pending => ("·", self.t.faint),
                };
                let mw = self.text(x, y, mark, Font::Strong, c);
                let nw = self.text_fit(
                    x + mw + self.u * 2.0,
                    y,
                    &g.name,
                    Font::Body,
                    self.t.text,
                    w * 0.6,
                );
                if let Some(detail) = &g.detail {
                    let dx = x + mw + self.u * 4.0 + nw;
                    self.text_fit(dx, y, detail, Font::Small, self.t.muted, x + w - dx);
                }
                y += self.lh_of(Font::Body) + self.u;
            }
            y += self.u * 3.0;
        }
        y += self.section_label(x, y, "TIMELINE");
        if f.timeline.is_empty() {
            self.text(x, y, "No activity yet.", Font::Small, self.t.faint);
            y += self.lh_of(Font::Small);
        }
        for entry in f.timeline.iter().rev() {
            y = self.timeline_entry(f, entry, x, y, w);
            y += self.u * 3.0;
        }
        y - y0
    }

    /// Tier, engine, effort and why; returns the height.
    fn route_summary(&mut self, route: &RouteInfo, x: f32, y0: f32, w: f32) -> f32 {
        let mut y = y0;
        let tc = self.tier_chip(x, y, &route.tier);
        let mut parts: Vec<String> = Vec::new();
        if let Some(e) = &route.engine {
            parts.push(e.clone());
        }
        if !route.effort.is_empty() {
            parts.push(format!("effort {}", route.effort));
        }
        parts.push(
            match route.by.as_str() {
                "you" => "set by you",
                "triage" => "set by triage",
                "escalated" => "escalated",
                "bandit" => "learned pick",
                _ => "from the task text",
            }
            .to_string(),
        );
        let tx = tc.right() + self.u * 2.0;
        let ty = y + (self.chip_h() - self.lh_of(Font::Body)) / 2.0;
        self.text_fit(
            tx,
            ty,
            &parts.join(" · "),
            Font::Body,
            self.t.text,
            x + w - tx,
        );
        y += self.chip_h() + self.u * 1.5;
        if !route.floor.is_empty() && route.floor != "low" {
            let floor = format!(
                "Never below {}: risk or reasoning in the task.",
                route.floor
            );
            self.text_fit(x, y, &floor, Font::Small, self.t.muted, w);
            y += self.lh_of(Font::Small) + self.u;
        }
        if !route.reason.is_empty() {
            y += self.para(x, y, &route.reason, Font::Small, self.t.faint, w, 3);
        }
        if let Some(b) = &route.bandit {
            let ran = route
                .engine
                .as_ref()
                .map(|e| format!("{}:{}", route.tier, e));
            if ran.as_deref() != Some(b.as_str()) {
                y += self.u;
                let mode = if self.md.snap.bandit_mode.is_empty() {
                    "shadow"
                } else {
                    self.md.snap.bandit_mode.as_str()
                };
                let text = format!("Learned policy ({}) would pick {}", mode, b);
                self.text_fit(x, y, &text, Font::Small, self.t.violet, w);
                y += self.lh_of(Font::Small);
            }
        }
        y - y0
    }

    fn tier_picker(&mut self, x: f32, y: f32, w: f32, selected: Option<&str>) -> f32 {
        let items: [(&str, Option<&str>); 4] = [
            ("Auto", None),
            ("Low", Some("low")),
            ("Medium", Some("medium")),
            ("High", Some("high")),
        ];
        let th = self.lh_of(Font::Strong);
        let ch = th + self.u * 2.0;
        let mut cx = x;
        let mut cy = y;
        for (label, tier) in items.iter() {
            let cw = self.m.width(label, Font::Strong) + ch;
            if cx + cw > x + w && cx > x {
                cx = x;
                cy += ch + self.u * 1.5;
            }
            let color = match tier {
                Some(t) => self.tier_color(t),
                None => self.t.accent,
            };
            let r = self.pick_chip(
                cx,
                cy,
                label,
                color,
                *tier == selected,
                Hit::Button(Action::SheetTier(tier.map(str::to_string))),
            );
            cx = r.right() + self.u * 1.5;
        }
        let hint = match selected {
            None => "Tower scores the task (size, keywords, risk); triage may adjust it. Routed roles run the tier's engine.",
            Some(_) => "Your tier wins: it picks the engine for routed roles and every agent's effort.",
        };
        let hy = cy + ch + self.u;
        self.text_fit(x, hy, hint, Font::Small, self.t.faint, w);
        hy + self.lh_of(Font::Small) - y
    }

    fn who(&self, agent_or_role: &str) -> (String, Color) {
        let snap = &self.md.snap;
        if agent_or_role == "you" {
            return ("You".into(), self.t.accent);
        }
        if agent_or_role == "issue" {
            return ("Issue".into(), self.t.info);
        }
        if let Some(a) = snap.agent(agent_or_role) {
            let role = snap.role(&a.role_id);
            return (
                format!(
                    "{} · {}",
                    role.map(|r| r.name.as_str()).unwrap_or("?"),
                    a.id
                ),
                self.role_color(role),
            );
        }
        if let Some(r) = snap.role(agent_or_role) {
            return (r.name.clone(), Color::from_u8(r.color));
        }
        (agent_or_role.to_string(), self.t.muted)
    }

    fn avatar(&mut self, x: f32, y: f32, name: &str, color: Color) -> f32 {
        let d = self.lh_of(Font::Strong) + self.u;
        self.fill(Rect::new(x, y, d, d), self.t.tint(color), d / 2.0);
        let initial: String = name
            .chars()
            .next()
            .map(|c| c.to_string())
            .unwrap_or_default();
        let iw = self.m.width(&initial, Font::Strong);
        let fg = self.t.readable(color);
        self.text(
            x + (d - iw) / 2.0,
            y + self.u * 0.5,
            &initial,
            Font::Strong,
            fg,
        );
        d
    }

    fn timeline_entry(&mut self, f: &Flight, e: &TimelineEntry, x: f32, y: f32, w: f32) -> f32 {
        let when = age(self.now, e.at_unix);
        let mut y = y;
        match &e.body {
            TimelineBody::Message { from, text } => {
                let (name, color) = self.who(from);
                let d = self.avatar(x, y, &name, color);
                let tx = x + d + self.u * 2.5;
                let nw = self.text_fit(tx, y, &name, Font::Strong, self.t.text, w * 0.7);
                self.text(tx + nw + self.u * 2.0, y, &when, Font::Small, self.t.faint);
                y += self.lh_of(Font::Strong);
                y += self.para(tx, y, text, Font::Body, self.t.text, x + w - tx, 16);
            }
            TimelineBody::ToolCall {
                agent,
                tool,
                summary,
            } => {
                let (name, _) = self.who(agent);
                let h = self.lh_of(Font::Small) * 2.0 + self.u * 3.0;
                let r = Rect::new(x, y, w, h);
                self.fill(r, self.t.card.alpha(0.6), self.u * 1.6);
                let ix = x + self.u * 2.5;
                let tw = self.text(ix, y + self.u * 1.5, tool, Font::SmallStrong, self.t.violet);
                let meta = format!("{} · {}", name, when);
                self.text_fit(
                    ix + tw + self.u * 2.0,
                    y + self.u * 1.5,
                    &meta,
                    Font::Small,
                    self.t.faint,
                    w - tw - self.u * 7.0,
                );
                self.text_fit(
                    ix,
                    y + self.u * 1.5 + self.lh_of(Font::Small),
                    summary,
                    Font::Small,
                    self.t.muted,
                    w - self.u * 5.0,
                );
                y += h;
            }
            TimelineBody::Handoff { packet } => {
                y = self.handoff_card(f, packet, x, y, w, &when);
            }
            TimelineBody::Gate { gate } => {
                let label = format!("Gate {} {:?}", gate.name, gate.state).to_lowercase();
                self.text_fit(x, y, &label, Font::Small, self.t.muted, w);
                y += self.lh_of(Font::Small);
            }
            TimelineBody::System { text } => {
                let label = format!("{}  ·  {}", text, when);
                let label = ellipsize(self.m, &label, Font::Small, w);
                let tw = self.m.width(&label, Font::Small).min(w);
                let lx = x + (w - tw) / 2.0;
                let ly = y + self.lh_of(Font::Small) / 2.0;
                let line = self.t.border.alpha(0.6);
                self.fill(
                    Rect::new(x, ly, (lx - x - self.u * 2.0).max(0.0), 1.0),
                    line,
                    0.0,
                );
                self.fill(
                    Rect::new(
                        lx + tw + self.u * 2.0,
                        ly,
                        (x + w - lx - tw - self.u * 2.0).max(0.0),
                        1.0,
                    ),
                    line,
                    0.0,
                );
                self.text(lx, y, &label, Font::Small, self.t.faint);
                y += self.lh_of(Font::Small);
            }
        }
        y
    }

    fn handoff_card(
        &mut self,
        f: &Flight,
        p: &HandoffPacket,
        x: f32,
        y: f32,
        w: f32,
        when: &str,
    ) -> f32 {
        let pad = self.u * 3.0;
        let inner = w - pad * 2.0;
        let md = self.md;
        let to = md.snap.role(&p.to_role);
        let from = md.snap.role(&p.from_role);
        let accent = self.role_color(to);

        // Measure first so the card background can be drawn behind the text.
        let measure = self.push_clip(EMPTY);
        let h = self.handoff_body(f, p, x + pad, y + pad, inner, when, from, to) - y + pad;
        self.pop_clip(measure);

        let r = Rect::new(x, y, w, h);
        self.fill(r.inset(-1.0), accent.alpha(0.55), self.u * 2.0 + 1.0);
        self.fill(r, self.t.card, self.u * 2.0);
        self.handoff_body(f, p, x + pad, y + pad, inner, when, from, to);
        y + h
    }

    #[allow(clippy::too_many_arguments)]
    fn handoff_body(
        &mut self,
        f: &Flight,
        p: &HandoffPacket,
        x: f32,
        y: f32,
        w: f32,
        when: &str,
        from: Option<&Role>,
        to: Option<&Role>,
    ) -> f32 {
        let mut y = y;
        let from_name = from
            .map(|r| r.name.clone())
            .unwrap_or_else(|| p.from_role.clone());
        let to_name = to
            .map(|r| r.name.clone())
            .unwrap_or_else(|| p.to_role.clone());
        let tw = self.text(x, y, "Handoff", Font::Strong, self.t.text);
        let route = format!("{} → {}", from_name, to_name);
        let rw = self.text(x + tw + self.u * 2.0, y, &route, Font::Body, self.t.muted);
        let state_c = match p.state {
            HandoffState::Draft => self.t.warn,
            HandoffState::Released | HandoffState::Accepted => self.t.info,
            HandoffState::Returned => self.t.ok,
            HandoffState::Rejected => self.t.bad,
        };
        let label = p.state.label();
        let cw = self.chip_w(label);
        if x + tw + rw + cw + self.u * 6.0 < x + w {
            let bg = self.t.tint(state_c);
            let fg = self.t.readable(state_c);
            self.chip(x + w - cw, y, label, fg, bg);
        }
        y += self.lh_of(Font::Strong) + self.u;
        self.text(
            x,
            y,
            &format!("{}  ·  {}", p.id, when),
            Font::Small,
            self.t.faint,
        );
        y += self.lh_of(Font::Small) + self.u * 2.0;

        y += self.section_label(x, y, "GOAL");
        y += self.para(x, y, &p.goal, Font::Body, self.t.text, w, 6);
        y += self.u * 2.0;
        let lists: [(&str, &Vec<String>); 3] = [
            ("CONTEXT", &p.context),
            ("CONSTRAINTS", &p.constraints),
            ("ACCEPTANCE", &p.acceptance),
        ];
        for (label, items) in lists.iter() {
            if items.is_empty() {
                continue;
            }
            y += self.section_label(x, y, label);
            for item in items.iter() {
                let bw = self.text(x, y, "•", Font::Small, self.t.faint);
                for line in wrap(self.m, item, Font::Small, w - bw - self.u * 2.0, 4) {
                    self.text(x + bw + self.u * 2.0, y, &line, Font::Small, self.t.muted);
                    y += self.lh_of(Font::Small);
                }
            }
            y += self.u * 2.0;
        }
        if p.state == HandoffState::Draft {
            y += self.u;
            let b = self.button(
                x,
                y,
                "Release",
                true,
                Hit::Button(Action::Release {
                    flight_id: f.id.clone(),
                    handoff_id: p.id.clone(),
                }),
            );
            y += b.h;
        }
        y
    }

    fn agent_drawer(&mut self, main: Rect, a: &Agent) {
        let panel = self.drawer_panel(main);
        let pad = self.u * 5.0;
        let x = panel.x + pad;
        let right = panel.right() - pad;
        let inner_w = right - x;
        let md = self.md;
        let role = md.snap.role(&a.role_id);
        let flight = a.flight_id.as_ref().and_then(|f| md.snap.flight(f));

        let mut y = panel.y + pad;
        let cs = self.close_button(right, y);
        let key = flight.map(|f| f.key.clone()).unwrap_or_default();
        let top = format!("{}  ·  {}", a.driver.label(), key);
        self.text_fit(x, y, &top, Font::SmallStrong, self.t.muted, inner_w - cs);
        y += self.lh_of(Font::SmallStrong) + self.u * 1.5;
        self.text_fit(x, y, &a.id, Font::Heading, self.t.text, inner_w - cs);
        y += self.lh_of(Font::Heading) + self.u * 2.0;
        let mut cx = x;
        let c = self.status_chip(cx, y, a.status);
        cx = c.right() + self.u * 1.5;
        if let Some(role) = role {
            let c = self.role_chip(cx, y, role);
            cx = c.right() + self.u * 1.5;
        }
        let c = self.model_chip(cx, y, &a.model);
        cx = c.right() + self.u * 1.5;
        let spent = usage_label(
            a.usage.input_tokens,
            a.usage.output_tokens,
            a.usage.cost_usd,
        );
        if !spent.is_empty() && cx + self.chip_w(&spent) < right {
            let fg = self.t.muted;
            let bg = self.t.tint(self.t.muted);
            self.chip(cx, y, &spent, fg, bg);
        }
        y += self.chip_h() + self.u * 3.0;

        // Action row.
        let mut items: Vec<(String, bool, Hit)> = Vec::new();
        if let Some(p) = a.pane_id.filter(|_| a.status.is_live()) {
            items.push((
                "Focus pane".into(),
                true,
                Hit::Button(Action::FocusPane { pane_id: p }),
            ));
        }
        if let Some(f) = flight {
            items.push((
                "Flight".into(),
                false,
                Hit::Button(Action::OpenFlight {
                    flight_id: f.id.clone(),
                }),
            ));
        }
        if a.status.is_live() {
            items.push((
                "Stop".into(),
                false,
                Hit::Button(Action::StopAgent {
                    agent_id: a.id.clone(),
                }),
            ));
        }
        let mut bx = x;
        let (_, bh) = self.button_size("Stop");
        for (label, primary, hit) in items {
            let b = self.button(bx, y, &label, primary, hit);
            bx = b.right() + self.u * 2.0;
        }
        y += bh + self.u * 3.0;
        if let Some(err) = &a.error {
            let c = self.t.bad;
            y += self.banner(x, y, inner_w, err, c) + self.u * 3.0;
        }

        // Compose at the bottom.
        let compose_h = self.field_height(1);
        let (sw, _) = self.button_size("Send");
        let footer_h = compose_h + pad * 2.0;
        let footer = Rect::new(panel.x, panel.bottom() - footer_h, panel.w, footer_h);
        self.fill(
            Rect::new(footer.x, footer.y, footer.w, 1.0),
            self.t.border.alpha(0.6),
            0.0,
        );
        if a.status.is_live() {
            let fr = Rect::new(x, footer.y + pad, inner_w - sw - self.u * 2.0, compose_h);
            self.field(fr, FieldId::Compose, &md.compose, 1);
            let by = fr.y + (compose_h - bh) / 2.0;
            self.button(
                right - sw,
                by,
                "Send",
                true,
                Hit::Button(Action::SendCompose),
            );
        } else {
            self.text(
                x,
                footer.y + pad + self.u * 2.0,
                "This agent is offline.",
                Font::Small,
                self.t.faint,
            );
        }

        let body = Rect::new(panel.x, y, panel.w, (footer.y - y).max(0.0));
        let measure = self.push_clip(EMPTY);
        let content = self.agent_body(a, x, inner_w, 0.0) + self.u * 3.0;
        self.pop_clip(measure);
        let off = self.scroll_region(ScrollId::Drawer, body, content, false, true);
        let old = self.push_clip(body);
        self.agent_body(a, x, inner_w, body.y - off);
        self.pop_clip(old);
        self.scrollbar(body, content, off);
    }

    fn agent_body(&mut self, a: &Agent, x: f32, w: f32, y0: f32) -> f32 {
        let mut y = y0;
        if a.driver == DriverKind::Pane {
            let msg = match a.pane_id {
                Some(p) => format!(
                    "This agent lives in terminal pane {} next to Tower. Talk to it there, or send a message below: it is typed into the pane when the agent is idle.",
                    p
                ),
                None => "Waiting for its pane to open…".to_string(),
            };
            let c = self.t.info;
            y += self.banner(x, y, w, &msg, c) + self.u * 3.0;
            if a.queued > 0 {
                let q = format!("{} message(s) queued until it is idle", a.queued);
                self.text(x, y, &q, Font::Small, self.t.warn);
                y += self.lh_of(Font::Small) + self.u * 2.0;
            }
            if let Some(act) = &a.activity {
                y += self.section_label(x, y, "NOW");
                y += self.para(x, y, act, Font::Body, self.t.text, w, 3) + self.u * 2.0;
            }
            return y - y0;
        }
        if a.transcript.is_empty() {
            self.text(x, y, "No conversation yet.", Font::Small, self.t.faint);
            return self.lh_of(Font::Small);
        }
        let md = self.md;
        let role = md.snap.role(&a.role_id);
        let role_name = role
            .map(|r| r.name.clone())
            .unwrap_or_else(|| a.role_id.clone());
        let rc = self.role_color(role);
        for e in &a.transcript {
            let when = age(self.now, e.at_unix);
            match e.kind {
                ChatKind::User | ChatKind::Agent => {
                    let (name, color) = if e.kind == ChatKind::User {
                        ("You / Tower".to_string(), self.t.accent)
                    } else {
                        (role_name.clone(), rc)
                    };
                    let d = self.avatar(x, y, &name, color);
                    let tx = x + d + self.u * 2.5;
                    let nw = self.text(tx, y, &name, Font::Strong, self.t.text);
                    self.text(tx + nw + self.u * 2.0, y, &when, Font::Small, self.t.faint);
                    y += self.lh_of(Font::Strong);
                    let max = if e.kind == ChatKind::User { 10 } else { 60 };
                    y += self.para(tx, y, &e.text, Font::Body, self.t.text, x + w - tx, max);
                    y += self.u * 3.0;
                }
                ChatKind::Tool => {
                    let h = self.lh_of(Font::Small) + self.u * 2.0;
                    let r = Rect::new(x, y, w, h);
                    self.fill(r, self.t.card.alpha(0.6), self.u * 1.4);
                    let tw = self.text(
                        x + self.u * 2.0,
                        y + self.u,
                        "tool",
                        Font::SmallStrong,
                        self.t.violet,
                    );
                    self.text_fit(
                        x + self.u * 4.0 + tw,
                        y + self.u,
                        &e.text,
                        Font::Small,
                        self.t.muted,
                        w - tw - self.u * 6.0,
                    );
                    y += h + self.u * 1.5;
                }
                ChatKind::System => {
                    y += self.para(x, y, &e.text, Font::Small, self.t.faint, w, 4) + self.u * 2.0;
                }
                ChatKind::Error => {
                    let c = self.t.bad;
                    y += self.banner(x, y, w, &e.text, c) + self.u * 2.0;
                }
            }
        }
        y - y0
    }

    // -- sheets (modal forms) ------------------------------------------------

    fn sheet(&mut self, area: Rect, sheet: &Sheet) {
        self.fill(area, self.t.scrim.alpha(self.t.scrim.a * 1.4), 0.0);
        self.hit(area, Hit::SheetScrim);
        let w = (self.lh * 34.0).min(area.w - self.lh * 2.0).max(0.0);
        let pad = self.u * 6.0;
        let measure = self.push_clip(EMPTY);
        let content = self.sheet_body(sheet, 0.0, 0.0, w - pad * 2.0);
        self.pop_clip(measure);
        let max_h = area.h - self.lh * 2.0;
        let h = (content + pad * 2.0).min(max_h).max(0.0);
        let r = Rect::new(
            area.x + (area.w - w) / 2.0,
            area.y + ((area.h - h) / 2.0).max(self.lh),
            w,
            h,
        );
        let radius = self.u * 3.0;
        self.shadow(r.inset(-self.u), radius);
        self.fill(r, self.t.surface, radius);
        self.hit(r, Hit::Surface);
        let body = r.inset(pad);
        let off = self.scroll_region(ScrollId::Sheet, body, content, false, false);
        let old = self.push_clip(r.inset_xy(0.0, self.u));
        self.sheet_body(sheet, body.x, body.y - off, body.w);
        self.pop_clip(old);
        self.scrollbar(body, content, off);
    }

    fn labeled(&mut self, x: f32, y: f32, label: &str) -> f32 {
        self.text(x, y, label, Font::SmallStrong, self.t.muted);
        self.lh_of(Font::SmallStrong) + self.u * 1.5
    }

    /// Selectable chips that wrap at `w`: (label, selected, colour, hit).
    /// Returns the height.
    fn chip_row(&mut self, x: f32, y: f32, w: f32, items: Vec<(String, bool, Color, Hit)>) -> f32 {
        let ch = self.lh_of(Font::Strong) + self.u * 2.0;
        let mut cx = x;
        let mut cy = y;
        for (label, selected, color, hit) in items {
            let cw = self.m.width(&label, Font::Strong) + ch;
            if cx + cw > x + w && cx > x {
                cx = x;
                cy += ch + self.u * 1.5;
            }
            let r = self.pick_chip(cx, cy, &label, color, selected, hit);
            cx = r.right() + self.u * 1.5;
        }
        cy + ch - y
    }

    /// A small hint line under a field; returns its height.
    fn hint(&mut self, x: f32, y: f32, w: f32, text: &str) -> f32 {
        self.para(x, y, text, Font::Small, self.t.faint, w, 3)
    }

    fn role_picker(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        selected: Option<&str>,
        allow_none: bool,
        nobody: bool,
        exclude: Option<&str>,
    ) -> f32 {
        let md = self.md;
        let th = self.lh_of(Font::Strong);
        let ch = th + self.u * 2.0;
        let mut cx = x;
        let mut cy = y;
        let mut items: Vec<(String, Option<String>, Color)> = Vec::new();
        if allow_none {
            if !md.snap.triage_role.is_empty() {
                items.push(("Auto".into(), Some(AUTO_ROLE.to_string()), self.t.accent));
            }
            if nobody {
                items.push(("Nobody yet".into(), None, self.t.muted));
            }
        }
        for r in md.snap.roles.iter() {
            if Some(r.id.as_str()) == exclude {
                continue;
            }
            items.push((r.name.clone(), Some(r.id.clone()), Color::from_u8(r.color)));
        }
        for (label, id, color) in items {
            let cw = self.m.width(&label, Font::Strong) + ch;
            if cx + cw > x + w && cx > x {
                cx = x;
                cy += ch + self.u * 1.5;
            }
            let sel = selected == id.as_deref();
            let rr = self.pick_chip(
                cx,
                cy,
                &label,
                color,
                sel,
                Hit::Button(Action::SheetRole(id)),
            );
            cx = rr.right() + self.u * 1.5;
        }
        cy + ch - y
    }

    /// The "where does the work happen" chips; returns the height.
    fn place_picker(&mut self, x: f32, y: f32, w: f32, selected: Place, worktree: bool) -> f32 {
        let md = self.md;
        let root = if md.snap.flights_root.is_empty() {
            "~/Tower".to_string()
        } else {
            md.snap.flights_root.clone()
        };
        let here = md
            .workdir
            .as_deref()
            .map(|w| {
                let name = std::path::Path::new(w)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| w.to_string());
                format!("Here: {}", name)
            })
            .unwrap_or_else(|| "Here".to_string());
        let mut items = vec![
            (format!("New folder in {}", root), Place::NewFolder),
            (here, Place::Here),
        ];
        if worktree {
            items.push(("Git worktree".to_string(), Place::Worktree));
        }
        let th = self.lh_of(Font::Strong);
        let ch = th + self.u * 2.0;
        let mut cx = x;
        let mut cy = y;
        let color = self.t.accent;
        for (label, place) in items {
            let cw = self.m.width(&label, Font::Strong) + ch;
            if cx + cw > x + w && cx > x {
                cx = x;
                cy += ch + self.u * 1.5;
            }
            let r = self.pick_chip(
                cx,
                cy,
                &label,
                color,
                place == selected,
                Hit::Button(Action::SheetPlace(place)),
            );
            cx = r.right() + self.u * 1.5;
        }
        let hint = match selected {
            Place::NewFolder => {
                "A fresh git repo just for this flight; Claude can start there without asking."
            }
            Place::Here => "Agents work directly in this folder.",
            Place::Worktree => {
                "A new branch in a worktree next to this repo; parallel flights do not collide."
            }
        };
        let hy = cy + ch + self.u;
        self.text_fit(x, hy, hint, Font::Small, self.t.faint, w);
        hy + self.lh_of(Font::Small) - y
    }

    /// Lay out a sheet's contents at (x, y0); returns the height.
    fn sheet_body(&mut self, sheet: &Sheet, x: f32, y0: f32, w: f32) -> f32 {
        let md = self.md;
        let mut y = y0;
        let gap = self.u * 4.0;
        let (_, bh) = self.button_size("Create");
        match sheet {
            Sheet::NewFlight(f) => {
                self.text(x, y, "New flight", Font::Heading, self.t.text);
                y += self.lh_of(Font::Heading) + gap;
                y += self.labeled(x, y, "TITLE");
                let h = self.field_height(1);
                self.field(Rect::new(x, y, w, h), FieldId::Title, &f.title, 1);
                y += h + gap;
                y += self.labeled(x, y, "BRIEF");
                let h = self.field_height(6);
                self.field(Rect::new(x, y, w, h), FieldId::Brief, &f.brief, 6);
                y += h + gap;
                y += self.labeled(x, y, "ASSIGN TO");
                y += self.role_picker(x, y, w, f.role.as_deref(), true, true, None);
                if f.role.as_deref() == Some(AUTO_ROLE) {
                    let who = md
                        .snap
                        .role(&md.snap.triage_role)
                        .map(|r| r.name.clone())
                        .unwrap_or_else(|| md.snap.triage_role.clone());
                    let hint = format!(
                        "{} reads the task and picks the cheapest role that can do it well.",
                        who
                    );
                    y += self.u;
                    self.text_fit(x, y, &hint, Font::Small, self.t.faint, w);
                    y += self.lh_of(Font::Small);
                }
                y += gap;
                if let Some(warn) = f
                    .role
                    .as_deref()
                    .and_then(|r| md.snap.role(r))
                    .and_then(|r| r.warning.clone())
                {
                    let c = self.t.warn;
                    y += self.banner(x, y, w, &warn, c) + gap;
                }
                y += self.labeled(x, y, "HOW HARD");
                y += self.tier_picker(x, y, w, f.tier.as_deref()) + gap;
                y += self.labeled(x, y, "WHERE");
                y += self.place_picker(x, y, w, f.place, true) + gap;
                y += self.local_only_toggle(x, y, w, f.local_only) + gap + self.u * 2.0;
                let launch = f.role.is_some();
                let mut items: Vec<(String, bool, Hit)> =
                    vec![("Cancel".into(), false, Hit::Button(Action::CloseSheet))];
                if launch {
                    items.push((
                        "Create".into(),
                        false,
                        Hit::Button(Action::Submit { launch: false }),
                    ));
                    items.push((
                        "Create & launch".into(),
                        true,
                        Hit::Button(Action::Submit { launch: true }),
                    ));
                } else {
                    items.push((
                        "Create".into(),
                        true,
                        Hit::Button(Action::Submit { launch: false }),
                    ));
                }
                self.buttons_right(x + w, y, items);
                y += bh;
            }
            Sheet::Launch(f) => {
                let title = format!("Launch {}", f.role_name);
                self.text(x, y, &title, Font::Heading, self.t.text);
                y += self.lh_of(Font::Heading) + self.u * 2.0;
                if let Some(role) = md.snap.role(&f.role_id) {
                    let first = role.mission.lines().next().unwrap_or("").to_string();
                    y += self.para(x, y, &first, Font::Body, self.t.muted, w, 3) + gap;
                    if let Some(warn) = role.warning.clone() {
                        let c = self.t.warn;
                        y += self.banner(x, y, w, &warn, c) + gap;
                    }
                }
                y += self.labeled(x, y, "WHERE");
                y += self.place_picker(x, y, w, f.place, false) + gap;
                y += self.labeled(x, y, "TASK");
                let h = self.field_height(7);
                self.field(Rect::new(x, y, w, h), FieldId::Task, &f.task, 7);
                y += h + self.u * 2.0;
                self.text(
                    x,
                    y,
                    "Ctrl+Enter launches. A flight is created for the task.",
                    Font::Small,
                    self.t.faint,
                );
                y += self.lh_of(Font::Small) + gap;
                self.buttons_right(
                    x + w,
                    y,
                    vec![
                        ("Cancel".into(), false, Hit::Button(Action::CloseSheet)),
                        (
                            "Launch".into(),
                            true,
                            Hit::Button(Action::Submit { launch: true }),
                        ),
                    ],
                );
                y += bh;
            }
            Sheet::Packet(f) => {
                let title = format!("Hand down {}", f.flight_key);
                self.text(x, y, &title, Font::Heading, self.t.text);
                y += self.lh_of(Font::Heading) + self.u * 2.0;
                y += self.para(
                    x,
                    y,
                    "Write the packet so a smaller model can do it with nothing else. The result comes back to the flight's agent.",
                    Font::Small,
                    self.t.muted,
                    w,
                    3,
                ) + gap;
                y += self.labeled(x, y, "TO");
                let exclude = md
                    .snap
                    .flight(&f.flight_id)
                    .and_then(|fl| fl.role_id.clone());
                y += self.role_picker(
                    x,
                    y,
                    w,
                    f.to_role.as_deref(),
                    false,
                    false,
                    exclude.as_deref(),
                ) + gap;
                let fields = [
                    ("GOAL", FieldId::Goal, &f.goal, 3usize),
                    ("CONTEXT", FieldId::Context, &f.context, 3),
                    ("CONSTRAINTS", FieldId::Constraints, &f.constraints, 2),
                    ("ACCEPTANCE CRITERIA", FieldId::Acceptance, &f.acceptance, 3),
                ];
                for (label, id, field, rows) in fields.iter() {
                    y += self.labeled(x, y, label);
                    let h = self.field_height(*rows);
                    self.field(Rect::new(x, y, w, h), *id, field, *rows);
                    y += h + gap;
                }
                self.buttons_right(
                    x + w,
                    y,
                    vec![
                        ("Cancel".into(), false, Hit::Button(Action::CloseSheet)),
                        (
                            "Release handoff".into(),
                            true,
                            Hit::Button(Action::Submit { launch: true }),
                        ),
                    ],
                );
                y += bh;
            }
            Sheet::Schedule(f) => {
                let title = if f.id.is_empty() {
                    "New schedule"
                } else {
                    "Edit schedule"
                };
                self.text(x, y, title, Font::Heading, self.t.text);
                y += self.lh_of(Font::Heading) + gap;
                if f.id.is_empty() {
                    y += self.labeled(x, y, "START FROM");
                    let th = self.lh_of(Font::Strong);
                    let ch = th + self.u * 2.0;
                    let mut cx = x;
                    let mut cy = y;
                    let color = self.t.violet;
                    for (i, t) in TEMPLATES.iter().enumerate() {
                        let cw = self.m.width(t.0, Font::Strong) + ch;
                        if cx + cw > x + w && cx > x {
                            cx = x;
                            cy += ch + self.u * 1.5;
                        }
                        let r = self.pick_chip(
                            cx,
                            cy,
                            t.0,
                            color,
                            false,
                            Hit::Button(Action::ScheduleTemplate(i)),
                        );
                        cx = r.right() + self.u * 1.5;
                    }
                    y = cy + ch + gap;
                }
                y += self.labeled(x, y, "NAME");
                let h = self.field_height(1);
                self.field(Rect::new(x, y, w, h), FieldId::SchedName, &f.name, 1);
                y += h + gap;
                y += self.labeled(x, y, "WHEN");
                self.field(Rect::new(x, y, w, h), FieldId::SchedWhen, &f.when, 1);
                y += h + self.u;
                let (hint, color) = match when_preview(&f.when.text(), self.now) {
                    Ok(p) => (p, self.t.muted),
                    Err(_) if f.when.is_empty() => (
                        "A cron line in local time (30 8 * * 1-5 = weekdays 08:30), @daily, or every 2h"
                            .to_string(),
                        self.t.faint,
                    ),
                    Err(e) => (e, self.t.bad),
                };
                self.text_fit(x, y, &hint, Font::Small, color, w);
                y += self.lh_of(Font::Small) + gap;
                y += self.labeled(x, y, "ASSIGN TO");
                y += self.role_picker(x, y, w, f.role.as_deref(), true, false, None) + gap;
                y += self.labeled(x, y, "HOW HARD");
                y += self.tier_picker(x, y, w, f.tier.as_deref()) + gap;
                y += self.labeled(x, y, "WHERE");
                let th = self.lh_of(Font::Strong);
                let ch = th + self.u * 2.0;
                let mut cx = x;
                let mut cy = y;
                let color = self.t.accent;
                for (label, place) in [
                    ("New folder each run", Place::NewFolder),
                    ("A folder", Place::Here),
                    ("Worktree of a repo", Place::Worktree),
                ]
                .iter()
                {
                    let cw = self.m.width(label, Font::Strong) + ch;
                    if cx + cw > x + w && cx > x {
                        cx = x;
                        cy += ch + self.u * 1.5;
                    }
                    let r = self.pick_chip(
                        cx,
                        cy,
                        label,
                        color,
                        *place == f.place,
                        Hit::Button(Action::SheetPlace(*place)),
                    );
                    cx = r.right() + self.u * 1.5;
                }
                y = cy + ch + self.u * 2.0;
                if f.place != Place::NewFolder {
                    self.field(Rect::new(x, y, w, h), FieldId::SchedPath, &f.path, 1);
                    y += h + self.u;
                    let hint = if f.place == Place::Here {
                        "Every run works directly in this folder."
                    } else {
                        "Every run gets a fresh branch in a worktree of this repo."
                    };
                    self.text_fit(x, y, hint, Font::Small, self.t.faint, w);
                    y += self.lh_of(Font::Small);
                }
                y += gap;
                y += self.labeled(x, y, "FLIGHT TITLE");
                self.field(Rect::new(x, y, w, h), FieldId::Title, &f.title, 1);
                y += h + gap;
                y += self.labeled(x, y, "TASK");
                let bh5 = self.field_height(5);
                self.field(Rect::new(x, y, w, bh5), FieldId::Brief, &f.brief, 5);
                y += bh5 + gap;
                y += self.labeled(x, y, "WHEN A RUN IS DUE");
                let mut cx = x;
                let flags = [
                    (
                        "Skip it while the last run is open",
                        !f.overlap_run,
                        SheetFlag::Overlap,
                    ),
                    (
                        "Catch up after Tower was off",
                        f.catch_up,
                        SheetFlag::CatchUp,
                    ),
                    (
                        "Sensitive: local models only",
                        f.local_only,
                        SheetFlag::LocalOnly,
                    ),
                ];
                let mut cy = y;
                for (label, on, flag) in flags.iter() {
                    let cw = self.m.width(label, Font::Strong) + ch;
                    if cx + cw > x + w && cx > x {
                        cx = x;
                        cy += ch + self.u * 1.5;
                    }
                    let r = self.pick_chip(
                        cx,
                        cy,
                        label,
                        self.t.info,
                        *on,
                        Hit::Button(Action::SheetFlag(*flag)),
                    );
                    cx = r.right() + self.u * 1.5;
                }
                y = cy + ch + gap;
                y += self.para(
                    x,
                    y,
                    "Saving it is your OK: it runs on time without Armed. What its agent then writes outside Tower (comments, PRs, landing) still follows Policy.",
                    Font::Small,
                    self.t.faint,
                    w,
                    3,
                ) + gap;
                self.buttons_right(
                    x + w,
                    y,
                    vec![
                        ("Cancel".into(), false, Hit::Button(Action::CloseSheet)),
                        (
                            "Save schedule".into(),
                            true,
                            Hit::Button(Action::Submit { launch: false }),
                        ),
                    ],
                );
                y += bh;
            }
            Sheet::Endpoint(f) => {
                let editing = f.old_name.is_some();
                let title = if editing {
                    "Edit model server"
                } else {
                    "Add model server"
                };
                self.text(x, y, title, Font::Heading, self.t.text);
                y += self.lh_of(Font::Heading) + gap;
                let h = self.field_height(1);
                y += self.labeled(x, y, "NAME");
                self.field(Rect::new(x, y, w, h), FieldId::EpName, &f.name, 1);
                y += h + self.u;
                let name = f.name.text();
                let name = if name.trim().is_empty() {
                    "<name>".to_string()
                } else {
                    name.trim().to_string()
                };
                let hint = format!(
                    "Its models are named {}/<model> in roles and handoffs.",
                    name
                );
                y += self.hint(x, y, w, &hint) + gap;
                y += self.labeled(x, y, "KIND");
                let accent = self.t.accent;
                let kinds = ENDPOINT_KINDS
                    .iter()
                    .map(|(k, _)| {
                        (
                            k.to_string(),
                            f.kind == *k,
                            accent,
                            Hit::Button(Action::SheetKind(k.to_string())),
                        )
                    })
                    .collect();
                y += self.chip_row(x, y, w, kinds) + gap;
                y += self.labeled(x, y, "ADDRESS");
                self.field(Rect::new(x, y, w, h), FieldId::EpUrl, &f.url, 1);
                y += h + gap;
                y += self.labeled(x, y, "WHERE IT RUNS");
                let guess = if looks_local(&f.url.text()) {
                    "local"
                } else {
                    "cloud"
                };
                let (ok, info) = (self.t.ok, self.t.info);
                let places = vec![
                    (
                        format!("Guess from the address ({})", guess),
                        f.local.is_none(),
                        self.t.muted,
                        Hit::Button(Action::SheetLocal(None)),
                    ),
                    (
                        "Inside our network".to_string(),
                        f.local == Some(true),
                        ok,
                        Hit::Button(Action::SheetLocal(Some(true))),
                    ),
                    (
                        "Cloud".to_string(),
                        f.local == Some(false),
                        info,
                        Hit::Button(Action::SheetLocal(Some(false))),
                    ),
                ];
                y += self.chip_row(x, y, w, places) + self.u;
                y += self.hint(
                    x,
                    y,
                    w,
                    "Local-only work only ever goes to servers inside your network. A company server on an internal hostname looks like cloud to the guess: say so here.",
                ) + gap;
                y += self.labeled(x, y, "API KEY");
                self.field(Rect::new(x, y, w, h), FieldId::EpKeyEnv, &f.key_env, 1);
                y += h + self.u;
                y += self.hint(
                    x,
                    y,
                    w,
                    "The name of an environment variable holding the key. Tower never stores the key itself.",
                ) + gap;
                if let Some(old) = &f.old_name {
                    let armed = md.confirm_delete.as_deref() == Some(&format!("endpoint:{}", old));
                    let label = if armed { "Delete: sure?" } else { "Delete" };
                    self.buttons_left(
                        x,
                        y,
                        vec![(
                            label.into(),
                            false,
                            Hit::Button(Action::DeleteEndpoint(old.clone())),
                        )],
                    );
                }
                self.buttons_right(
                    x + w,
                    y,
                    vec![
                        ("Cancel".into(), false, Hit::Button(Action::CloseSheet)),
                        (
                            if editing { "Save server" } else { "Add server" }.into(),
                            true,
                            Hit::Button(Action::Submit { launch: false }),
                        ),
                    ],
                );
                y += bh;
            }
            Sheet::Role(f) => {
                let title = if f.old_id.is_some() {
                    "Edit role"
                } else {
                    "New role"
                };
                self.text(x, y, title, Font::Heading, self.t.text);
                y += self.lh_of(Font::Heading) + gap;
                let h = self.field_height(1);
                y += self.labeled(x, y, "NAME");
                self.field(Rect::new(x, y, w, h), FieldId::RoleName, &f.name, 1);
                y += h + gap;
                y += self.labeled(x, y, "COLOUR");
                let colors = ROLE_COLORS
                    .iter()
                    .map(|(name, hex)| {
                        (
                            name.to_string(),
                            f.color.eq_ignore_ascii_case(hex),
                            Color::from_u8(hex_rgb(hex).unwrap_or([0x80, 0x80, 0x80])),
                            Hit::Button(Action::SheetColor(hex.to_string())),
                        )
                    })
                    .collect();
                y += self.chip_row(x, y, w, colors) + gap;
                y += self.labeled(x, y, "HOW IT RUNS");
                let accent = self.t.accent;
                let drivers = DRIVERS
                    .iter()
                    .map(|(d, _)| {
                        (
                            d.to_string(),
                            f.driver == *d,
                            accent,
                            Hit::Button(Action::SheetDriver(d.to_string())),
                        )
                    })
                    .collect();
                y += self.chip_row(x, y, w, drivers) + self.u;
                let what = DRIVERS
                    .iter()
                    .find(|(d, _)| *d == f.driver)
                    .map(|(_, w)| *w)
                    .unwrap_or("");
                y += self.hint(x, y, w, what) + gap;
                if !matches!(f.driver.as_str(), "routed" | "llm") {
                    y += self.labeled(x, y, "PROGRAM");
                    self.field(Rect::new(x, y, w, h), FieldId::RoleCommand, &f.command, 1);
                    y += h + self.u;
                    y += self.hint(
                        x,
                        y,
                        w,
                        "The command and its arguments, e.g. claude, codex, opencode acp.",
                    ) + gap;
                }
                y += self.labeled(x, y, "MISSION");
                let h5 = self.field_height(5);
                self.field(Rect::new(x, y, w, h5), FieldId::RoleMission, &f.mission, 5);
                y += h5 + gap;
                if f.driver != "routed" {
                    y += self.labeled(x, y, "MODELS, CHEAPEST FIRST");
                    let h3 = self.field_height(3);
                    self.field(Rect::new(x, y, w, h3), FieldId::RoleLadder, &f.ladder, 3);
                    y += h3 + self.u;
                    let ok = self.t.ok;
                    let local: Vec<(String, bool, Color, Hit)> = md
                        .snap
                        .models
                        .iter()
                        .filter(|m| m.locality == Locality::Local)
                        .map(|m| {
                            (
                                format!("+ {}", m.label),
                                false,
                                ok,
                                Hit::Button(Action::RoleAddModel(m.id.clone())),
                            )
                        })
                        .collect();
                    if !local.is_empty() {
                        y += self.chip_row(x, y, w, local) + self.u;
                    }
                    y += self.hint(
                        x,
                        y,
                        w,
                        "Escalate moves one step down this list. Local models are <server>/<model> from the Models view; cloud ones are what the program calls them (sonnet, opencode-go/kimi-k2.7-code).",
                    ) + gap;
                }
                y += self.labeled(x, y, "AGENTS AT ONCE");
                let half = (w - gap) / 2.0;
                self.field(Rect::new(x, y, half, h), FieldId::RoleAtOnce, &f.at_once, 1);
                y += h + gap;
                y += self.hint(
                    x,
                    y,
                    w,
                    "MCP servers, ACP options, effort and sign-in stay in the role's file: Open file.",
                ) + gap;
                let mut left: Vec<(String, bool, Hit)> = vec![];
                if let Some(old) = &f.old_id {
                    let armed = md.confirm_delete.as_deref() == Some(&format!("role:{}", old));
                    let label = if armed { "Delete: sure?" } else { "Delete" };
                    left.push((
                        label.into(),
                        false,
                        Hit::Button(Action::DeleteRole(old.clone())),
                    ));
                    if let Some(file) = &f.file {
                        left.push((
                            "Open file".into(),
                            false,
                            Hit::Button(Action::EditFile { path: file.clone() }),
                        ));
                    }
                }
                if !left.is_empty() {
                    self.buttons_left(x, y, left);
                }
                self.buttons_right(
                    x + w,
                    y,
                    vec![
                        ("Cancel".into(), false, Hit::Button(Action::CloseSheet)),
                        (
                            "Save role".into(),
                            true,
                            Hit::Button(Action::Submit { launch: false }),
                        ),
                    ],
                );
                y += bh;
            }
            Sheet::Routing(f) => {
                self.text(x, y, "Routing", Font::Heading, self.t.text);
                y += self.lh_of(Font::Heading) + gap;
                y += self.para(
                    x,
                    y,
                    "Which engines run each task size, in order of preference. Routed roles (Crew) take the first one that is available; local-only work only ever gets local ones.",
                    Font::Small,
                    self.t.muted,
                    w,
                    4,
                ) + gap;
                let h = self.field_height(1);
                for (label, id, field) in [
                    ("LOW", FieldId::RouteLow, &f.low),
                    ("MEDIUM", FieldId::RouteMedium, &f.medium),
                    ("HIGH", FieldId::RouteHigh, &f.high),
                ]
                .iter()
                {
                    y += self.labeled(x, y, label);
                    self.field(Rect::new(x, y, w, h), *id, field, 1);
                    y += h + gap;
                }
                let known = if md.snap.engine_ids.is_empty() {
                    "No engines in tower.toml [routing.engines] yet.".to_string()
                } else {
                    format!("Engines: {}", md.snap.engine_ids.join(", "))
                };
                y += self.hint(x, y, w, &known) + gap;
                self.buttons_right(
                    x + w,
                    y,
                    vec![
                        ("Cancel".into(), false, Hit::Button(Action::CloseSheet)),
                        (
                            "Save routing".into(),
                            true,
                            Hit::Button(Action::Submit { launch: false }),
                        ),
                    ],
                );
                y += bh;
            }
            Sheet::LocalOnly(field) => {
                self.text(x, y, "Local-only folders", Font::Heading, self.t.text);
                y += self.lh_of(Font::Heading) + gap;
                y += self.para(
                    x,
                    y,
                    "Work in these folders is sensitive: Tower never starts a cloud model on it, triage included, and a cloud agent that handed part of it to a local model only hears whether it is done. A repo can also mark itself with [privacy] local_only = true in its .tower.toml, and list sensitive_paths there.",
                    Font::Small,
                    self.t.muted,
                    w,
                    6,
                ) + gap;
                y += self.labeled(x, y, "ONE FOLDER PER LINE");
                let h5 = self.field_height(5);
                self.field(Rect::new(x, y, w, h5), FieldId::LocalPaths, field, 5);
                y += h5 + gap;
                self.buttons_right(
                    x + w,
                    y,
                    vec![
                        ("Cancel".into(), false, Hit::Button(Action::CloseSheet)),
                        (
                            "Save folders".into(),
                            true,
                            Hit::Button(Action::Submit { launch: false }),
                        ),
                    ],
                );
                y += bh;
            }
            Sheet::Connect(on) => {
                let title = if *on {
                    "Connect your agents"
                } else {
                    "Disconnect your agents"
                };
                self.text(x, y, title, Font::Heading, self.t.text);
                y += self.lh_of(Font::Heading) + gap;
                let text = if *on {
                    "Claude Code and Codex sessions you start yourself in any TGZTerminal pane get three tools: tower_models, tower_delegate and tower_await. With them they hand well-specified subtasks to your local models, keep working, and pick up the result. Tower never types into those panes.\n\nThis adds one MCP server named \"tower\" to ~/.claude.json and ~/.codex/config.toml (Codex only when it is installed). Nothing else in those files changes, and each is backed up once first (.tower-backup). Outside TGZTerminal the server offers no tools.\n\nSessions already running pick it up when you restart them."
                } else {
                    "Removes the \"tower\" MCP server from ~/.claude.json and ~/.codex/config.toml. Nothing else changes. Sessions already running keep it until you restart them."
                };
                y += self.para(x, y, text, Font::Body, self.t.text, w, 20) + gap;
                self.buttons_right(
                    x + w,
                    y,
                    vec![
                        ("Cancel".into(), false, Hit::Button(Action::CloseSheet)),
                        (
                            if *on { "Connect" } else { "Disconnect" }.into(),
                            true,
                            Hit::Button(Action::Submit { launch: false }),
                        ),
                    ],
                );
                y += bh;
            }
            Sheet::Model(f) => {
                let editing = f.old_id.is_some();
                let title = if editing { "Edit model" } else { "Add model" };
                self.text(x, y, title, Font::Heading, self.t.text);
                y += self.lh_of(Font::Heading) + gap;
                y += self.labeled(x, y, "SERVER");
                if md.snap.endpoints.is_empty() {
                    y += self.hint(x, y, w, "Add a model server first.") + gap;
                } else {
                    let (ok, info) = (self.t.ok, self.t.info);
                    let servers = md
                        .snap
                        .endpoints
                        .iter()
                        .map(|e| {
                            (
                                e.name.clone(),
                                f.endpoint.as_deref() == Some(e.name.as_str()),
                                if e.local { ok } else { info },
                                Hit::Button(Action::SheetEndpoint(e.name.clone())),
                            )
                        })
                        .collect();
                    y += self.chip_row(x, y, w, servers) + gap;
                }
                let h = self.field_height(1);
                y += self.labeled(x, y, "NAME ON THE SERVER");
                self.field(Rect::new(x, y, w, h), FieldId::ModelName, &f.name, 1);
                y += h + self.u;
                let served = f
                    .endpoint
                    .as_deref()
                    .and_then(|n| md.snap.endpoints.iter().find(|e| e.name == n))
                    .map(|e| e.models.clone())
                    .unwrap_or_default();
                let hint = if served.is_empty() {
                    "As the server lists it (ollama list, /v1/models).".to_string()
                } else {
                    format!("It serves: {}", served.join(", "))
                };
                y += self.hint(x, y, w, &hint) + gap;
                let half = (w - gap) / 2.0;
                self.labeled(x, y, "LABEL");
                y += self.labeled(x + half + gap, y, "SIZE");
                self.field(Rect::new(x, y, half, h), FieldId::ModelLabel, &f.label, 1);
                self.field(
                    Rect::new(x + half + gap, y, half, h),
                    FieldId::ModelSize,
                    &f.size,
                    1,
                );
                y += h + gap;
                self.labeled(x, y, "CONTEXT (TOKENS)");
                y += self.labeled(x + half + gap, y, "AGENTS AT ONCE");
                self.field(
                    Rect::new(x, y, half, h),
                    FieldId::ModelContext,
                    &f.context,
                    1,
                );
                self.field(
                    Rect::new(x + half + gap, y, half, h),
                    FieldId::ModelAtOnce,
                    &f.at_once,
                    1,
                );
                y += h + gap;
                y += self.labeled(x, y, "GOOD FOR");
                let h3 = self.field_height(3);
                self.field(
                    Rect::new(x, y, w, h3),
                    FieldId::ModelGoodFor,
                    &f.good_for,
                    3,
                );
                y += h3 + self.u;
                y += self.hint(
                    x,
                    y,
                    w,
                    "Agents read this when they decide which subtasks to hand the model.",
                ) + gap;
                y += self.labeled(x, y, "NOTES");
                self.field(Rect::new(x, y, w, h3), FieldId::ModelNotes, &f.notes, 3);
                y += h3 + gap;
                if let Some(old) = &f.old_id {
                    let armed = md.confirm_delete.as_deref() == Some(&format!("model:{}", old));
                    let label = if armed { "Delete: sure?" } else { "Delete" };
                    self.buttons_left(
                        x,
                        y,
                        vec![(
                            label.into(),
                            false,
                            Hit::Button(Action::DeleteModel(old.clone())),
                        )],
                    );
                }
                self.buttons_right(
                    x + w,
                    y,
                    vec![
                        ("Cancel".into(), false, Hit::Button(Action::CloseSheet)),
                        (
                            if editing { "Save model" } else { "Add model" }.into(),
                            true,
                            Hit::Button(Action::Submit { launch: false }),
                        ),
                    ],
                );
                y += bh;
            }
            Sheet::Answer(f) => {
                self.text(x, y, &f.title, Font::Heading, self.t.text);
                y += self.lh_of(Font::Heading) + self.u * 2.0;
                y += self.para(x, y, &f.question, Font::Body, self.t.text, w, 12) + gap;
                y += self.labeled(x, y, "ANSWER");
                let h = self.field_height(5);
                self.field(Rect::new(x, y, w, h), FieldId::Answer, &f.text, 5);
                y += h + gap;
                self.buttons_right(
                    x + w,
                    y,
                    vec![
                        ("Cancel".into(), false, Hit::Button(Action::CloseSheet)),
                        (
                            "Send answer".into(),
                            true,
                            Hit::Button(Action::Submit { launch: true }),
                        ),
                    ],
                );
                y += bh;
            }
        }
        y - y0
    }

    // -- roster -------------------------------------------------------------

    /// Role cards in a grid at (x0, y0); returns the height.
    fn roles_body(&mut self, x0: f32, y0: f32, w: f32) -> f32 {
        let gap = self.u * 4.0;
        let min_w = self.lh * 17.0;
        let cols = (((w + gap) / (min_w + gap)).floor() as usize).max(1);
        let card_w = ((w - gap * (cols as f32 - 1.0)) / cols as f32).floor();
        let md = self.md;
        let roles: Vec<&Role> = md.snap.roles.iter().collect();

        let measure = self.push_clip(EMPTY);
        let heights: Vec<f32> = roles
            .iter()
            .map(|r| self.role_card(r, 0.0, 0.0, card_w))
            .collect();
        self.pop_clip(measure);

        let mut rows: Vec<f32> = Vec::new();
        for chunk in heights.chunks(cols) {
            rows.push(chunk.iter().cloned().fold(0.0, f32::max));
        }
        let mut y = y0;
        for (row_idx, row_h) in rows.iter().enumerate() {
            for col in 0..cols {
                let i = row_idx * cols + col;
                if let Some(role) = roles.get(i) {
                    let x = x0 + col as f32 * (card_w + gap);
                    let radius = self.u * 2.4;
                    let r = Rect::new(x, y, card_w, *row_h);
                    self.shadow(r, radius);
                    self.fill(r, self.t.card, radius);
                    self.role_card(role, x, y, card_w);
                }
            }
            y += row_h + gap;
        }
        (y - y0 - gap).max(0.0)
    }

    /// Draws a role card's contents at (x, y); returns its height.
    fn role_card(&mut self, role: &Role, x0: f32, y0: f32, w: f32) -> f32 {
        let pad = self.u * 4.0;
        let x = x0 + pad;
        let right = x0 + w - pad;
        let inner = right - x;
        let mut y = y0 + pad;
        let c = Color::from_u8(role.color);
        let d = self.lh_of(Font::Heading) * 0.55;
        let hh = self.lh_of(Font::Heading);
        self.fill(Rect::new(x, y + (hh - d) / 2.0, d, d), c, d / 2.0);
        self.text_fit(
            x + d + self.u * 2.5,
            y,
            &role.name,
            Font::Heading,
            self.t.text,
            inner * 0.5,
        );
        let driver = format!("{} · {}", role.driver.label(), role.program);
        let driver = ellipsize(self.m, &driver, Font::SmallStrong, inner * 0.45);
        let dw = self.chip_w(&driver);
        let fg = self.t.muted;
        let bg = self.t.tint(self.t.muted);
        self.chip(right - dw, y + (hh - self.chip_h()) / 2.0, &driver, fg, bg);
        y += hh + self.u * 2.0;
        if let Some(warn) = &role.warning {
            let wc = self.t.warn;
            y += self.banner(x, y, inner, warn, wc) + self.u * 2.0;
        }
        y += self.para(x, y, &role.mission, Font::Body, self.t.muted, inner, 4);
        y += self.u * 3.0;

        y += self.section_label(x, y, "LADDER");
        let mut cx = x;
        for (i, model) in role.ladder.iter().enumerate() {
            if i > 0 {
                let aw = self.text(cx, y + self.u * 0.3, "→", Font::Small, self.t.faint);
                cx += aw + self.u * 1.5;
            }
            let chip = self.model_chip(cx, y, model);
            cx = chip.right() + self.u * 1.5;
        }
        let auto = if role.auto_release {
            "takes handoffs automatically"
        } else {
            "handoffs need your clearance"
        };
        y += self.chip_h() + self.u;
        self.text(x, y, auto, Font::Small, self.t.faint);
        y += self.lh_of(Font::Small) + self.u * 3.0;

        y += self.section_label(x, y, "AGENTS");
        let md = self.md;
        let agents: Vec<&Agent> = md
            .snap
            .agents_of_role(&role.id)
            .filter(|a| a.status.is_live())
            .collect();
        if agents.is_empty() {
            self.text(x, y, "None airborne", Font::Small, self.t.faint);
            y += self.lh_of(Font::Small);
        }
        for a in agents {
            let row_h = self.lh_of(Font::Strong) + self.lh_of(Font::Small);
            let row = Rect::new(
                x - self.u * 1.5,
                y - self.u * 0.5,
                inner + self.u * 3.0,
                row_h + self.u,
            );
            let hit = Hit::Button(Action::OpenAgent {
                agent_id: a.id.clone(),
            });
            if self.hovered(&hit) {
                self.fill(row, self.t.card_hover, self.u * 1.4);
            }
            self.hit(row, hit);
            let dd = self.u * 1.4;
            let sc = self.agent_color(a.status);
            self.dot(x + dd / 2.0, y + self.lh_of(Font::Strong) / 2.0, dd, sc);
            let nx = x + dd + self.u * 2.0;
            let nw = self.text(nx, y, &a.id, Font::Strong, self.t.text);
            let status = a.status.label();
            self.text(nx + nw + self.u * 2.0, y, status, Font::Small, sc);
            y += self.lh_of(Font::Strong);
            let flight = a
                .flight_id
                .as_ref()
                .and_then(|f| md.snap.flight(f))
                .map(|f| f.key.clone())
                .unwrap_or_default();
            let detail = match &a.activity {
                Some(act) => format!("{} · {} · {}", flight, a.model.label, act),
                None => format!("{} · {}", flight, a.model.label),
            };
            self.text_fit(nx, y, &detail, Font::Small, self.t.muted, right - nx);
            y += self.lh_of(Font::Small) + self.u * 1.5;
        }
        y += self.u * 2.0;
        let b = self.button(
            x,
            y,
            "Launch",
            true,
            Hit::Button(Action::OpenLaunch {
                role_id: role.id.clone(),
            }),
        );
        self.button(
            b.right() + self.u * 2.0,
            y,
            "Edit role",
            false,
            Hit::Button(Action::EditRole(role.id.clone())),
        );
        y += b.h + pad;
        y - y0
    }

    // -- inbox --------------------------------------------------------------

    fn inbox_buttons(&self, item: &InboxItem) -> Vec<(String, bool, Hit)> {
        let mut out: Vec<(String, bool, Hit)> = Vec::new();
        if item.answerable {
            // Choices that need no typing (a project, "Post on issue").
            for o in &item.options {
                out.push((
                    o.label.clone(),
                    false,
                    Hit::Button(Action::Resolve {
                        item_id: item.id.clone(),
                        approve: o.primary,
                        option_id: Some(o.id.clone()),
                    }),
                ));
            }
            out.push((
                "Dismiss".into(),
                false,
                Hit::Button(Action::Resolve {
                    item_id: item.id.clone(),
                    approve: false,
                    option_id: None,
                }),
            ));
            out.push((
                "Answer".into(),
                true,
                Hit::Button(Action::Answer {
                    item_id: item.id.clone(),
                }),
            ));
            return out;
        }
        if item.options.is_empty() {
            out.push((
                "Deny".into(),
                false,
                Hit::Button(Action::Resolve {
                    item_id: item.id.clone(),
                    approve: false,
                    option_id: None,
                }),
            ));
            out.push((
                "Approve".into(),
                true,
                Hit::Button(Action::Resolve {
                    item_id: item.id.clone(),
                    approve: true,
                    option_id: None,
                }),
            ));
            return out;
        }
        for o in &item.options {
            let hit = match (o.id.as_str(), item.pane_id) {
                // Claude asks inside its pane: go there directly.
                ("focus", Some(p)) => Hit::Button(Action::FocusPane { pane_id: p }),
                _ => Hit::Button(Action::Resolve {
                    item_id: item.id.clone(),
                    approve: o.primary,
                    option_id: Some(o.id.clone()),
                }),
            };
            out.push((o.label.clone(), o.primary, hit));
        }
        // Primary last, so it sits at the right edge.
        out.sort_by_key(|(_, p, _)| *p);
        out
    }

    /// Draw an inbox row's contents; returns its height.
    fn inbox_row(&mut self, item: &InboxItem, x0: f32, y0: f32, w: f32) -> f32 {
        let pad = self.u * 4.0;
        let kc = match item.kind {
            InboxKind::Permission => self.t.warn,
            InboxKind::Clearance => self.t.info,
            InboxKind::Question => self.t.violet,
            InboxKind::GateFailure | InboxKind::Deadline => self.t.bad,
        };
        let x = x0 + pad;
        let right = x0 + w - pad;
        let ty = y0 + pad;
        let fg = self.t.readable(kc);
        let bg = self.t.tint(kc);
        let chip = self.chip(x, ty, item.kind.label(), fg, bg);
        let tx = chip.right() + self.u * 2.5;
        self.text_fit(
            tx,
            ty,
            &item.title,
            Font::Strong,
            self.t.text,
            right - tx - self.u * 12.0,
        );
        let when = age(self.now, item.created_unix);
        self.text_right(right, ty, &when, Font::Small, self.t.faint);
        let mut y = ty + self.lh_of(Font::Strong) + self.u;
        let detail_rows = if item.answerable { 6 } else { 3 };
        y += self.para(
            x,
            y,
            &item.detail,
            Font::Small,
            self.t.muted,
            right - x,
            detail_rows,
        );
        y += self.u * 2.5;
        let buttons = self.inbox_buttons(item);
        let (_, bh) = self.button_size("Approve");
        self.buttons_right(right, y, buttons);
        y += bh + pad;
        y - y0
    }

    /// A centred title and explanation, with the one next step as a button.
    fn empty_state(&mut self, area: Rect, title: &str, detail: &str, action: Option<(&str, Hit)>) {
        let y = area.y + self.lh * 3.0;
        let tw = self.m.width(title, Font::Heading);
        self.text(
            area.x + (area.w - tw) / 2.0,
            y,
            title,
            Font::Heading,
            self.t.muted,
        );
        let lines = wrap(self.m, detail, Font::Body, area.w.min(self.lh * 26.0), 3);
        let mut ly = y + self.lh_of(Font::Heading) + self.u * 2.0;
        for line in lines {
            let lw = self.m.width(&line, Font::Body);
            self.text(
                area.x + (area.w - lw) / 2.0,
                ly,
                &line,
                Font::Body,
                self.t.faint,
            );
            ly += self.lh_of(Font::Body);
        }
        if let Some((label, hit)) = action {
            let (bw, _) = self.button_size(label);
            self.button(area.x + (area.w - bw) / 2.0, ly + self.lh, label, true, hit);
        }
    }

    /// A banner with one button at its right; returns its height.
    fn callout(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        text: &str,
        color: Color,
        action: Option<(String, Hit)>,
    ) -> f32 {
        let pad = self.u * 2.4;
        let (bw, bh) = match &action {
            Some((label, _)) => self.button_size(label),
            None => (0.0, 0.0),
        };
        let reserve = if action.is_some() {
            bw + self.u * 3.0
        } else {
            0.0
        };
        let tw = (w - pad * 2.0 - self.u - reserve).max(self.lh);
        let measure = self.push_clip(EMPTY);
        let th = self.para(0.0, 0.0, text, Font::Small, color, tw, 4);
        self.pop_clip(measure);
        let h = (th + pad * 2.0).max(bh + pad * 2.0);
        let r = Rect::new(x, y, w, h);
        self.fill(r, self.t.tint(color), self.u * 1.6);
        let old = self.push_clip(r);
        self.fill(Rect::new(x, y, self.u * 0.8, h), color, 0.0);
        self.pop_clip(old);
        let fg = self.t.readable(color);
        self.para(
            x + pad + self.u,
            y + (h - th) / 2.0,
            text,
            Font::Small,
            fg,
            tw,
            4,
        );
        if let Some((label, hit)) = action {
            self.button(x + w - pad - bw, y + (h - bh) / 2.0, &label, false, hit);
        }
        h
    }

    /// A section of Setup: title, a line under it, buttons on the right.
    /// Returns its height.
    fn section_header(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        title: &str,
        sub: &str,
        actions: Vec<(String, bool, Hit)>,
    ) -> f32 {
        let (_, bh) = self.button_size("x");
        let th = self.lh_of(Font::Strong);
        self.text_fit(x, y, title, Font::Strong, self.t.text, w * 0.5);
        let mut h = th;
        if !sub.is_empty() {
            self.text_fit(x, y + th, sub, Font::Small, self.t.muted, w * 0.7);
            h += self.lh_of(Font::Small);
        }
        if !actions.is_empty() {
            self.buttons_right(x + w, y + (th - bh) / 2.0, actions);
            h = h.max(bh);
        }
        h + self.u * 2.0
    }

    /// A note card with one button under the text; returns its height.
    fn note_card_action(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        title: &str,
        detail: &str,
        action: Option<(String, Hit)>,
    ) -> f32 {
        self.boxed(x, y, w, &|l: &mut Self, x, y, w| {
            l.text(x, y, title, Font::Strong, l.t.muted);
            let th = l.lh_of(Font::Strong) + l.u;
            let mut h = th + l.para(x, y + th, detail, Font::Small, l.t.faint, w, 4);
            if let Some((label, hit)) = &action {
                h += l.u * 2.0;
                let r = l.button(x, y + h, label, true, hit.clone());
                h += r.h;
            }
            h
        })
    }

    // -- setup --------------------------------------------------------------

    /// Which sections Setup shows, top to bottom.
    fn setup_sections(&self) -> Vec<Section> {
        let md = self.md;
        let mut v = Vec::new();
        if !md.snap.notices.is_empty() {
            v.push(Section::Notices);
        }
        if !md.snap.setup.terms_pending.is_empty() {
            v.push(Section::Terms);
        }
        v.extend_from_slice(&[
            Section::Roles,
            Section::Models,
            Section::Routing,
            Section::Connect,
            Section::Policy,
            Section::LocalOnly,
            Section::Schedules,
            Section::Sources,
        ]);
        if md.snap.extras.machine_routines {
            v.push(Section::Machine);
        }
        v.push(Section::Extras);
        v
    }

    fn setup(&mut self, area: Rect) {
        let w = area.w.min(self.lh * 64.0);
        let area = Rect::new(
            area.x,
            area.y + self.u * 4.0,
            w,
            (area.h - self.u * 4.0).max(0.0),
        );
        let inner = area.w - self.u * 3.0;
        let sections = self.setup_sections();
        let measure = self.push_clip(EMPTY);
        let heights: Vec<f32> = sections
            .iter()
            .map(|s| self.section(*s, area.x, 0.0, inner))
            .collect();
        self.pop_clip(measure);
        let gap = self.u * 7.0;
        let content = heights.iter().sum::<f32>() + gap * heights.len().saturating_sub(1) as f32;
        let off = self.scroll_region(ScrollId::Setup, area, content, false, false);
        let old = self.push_clip(area);
        let mut y = area.y - off;
        for (s, h) in sections.iter().zip(heights.iter()) {
            // Only what is on screen is laid out a second time.
            if y + h >= area.y && y <= area.bottom() {
                self.section(*s, area.x, y, inner);
            }
            y += h + gap;
        }
        self.pop_clip(old);
        self.scrollbar(area, content, off);
    }

    /// One Setup section at (x, y0); returns its height.
    fn section(&mut self, s: Section, x: f32, y0: f32, w: f32) -> f32 {
        let md = self.md;
        let snap = &md.snap;
        let gap = self.u * 3.0;
        let cfg = snap.config_dir.clone();
        let edit_file = |name: &str| {
            Hit::Button(Action::EditFile {
                path: format!("{}/{}", cfg, name),
            })
        };
        let mut y = y0;
        match s {
            Section::Notices => {
                y += self.section_header(x, y, w, "Notices", "", vec![]);
                let n = snap.notices.len();
                for notice in snap.notices.iter().rev().take(5) {
                    let c = match notice.level {
                        NoticeLevel::Error => self.t.bad,
                        NoticeLevel::Warn => self.t.warn,
                        NoticeLevel::Info => self.t.info,
                    };
                    y += self.banner(x, y, w, &notice.text, c) + gap;
                }
                if n > 5 {
                    self.text(
                        x,
                        y,
                        &format!("+{} older", n - 5),
                        Font::Small,
                        self.t.faint,
                    );
                    y += self.lh_of(Font::Small);
                }
            }
            Section::Terms => {
                y += self.section_header(
                    x,
                    y,
                    w,
                    "Before agents start",
                    "A vendor-terms note for a program you have; accept it once",
                    vec![],
                );
                for t in snap.setup.terms_pending.iter() {
                    if md.terms_deferred.contains(&t.id) {
                        let c = self.t.warn;
                        y += self.callout(
                            x,
                            y,
                            w,
                            &format!("{}: not accepted yet", t.title),
                            c,
                            Some((
                                "Review".to_string(),
                                Hit::Button(Action::DeferTerms {
                                    id: t.id.clone(),
                                    defer: false,
                                }),
                            )),
                        ) + gap;
                        continue;
                    }
                    y += self.boxed(x, y, w, &|l: &mut Self, x, y0, w| {
                        let mut y = y0;
                        let tw = l.text(x, y, &t.title, Font::Strong, l.t.text);
                        let cy = y + (l.lh_of(Font::Strong) - l.chip_h()) / 2.0;
                        let fg = l.t.warn;
                        let bg = l.t.tint(l.t.warn);
                        l.chip(x + tw + l.u * 2.0, cy, &t.vendor, fg, bg);
                        y += l.lh_of(Font::Strong) + l.u * 2.0;
                        y += l.para(x, y, &t.text, Font::Small, l.t.muted, w, 60) + l.u * 3.0;
                        y += l.buttons_left(
                            x,
                            y,
                            vec![
                                (
                                    "Not now".to_string(),
                                    false,
                                    Hit::Button(Action::DeferTerms {
                                        id: t.id.clone(),
                                        defer: true,
                                    }),
                                ),
                                (
                                    "I accept".to_string(),
                                    true,
                                    Hit::Button(Action::AcceptTerms(t.id.clone())),
                                ),
                            ],
                        );
                        y - y0
                    }) + gap;
                }
            }
            Section::Roles => {
                let n = snap.roles.len();
                let sub = format!(
                    "{} role{} · {} can run",
                    n,
                    if n == 1 { "" } else { "s" },
                    snap.setup.roles_usable
                );
                y += self.section_header(
                    x,
                    y,
                    w,
                    "Roles",
                    &sub,
                    vec![("New role".to_string(), false, Hit::Button(Action::NewRole))],
                );
                if snap.roles.is_empty() {
                    y += if snap.setup.installed_clis.is_empty() {
                        self.note_card_action(
                            x,
                            y,
                            w,
                            "No agent CLI found",
                            "Install claude, codex, opencode, agy or gemini; Tower seeds a role for each one it finds on PATH.",
                            Some((
                                "Open terminal".to_string(),
                                Hit::Button(Action::OpenTerminal { cwd: None }),
                            )),
                        )
                    } else {
                        self.note_card_action(
                            x,
                            y,
                            w,
                            "No roles yet",
                            "A role is an agent with a mission: a program, its models cheapest first, and what it does with a flight.",
                            Some(("New role".to_string(), Hit::Button(Action::NewRole))),
                        )
                    };
                } else {
                    y += self.roles_body(x, y, w);
                }
            }
            Section::Models => {
                let eps = &snap.endpoints;
                let up = eps.iter().filter(|e| e.health == Health::Up).count();
                let sub = format!(
                    "{} of {} servers up · models agents can hand work to",
                    up,
                    eps.len()
                );
                let mut actions = vec![(
                    "Add server".to_string(),
                    false,
                    Hit::Button(Action::NewEndpoint),
                )];
                if !eps.is_empty() {
                    actions.push((
                        "Add model".to_string(),
                        false,
                        Hit::Button(Action::NewModel(None)),
                    ));
                    actions.push(("Probe".to_string(), false, Hit::Button(Action::Probe)));
                }
                y += self.section_header(x, y, w, "Models & servers", &sub, actions);
                if eps.is_empty() && snap.models.is_empty() {
                    y += self.note_card_action(
                        x,
                        y,
                        w,
                        "No model servers",
                        "Ollama, LM Studio, llama.cpp, vLLM, MLX or any OpenAI-compatible host, on this Mac, the LAN or your company network. Then add the models on it that agents may hand work to.",
                        Some(("Add server".to_string(), Hit::Button(Action::NewEndpoint))),
                    );
                } else {
                    y += self.models_body(x, y, w);
                }
            }
            Section::Routing => {
                let mode = if snap.bandit_mode.is_empty() {
                    "shadow"
                } else {
                    snap.bandit_mode.as_str()
                };
                let sub = format!(
                    "learned policy: {} · click the card to change the tiers",
                    mode
                );
                y += self.section_header(x, y, w, "Routing", &sub, vec![]);
                if snap.engines.is_empty() {
                    y += self.note_card_action(
                        x,
                        y,
                        w,
                        "No routing yet",
                        "Roles with driver = \"routed\" run on the first available engine of the task's tier (low, medium, high).",
                        Some((
                            "Set up routing".to_string(),
                            Hit::Button(Action::EditRouting),
                        )),
                    );
                } else {
                    y += self.routing_body(x, y, w);
                }
            }
            Section::Connect => {
                let action = if snap.connected_clis.is_empty() {
                    (
                        "Connect agents".to_string(),
                        false,
                        Hit::Button(Action::OpenConnect(true)),
                    )
                } else {
                    (
                        format!("Connected: {}", snap.connected_clis.join(", ")),
                        false,
                        Hit::Button(Action::OpenConnect(false)),
                    )
                };
                y += self.section_header(x, y, w, "Your own sessions", "", vec![action]);
                y += self.para(
                    x,
                    y,
                    "A claude or codex you start yourself in any TGZTerminal pane gets Tower's delegation tools (tower_models, tower_delegate, tower_await) and the local-only guard. Tower never types into those panes.",
                    Font::Small,
                    self.t.muted,
                    w,
                    4,
                );
            }
            Section::Policy => {
                y += self.section_header(
                    x,
                    y,
                    w,
                    "Policy",
                    "What Tower may do without a click",
                    vec![],
                );
                y += self.policy_body(x, y, w);
            }
            Section::LocalOnly => {
                let paths = &snap.local_only_paths;
                y += self.section_header(
                    x,
                    y,
                    w,
                    "Local-only folders",
                    "Work there never reaches a cloud model",
                    vec![(
                        "Edit folders".to_string(),
                        false,
                        Hit::Button(Action::OpenLocalOnly),
                    )],
                );
                if paths.is_empty() {
                    y += self.para(
                        x,
                        y,
                        "None. Sensitive work can also be marked per flight, schedule, repo (.tower.toml) or intake source.",
                        Font::Small,
                        self.t.muted,
                        w,
                        3,
                    );
                }
                for p in paths.iter().take(5) {
                    self.text_fit(x, y, p, Font::Small, self.t.text, w);
                    y += self.lh_of(Font::Small);
                }
                if paths.len() > 5 {
                    self.text(
                        x,
                        y,
                        &format!("+{} more", paths.len() - 5),
                        Font::Small,
                        self.t.faint,
                    );
                    y += self.lh_of(Font::Small);
                }
            }
            Section::Schedules => {
                let mut actions = vec![];
                if !cfg.is_empty() {
                    actions.push((
                        "Edit schedules.toml".to_string(),
                        false,
                        edit_file("schedules.toml"),
                    ));
                }
                actions.push((
                    "New schedule".to_string(),
                    true,
                    Hit::Button(Action::NewSchedule),
                ));
                y += self.section_header(
                    x,
                    y,
                    w,
                    "Schedules",
                    "Flights that start on a timer",
                    actions,
                );
                y += self.schedules_body(x, y, w);
            }
            Section::Sources => {
                let sources = self.visible_sources();
                let broken = sources.iter().filter(|s| s.error.is_some()).count();
                let sub = match (sources.len(), broken) {
                    (0, _) => "Where flights come from besides you".to_string(),
                    (n, 0) => format!("{} watch{} polling", n, if n == 1 { "" } else { "es" }),
                    (n, b) => format!("{} watches · {} with errors", n, b),
                };
                let mut actions = vec![];
                if !cfg.is_empty() {
                    actions.push((
                        "Edit sources.toml".to_string(),
                        false,
                        edit_file("sources.toml"),
                    ));
                }
                if !sources.is_empty() {
                    actions.push((
                        "Poll now".to_string(),
                        true,
                        Hit::Button(Action::PollSources),
                    ));
                }
                y += self.section_header(x, y, w, "Sources", &sub, actions);
                if sources.is_empty() {
                    let detail = if snap.extras.scouts {
                        "Add a [[jira]], [[github]] or [[gitlab]] watch to sources.toml (Jira: store a token with `tower auth jira <site>`; GitHub/GitLab use your gh/glab login), or a [[scout]] that reads mail or Teams through an agent's own M365 tools."
                    } else {
                        "Add a [[jira]], [[github]] or [[gitlab]] watch to sources.toml (Jira: store a token with `tower auth jira <site>`; GitHub/GitLab use your gh/glab login). New items arrive as flights."
                    };
                    let action = if cfg.is_empty() {
                        None
                    } else {
                        Some(("Edit sources.toml".to_string(), edit_file("sources.toml")))
                    };
                    y += self.note_card_action(x, y, w, "No intake yet", detail, action);
                } else {
                    y += self.sources_body(x, y, w);
                }
            }
            Section::Machine => {
                y += self.section_header(
                    x,
                    y,
                    w,
                    "This machine",
                    "launchd agents and crontab, read-only",
                    vec![("Re-read".to_string(), false, Hit::Button(Action::Refresh))],
                );
                y += self.machine_body(x, y, w);
            }
            Section::Extras => {
                y += self.section_header(
                    x,
                    y,
                    w,
                    "Extras",
                    "Side features, off until you want them ([extras] in tower.toml)",
                    vec![],
                );
                for e in Extra::ALL.iter() {
                    let on = e.get(&snap.extras);
                    let label = format!("{}: {}", e.label(), if on { "on" } else { "off" });
                    let color = if on { self.t.ok } else { self.t.muted };
                    y += self.chip_row(
                        x,
                        y,
                        w,
                        vec![(label, on, color, Hit::Button(Action::ToggleExtra(*e)))],
                    ) + self.u;
                    y += self.para(x, y, e.help(), Font::Small, self.t.faint, w, 3) + gap;
                }
            }
        }
        y - y0
    }

    // -- team ---------------------------------------------------------------

    fn team(&mut self, area: Rect) {
        let area = Rect::new(
            area.x,
            area.y + self.u * 4.0,
            area.w,
            (area.h - self.u * 4.0).max(0.0),
        );
        let md = self.md;
        // Threads: the team, then every flight that has not landed.
        let mut threads: Vec<(String, String, String)> = Vec::new();
        if let Some(team) = md.snap.frequencies.iter().find(|f| f.id == "team") {
            threads.push((
                team.id.clone(),
                "# Team".into(),
                team.last_message
                    .clone()
                    .unwrap_or_else(|| "Everyone".into()),
            ));
        }
        for f in md
            .snap
            .flights
            .iter()
            .filter(|f| f.stage != Stage::Landed)
            .take(40)
        {
            threads.push((f.id.clone(), f.key.clone(), f.title.clone()));
        }
        if threads.is_empty() {
            self.empty_state(
                area,
                "Quiet",
                "The team frequency appears once towerd is connected.",
                None,
            );
            return;
        }
        let selected = if threads.iter().any(|t| t.0 == md.freq) {
            md.freq.clone()
        } else {
            threads[0].0.clone()
        };

        let list_w = (self.lh * 13.0).min(area.w * 0.35).round();
        let (list, convo) = area.take_left(list_w);
        let convo = Rect::new(
            convo.x + self.u * 4.0,
            convo.y,
            (convo.w - self.u * 4.0).max(0.0),
            convo.h,
        );
        let row_h = self.lh_of(Font::Strong) + self.lh_of(Font::Small) + self.u * 3.0;
        let old = self.push_clip(list);
        let mut y = list.y;
        for (id, title, sub) in &threads {
            let r = Rect::new(list.x, y, list.w, row_h);
            let hit = Hit::Button(Action::SelectFrequency(id.clone()));
            if *id == selected {
                self.fill(r, self.t.accent.alpha(0.16), self.u * 1.6);
            } else if self.hovered(&hit) {
                self.fill(r, self.t.card.alpha(0.7), self.u * 1.6);
            }
            let x = r.x + self.u * 2.5;
            let w = r.w - self.u * 5.0;
            self.text_fit(x, r.y + self.u * 1.5, title, Font::Strong, self.t.text, w);
            self.text_fit(
                x,
                r.y + self.u * 1.5 + self.lh_of(Font::Strong),
                sub,
                Font::Small,
                self.t.muted,
                w,
            );
            self.hit(r, hit);
            y += row_h + self.u;
        }
        self.pop_clip(old);

        let pad = self.u * 4.0;
        let compose_h = self.field_height(1);
        let (sw, bh) = self.button_size("Send");
        let footer = Rect::new(convo.x, convo.bottom() - compose_h, convo.w, compose_h);
        let fr = Rect::new(footer.x, footer.y, footer.w - sw - self.u * 2.0, compose_h);
        self.field(fr, FieldId::Compose, &md.compose, 1);
        self.button(
            footer.right() - sw,
            footer.y + (compose_h - bh) / 2.0,
            "Send",
            true,
            Hit::Button(Action::SendCompose),
        );

        let panel = Rect::new(
            convo.x,
            convo.y,
            convo.w,
            (footer.y - convo.y - self.u * 3.0).max(0.0),
        );
        self.fill(panel, self.t.surface, self.u * 2.4);
        let inner = panel.inset(pad);
        let measure = self.push_clip(EMPTY);
        let content = self.thread_messages(&selected, inner.x, 0.0, inner.w);
        self.pop_clip(measure);
        let off = self.scroll_region(ScrollId::Frequencies, inner, content, false, true);
        let old = self.push_clip(inner);
        if content <= 0.0 {
            let hint = if selected == "team" {
                "No messages yet. Agents post here with tower_post; @role reaches a role's agents."
            } else {
                "Messages here go to this flight's agent."
            };
            self.text(inner.x, inner.y, hint, Font::Small, self.t.faint);
        }
        self.thread_messages(&selected, inner.x, inner.y - off, inner.w);
        self.pop_clip(old);
        self.scrollbar(inner, content, off);
    }

    fn chat_line(&mut self, from: &str, text: &str, at: u64, x: f32, y: f32, w: f32) -> f32 {
        let (name, color) = if from == "tower" {
            ("Tower".to_string(), self.t.muted)
        } else {
            self.who(from)
        };
        let d = self.avatar(x, y, &name, color);
        let tx = x + d + self.u * 2.5;
        let nw = self.text_fit(tx, y, &name, Font::Strong, self.t.text, w * 0.7);
        let when = age(self.now, at);
        self.text(tx + nw + self.u * 2.0, y, &when, Font::Small, self.t.faint);
        let mut h = self.lh_of(Font::Strong);
        h += self.para(tx, y + h, text, Font::Body, self.t.text, x + w - tx, 20);
        h + self.u * 3.0
    }

    /// Messages of the team frequency or of a flight; returns the height.
    fn thread_messages(&mut self, id: &str, x: f32, y0: f32, w: f32) -> f32 {
        let md = self.md;
        let mut y = y0;
        if let Some(freq) = md.snap.frequencies.iter().find(|f| f.id == id) {
            for msg in &freq.messages {
                y += self.chat_line(&msg.from, &msg.text, msg.at_unix, x, y, w);
            }
        } else if let Some(f) = md.snap.flight(id) {
            for e in &f.timeline {
                if let TimelineBody::Message { from, text } = &e.body {
                    y += self.chat_line(from, text, e.at_unix, x, y, w);
                }
            }
        }
        y - y0
    }

    // -- models -------------------------------------------------------------

    /// Model cards and server cards at (x, y0); returns the height.
    fn models_body(&mut self, x: f32, y0: f32, w: f32) -> f32 {
        let md = self.md;
        let eps = &md.snap.endpoints;
        let models = &md.snap.models;
        let pad = self.u * 4.0;
        let gap = self.u * 3.0;
        let row_h = pad * 2.0
            + self.lh_of(Font::Strong)
            + self.lh_of(Font::Small)
            + self.u * 2.5
            + self.chip_h();
        let model_h = self.model_card_h();
        let mut y = y0;
        if !models.is_empty() {
            y += self.section_label(x, y, "MODELS AGENTS CAN HAND WORK TO");
            for m in models.iter() {
                self.model_card(m, Rect::new(x, y, w, model_h));
                y += model_h + gap;
            }
            y += gap;
        }
        if !eps.is_empty() {
            y += self.section_label(x, y, "MODEL SERVERS");
        }
        for ep in eps.iter() {
            let r = Rect::new(x, y, w, row_h);
            let radius = self.u * 2.4;
            let card_hit = Hit::Button(Action::EditEndpoint(ep.name.clone()));
            let bg = if self.hovered(&card_hit) {
                self.t.card_hover
            } else {
                self.t.card
            };
            self.shadow(r, radius);
            self.fill(r, bg, radius);
            self.hit(r, card_hit);
            let x = r.x + pad;
            let ty = r.y + pad;
            let (hc, hl) = match ep.health {
                Health::Up => (self.t.ok, "up"),
                Health::Down => (self.t.faint, "not running"),
                Health::Unknown => (self.t.faint, "not probed yet"),
            };
            let d = self.u * 1.6;
            self.dot(x + d / 2.0, ty + self.lh_of(Font::Strong) / 2.0, d, hc);
            let nx = x + d + self.u * 2.5;
            let nw = self.text(nx, ty, &ep.name, Font::Strong, self.t.text);
            let fg = self.t.muted;
            let bg = self.t.tint(self.t.muted);
            let cy = ty + (self.lh_of(Font::Strong) - self.chip_h()) / 2.0;
            let kc = self.chip(nx + nw + self.u * 2.0, cy, &ep.kind, fg, bg);
            let locality = if ep.local {
                Locality::Local
            } else {
                Locality::Cloud
            };
            self.locality_chip(kc.right() + self.u * 1.5, cy, locality);
            let status = match ep.latency_ms {
                Some(ms) => format!("{} · {} ms", hl, ms),
                None => hl.to_string(),
            };
            self.text_right(r.right() - pad, ty, &status, Font::Small, hc);
            let uy = ty + self.lh_of(Font::Strong) + self.u;
            self.text_fit(
                nx,
                uy,
                &ep.base_url,
                Font::Small,
                self.t.muted,
                r.w - pad * 2.0,
            );
            let my = uy + self.lh_of(Font::Small) + self.u * 1.5;
            let mut cx = nx;
            if ep.models.is_empty() {
                let msg = if ep.health == Health::Up {
                    "no models installed"
                } else {
                    "no models reported"
                };
                self.text(cx, my, msg, Font::Small, self.t.faint);
            }
            for model in &ep.models {
                // Served models: click one to describe it for agents.
                let id = format!("{}/{}", ep.name, model);
                let known = models.iter().any(|m| m.id == id);
                let label = if known {
                    model.clone()
                } else {
                    format!("+ {}", model)
                };
                let cw = self.chip_w(&label);
                if cx + cw > r.right() - pad {
                    break;
                }
                let (fg, bg) = if known {
                    (self.t.text, self.t.tint(self.t.ok))
                } else {
                    (self.t.muted, self.t.tint(self.t.muted))
                };
                let c = self.chip(cx, my, &label, fg, bg);
                let hit = if known {
                    Hit::Button(Action::EditModel(id))
                } else {
                    Hit::Button(Action::NewModel(Some(id)))
                };
                self.hit(c, hit);
                cx = c.right() + self.u * 1.5;
            }
            y += row_h + gap;
        }
        (y - y0 - gap).max(0.0)
    }

    /// The `[routing]` engines, one card; click to change the tiers.
    /// Returns the height.
    fn routing_body(&mut self, x: f32, y0: f32, w: f32) -> f32 {
        let md = self.md;
        let engines = &md.snap.engines;
        let pad = self.u * 4.0;
        let eng_row = self.lh_of(Font::Body).max(self.chip_h()) + self.u * 2.0;
        let card_h = pad * 2.0 + engines.len() as f32 * eng_row;
        let r = Rect::new(x, y0, w, card_h);
        let radius = self.u * 2.4;
        let hit = Hit::Button(Action::EditRouting);
        let bg = if self.hovered(&hit) {
            self.t.card_hover
        } else {
            self.t.card
        };
        self.shadow(r, radius);
        self.fill(r, bg, radius);
        self.hit(r, hit);
        let x = r.x + pad;
        let right = r.right() - pad;
        let mut ry = r.y + pad;
        let tier_w = self.chip_w("medium");
        for e in engines.iter() {
            let cy = ry + (eng_row - self.chip_h()) / 2.0;
            let ty = ry + (eng_row - self.lh_of(Font::Body)) / 2.0;
            let d = self.u * 1.6;
            // Missing program: quiet, it is a fact about this machine, not
            // a fault; anything else that blocks the engine is a warning.
            let dc = if e.available {
                self.t.ok
            } else if e.missing {
                self.t.faint
            } else {
                self.t.bad
            };
            self.dot(x + d / 2.0, ry + eng_row / 2.0, d, dc);
            self.tier_chip(x + d + self.u * 2.0, cy, &e.tier);
            let nx = x + d + self.u * 4.0 + tier_w;
            let nw = self.text(nx, ty, &e.id, Font::Strong, self.t.text);
            let lc = self.locality_chip(nx + nw + self.u * 2.0, cy, e.locality);
            let mut detail = format!("{} {}", e.driver.label(), e.model);
            if !e.effort.is_empty() {
                detail.push_str(&format!(" · effort {}", e.effort));
            }
            let (status, sc) = match &e.why {
                Some(w) if e.missing => (w.clone(), self.t.faint),
                Some(w) => (w.clone(), self.t.warn),
                None if e.trained > 0 => (format!("{} outcomes", e.trained), self.t.muted),
                None => ("ready".to_string(), self.t.muted),
            };
            let sw = self.text_right(right, ty, &status, Font::Small, sc);
            let dx = lc.right() + self.u * 2.5;
            self.text_fit(
                dx,
                ty,
                &detail,
                Font::Small,
                self.t.muted,
                right - sw - self.u * 3.0 - dx,
            );
            ry += eng_row;
        }
        card_h
    }

    fn model_card_h(&self) -> f32 {
        let pad = self.u * 4.0;
        pad * 2.0
            + self.lh_of(Font::Strong)
            + self.u
            + self.lh_of(Font::Small)
            + self.u * 2.0
            + self.chip_h()
            + self.u
            + self.lh_of(Font::Small)
    }

    /// One `[[model]]`: what agents read before handing it work.
    fn model_card(&mut self, m: &ModelInfo, r: Rect) {
        let pad = self.u * 4.0;
        let radius = self.u * 2.4;
        let hit = Hit::Button(Action::EditModel(m.id.clone()));
        let bg = if self.hovered(&hit) {
            self.t.card_hover
        } else {
            self.t.card
        };
        self.shadow(r, radius);
        self.fill(r, bg, radius);
        self.hit(r, hit);
        let x = r.x + pad;
        let right = r.right() - pad;
        let ty = r.y + pad;
        let d = self.u * 1.6;
        let down = m
            .why
            .as_deref()
            .map(|w| w.ends_with("is not reachable"))
            .unwrap_or(false);
        let dc = if m.available {
            self.t.ok
        } else if down {
            self.t.faint
        } else {
            self.t.bad
        };
        self.dot(x + d / 2.0, ty + self.lh_of(Font::Strong) / 2.0, d, dc);
        let nx = x + d + self.u * 2.5;
        let nw = self.text(nx, ty, &m.label, Font::Strong, self.t.text);
        let cy = ty + (self.lh_of(Font::Strong) - self.chip_h()) / 2.0;
        self.locality_chip(nx + nw + self.u * 2.0, cy, m.locality);
        let (status, sc) = match &m.why {
            Some(_) if down => ("server not running".to_string(), self.t.faint),
            Some(w) => (w.clone(), self.t.warn),
            None => (
                format!("{} of {} running", m.busy, m.max_concurrency),
                self.t.muted,
            ),
        };
        self.text_right(right, ty, &status, Font::Small, sc);
        let mut facts = vec![m.id.clone()];
        if !m.size.is_empty() {
            facts.push(m.size.clone());
        }
        if let Some(c) = m.context {
            facts.push(format!("{} context", compact(c as f64)));
        }
        let ly = ty + self.lh_of(Font::Strong) + self.u;
        self.text_fit(
            nx,
            ly,
            &facts.join("  ·  "),
            Font::Small,
            self.t.muted,
            right - nx,
        );
        let gy = ly + self.lh_of(Font::Small) + self.u * 2.0;
        let mut cx = nx;
        if m.good_for.is_empty() {
            self.text(
                cx,
                gy,
                "Say what it is good for, so agents know what to hand it",
                Font::Small,
                self.t.faint,
            );
        }
        for g in &m.good_for {
            let cw = self.chip_w(g);
            if cx + cw > right {
                break;
            }
            let fg = self.t.text;
            let bg = self.t.tint(self.t.violet);
            let c = self.chip(cx, gy, g, fg, bg);
            cx = c.right() + self.u * 1.5;
        }
        let ny = gy + self.chip_h() + self.u;
        if !m.notes.is_empty() {
            let note = m.notes.lines().next().unwrap_or("");
            self.text_fit(nx, ny, note, Font::Small, self.t.faint, right - nx);
        }
    }

    /// Sensitive work: only models inside your network may see it.
    fn local_only_chip(&mut self, x: f32, y: f32) -> Rect {
        let c = self.t.ok;
        let fg = self.t.readable(c);
        let bg = self.t.tint(c);
        self.dot_chip(x, y, "local only", c, fg, bg)
    }

    /// The "sensitive" toggle of the New flight form; returns its height.
    fn local_only_toggle(&mut self, x: f32, y: f32, w: f32, on: bool) -> f32 {
        let ok = self.t.ok;
        let h = self.chip_row(
            x,
            y,
            w,
            vec![(
                "Sensitive: local models only".to_string(),
                on,
                ok,
                Hit::Button(Action::SheetFlag(SheetFlag::LocalOnly)),
            )],
        );
        let hint = if on {
            "Only models inside your network work on it; cloud agents are refused, triage too."
        } else {
            "Credentials, customer or personal data? Keep it on your own models."
        };
        let hy = y + h + self.u;
        self.text_fit(x, hy, hint, Font::Small, self.t.faint, w);
        hy + self.lh_of(Font::Small) - y
    }

    /// "local" (inside your network) or "cloud".
    fn locality_chip(&mut self, x: f32, y: f32, l: Locality) -> Rect {
        let (label, c) = match l {
            Locality::Local => ("local", self.t.ok),
            Locality::Cloud => ("cloud", self.t.info),
        };
        let fg = self.t.readable(c);
        let bg = self.t.tint(c);
        self.chip(x, y, label, fg, bg)
    }

    // -- intake -------------------------------------------------------------

    /// The intake watches Setup shows: scouts only while that extra is on.
    fn visible_sources(&self) -> Vec<&'a SourceStatus> {
        let md = self.md;
        md.snap
            .sources
            .iter()
            .filter(|s| md.snap.extras.scouts || !matches!(s.kind.as_str(), "mail" | "teams"))
            .collect()
    }

    /// Source cards at (x, y0); returns the height.
    fn sources_body(&mut self, x: f32, y0: f32, w: f32) -> f32 {
        let sources = self.visible_sources();
        let gap = self.u * 3.0;
        let measure = self.push_clip(EMPTY);
        let heights: Vec<f32> = sources
            .iter()
            .map(|s| self.source_card(s, x, 0.0, w))
            .collect();
        self.pop_clip(measure);
        let mut y = y0;
        for (src, h) in sources.iter().zip(heights.iter()) {
            let r = Rect::new(x, y, w, *h);
            let radius = self.u * 2.4;
            self.shadow(r, radius);
            self.fill(r, self.t.card, radius);
            self.source_card(src, x, y, w);
            y += h + gap;
        }
        (y - y0 - gap).max(0.0)
    }

    /// One intake watch; returns its height.
    fn source_card(&mut self, src: &SourceStatus, x0: f32, y0: f32, w: f32) -> f32 {
        let pad = self.u * 4.0;
        let x = x0 + pad;
        let right = x0 + w - pad;
        let mut y = y0 + pad;
        let (dot, state) = if src.polling {
            (self.t.info, "polling…".to_string())
        } else if src.error.is_some() {
            (self.t.bad, "failing".to_string())
        } else if src.last_poll_unix == 0 {
            (self.t.faint, "not polled yet".to_string())
        } else {
            (
                self.t.ok,
                format!("polled {} ago", age(self.now, src.last_poll_unix)),
            )
        };
        let d = self.u * 1.6;
        self.dot(x + d / 2.0, y + self.lh_of(Font::Strong) / 2.0, d, dot);
        let nx = x + d + self.u * 2.5;
        let nw = self.text_fit(nx, y, &src.name, Font::Strong, self.t.text, w * 0.5);
        let kind = match src.kind.as_str() {
            "jira" => "Jira",
            "github" => "GitHub",
            "gitlab" => "GitLab",
            "mail" => "Mail scout",
            "teams" => "Teams scout",
            other => other,
        };
        let fg = self.t.info;
        let bg = self.t.tint(self.t.info);
        self.chip(
            nx + nw + self.u * 2.0,
            y + (self.lh_of(Font::Strong) - self.chip_h()) / 2.0,
            kind,
            fg,
            bg,
        );
        self.text_right(right, y, &state, Font::Small, dot);
        y += self.lh_of(Font::Strong) + self.u * 2.0;
        y += self.para(nx, y, &src.query, Font::Small, self.t.muted, right - nx, 3);
        y += self.u * 2.0;
        let md = self.md;
        let role = src
            .role_id
            .as_deref()
            .map(|r| {
                md.snap
                    .role(r)
                    .map(|x| x.name.clone())
                    .unwrap_or_else(|| r.to_string())
            })
            .unwrap_or_else(|| "held for you".into());
        let mut line = format!("→ {}", role);
        let scout = matches!(src.kind.as_str(), "mail" | "teams");
        if src.auto_launch {
            line.push_str(if scout {
                " · runs on its own while armed"
            } else {
                " · starts on its own while armed"
            });
        }
        line.push_str(&format!(
            " · {} flight{} so far",
            src.flights,
            if src.flights == 1 { "" } else { "s" }
        ));
        self.text_fit(nx, y, &line, Font::Small, self.t.faint, right - nx);
        y += self.lh_of(Font::Small) + self.u * 2.0;
        if scout && !src.polling {
            let (_, bh) = self.button_size("Run scout");
            self.button(
                nx,
                y,
                "Run scout",
                true,
                Hit::Button(Action::RunScout(src.name.clone())),
            );
            y += bh + self.u * 2.0;
        }
        if let Some(err) = &src.error {
            let c = self.t.bad;
            y += self.banner(nx, y, right - nx, err, c) + self.u * 2.0;
        }
        y + pad - self.u * 2.0 - y0
    }

    // -- response clocks ----------------------------------------------------

    fn tone_color(&self, tone: Tone) -> Color {
        match tone {
            Tone::Quiet => self.t.muted,
            Tone::Calm => self.t.ok,
            Tone::Half => self.t.warn,
            Tone::Late => self.t.bad,
        }
    }

    // -- policy -------------------------------------------------------------

    /// Seconds Armed has left, if armed.
    fn armed_left(&self) -> Option<u64> {
        self.md
            .snap
            .armed_until
            .filter(|until| *until > self.now)
            .map(|until| until - self.now)
    }

    /// The policy rows (one chip per level) at (x0, y0); returns the height.
    fn policy_body(&mut self, x0: f32, y0: f32, w: f32) -> f32 {
        let md = self.md;
        let rows = &md.snap.policy;
        if rows.is_empty() {
            return self.note_card(
                x0,
                y0,
                w,
                "No policy yet",
                "This towerd predates [policy]: restart it to see what Tower may do on its own.",
            );
        }
        let pad = self.u * 4.0;
        let row_h = self.lh_of(Font::Strong) + self.lh_of(Font::Small) + self.u * 5.0;
        let intro = if self.armed_left().is_some() {
            "Armed: actions set to auto run on their own until the timer ends; the rest still ask. Levels: off · draft (prepared, you do it) · confirm (one click in the Inbox) · auto."
        } else {
            "Disarmed: every action asks first, even those set to auto. Arm Tower when you want auto actions to run. Levels: off · draft (prepared, you do it) · confirm (one click in the Inbox) · auto."
        };
        let card_h = row_h * rows.len() as f32 + pad * 2.0;
        let mut y = y0;
        y += self.para(x0, y, intro, Font::Small, self.t.muted, w, 4) + self.u * 4.0;
        let card = Rect::new(x0, y, w, card_h);
        let radius = self.u * 2.4;
        self.shadow(card, radius);
        self.fill(card, self.t.card, radius);
        let x = card.x + pad;
        let right = card.right() - pad;
        y += pad;
        for row in rows.iter() {
            // One chip per level; the current one is filled. Click to set.
            let mut cx = right;
            for level in ["auto", "confirm", "draft", "off"].iter() {
                let color = match *level {
                    "auto" => self.t.bad,
                    "confirm" => self.t.info,
                    "draft" => self.t.muted,
                    _ => self.t.faint,
                };
                let cw =
                    self.m.width(level, Font::Strong) + self.lh_of(Font::Strong) + self.u * 2.0;
                cx -= cw;
                self.pick_chip(
                    cx,
                    y - self.u,
                    level,
                    color,
                    row.level == *level,
                    Hit::Button(Action::SetPolicyLevel {
                        action: row.action.clone(),
                        level: level.to_string(),
                    }),
                );
                cx -= self.u * 1.2;
            }
            let cw = right - cx;
            let text_w = (right - x - cw - self.u * 4.0).max(0.0);
            self.text_fit(x, y, &row.action, Font::Strong, self.t.text, text_w);
            let sy = y + self.lh_of(Font::Strong) + self.u;
            let help = if row.configured {
                format!("{} · set here", action_help(&row.action))
            } else {
                action_help(&row.action).to_string()
            };
            self.text_fit(x, sy, &help, Font::Small, self.t.muted, text_w);
            y += row_h;
        }
        y + pad - y0
    }

    // -- routines -----------------------------------------------------------

    /// A card around `body`, which is laid out twice: once to measure it,
    /// once to draw it. Returns the card's height.
    fn boxed(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        body: &dyn Fn(&mut Self, f32, f32, f32) -> f32,
    ) -> f32 {
        let pad = self.u * 4.0;
        let measure = self.push_clip(EMPTY);
        let inner = body(self, x + pad, 0.0, w - pad * 2.0);
        self.pop_clip(measure);
        let h = inner + pad * 2.0;
        let r = Rect::new(x, y, w, h);
        let radius = self.u * 2.4;
        self.shadow(r, radius);
        self.fill(r, self.t.card, radius);
        body(self, x + pad, y + pad, w - pad * 2.0);
        h
    }

    /// A card with a title and a line of explanation; returns its height.
    fn note_card(&mut self, x: f32, y: f32, w: f32, title: &str, detail: &str) -> f32 {
        self.boxed(x, y, w, &|l: &mut Self, x, y, w| {
            l.text(x, y, title, Font::Strong, l.t.muted);
            let th = l.lh_of(Font::Strong) + l.u;
            th + l.para(x, y + th, detail, Font::Small, l.t.faint, w, 4)
        })
    }

    /// A row of buttons from `x`; returns the button height.
    fn buttons_left(&mut self, x: f32, y: f32, items: Vec<(String, bool, Hit)>) -> f32 {
        let mut bx = x;
        let mut bh = 0.0f32;
        for (label, primary, hit) in items {
            let r = self.button(bx, y, &label, primary, hit);
            bx = r.right() + self.u * 2.0;
            bh = bh.max(r.h);
        }
        bh
    }

    /// `Mon 08:30 · in 3h 12m`.
    fn when_label(&self, t: u64) -> String {
        format!(
            "{} · in {}",
            civil_label(&local_civil(t)),
            duration(t.saturating_sub(self.now))
        )
    }

    fn run_color(&self, r: RunResult) -> Color {
        match r {
            RunResult::Running => self.t.info,
            RunResult::Landed => self.t.ok,
            RunResult::Proposed => self.t.warn,
            RunResult::Declined | RunResult::Failed => self.t.bad,
            RunResult::Skipped => self.t.faint,
        }
    }

    /// Tower's schedules at (x, y0); returns the height.
    fn schedules_body(&mut self, x: f32, y0: f32, w: f32) -> f32 {
        let md = self.md;
        let gap = self.u * 3.0;
        let mut y = y0;
        if md.snap.schedules.is_empty() {
            y += self.note_card(
                x,
                y,
                w,
                "No schedules yet",
                "A schedule starts a flight at a time: a morning PR review, a nightly dependency audit. New schedule (key s) has templates. Saving one is your OK for it to run on time, without Armed.",
            ) + gap;
        }
        for row in md.snap.schedules.iter() {
            y += self.boxed(x, y, w, &|l: &mut Self, x, y, w| {
                l.schedule_body(row, x, y, w)
            }) + gap;
        }
        (y - y0 - gap).max(0.0)
    }

    /// This machine's launchd agents and crontab lines, read-only, at
    /// (x, y0); returns the height.
    fn machine_body(&mut self, x: f32, y0: f32, w: f32) -> f32 {
        let md = self.md;
        let gap = self.u * 3.0;
        let mut y = y0;
        if !md.snap.routines_note.is_empty() {
            let c = self.t.warn;
            y += self.banner(x, y, w, &md.snap.routines_note, c) + gap;
        }
        let mine: Vec<&RoutineRow> = md.snap.routines.iter().filter(|r| r.yours).collect();
        let other: Vec<&RoutineRow> = md.snap.routines.iter().filter(|r| !r.yours).collect();
        if mine.is_empty() && md.snap.routines_note.is_empty() {
            y += self.note_card(
                x,
                y,
                w,
                "None of your own",
                "No launchd agents of yours in ~/Library/LaunchAgents and no crontab lines. Tower only reads these; it never changes them.",
            ) + gap;
        }
        for r in mine {
            y += self.boxed(x, y, w, &|l: &mut Self, x, y, w| l.routine_body(r, x, y, w)) + gap;
        }
        if !other.is_empty() {
            let label = if md.show_other_routines {
                format!("Hide {} other jobs", other.len())
            } else {
                format!("Show {} other jobs (updaters, helpers)", other.len())
            };
            let (_, bh) = self.button_size(&label);
            self.button(
                x,
                y,
                &label,
                false,
                Hit::Button(Action::ToggleOtherRoutines),
            );
            y += bh + gap;
            if md.show_other_routines {
                for r in other {
                    y += self.boxed(x, y, w, &|l: &mut Self, x, y, w| l.routine_body(r, x, y, w))
                        + gap;
                }
            }
        }
        (y - y0 - gap).max(0.0)
    }

    /// One Tower schedule; returns its height.
    fn schedule_body(&mut self, row: &ScheduleRow, x: f32, y0: f32, w: f32) -> f32 {
        let md = self.md;
        let spec = &row.spec;
        let right = x + w;
        let mut y = y0;
        let (dot, state) = if row.error.is_some() {
            (self.t.bad, "can't run".to_string())
        } else if !spec.enabled {
            (self.t.faint, "paused by you".to_string())
        } else if row.paused_reason.is_some() {
            (self.t.warn, "paused by Tower".to_string())
        } else {
            match row.next_fire_unix {
                Some(t) => (self.t.ok, format!("next {}", self.when_label(t))),
                None => (self.t.muted, "planning…".to_string()),
            }
        };
        let sl = self.lh_of(Font::Strong);
        let d = self.u * 1.6;
        self.dot(x + d / 2.0, y + sl / 2.0, d, dot);
        let nx = x + d + self.u * 2.5;
        let state_w = self.m.width(&state, Font::Small);
        let name_w = (right - nx - state_w - self.u * 4.0).max(0.0);
        self.text_fit(nx, y, &spec.name, Font::Strong, self.t.text, name_w);
        self.text_right(right, y, &state, Font::Small, dot);
        y += sl + self.u;
        let role = match spec.role.as_str() {
            "auto" => "Auto".to_string(),
            r => md
                .snap
                .role(r)
                .map(|x| x.name.clone())
                .unwrap_or_else(|| r.to_string()),
        };
        let place = match spec.place.as_str() {
            "dir" => format!("in {}", spec.path.clone().unwrap_or_default()),
            "worktree" => format!("worktree of {}", spec.path.clone().unwrap_or_default()),
            _ => "new folder each run".to_string(),
        };
        let line = format!("{} · {} · {}", row.when_text, role, place);
        self.text_fit(nx, y, &line, Font::Small, self.t.muted, right - nx);
        y += self.lh_of(Font::Small) + self.u * 2.0;

        // The latest runs, oldest first; a dot with a flight opens it.
        let shown = row.recent.len().min(20);
        if shown == 0 {
            self.text(nx, y, "No runs yet", Font::Small, self.t.faint);
            y += self.lh_of(Font::Small) + self.u * 2.0;
        } else {
            let dd = self.u * 2.2;
            let step = dd + self.u * 1.4;
            let marks = &row.recent[row.recent.len() - shown..];
            for (i, m) in marks.iter().enumerate() {
                let cx = nx + dd / 2.0 + i as f32 * step;
                let cy = y + self.lh_of(Font::Small) / 2.0;
                let c = self.run_color(m.result);
                self.dot(cx, cy, dd, c);
                if let Some(fid) = m.flight.as_ref().filter(|f| md.snap.flight(f).is_some()) {
                    self.hit(
                        Rect::new(cx - step / 2.0, cy - step / 2.0, step, step),
                        Hit::Button(Action::OpenFlight {
                            flight_id: fid.clone(),
                        }),
                    );
                }
            }
            if let Some(last) = marks.last() {
                let what = match last.result {
                    RunResult::Running => "running",
                    RunResult::Landed => "landed",
                    RunResult::Proposed => "proposed",
                    RunResult::Declined => "declined",
                    RunResult::Failed => "failed",
                    RunResult::Skipped => "skipped",
                };
                let mut text = format!("last {} ago: {}", age(self.now, last.at_unix), what);
                if !last.note.is_empty() {
                    text.push_str(&format!(" ({})", last.note));
                }
                let tx = nx + shown as f32 * step + self.u * 2.0;
                self.text_fit(
                    tx,
                    y,
                    &text,
                    Font::Small,
                    self.t.muted,
                    (right - tx).max(0.0),
                );
            }
            y += self.lh_of(Font::Small) + self.u * 2.0;
        }
        if let Some(why) = &row.paused_reason {
            let c = self.t.warn;
            y += self.banner(nx, y, right - nx, &format!("Paused: {}", why), c) + self.u * 2.0;
        }
        if let Some(err) = &row.error {
            let c = self.t.bad;
            y += self.banner(nx, y, right - nx, err, c) + self.u * 2.0;
        }
        let id = spec.id.clone();
        let paused = !spec.enabled || row.paused_reason.is_some();
        let delete = if md.confirm_delete.as_deref() == Some(id.as_str()) {
            "Really delete?"
        } else {
            "Delete"
        };
        y += self.buttons_left(
            nx,
            y,
            vec![
                (
                    "Run now".to_string(),
                    true,
                    Hit::Button(Action::RunSchedule(id.clone())),
                ),
                (
                    if paused { "Resume" } else { "Pause" }.to_string(),
                    false,
                    Hit::Button(Action::SetScheduleEnabled {
                        id: id.clone(),
                        enabled: paused,
                    }),
                ),
                (
                    "Edit".to_string(),
                    false,
                    Hit::Button(Action::EditSchedule(id.clone())),
                ),
                (
                    delete.to_string(),
                    false,
                    Hit::Button(Action::DeleteSchedule(id)),
                ),
            ],
        );
        y - y0
    }

    /// One launchd job or crontab line; returns its height.
    fn routine_body(&mut self, r: &RoutineRow, x: f32, y0: f32, w: f32) -> f32 {
        let right = x + w;
        let mut y = y0;
        let sl = self.lh_of(Font::Strong);
        let src = if r.source == "cron" {
            "cron"
        } else {
            "launchd"
        };
        let fg = self.t.info;
        let bg = self.t.tint(self.t.info);
        let chip = self.chip(x, y + (sl - self.chip_h()) / 2.0, src, fg, bg);
        let (state, color) = if r.stalled {
            ("looks stalled".to_string(), self.t.warn)
        } else if r.pid.is_some() {
            ("running".to_string(), self.t.info)
        } else if r.disabled {
            ("disabled".to_string(), self.t.faint)
        } else if !r.loaded {
            ("not loaded".to_string(), self.t.faint)
        } else {
            match r.last_exit {
                Some(0) => ("last exit 0".to_string(), self.t.ok),
                Some(c) => (format!("last exit {}", c), self.t.bad),
                None => (String::new(), self.t.muted),
            }
        };
        let state_w = self.m.width(&state, Font::Small);
        let nx = chip.right() + self.u * 2.0;
        let name_w = (right - nx - state_w - self.u * 4.0).max(0.0);
        self.text_fit(nx, y, &r.label, Font::Strong, self.t.text, name_w);
        self.text_right(right, y, &state, Font::Small, color);
        y += sl + self.u;
        let mut line = r.schedule_text.clone();
        if let Some(t) = r.next_fire_unix {
            line.push_str(&format!(" · next {}", self.when_label(t)));
        }
        if let Some(t) = r.last_activity_unix {
            line.push_str(&format!(" · log written {} ago", age(self.now, t)));
        }
        self.text_fit(x, y, &line, Font::Small, self.t.muted, w);
        y += self.lh_of(Font::Small) + self.u;
        if !r.program.is_empty() && r.program != r.label {
            self.text_fit(x, y, &r.program, Font::Small, self.t.faint, w);
            y += self.lh_of(Font::Small) + self.u;
        }
        let mut items = vec![];
        if let Some(p) = &r.file_path {
            items.push((
                "Open file".to_string(),
                false,
                Hit::Button(Action::EditFile { path: p.clone() }),
            ));
        }
        if let Some(p) = &r.log_path {
            items.push((
                "Open log".to_string(),
                false,
                Hit::Button(Action::OpenPath { path: p.clone() }),
            ));
        }
        if !items.is_empty() {
            y += self.u + self.buttons_left(x, y + self.u, items);
        }
        y - y0
    }

    // -- activity -----------------------------------------------------------

    fn history(&mut self, area: Rect) {
        let w = area.w.min(self.lh * 64.0);
        let area = Rect::new(
            area.x,
            area.y + self.u * 4.0,
            w,
            (area.h - self.u * 4.0).max(0.0),
        );
        let inner = area.w - self.u * 3.0;
        let measure = self.push_clip(EMPTY);
        let content = self.history_body(area.x, 0.0, inner);
        self.pop_clip(measure);
        let off = self.scroll_region(ScrollId::History, area, content, false, false);
        let old = self.push_clip(area);
        self.history_body(area.x, area.y - off, inner);
        self.pop_clip(old);
        self.scrollbar(area, content, off);
    }

    /// Charts while the activity extra is on, then what ran by itself.
    fn history_body(&mut self, x: f32, y0: f32, w: f32) -> f32 {
        let md = self.md;
        let gap = self.u * 3.0;
        let mut y = y0;
        let limit = if md.snap.extras.activity {
            y += self.activity_charts(x, y, w) + gap;
            12
        } else {
            40
        };
        y += self.recent_body(x, y, w, limit);
        y - y0
    }

    /// The days the charts show, oldest first.
    fn activity_days(&self) -> &'a [DayStats] {
        let md = self.md;
        let days = &md.snap.activity.days;
        let n = md.activity_days.min(days.len());
        &days[days.len() - n..]
    }

    /// Tiles and charts for the chosen days; returns the height.
    fn activity_charts(&mut self, x: f32, y0: f32, w: f32) -> f32 {
        let days = self.activity_days();
        let gap = self.u * 3.0;
        let mut y = y0;
        if days
            .iter()
            .all(|d| d.started.is_empty() && d.auto.is_empty() && d.asked == 0 && d.cost_usd == 0.0)
        {
            y += self.note_card(
                x,
                y,
                w,
                "Nothing yet",
                "Once flights start (by you, a watch or a schedule), this shows what ran by itself, what asked you, how runs ended and what it cost.",
            );
            return y - y0;
        }
        let total = |f: &dyn Fn(&DayStats) -> f64| days.iter().map(|d| f(d)).collect::<Vec<f64>>();
        let auto_all = total(&|d| d.auto.values().sum::<u32>() as f64);
        let scheduled = total(&|d| count(&d.auto, "schedule.run"));
        let landed = total(&|d| count(&d.outcomes, "landed") + count(&d.outcomes, "proposed"));
        let asked = total(&|d| d.asked as f64);
        let spent = total(&|d| d.cost_usd);
        let gates = total(&|d| count(&d.outcomes, "gates_failed"));
        let sum = |v: &[f64]| v.iter().sum::<f64>();
        let tiles: Vec<(&str, String, &[f64], Color)> = vec![
            (
                "Ran by itself",
                compact(sum(&auto_all)),
                &auto_all[..],
                self.t.accent,
            ),
            (
                "Scheduled runs",
                compact(sum(&scheduled)),
                &scheduled[..],
                self.t.ok,
            ),
            ("Landed", compact(sum(&landed)), &landed[..], self.t.ok),
            ("Asked you", compact(sum(&asked)), &asked[..], self.t.info),
            ("Spent", compact_money(sum(&spent)), &spent[..], self.t.warn),
            (
                "Gate failures",
                compact(sum(&gates)),
                &gates[..],
                self.t.bad,
            ),
        ];
        let min_tile = self.lh * 10.0;
        let cols = (((w + gap) / (min_tile + gap)).floor() as usize)
            .max(1)
            .min(tiles.len());
        let tw = (w - gap * (cols as f32 - 1.0)) / cols as f32;
        let pad = self.u * 3.0;
        let spark_h = (self.lh * 1.4).round();
        let th = pad * 2.0 + self.lh_of(Font::Small) + self.lh_of(Font::Heading) + self.u + spark_h;
        for (i, (label, value, series, color)) in tiles.iter().enumerate() {
            let cx = x + (i % cols) as f32 * (tw + gap);
            let cy = y + (i / cols) as f32 * (th + gap);
            let r = Rect::new(cx, cy, tw, th);
            let radius = self.u * 2.4;
            self.shadow(r, radius);
            self.fill(r, self.t.card, radius);
            self.text(cx + pad, cy + pad, label, Font::Small, self.t.muted);
            let vy = cy + pad + self.lh_of(Font::Small);
            self.text(cx + pad, vy, value, Font::Heading, self.t.text);
            let spark = Rect::new(
                cx + pad,
                vy + self.lh_of(Font::Heading) + self.u,
                tw - pad * 2.0,
                spark_h,
            );
            for b in spark_bars(spark, series, self.u * 0.5) {
                self.fill(b, color.alpha(0.75), self.u * 0.3);
            }
        }
        let rows = (tiles.len() + cols - 1) / cols;
        y += rows as f32 * (th + gap);

        // Flights started, by origin. Colours follow ORIGINS, so a series
        // keeps its colour whichever others are present.
        let origins: Vec<usize> = (0..ORIGINS.len())
            .filter(|i| days.iter().any(|d| count(&d.started, ORIGINS[*i].0) > 0.0))
            .collect();
        let columns: Vec<Column> = days
            .iter()
            .map(|d| Column {
                label: short_day(&d.day),
                values: origins
                    .iter()
                    .map(|i| count(&d.started, ORIGINS[*i].0))
                    .collect(),
            })
            .collect();
        let legend: Vec<(String, Color)> = origins
            .iter()
            .map(|i| (ORIGINS[*i].1.to_string(), self.t.series(*i)))
            .collect();
        y += self.chart_card(
            x,
            y,
            w,
            "Flights started",
            "by what started them",
            &columns,
            &legend,
            false,
        ) + gap;

        // What ran by itself, by action: the busiest first.
        let mut keys: Vec<(String, f64)> = Vec::new();
        for d in days {
            for (k, n) in d.auto.iter() {
                match keys.iter_mut().find(|(kk, _)| kk == k) {
                    Some(e) => e.1 += *n as f64,
                    None => keys.push((k.clone(), *n as f64)),
                }
            }
        }
        keys.retain(|(_, n)| *n > 0.0);
        keys.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        if !keys.is_empty() {
            let columns: Vec<Column> = days
                .iter()
                .map(|d| Column {
                    label: short_day(&d.day),
                    values: keys.iter().map(|(k, _)| count(&d.auto, k)).collect(),
                })
                .collect();
            let legend: Vec<(String, Color)> = keys
                .iter()
                .enumerate()
                .map(|(i, (k, _))| (action_short(k).to_string(), self.t.series(i)))
                .collect();
            y += self.chart_card(
                x,
                y,
                w,
                "Ran by itself",
                "actions Tower took without a click",
                &columns,
                &legend,
                false,
            ) + gap;
        }

        // How flights ended.
        let ends: Vec<(&str, &str, Color)> = vec![
            ("landed", "Landed", self.t.ok),
            ("proposed", "Proposed", self.t.warn),
            ("declined", "Declined", self.t.violet),
            ("gates_failed", "Gate failed", self.t.bad),
            ("escalated", "Escalated", self.t.info),
        ];
        let values: Vec<f64> = ends
            .iter()
            .map(|(k, _, _)| days.iter().map(|d| count(&d.outcomes, k)).sum())
            .collect();
        if values.iter().any(|v| *v > 0.0) {
            y += self.boxed(x, y, w, &|l: &mut Self, x, y0, w| {
                let mut y = y0;
                l.text(x, y, "How runs ended", Font::Strong, l.t.text);
                y += l.lh_of(Font::Strong) + l.u * 2.0;
                let bar = Rect::new(x, y, w, l.u * 3.0);
                l.fill(bar, l.t.border.alpha(0.4), l.u * 1.5);
                for p in share_bar(bar, &values, l.u * 2.0) {
                    l.fill(p.rect, ends[p.series].2, 0.0);
                }
                y += bar.h + l.u * 2.0;
                let legend: Vec<(String, Color)> = ends
                    .iter()
                    .zip(values.iter())
                    .filter(|(_, v)| **v > 0.0)
                    .map(|((_, label, c), v)| (format!("{} {}", label, compact(*v)), *c))
                    .collect();
                y += l.legend(x, y, w, &legend);
                y - y0
            }) + gap;
        }

        // Spend per day.
        if spent.iter().any(|v| *v > 0.0) {
            let columns: Vec<Column> = days
                .iter()
                .map(|d| Column {
                    label: short_day(&d.day),
                    values: vec![d.cost_usd],
                })
                .collect();
            let legend = vec![(
                format!("{} in {} days", compact_money(sum(&spent)), days.len()),
                self.t.warn,
            )];
            y += self.chart_card(
                x,
                y,
                w,
                "Spend",
                "API-equivalent cost",
                &columns,
                &legend,
                true,
            ) + gap;
        }
        (y - y0 - gap).max(0.0)
    }

    /// The latest things that ran by themselves (from the audit log);
    /// returns the height.
    fn recent_body(&mut self, x: f32, y0: f32, w: f32, limit: usize) -> f32 {
        let md = self.md;
        let recent = &md.snap.activity.recent;
        {
            self.boxed(x, y0, w, &|l: &mut Self, x, y0, w| {
                let mut y = y0;
                l.text(x, y, "Ran by itself, latest", Font::Strong, l.t.text);
                y += l.lh_of(Font::Strong) + l.u * 2.0;
                if recent.is_empty() {
                    y += l.para(
                        x,
                        y,
                        "Nothing ran by itself yet · What may run on its own is set in Setup → Policy.",
                        Font::Small,
                        l.t.faint,
                        w,
                        3,
                    );
                }
                let row_h = l.lh_of(Font::Small) + l.u * 1.5;
                let when_w = l.m.width("00h ago", Font::Small) + l.u * 2.0;
                for e in recent.iter().take(limit) {
                    let flight = e.flight.as_ref().and_then(|f| md.snap.flight(f));
                    let what = format!(
                        "{}{}{}",
                        action_short(&e.action),
                        e.target
                            .as_ref()
                            .map(|t| format!(" · {}", t))
                            .unwrap_or_default(),
                        flight.map(|f| format!(" · {}", f.key)).unwrap_or_default()
                    );
                    let hit = flight.map(|f| {
                        Hit::Button(Action::OpenFlight {
                            flight_id: f.id.clone(),
                        })
                    });
                    let row = Rect::new(x - l.u, y - l.u * 0.5, w + l.u * 2.0, row_h);
                    if let Some(h) = &hit {
                        if l.hovered(h) {
                            l.fill(row, l.t.card_hover, l.u);
                        }
                    }
                    l.text(
                        x,
                        y,
                        &format!("{} ago", age(l.now, e.at_unix)),
                        Font::Small,
                        l.t.faint,
                    );
                    let c = if e.outcome.starts_with("failed") {
                        l.t.bad
                    } else {
                        l.t.text
                    };
                    l.text_fit(x + when_w, y, &what, Font::Small, c, w - when_w);
                    if let Some(h) = hit {
                        l.hit(row, h);
                    }
                    y += row_h;
                }
                y - y0
            })
        }
    }

    /// Coloured legend entries, wrapping; returns the height.
    fn legend(&mut self, x: f32, y: f32, w: f32, items: &[(String, Color)]) -> f32 {
        let sh = self.lh_of(Font::Small);
        let d = self.u * 1.6;
        let mut cx = x;
        let mut cy = y;
        for (label, color) in items {
            let iw = d + self.u * 1.5 + self.m.width(label, Font::Small) + self.u * 4.0;
            if cx + iw > x + w && cx > x {
                cx = x;
                cy += sh + self.u;
            }
            self.fill(
                Rect::new(cx, cy + (sh - d) / 2.0, d, d),
                *color,
                self.u * 0.4,
            );
            self.text(cx + d + self.u * 1.5, cy, label, Font::Small, self.t.muted);
            cx += iw;
        }
        cy + sh - y
    }

    /// A stacked bar chart in a card; returns its height.
    #[allow(clippy::too_many_arguments)]
    fn chart_card(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        title: &str,
        subtitle: &str,
        columns: &[Column],
        legend: &[(String, Color)],
        money: bool,
    ) -> f32 {
        self.boxed(x, y, w, &|l: &mut Self, x, y0, w| {
            let mut y = y0;
            let tw = l.text(x, y, title, Font::Strong, l.t.text);
            let sx = x + tw + l.u * 2.0;
            l.text_fit(
                sx,
                y + (l.lh_of(Font::Strong) - l.lh_of(Font::Small)),
                subtitle,
                Font::Small,
                l.t.faint,
                (x + w - sx).max(0.0),
            );
            y += l.lh_of(Font::Strong) + l.u * 1.5;
            y += l.legend(x, y, w, legend) + l.u * 2.0;
            let sh = l.lh_of(Font::Small);
            let axis_w = l.m.width(if money { "$1.2k" } else { "1.2k" }, Font::Small) + l.u * 2.0;
            let label_w = l.m.width("Oct 24", Font::Small) + l.u * 2.0;
            let area = Rect::new(x, y + sh / 2.0, w, (l.lh * 7.0).round());
            let spec = BarChartSpec {
                columns,
                series: legend.len().max(1),
                gap: 0.3,
                label_every: 0,
                label_w,
                axis_w,
                label_h: sh + l.u,
            };
            let chart = bar_chart(area, &spec);
            for t in &chart.ticks {
                l.fill(
                    Rect::new(chart.plot.x, t.y, chart.plot.w, 1.0),
                    l.t.border.alpha(0.45),
                    0.0,
                );
                let label = if money {
                    compact_money(t.value)
                } else {
                    t.label.clone()
                };
                l.text_right(
                    chart.plot.x - l.u,
                    t.y - sh / 2.0,
                    &label,
                    Font::Small,
                    l.t.faint,
                );
            }
            for s in &chart.segments {
                let c = legend.get(s.series).map(|x| x.1).unwrap_or(l.t.muted);
                l.fill(s.rect, c, l.u * 0.3);
            }
            for lab in &chart.labels {
                let lw = l.m.width(&lab.text, Font::Small);
                l.text(
                    lab.x_center - lw / 2.0,
                    lab.y + l.u * 0.5,
                    &lab.text,
                    Font::Small,
                    l.t.faint,
                );
            }
            area.bottom() - y0
        })
    }

    // -- toast --------------------------------------------------------------

    fn toast(&mut self, area: Rect) {
        let text = match &self.md.toast {
            Some((text, _)) => text.clone(),
            None => return,
        };
        let max_w = (area.w - self.lh * 4.0).max(0.0);
        let label = ellipsize(self.m, &text, Font::Body, max_w - self.u * 10.0);
        let tw = self.m.width(&label, Font::Body);
        let h = self.lh_of(Font::Body) + self.u * 4.0;
        let w = tw + self.u * 10.0;
        let r = Rect::new(
            area.x + (area.w - w) / 2.0,
            area.bottom() - h - self.u * 6.0,
            w,
            h,
        );
        self.shadow(r, h / 2.0);
        let bg = self.t.text.mix(self.t.bg, 0.08);
        self.fill(r, bg, h / 2.0);
        let fg = self.t.bg;
        self.text(
            r.x + self.u * 5.0,
            r.y + self.u * 2.0,
            &label,
            Font::Body,
            fg,
        );
    }
}

/// How loudly a response clock should show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    /// Not running (met, not started) or paused.
    Quiet,
    Calm,
    Half,
    Late,
}

/// A clock's state in words, and how loud it is. Wall-clock share of the
/// time used, like towerd's Deadline items.
fn sla_status(c: &SlaClock, now: u64) -> (String, Tone) {
    if c.stopped {
        return if c.breached {
            ("breached".to_string(), Tone::Late)
        } else {
            ("not running".to_string(), Tone::Quiet)
        };
    }
    if c.paused {
        return ("paused".to_string(), Tone::Quiet);
    }
    if c.breached || (c.due_unix > 0 && now >= c.due_unix) {
        return ("breached".to_string(), Tone::Late);
    }
    if c.due_unix == 0 {
        return ("running".to_string(), Tone::Calm);
    }
    let left = c.due_unix - now;
    let span = c.due_unix.saturating_sub(c.started_unix).max(1);
    let used = now.saturating_sub(c.started_unix) as f64 / span as f64;
    let tone = if used >= 0.8 {
        Tone::Late
    } else if used >= 0.5 {
        Tone::Half
    } else {
        Tone::Calm
    };
    (format!("{} left", duration(left)), tone)
}

/// The running clock that breaches first (a breached one first of all).
fn most_urgent(clocks: &[SlaClock]) -> Option<&SlaClock> {
    clocks
        .iter()
        .filter(|c| !c.stopped && !c.paused && (c.breached || c.due_unix > 0))
        .min_by_key(|c| if c.breached { 0 } else { c.due_unix })
}

/// `Ana → Bo · High · vpn, urgent`.
fn tracker_line(i: &IssueInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    match (i.reporter.is_empty(), i.assignee.is_empty()) {
        (false, false) => parts.push(format!("{} → {}", i.reporter, i.assignee)),
        (false, true) => parts.push(format!("from {}", i.reporter)),
        (true, false) => parts.push(format!("assigned to {}", i.assignee)),
        (true, true) => {}
    }
    if !i.priority.is_empty() {
        parts.push(i.priority.clone());
    }
    if !i.labels.is_empty() {
        parts.push(i.labels.join(", "));
    }
    parts.join(" · ")
}

/// What a `[policy]` action does, for the Policy view.
fn action_help(action: &str) -> &'static str {
    match action {
        "intake.launch" => "Start an agent on a new issue from a watch",
        "intake.triage" => "Ask the triage role which role a new issue needs",
        "ack.post" => "Post the \"received\" note on a new issue",
        "issue.comment" => "Comment on the issue when its flight lands",
        "issue.transition" => "Move the issue to its done status with that comment",
        "pr.draft" => "Push the branch and open a draft PR/MR after an auto-fix",
        "land" => "Land a flight whose gates passed",
        "handoff.release" => "Start the next role on a handoff an agent made",
        "handoff.local" => {
            "Hand an agent's subtask to a local model and give it the result (auto needs no Armed: nothing leaves your network)"
        }
        "scout.run" => "Run a scout that reads mail or Teams for new items",
        "pane.message" => "Send a message Tower wrote into a running agent",
        "schedule.run" => {
            "Start a flight from a schedule (auto needs no Armed: saving it was your OK)"
        }
        _ => "",
    }
}

/// What starts flights, in chart order, with the label shown.
const ORIGINS: [(&str, &str); 6] = [
    ("you", "You"),
    ("schedule", "Schedule"),
    ("intake", "Intake"),
    ("autofix", "Auto-fix"),
    ("scout", "Scout"),
    ("handoff", "Handoff"),
];

fn count(map: &std::collections::BTreeMap<String, u32>, key: &str) -> f64 {
    map.get(key).copied().unwrap_or(0) as f64
}

/// `2026-10-24` -> `Oct 24`.
fn short_day(day: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut parts = day.split('-').skip(1);
    let month = parts.next().and_then(|m| m.parse::<usize>().ok());
    let d = parts.next().and_then(|d| d.parse::<u32>().ok());
    match (month, d) {
        (Some(m), Some(d)) if (1..=12).contains(&m) => format!("{} {}", MONTHS[m - 1], d),
        _ => day.to_string(),
    }
}

/// A policy key as a short chart label.
fn action_short(key: &str) -> &str {
    match key {
        "intake.launch" => "Launch",
        "intake.triage" => "Triage",
        "ack.post" => "Ack",
        "issue.comment" => "Comment",
        "issue.transition" => "Transition",
        "pr.draft" => "Draft PR",
        "land" => "Land",
        "handoff.release" => "Release",
        "scout.run" => "Scout",
        "pane.message" => "Message",
        "schedule.run" => "Schedule",
        other => other,
    }
}

/// `7h 52m`, `12m`, `40s`.
fn duration(secs: u64) -> String {
    match secs {
        s if s >= 3600 => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
        s if s >= 60 => format!("{}m", s / 60),
        s => format!("{}s", s),
    }
}

#[cfg(test)]
mod tests {
    use super::super::forms::Sheet;
    use super::super::model::Model;
    use super::super::proto::{demo_snapshot, Notice, PolicyRow, TermsNote};
    use super::super::text::fake::FakeMeasure;
    use super::*;

    fn theme() -> Theme {
        Theme::from_palette(
            Color::from_u8([0x1a, 0x1b, 0x26]),
            Color::from_u8([0xc0, 0xca, 0xf5]),
        )
    }

    const AREA: Rect = Rect::new(100.0, 50.0, 1600.0, 900.0);

    fn scene_for(model: &Model) -> Scene {
        layout(model, &theme(), &mut FakeMeasure, AREA, 1_000_000)
    }

    fn has(scene: &Scene, hit: &Hit) -> bool {
        scene.hits.iter().any(|(_, h)| h == hit)
    }

    /// Tall enough that every Setup section (and the whole inbox strip) is
    /// on screen: hits are clipped, so a short pane hides the lower ones.
    const TALL: Rect = Rect::new(0.0, 0.0, 1600.0, 20000.0);

    fn scene_tall(model: &Model) -> Scene {
        layout(model, &theme(), &mut FakeMeasure, TALL, 1_000_000)
    }

    fn with_extras(model: &mut Model, on: bool) {
        model.snap.extras = super::super::proto::Extras {
            scouts: on,
            machine_routines: on,
            activity: on,
            frequencies: on,
        };
    }

    #[test]
    fn every_view_lays_out_inside_the_area() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        for on in [false, true].iter() {
            with_extras(&mut model, *on);
            for view in model.views() {
                model.view = view;
                let scene = scene_for(&model);
                assert!(!scene.cmds.is_empty(), "{:?} draws something", view);
                for (r, h) in &scene.hits {
                    assert!(
                        r.x >= AREA.x - 0.5
                            && r.right() <= AREA.right() + 0.5
                            && r.y >= AREA.y - 0.5
                            && r.bottom() <= AREA.bottom() + 0.5,
                        "{:?}: hit {:?} at {:?} leaves the pane",
                        view,
                        h,
                        r
                    );
                }
            }
        }
    }

    #[test]
    fn board_has_a_card_per_flight_and_nav_for_each_view() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        let scene = scene_for(&model);
        for f in &model.snap.flights {
            assert!(
                has(&scene, &Hit::Card(f.id.clone())),
                "card for {} is clickable",
                f.key
            );
        }
        for view in View::ALL.iter() {
            assert!(has(&scene, &Hit::Nav(*view)));
        }
        assert_eq!(
            scene
                .hits
                .iter()
                .filter(|(_, h)| matches!(h, Hit::Nav(_)))
                .count(),
            3
        );
        assert!(!has(&scene, &Hit::Nav(View::Team)));
        assert!(has(&scene, &Hit::Button(Action::NewFlight)));
        model.snap.extras.frequencies = true;
        assert!(has(&scene_for(&model), &Hit::Nav(View::Team)));
    }

    #[test]
    fn board_draws_the_inbox_strip_above_the_columns() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        let scene = scene_tall(&model);
        let strip_y = scene
            .hits
            .iter()
            .filter_map(|(r, h)| matches!(h, Hit::InboxRow(_)).then_some(r.y))
            .fold(f32::MAX, f32::min);
        let card_y = scene
            .hits
            .iter()
            .filter_map(|(r, h)| matches!(h, Hit::Card(_)).then_some(r.y))
            .fold(f32::MAX, f32::min);
        assert!(strip_y < card_y, "the strip sits above the flights");
        assert!(texts(&scene)
            .iter()
            .any(|t| t.starts_with("WAITING ON YOU · ")));
        // Many items: the strip caps at 40% of the height and scrolls.
        let item = model.snap.inbox[0].clone();
        for i in 0..20 {
            let mut it = item.clone();
            it.id = format!("i-x{}", i);
            model.snap.inbox.push(it);
        }
        let scene = scene_for(&model);
        let strip = scene
            .scrolls
            .iter()
            .find(|s| s.id == ScrollId::Inbox)
            .expect("a strip region");
        assert!(strip.viewport.h <= AREA.h * 0.4 + 0.5);
        assert!(strip.max_offset() > 0.0);
        // Nothing waiting: no strip at all.
        model.snap.inbox.clear();
        let scene = scene_for(&model);
        assert!(!scene.scrolls.iter().any(|s| s.id == ScrollId::Inbox));
        assert!(!texts(&scene)
            .iter()
            .any(|t| t.starts_with("WAITING ON YOU")));
        // No flights: the strip still shows, and so does New flight.
        model.snap.inbox.push(item);
        model.snap.flights.clear();
        let scene = scene_tall(&model);
        assert!(scene.scrolls.iter().any(|s| s.id == ScrollId::Inbox));
        assert!(has(&scene, &Hit::Button(Action::NewFlight)));
    }

    #[test]
    fn flight_drawer_offers_real_actions() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        model.drawer = Some(Drawer::Flight("f-13".into()));
        let scene = scene_for(&model);
        assert!(has(&scene, &Hit::CloseDrawer));
        assert!(has(
            &scene,
            &Hit::Button(Action::Release {
                flight_id: "f-13".into(),
                handoff_id: "h-33".into()
            })
        ));
        assert!(has(
            &scene,
            &Hit::Button(Action::LaunchFlight {
                flight_id: "f-13".into(),
                role_id: "ground".into()
            })
        ));
        assert!(has(
            &scene,
            &Hit::Button(Action::OpenPacket {
                flight_id: "f-13".into()
            })
        ));
        // An unassigned flight offers every role.
        model.drawer = Some(Drawer::Flight("f-16".into()));
        let scene = scene_for(&model);
        for r in &model.snap.roles {
            assert!(has(
                &scene,
                &Hit::Button(Action::LaunchFlight {
                    flight_id: "f-16".into(),
                    role_id: r.id.clone()
                })
            ));
        }
    }

    #[test]
    fn agent_drawer_has_transcript_compose_and_stop() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        model.drawer = Some(Drawer::Agent("ground-1".into()));
        let scene = scene_for(&model);
        assert!(has(&scene, &Hit::Field(FieldId::Compose)));
        assert!(has(&scene, &Hit::Button(Action::SendCompose)));
        assert!(has(
            &scene,
            &Hit::Button(Action::StopAgent {
                agent_id: "ground-1".into()
            })
        ));
        assert!(scene
            .cmds
            .iter()
            .any(|c| matches!(c, Cmd::Text { text, .. } if text.contains("Ported 14 tests"))));
        assert!(scene
            .scrolls
            .iter()
            .any(|s| s.id == ScrollId::Drawer && s.from_bottom));
        // A pane agent offers its pane instead.
        model.drawer = Some(Drawer::Agent("approach-1".into()));
        let scene = scene_for(&model);
        assert!(has(&scene, &Hit::Button(Action::FocusPane { pane_id: 7 })));
    }

    #[test]
    fn sheets_render_fields_pickers_and_submit() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        model.sheet = Some(Sheet::new_flight(Some("approach".into()), Place::NewFolder));
        model.focus = Some(FieldId::Title);
        let scene = scene_for(&model);
        assert!(has(&scene, &Hit::Field(FieldId::Title)));
        assert!(has(&scene, &Hit::Field(FieldId::Brief)));
        assert!(has(
            &scene,
            &Hit::Button(Action::SheetRole(Some("ground".into())))
        ));
        assert!(has(&scene, &Hit::Button(Action::Submit { launch: true })));
        assert!(has(
            &scene,
            &Hit::Button(Action::SheetRole(Some(AUTO_ROLE.into())))
        ));
        assert!(has(
            &scene,
            &Hit::Button(Action::SheetPlace(Place::NewFolder))
        ));
        assert!(has(
            &scene,
            &Hit::Button(Action::SheetPlace(Place::Worktree))
        ));
        // The sheet covers the board: cards are not clickable under it.
        let top = scene
            .hits
            .iter()
            .rposition(|(_, h)| *h == Hit::SheetScrim)
            .unwrap();
        assert!(scene.hits[..top]
            .iter()
            .any(|(_, h)| matches!(h, Hit::Card(_))));
        assert!(!scene.hits[top..]
            .iter()
            .any(|(_, h)| matches!(h, Hit::Card(_))));

        model.sheet = Some(Sheet::packet("f-12", "TWR-12", "Split it"));
        let scene = scene_for(&model);
        assert!(has(&scene, &Hit::Field(FieldId::Acceptance)));
        assert!(
            !has(
                &scene,
                &Hit::Button(Action::SheetRole(Some("approach".into())))
            ),
            "no handing to yourself"
        );
    }

    #[test]
    fn inbox_buttons_follow_item_options() {
        let mut snap = demo_snapshot(1_000_000);
        snap.inbox[0].options = vec![
            super::super::proto::InboxOption {
                id: "allow".into(),
                label: "Allow once".into(),
                primary: true,
            },
            super::super::proto::InboxOption {
                id: "reject".into(),
                label: "Reject".into(),
                primary: false,
            },
        ];
        snap.inbox.push(InboxItem {
            id: "i-9".into(),
            kind: InboxKind::Question,
            title: "Ground asks".into(),
            detail: "Which fixture?".into(),
            answerable: true,
            ..Default::default()
        });
        let model = Model::new(snap);
        let scene = scene_tall(&model);
        assert!(has(
            &scene,
            &Hit::Button(Action::Resolve {
                item_id: "i-5".into(),
                approve: true,
                option_id: Some("allow".into())
            })
        ));
        assert!(has(
            &scene,
            &Hit::Button(Action::Answer {
                item_id: "i-9".into()
            })
        ));
    }

    #[test]
    fn narrow_panes_scroll_the_board_horizontally() {
        let model = Model::new(demo_snapshot(1_000_000));
        let narrow = Rect::new(0.0, 0.0, 700.0, 500.0);
        let scene = layout(&model, &theme(), &mut FakeMeasure, narrow, 1_000_000);
        let board = scene
            .scrolls
            .iter()
            .find(|s| s.id == ScrollId::BoardX)
            .unwrap();
        assert!(board.max_offset() > 0.0);
        for (r, _) in &scene.hits {
            assert!(
                r.right() <= narrow.right() + 0.5,
                "hits are clipped to the pane"
            );
        }
    }

    #[test]
    fn empty_states_explain_and_offer_the_next_step() {
        let mut snap = demo_snapshot(1_000_000);
        snap.flights.clear();
        snap.inbox.clear();
        let mut model = Model::new(snap);
        let scene = scene_for(&model);
        assert!(texts(&scene).iter().any(|t| t == "No flights yet"));
        assert!(has(&scene, &Hit::Button(Action::NewFlight)));

        // A first run, step by step: no CLI, then a terms note, then no
        // roles, then a role that cannot run.
        model.conn = Conn::Live { pid: 1 };
        model.snap.roles.clear();
        model.snap.setup.roles_usable = 0;
        model.snap.setup.installed_clis.clear();
        let scene = scene_for(&model);
        assert!(texts(&scene).iter().any(|t| t == "No agent CLI found"));
        assert!(has(
            &scene,
            &Hit::Button(Action::OpenTerminal { cwd: None })
        ));
        model.snap.setup.installed_clis = vec!["claude".into()];
        model.snap.setup.terms_pending = vec![TermsNote {
            id: "claude-agent-sdk".into(),
            vendor: "Claude".into(),
            title: "Claude: vendor terms note".into(),
            text: "words".into(),
        }];
        let scene = scene_for(&model);
        assert!(texts(&scene)
            .iter()
            .any(|t| t == "One note before agents start"));
        assert!(has(&scene, &Hit::Nav(View::Setup)));
        model.snap.setup.terms_pending.clear();
        let scene = scene_for(&model);
        assert!(texts(&scene).iter().any(|t| t == "No roles yet"));
        assert!(has(&scene, &Hit::Button(Action::NewRole)));
        let mut role = demo_snapshot(1_000_000).roles[0].clone();
        role.warning = Some("opencode is not installed".into());
        role.missing = true;
        model.snap.roles = vec![role];
        let scene = scene_for(&model);
        assert!(texts(&scene)
            .iter()
            .any(|t| t == "Nothing can take off yet"));
        assert!(texts(&scene)
            .iter()
            .any(|t| t.contains("opencode is not installed")));

        // Setup's own empty states each offer one action.
        let mut model = Model::new(demo_snapshot(1_000_000));
        model.view = View::Setup;
        model.snap.endpoints.clear();
        model.snap.models.clear();
        model.snap.sources.clear();
        model.snap.config_dir = "/cfg".into();
        let scene = scene_tall(&model);
        let t = texts(&scene);
        assert!(t.iter().any(|t| t == "No model servers"));
        assert!(has(&scene, &Hit::Button(Action::NewEndpoint)));
        assert!(
            !has(&scene, &Hit::Button(Action::NewModel(None))),
            "no server to add it to"
        );
        assert!(t.iter().any(|t| t == "No intake yet"));
        assert!(has(
            &scene,
            &Hit::Button(Action::EditFile {
                path: "/cfg/sources.toml".into()
            })
        ));
        assert!(!has(&scene, &Hit::Button(Action::PollSources)));
        model.snap.roles.clear();
        model.snap.setup.roles_usable = 0;
        let scene = scene_tall(&model);
        assert!(texts(&scene).iter().any(|t| t == "No roles yet"));
        assert!(has(&scene, &Hit::Button(Action::NewRole)));

        let model = Model::new(super::super::model::empty_snapshot());
        let scene = scene_for(&model);
        assert!(texts(&scene).iter().any(|t| t.starts_with("Connecting")));
    }

    fn texts(scene: &Scene) -> Vec<String> {
        scene
            .cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn setup_lists_schedules_and_machine_jobs_when_on() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        model.view = View::Setup;
        // Machine jobs are an extra: hidden until switched on.
        let t = texts(&scene_tall(&model));
        assert!(!t.iter().any(|s| s.contains("sla-checker")));
        assert!(!has(
            &scene_tall(&model),
            &Hit::Button(Action::ToggleOtherRoutines)
        ));
        model.snap.extras.machine_routines = true;
        let scene = scene_tall(&model);
        assert!(has(
            &scene,
            &Hit::Button(Action::RunSchedule("morning-prs".into()))
        ));
        assert!(has(
            &scene,
            &Hit::Button(Action::EditSchedule("morning-prs".into()))
        ));
        // Paused by Tower: the button resumes it.
        assert!(has(
            &scene,
            &Hit::Button(Action::SetScheduleEnabled {
                id: "deps-audit".into(),
                enabled: true,
            })
        ));
        let t = texts(&scene);
        assert!(t.iter().any(|s| s.contains("sla-checker")));
        assert!(t.iter().any(|s| s.contains("Paused: 3 failed runs")));
        // Vendor jobs stay folded away until asked for.
        assert!(!t.iter().any(|s| s.contains("com.vendor.updater")));
        assert!(has(&scene, &Hit::Button(Action::ToggleOtherRoutines)));
        model.show_other_routines = true;
        let t = texts(&scene_tall(&model));
        assert!(t.iter().any(|s| s.contains("com.vendor.updater")));
    }

    #[test]
    fn models_view_describes_models_and_edits_servers() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        model.view = View::Setup;
        let scene = scene_tall(&model);
        let t = texts(&scene);
        assert!(t.iter().any(|s| s == "Qwen 3.6 27B"));
        assert!(t.iter().any(|s| s == "code edits"), "strengths are chips");
        assert!(t.iter().any(|s| s == "local"));
        assert!(has(
            &scene,
            &Hit::Button(Action::EditModel("localhost/qwen3.6:27b".into()))
        ));
        assert!(has(
            &scene,
            &Hit::Button(Action::EditEndpoint("localhost".into()))
        ));
        assert!(has(&scene, &Hit::Button(Action::NewEndpoint)));
        assert!(has(&scene, &Hit::Button(Action::NewModel(None))));
        // The demo has Claude connected: the button offers to disconnect.
        assert!(has(&scene, &Hit::Button(Action::OpenConnect(false))));
        model.snap.connected_clis.clear();
        assert!(has(
            &scene_tall(&model),
            &Hit::Button(Action::OpenConnect(true))
        ));
        model.sheet = Some(Sheet::Connect(true));
        let t = texts(&scene_for(&model));
        assert!(t.iter().any(|s| s == "Connect your agents"));
        model.sheet = None;

        // A served model nobody described yet can be added from its chip.
        model.snap.models.clear();
        let scene = scene_tall(&model);
        assert!(has(
            &scene,
            &Hit::Button(Action::NewModel(Some("localhost/qwen3.6:27b".into())))
        ));
    }

    #[test]
    fn roles_routing_and_policy_are_edited_in_place() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        model.view = View::Setup;
        let scene = scene_tall(&model);
        assert!(has(&scene, &Hit::Button(Action::NewRole)));
        assert!(has(
            &scene,
            &Hit::Button(Action::EditRole("approach".into()))
        ));
        model.sheet = Some(Sheet::role(None));
        let scene = scene_for(&model);
        assert!(has(
            &scene,
            &Hit::Button(Action::SheetDriver("routed".into()))
        ));
        assert!(has(
            &scene,
            &Hit::Button(Action::SheetColor("#73daca".into()))
        ));
        assert!(has(
            &scene,
            &Hit::Button(Action::RoleAddModel("localhost/qwen3.6:27b".into()))
        ));
        model.sheet = None;

        assert!(has(&scene_tall(&model), &Hit::Button(Action::EditRouting)));
        model.sheet = Some(Sheet::routing(&[], &[], &[]));
        assert!(has(&scene_for(&model), &Hit::Field(FieldId::RouteHigh)));
        model.sheet = None;

        model.snap.policy = vec![super::super::proto::PolicyRow {
            action: "handoff.local".into(),
            level: "auto".into(),
            configured: false,
        }];
        let scene = scene_tall(&model);
        assert!(has(
            &scene,
            &Hit::Button(Action::SetPolicyLevel {
                action: "handoff.local".into(),
                level: "confirm".into()
            })
        ));
        assert!(has(&scene, &Hit::Button(Action::OpenLocalOnly)));
        model.sheet = Some(Sheet::local_only(&model.snap.local_only_paths.clone()));
        assert!(texts(&scene_for(&model))
            .iter()
            .any(|s| s == "Local-only folders"));
    }

    #[test]
    fn local_only_flights_wear_a_badge() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        assert!(!texts(&scene_for(&model)).iter().any(|s| s == "local only"));
        model.snap.flights[0].local_only = true;
        assert!(texts(&scene_for(&model)).iter().any(|s| s == "local only"));
        let id = model.snap.flights[0].id.clone();
        model.drawer = Some(Drawer::Flight(id));
        let n = texts(&scene_for(&model))
            .iter()
            .filter(|s| *s == "local only")
            .count();
        assert!(n >= 2, "card and drawer");
        model.drawer = None;
        model.sheet = Some(Sheet::new_flight(None, Place::NewFolder));
        assert!(has(
            &scene_for(&model),
            &Hit::Button(Action::SheetFlag(SheetFlag::LocalOnly))
        ));
    }

    #[test]
    fn server_and_model_sheets_paint_their_pickers() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        model.sheet = Some(Sheet::endpoint(None));
        let scene = scene_for(&model);
        assert!(has(&scene, &Hit::Button(Action::SheetKind("vllm".into()))));
        assert!(has(&scene, &Hit::Button(Action::SheetLocal(Some(true)))));
        assert!(has(&scene, &Hit::Field(FieldId::EpKeyEnv)));
        assert!(!has(
            &scene,
            &Hit::Button(Action::DeleteEndpoint("localhost".into()))
        ));
        let ep = model.snap.endpoints[0].clone();
        model.sheet = Some(Sheet::endpoint(Some(&ep)));
        let scene = scene_for(&model);
        assert!(has(
            &scene,
            &Hit::Button(Action::DeleteEndpoint(ep.name.clone()))
        ));

        let m = model.snap.models[0].clone();
        model.sheet = Some(Sheet::model(Some(&m), None));
        let scene = scene_for(&model);
        assert!(has(
            &scene,
            &Hit::Button(Action::SheetEndpoint("localhost".into()))
        ));
        assert!(has(&scene, &Hit::Field(FieldId::ModelGoodFor)));
        assert!(has(&scene, &Hit::Button(Action::DeleteModel(m.id.clone()))));
        let t = texts(&scene);
        assert!(t.iter().any(|s| s.starts_with("It serves:")));
    }

    #[test]
    fn history_charts_follow_the_activity_extra() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        model.view = View::History;
        // Off (the default): only what ran by itself, no charts, no day buttons.
        let scene = scene_for(&model);
        let t = texts(&scene);
        assert!(!t.iter().any(|s| s == "Flights started"));
        assert!(!has(&scene, &Hit::Button(Action::ActivityDays(30))));
        assert!(t.iter().any(|s| s == "Ran by itself, latest"));
        assert!(has(
            &scene,
            &Hit::Button(Action::OpenFlight {
                flight_id: "f-17".into()
            })
        ));
        // On: tiles and charts above the list.
        model.snap.extras.activity = true;
        let scene = scene_for(&model);
        let t = texts(&scene);
        for want in &[
            "Ran by itself",
            "Flights started",
            "How runs ended",
            "Spend",
        ] {
            assert!(t.iter().any(|s| s == want), "{} is drawn", want);
        }
        assert!(has(&scene, &Hit::Button(Action::ActivityDays(30))));
        // The latest automatic event (at the bottom) opens its flight.
        model.scroll.insert(ScrollId::History, 1.0e6);
        let bottom = scene_for(&model);
        assert!(has(
            &bottom,
            &Hit::Button(Action::OpenFlight {
                flight_id: "f-17".into()
            })
        ));
        // An empty history explains itself instead of drawing empty charts.
        model.snap.activity = Default::default();
        model.scroll.remove(&ScrollId::History);
        let t = texts(&scene_for(&model));
        assert!(t.iter().any(|s| s == "Nothing yet"));
        model.snap.extras.activity = false;
        let t = texts(&scene_for(&model));
        assert!(t.iter().any(|s| s.starts_with("Nothing ran by itself yet")));
    }

    #[test]
    fn schedule_sheet_offers_templates_and_previews_the_time() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        model.sheet = Some(Sheet::schedule(None, None));
        let scene = scene_for(&model);
        assert!(has(&scene, &Hit::Button(Action::ScheduleTemplate(0))));
        // The sheet scrolls; Save is at its bottom.
        model.scroll.insert(ScrollId::Sheet, 1.0e6);
        let bottom = scene_for(&model);
        assert!(has(&bottom, &Hit::Button(Action::Submit { launch: false })));
        assert!(has(
            &bottom,
            &Hit::Button(Action::SheetFlag(SheetFlag::LocalOnly))
        ));
        model.scroll.remove(&ScrollId::Sheet);
        if let Some(Sheet::Schedule(f)) = &mut model.sheet {
            f.apply_template(0);
        }
        let t = texts(&scene_for(&model));
        assert!(t.iter().any(|s| s.starts_with("weekdays 08:30 · next")));
    }

    #[test]
    fn days_and_actions_read_short() {
        assert_eq!(short_day("2026-10-24"), "Oct 24");
        assert_eq!(short_day("junk"), "junk");
        assert_eq!(action_short("schedule.run"), "Schedule");
    }

    #[test]
    fn policy_view_lists_levels_and_arms() {
        let mut snap = demo_snapshot(1_000_000);
        snap.config_dir = "/cfg".into();
        snap.policy = vec![
            PolicyRow {
                action: "intake.launch".into(),
                level: "auto".into(),
                configured: true,
            },
            PolicyRow {
                action: "land".into(),
                level: "confirm".into(),
                configured: false,
            },
        ];
        let mut model = Model::new(snap);
        model.view = View::Setup;
        let scene = scene_tall(&model);
        assert!(has(&scene, &Hit::Button(Action::SetArmed(true))));
        // Levels are chips you click; no file to edit.
        assert!(has(
            &scene,
            &Hit::Button(Action::SetPolicyLevel {
                action: "land".into(),
                level: "off".into()
            })
        ));
        let t = texts(&scene);
        assert!(t.iter().any(|x| x == "intake.launch"));
        assert!(t.iter().any(|x| x.ends_with("· set here")));
        assert!(t.iter().any(|x| x.starts_with("Disarmed")));
        assert!(
            !t.iter().any(|x| x == "Armed"),
            "no Armed badge while disarmed"
        );

        model.snap.armed_until = Some(1_000_000 + 2 * 3600 + 5 * 60);
        let scene = scene_tall(&model);
        assert!(has(&scene, &Hit::Button(Action::SetArmed(false))));
        let t = texts(&scene);
        assert!(t.iter().any(|x| x == "Armed"), "rail shows Armed");
        assert!(t.iter().any(|x| x == "2h 05m left"));

        // An expired timestamp reads as disarmed.
        model.snap.armed_until = Some(999_999);
        let scene = scene_tall(&model);
        assert!(has(&scene, &Hit::Button(Action::SetArmed(true))));
    }

    fn clock(start: u64, due: u64) -> SlaClock {
        SlaClock {
            id: "9".into(),
            name: "First response".into(),
            source: "jsm".into(),
            started_unix: start,
            due_unix: due,
            goal_secs: due - start,
            ..Default::default()
        }
    }

    #[test]
    fn sla_status_reads_and_colours_by_time_used() {
        let c = clock(0, 1000);
        assert_eq!(sla_status(&c, 100), ("15m left".to_string(), Tone::Calm));
        assert_eq!(sla_status(&c, 600).1, Tone::Half);
        assert_eq!(sla_status(&c, 900).1, Tone::Late);
        assert_eq!(sla_status(&c, 1000), ("breached".to_string(), Tone::Late));
        let mut p = c.clone();
        p.paused = true;
        assert_eq!(sla_status(&p, 900), ("paused".to_string(), Tone::Quiet));
        let mut done = c.clone();
        done.stopped = true;
        assert_eq!(sla_status(&done, 5000).1, Tone::Quiet);
        done.breached = true;
        assert_eq!(sla_status(&done, 5000).0, "breached");
    }

    #[test]
    fn the_most_urgent_running_clock_wins() {
        let soon = clock(0, 100);
        let later = clock(0, 900);
        let mut stopped = clock(0, 10);
        stopped.stopped = true;
        let list = vec![later.clone(), stopped, soon.clone()];
        assert_eq!(most_urgent(&list), Some(&soon));
        assert_eq!(most_urgent(&[]), None);
    }

    #[test]
    fn tracker_line_skips_what_is_missing() {
        let full = IssueInfo {
            reporter: "Ana".into(),
            assignee: "Bo".into(),
            priority: "High".into(),
            labels: vec!["vpn".into()],
        };
        assert_eq!(tracker_line(&full), "Ana → Bo · High · vpn");
        assert_eq!(tracker_line(&IssueInfo::default()), "");
        let only = IssueInfo {
            reporter: "Ana".into(),
            ..Default::default()
        };
        assert_eq!(tracker_line(&only), "from Ana");
    }

    #[test]
    fn cards_show_the_countdown_instead_of_the_age() {
        let mut snap = demo_snapshot(1_000_000);
        let f = &mut snap.flights[0];
        f.sla = vec![clock(1_000_000 - 100, 1_000_000 + 3500)];
        f.issue = Some(IssueInfo {
            reporter: "Ana".into(),
            ..Default::default()
        });
        let id = f.id.clone();
        let mut model = Model::new(snap);
        let t = texts(&scene_for(&model));
        assert!(t.iter().any(|x| x == "58m left"), "{t:?}");
        model.drawer = Some(Drawer::Flight(id));
        let t = texts(&scene_for(&model));
        assert!(t.iter().any(|x| x == "TRACKER"));
        assert!(t.iter().any(|x| x == "from Ana"));
        assert!(t.iter().any(|x| x == "First response · 58m left"));
    }

    #[test]
    fn scouts_get_a_run_button_until_they_run() {
        let mut snap = demo_snapshot(1_000_000);
        snap.sources[0].kind = "mail".into();
        snap.sources[0].name = "Support".into();
        snap.sources[0].polling = false;
        let mut model = Model::new(snap);
        model.view = View::Setup;
        let run = Hit::Button(Action::RunScout("Support".into()));
        // Scouts are an extra: off, the scout is not even listed.
        assert!(!has(&scene_tall(&model), &run));
        assert!(!texts(&scene_tall(&model)).iter().any(|t| t == "Mail scout"));
        model.snap.extras.scouts = true;
        assert!(has(&scene_tall(&model), &run));
        assert!(texts(&scene_tall(&model)).iter().any(|t| t == "Mail scout"));
        model.snap.sources[0].polling = true;
        assert!(
            !has(&scene_tall(&model), &run),
            "no second run while one is going"
        );
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(40), "40s");
        assert_eq!(duration(12 * 60 + 5), "12m");
        assert_eq!(duration(7 * 3600 + 52 * 60), "7h 52m");
    }

    #[test]
    fn intake_shows_watches_errors_and_actions() {
        let mut snap = demo_snapshot(1_000_000);
        snap.config_dir = "/cfg".into();
        snap.sources[0].error = Some("HTTP 401: not authorized".into());
        let mut model = Model::new(snap);
        model.view = View::Setup;
        let scene = scene_tall(&model);
        assert!(has(&scene, &Hit::Button(Action::PollSources)));
        assert!(has(
            &scene,
            &Hit::Button(Action::EditFile {
                path: "/cfg/sources.toml".into()
            })
        ));
        let texts: Vec<String> = scene
            .cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(texts.iter().any(|t| t == "Team backlog"));
        assert!(texts.iter().any(|t| t.contains("not authorized")));
        model.snap.sources.clear();
        let scene = scene_tall(&model);
        assert!(scene
            .cmds
            .iter()
            .any(|c| matches!(c, Cmd::Text { text, .. } if text == "No intake yet")));
    }

    #[test]
    fn setup_has_every_section_and_opens_each_sheet() {
        let mut snap = demo_snapshot(1_000_000);
        snap.config_dir = "/cfg".into();
        let mut model = Model::new(snap);
        model.view = View::Setup;
        with_extras(&mut model, true);
        let scene = scene_tall(&model);
        let t = texts(&scene);
        for title in &[
            "Roles",
            "Models & servers",
            "Routing",
            "Your own sessions",
            "Policy",
            "Local-only folders",
            "Schedules",
            "Sources",
            "This machine",
            "Extras",
        ] {
            assert!(t.iter().any(|s| s == title), "{} section", title);
        }
        for hit in &[
            Hit::Button(Action::NewRole),
            Hit::Button(Action::EditRole("approach".into())),
            Hit::Button(Action::NewEndpoint),
            Hit::Button(Action::NewModel(None)),
            Hit::Button(Action::Probe),
            Hit::Button(Action::EditEndpoint("localhost".into())),
            Hit::Button(Action::EditRouting),
            Hit::Button(Action::OpenConnect(false)),
            Hit::Button(Action::OpenLocalOnly),
            Hit::Button(Action::NewSchedule),
            Hit::Button(Action::RunSchedule("morning-prs".into())),
            Hit::Button(Action::EditFile {
                path: "/cfg/sources.toml".into(),
            }),
            Hit::Button(Action::EditFile {
                path: "/cfg/schedules.toml".into(),
            }),
            Hit::Button(Action::PollSources),
            Hit::Button(Action::Refresh),
            Hit::Button(Action::SetArmed(true)),
        ] {
            assert!(has(&scene, hit), "{:?}", hit);
        }
        for e in Extra::ALL.iter() {
            assert!(
                has(&scene, &Hit::Button(Action::ToggleExtra(*e))),
                "{:?}",
                e
            );
        }
        // Setup is one vertical scroll region.
        assert!(scene.scrolls.iter().any(|s| s.id == ScrollId::Setup));
    }

    #[test]
    fn extras_off_hide_scouts_machine_jobs_and_team() {
        let mut snap = demo_snapshot(1_000_000);
        snap.sources[0].kind = "mail".into();
        snap.sources[0].name = "Support".into();
        let mut model = Model::new(snap);
        model.view = View::Setup;
        let scene = scene_tall(&model);
        let t = texts(&scene);
        assert!(!t.iter().any(|s| s == "Mail scout"));
        assert!(!has(
            &scene,
            &Hit::Button(Action::RunScout("Support".into()))
        ));
        assert!(
            !t.iter().any(|s| s == "scout.run"),
            "no policy row for scouts"
        );
        assert!(!t.iter().any(|s| s == "This machine"));
        assert!(!has(&scene, &Hit::Button(Action::ToggleOtherRoutines)));
        assert!(!has(&scene, &Hit::Nav(View::Team)));
        with_extras(&mut model, true);
        model.snap.policy.push(PolicyRow {
            action: "scout.run".into(),
            level: "off".into(),
            configured: false,
        });
        let scene = scene_tall(&model);
        let t = texts(&scene);
        assert!(t.iter().any(|s| s == "Mail scout"));
        assert!(t.iter().any(|s| s == "scout.run"));
        assert!(t.iter().any(|s| s == "This machine"));
        assert!(has(&scene, &Hit::Nav(View::Team)));
    }

    #[test]
    fn terms_card_accepts_defers_and_shows_on_the_board() {
        let mut snap = demo_snapshot(1_000_000);
        snap.setup.terms_pending = vec![TermsNote {
            id: "claude-agent-sdk".into(),
            vendor: "Claude".into(),
            title: "Claude: vendor terms note".into(),
            text: "Unless previously approved, Anthropic does not allow …".into(),
        }];
        let mut model = Model::new(snap);
        // The Board points at Setup.
        let scene = scene_for(&model);
        assert!(texts(&scene)
            .iter()
            .any(|t| t.contains("needs your OK before Claude agents start")));
        assert!(has(&scene, &Hit::Nav(View::Setup)));
        // Setup has the card with the verbatim text and both buttons.
        model.view = View::Setup;
        let scene = scene_tall(&model);
        let t = texts(&scene);
        assert!(t.iter().any(|s| s == "Before agents start"));
        assert!(t.iter().any(|s| s.contains("Unless previously approved")));
        assert!(has(
            &scene,
            &Hit::Button(Action::AcceptTerms("claude-agent-sdk".into()))
        ));
        assert!(has(
            &scene,
            &Hit::Button(Action::DeferTerms {
                id: "claude-agent-sdk".into(),
                defer: true
            })
        ));
        // Not now: folded to one line with Review; the Board stops nagging.
        model.terms_deferred.push("claude-agent-sdk".into());
        let scene = scene_tall(&model);
        assert!(!has(
            &scene,
            &Hit::Button(Action::AcceptTerms("claude-agent-sdk".into()))
        ));
        assert!(has(
            &scene,
            &Hit::Button(Action::DeferTerms {
                id: "claude-agent-sdk".into(),
                defer: false
            })
        ));
        assert!(texts(&scene)
            .iter()
            .any(|s| s.ends_with("not accepted yet")));
        model.view = View::Board;
        assert!(!texts(&scene_for(&model))
            .iter()
            .any(|t| t.contains("needs your OK")));
    }

    #[test]
    fn notices_show_in_setup_and_errors_on_the_board() {
        let mut snap = demo_snapshot(1_000_000);
        snap.notices = vec![
            Notice {
                level: NoticeLevel::Error,
                text: "Config: tower.toml: expected a table".into(),
                at_unix: 999_000,
            },
            Notice {
                level: NoticeLevel::Info,
                text: "Armed: actions set to auto run on their own".into(),
                at_unix: 999_500,
            },
        ];
        let mut model = Model::new(snap);
        let scene = scene_for(&model);
        let t = texts(&scene);
        assert!(t.iter().any(|s| s.starts_with("Config: tower.toml")));
        assert!(
            !t.iter().any(|s| s.starts_with("Armed: actions")),
            "info stays in Setup"
        );
        assert!(has(&scene, &Hit::Nav(View::Setup)));
        model.view = View::Setup;
        let t = texts(&scene_tall(&model));
        assert!(t.iter().any(|s| s == "Notices"));
        assert!(t.iter().any(|s| s.starts_with("Config: tower.toml")));
        assert!(t.iter().any(|s| s.starts_with("Armed: actions")));
    }

    #[test]
    fn missing_programs_read_as_not_installed() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        model.view = View::Setup;
        let e = &mut model.snap.engines[0];
        e.available = false;
        e.missing = true;
        e.why = Some("npx is not installed (needs Node.js)".into());
        let scene = scene_tall(&model);
        let faint = theme().faint;
        assert!(
            scene.cmds.iter().any(|c| matches!(
                c,
                Cmd::Text { text, color, .. }
                    if text == "npx is not installed (needs Node.js)" && *color == faint
            )),
            "a missing program is a fact, not a warning"
        );
        // A blocked engine whose program is there stays a warning.
        let e = &mut model.snap.engines[0];
        e.missing = false;
        e.why = Some("$K is not set".into());
        let scene = scene_tall(&model);
        assert!(scene.cmds.iter().any(|c| matches!(
            c,
            Cmd::Text { text, color, .. } if text == "$K is not set" && *color == theme().warn
        )));
        // Role warnings live on the role cards now, not as a wall of banners.
        let t = texts(&scene);
        assert!(!t.iter().any(|s| s.starts_with("Ground: ")));
    }

    #[test]
    fn routing_shows_on_drawers_sheets_and_models() {
        let texts = |scene: &Scene| -> Vec<String> {
            scene
                .cmds
                .iter()
                .filter_map(|c| match c {
                    Cmd::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect()
        };
        let mut m = Model::new(demo_snapshot(1_000_000));
        m.view = View::Setup;
        let t = texts(&scene_tall(&m));
        assert!(t.iter().any(|t| t == "Routing"), "{:?}", t);
        assert!(t.iter().any(|t| t == "ollama pull qwen3.6:27b"));
        m.view = View::Board;
        m.drawer = Some(Drawer::Flight("f-13".into()));
        if let Some(f) = m.snap.flights.iter_mut().find(|f| f.id == "f-13") {
            f.kind = Some("design".into());
            f.outcome = Some("proposed".into());
            f.artifacts = vec!["/tmp/x/DESIGN.md".into()];
        }
        let scene = scene_for(&m);
        assert!(has(
            &scene,
            &Hit::Button(Action::OpenPath {
                path: "/tmp/x/DESIGN.md".into()
            })
        ));
        let t = texts(&scene);
        assert!(t.iter().any(|t| t == "DESIGN.md") && t.iter().any(|t| t == "proposed"));
        assert!(t.iter().any(|t| t == "ROUTE"));
        assert!(t
            .iter()
            .any(|t| t.contains("would pick medium:claude-sonnet")));
        m.drawer = None;
        m.sheet = Some(Sheet::new_flight(None, Place::Here));
        let scene = scene_for(&m);
        for tier in [None, Some("low"), Some("medium"), Some("high")].iter() {
            assert!(has(
                &scene,
                &Hit::Button(Action::SheetTier(tier.map(str::to_string)))
            ));
        }
    }

    #[test]
    fn cards_show_what_a_flight_spent() {
        let mut snap = demo_snapshot(1_000_000);
        let f = snap.flights.iter_mut().find(|f| f.id == "f-12").unwrap();
        f.usage = super::super::proto::Usage {
            input_tokens: 12_000,
            output_tokens: 400,
            cost_usd: Some(0.05),
        };
        let scene = scene_for(&Model::new(snap));
        assert!(scene
            .cmds
            .iter()
            .any(|c| matches!(c, Cmd::Text { text, .. } if text == "12.4k tok · $0.05")));
    }

    #[test]
    fn focused_field_draws_a_caret() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        model.sheet = Some(Sheet::new_flight(None, Place::Here));
        let accent = theme().accent;
        let caret = |scene: &Scene| {
            scene
                .cmds
                .iter()
                .any(|c| matches!(c, Cmd::Rect { r, color, .. } if *color == accent && r.w < 2.5))
        };
        let placeholder = |scene: &Scene| {
            scene.cmds.iter().any(
                |c| matches!(c, Cmd::Text { text, .. } if text.starts_with("What should happen")),
            )
        };
        model.focus = Some(FieldId::Title);
        let focused = scene_for(&model);
        assert!(caret(&focused));
        assert!(!placeholder(&focused), "the caret replaces the placeholder");
        model.focus = None;
        let blurred = scene_for(&model);
        assert!(!caret(&blurred));
        assert!(placeholder(&blurred));
    }
}
