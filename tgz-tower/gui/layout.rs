//! Pure layout: model + measurer + theme -> display list, hit map, scroll regions.
//!
//! Everything is computed in window pixels inside `area`, the pane's content
//! rectangle. Nothing here touches the GPU; `render.rs` replays the list.

use super::clip::Rect;
use super::forms::{caret_segment, segments, Field, FieldId, Place, Sheet, AUTO_ROLE};
use super::model::{Action, Conn, Drawer, Hit, Model, ScrollId, ScrollRegion, View};
use super::proto::{
    Agent, AgentStatus, ChatKind, DriverKind, Flight, GateState, HandDirection, HandoffPacket,
    HandoffState, Health, InboxItem, InboxKind, Locality, ModelRef, Role, RouteInfo,
    SnapshotOrigin, Stage, TimelineBody, TimelineEntry,
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
        if md.snap.roles.is_empty()
            && md.snap.flights.is_empty()
            && md.view != View::Models
            && md.view != View::Intake
        {
            self.offline_state(content);
        } else {
            match md.view {
                View::Strips => self.board(content),
                View::Roster => self.roster(content),
                View::Inbox => self.inbox(content),
                View::Frequencies => self.frequencies(content),
                View::Models => self.models(content),
                View::Intake => self.intake(content),
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
            Conn::Live { .. } => (
                "No roles yet",
                "Add role files to the roles folder of the Tower config directory.",
            ),
        };
        let detail = detail.to_string();
        self.empty_state(area, title, &detail);
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
        let item_h = (self.lh * 1.7).round();
        for view in View::ALL.iter() {
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
            if *view == View::Inbox && inbox > 0 {
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
            View::Strips => format!(
                "{} flights · {} agents working · {} waiting on you",
                snap.flights.len(),
                airborne,
                needs
            ),
            View::Roster => format!(
                "{} roles · {} live agents",
                snap.roles.len(),
                snap.agents.iter().filter(|a| a.status.is_live()).count()
            ),
            View::Inbox => {
                if needs == 0 {
                    "Nothing is waiting on you".to_string()
                } else {
                    format!("{} waiting on you", needs)
                }
            }
            View::Frequencies => "Talk to the team · @role reaches its agents".to_string(),
            View::Models => {
                let up = snap
                    .endpoints
                    .iter()
                    .filter(|e| e.health == Health::Up)
                    .count();
                format!("{} of {} model servers up", up, snap.endpoints.len())
            }
            View::Intake => {
                let broken = snap.sources.iter().filter(|s| s.error.is_some()).count();
                match (snap.sources.len(), broken) {
                    (0, _) => "Where flights come from besides you".to_string(),
                    (n, 0) => format!("{} watch{} polling", n, if n == 1 { "" } else { "es" }),
                    (n, b) => format!("{} watches · {} with errors", n, b),
                }
            }
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
            View::Strips => vec![
                terminal,
                (
                    "New flight".to_string(),
                    true,
                    Hit::Button(Action::NewFlight),
                ),
            ],
            View::Roster if !cfg.is_empty() => vec![
                (
                    "Roles folder".to_string(),
                    false,
                    Hit::Button(Action::OpenTerminal {
                        cwd: Some(format!("{}/roles", cfg)),
                    }),
                ),
                (
                    "New flight".to_string(),
                    true,
                    Hit::Button(Action::NewFlight),
                ),
            ],
            View::Models if !cfg.is_empty() => vec![
                (
                    "Edit models.toml".to_string(),
                    false,
                    Hit::Button(Action::EditFile {
                        path: format!("{}/models.toml", cfg),
                    }),
                ),
                ("Probe".to_string(), true, Hit::Button(Action::Probe)),
            ],
            View::Intake if !cfg.is_empty() => vec![
                (
                    "Edit sources.toml".to_string(),
                    false,
                    Hit::Button(Action::EditFile {
                        path: format!("{}/sources.toml", cfg),
                    }),
                ),
                (
                    "Poll now".to_string(),
                    true,
                    Hit::Button(Action::PollSources),
                ),
            ],
            _ => vec![terminal],
        };
        self.buttons_right(right, by, items);
        self.fill(
            Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0),
            self.t.border.alpha(0.5),
            0.0,
        );
    }

    // -- board --------------------------------------------------------------

    fn board(&mut self, area: Rect) {
        if self.md.snap.flights.is_empty() {
            self.empty_state(
                area,
                "No flights yet",
                "A flight is a piece of work. Create one, give it a role, and an agent takes off.",
            );
            let (w, _) = self.button_size("New flight");
            let y = area.y + self.lh * 3.0 + self.lh_of(Font::Heading) + self.lh * 3.0;
            self.button(
                area.x + (area.w - w) / 2.0,
                y,
                "New flight",
                true,
                Hit::Button(Action::NewFlight),
            );
            return;
        }
        let gap = self.u * 3.0;
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
        let age = age(self.now, f.updated_unix);
        self.text_right(right, y, &age, Font::Small, self.t.faint);
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

    fn role_picker(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        selected: Option<&str>,
        allow_none: bool,
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
            items.push(("Nobody yet".into(), None, self.t.muted));
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
                y += self.role_picker(x, y, w, f.role.as_deref(), true, None);
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
                y += self.place_picker(x, y, w, f.place, true) + gap + self.u * 2.0;
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
                y += self.role_picker(x, y, w, f.to_role.as_deref(), false, exclude.as_deref())
                    + gap;
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

    fn roster(&mut self, area: Rect) {
        let area = Rect::new(
            area.x,
            area.y + self.u * 4.0,
            area.w,
            (area.h - self.u * 4.0).max(0.0),
        );
        let gap = self.u * 4.0;
        let min_w = self.lh * 17.0;
        let cols = (((area.w + gap) / (min_w + gap)).floor() as usize).max(1);
        let card_w = ((area.w - gap * (cols as f32 - 1.0)) / cols as f32).floor();
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
        let content = rows.iter().sum::<f32>() + gap * rows.len().saturating_sub(1) as f32;
        let off = self.scroll_region(ScrollId::Roster, area, content, false, false);
        let old = self.push_clip(area);
        let mut y = area.y - off;
        for (row_idx, row_h) in rows.iter().enumerate() {
            for col in 0..cols {
                let i = row_idx * cols + col;
                if let Some(role) = roles.get(i) {
                    let x = area.x + col as f32 * (card_w + gap);
                    let radius = self.u * 2.4;
                    let r = Rect::new(x, y, card_w, *row_h);
                    self.shadow(r, radius);
                    self.fill(r, self.t.card, radius);
                    self.role_card(role, x, y, card_w);
                }
            }
            y += row_h + gap;
        }
        self.pop_clip(old);
        self.scrollbar(area, content, off);
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
        if let Some(file) = &role.file {
            self.button(
                b.right() + self.u * 2.0,
                y,
                "Edit role",
                false,
                Hit::Button(Action::EditFile { path: file.clone() }),
            );
        }
        y += b.h + pad;
        y - y0
    }

    // -- inbox --------------------------------------------------------------

    fn list_area(&mut self, area: Rect) -> Rect {
        let w = area.w.min(self.lh * 44.0);
        Rect::new(
            area.x,
            area.y + self.u * 4.0,
            w,
            (area.h - self.u * 4.0).max(0.0),
        )
    }

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

    fn inbox(&mut self, area: Rect) {
        let area = self.list_area(area);
        let md = self.md;
        let items: Vec<&InboxItem> = md.snap.inbox.iter().rev().collect();
        if items.is_empty() {
            self.empty_state(area, "All clear", "Permission requests, clearances, gate results and questions from agents land here.");
            return;
        }
        let pad = self.u * 4.0;
        let gap = self.u * 3.0;
        let measure = self.push_clip(EMPTY);
        let heights: Vec<f32> = items
            .iter()
            .map(|i| self.inbox_row(i, area.x, 0.0, area.w))
            .collect();
        self.pop_clip(measure);
        let content = heights.iter().sum::<f32>() + gap * heights.len() as f32;
        let off = self.scroll_region(ScrollId::Inbox, area, content, false, false);
        let old = self.push_clip(area);
        let mut y = area.y - off;
        for (item, h) in items.iter().zip(heights.iter()) {
            let r = Rect::new(area.x, y, area.w, *h);
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
            self.inbox_row(item, area.x, y, area.w);
            y += h + gap;
        }
        let _ = pad;
        self.pop_clip(old);
        self.scrollbar(area, content, off);
    }

    /// Draw an inbox row's contents; returns its height.
    fn inbox_row(&mut self, item: &InboxItem, x0: f32, y0: f32, w: f32) -> f32 {
        let pad = self.u * 4.0;
        let kc = match item.kind {
            InboxKind::Permission => self.t.warn,
            InboxKind::Clearance => self.t.info,
            InboxKind::Question => self.t.violet,
            InboxKind::GateFailure => self.t.bad,
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

    fn empty_state(&mut self, area: Rect, title: &str, detail: &str) {
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
    }

    // -- frequencies --------------------------------------------------------

    fn frequencies(&mut self, area: Rect) {
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

    fn models(&mut self, area: Rect) {
        let area = self.list_area(area);
        let md = self.md;
        let eps = &md.snap.endpoints;
        let pad = self.u * 4.0;
        let gap = self.u * 3.0;
        let warnings: Vec<(String, String)> = md
            .snap
            .roles
            .iter()
            .filter_map(|r| r.warning.clone().map(|w| (r.name.clone(), w)))
            .collect();
        let row_h = pad * 2.0
            + self.lh_of(Font::Strong)
            + self.lh_of(Font::Small)
            + self.u * 2.5
            + self.chip_h();
        let measure = self.push_clip(EMPTY);
        let mut warn_h = 0.0;
        for (name, w) in &warnings {
            let text = format!("{}: {}", name, w);
            let c = self.t.warn;
            warn_h += self.banner(area.x, 0.0, area.w, &text, c) + gap;
        }
        self.pop_clip(measure);
        let engines = &md.snap.engines;
        let eng_row = self.lh_of(Font::Body).max(self.chip_h()) + self.u * 2.0;
        let route_h = if engines.is_empty() {
            0.0
        } else {
            self.lh_of(Font::SmallStrong)
                + self.u
                + pad
                + engines.len() as f32 * eng_row
                + pad
                + gap * 2.0
        };
        let content = warn_h + route_h + eps.len() as f32 * (row_h + gap);
        if eps.is_empty() && warnings.is_empty() && engines.is_empty() {
            self.empty_state(
                area,
                "No model servers",
                "Add Ollama, LM Studio, llama.cpp, vLLM or MLX hosts to models.toml.",
            );
            return;
        }
        let off = self.scroll_region(ScrollId::Models, area, content, false, false);
        let old = self.push_clip(area);
        let mut y = area.y - off;
        for (name, w) in &warnings {
            let text = format!("{}: {}", name, w);
            let c = self.t.warn;
            y += self.banner(area.x, y, area.w, &text, c) + gap;
        }
        if !engines.is_empty() {
            let mode = if md.snap.bandit_mode.is_empty() {
                "shadow"
            } else {
                md.snap.bandit_mode.as_str()
            };
            let label = format!("ROUTING  ·  learned policy: {}", mode);
            y += self.section_label(area.x, y, &label);
            let card_h = pad * 2.0 + engines.len() as f32 * eng_row;
            let r = Rect::new(area.x, y, area.w, card_h);
            let radius = self.u * 2.4;
            self.shadow(r, radius);
            self.fill(r, self.t.card, radius);
            let x = r.x + pad;
            let right = r.right() - pad;
            let mut ry = r.y + pad;
            let tier_w = self.chip_w("medium");
            for e in engines.iter() {
                let cy = ry + (eng_row - self.chip_h()) / 2.0;
                let ty = ry + (eng_row - self.lh_of(Font::Body)) / 2.0;
                let d = self.u * 1.6;
                let dc = if e.available { self.t.ok } else { self.t.bad };
                self.dot(x + d / 2.0, ry + eng_row / 2.0, d, dc);
                self.tier_chip(x + d + self.u * 2.0, cy, &e.tier);
                let nx = x + d + self.u * 4.0 + tier_w;
                let nw = self.text(nx, ty, &e.id, Font::Strong, self.t.text);
                let mut detail = format!("{} {}", e.driver.label(), e.model);
                if !e.effort.is_empty() {
                    detail.push_str(&format!(" · effort {}", e.effort));
                }
                let (status, sc) = match &e.why {
                    Some(w) => (w.clone(), self.t.warn),
                    None if e.trained > 0 => (format!("{} outcomes", e.trained), self.t.muted),
                    None => ("ready".to_string(), self.t.muted),
                };
                let sw = self.text_right(right, ty, &status, Font::Small, sc);
                let dx = nx + nw + self.u * 2.5;
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
            y += card_h + gap * 2.0;
        }
        for ep in eps.iter() {
            let r = Rect::new(area.x, y, area.w, row_h);
            let radius = self.u * 2.4;
            self.shadow(r, radius);
            self.fill(r, self.t.card, radius);
            let x = r.x + pad;
            let ty = r.y + pad;
            let (hc, hl) = match ep.health {
                Health::Up => (self.t.ok, "up"),
                Health::Down => (self.t.bad, "down"),
                Health::Unknown => (self.t.faint, "not probed yet"),
            };
            let d = self.u * 1.6;
            self.dot(x + d / 2.0, ty + self.lh_of(Font::Strong) / 2.0, d, hc);
            let nx = x + d + self.u * 2.5;
            let nw = self.text(nx, ty, &ep.name, Font::Strong, self.t.text);
            let fg = self.t.muted;
            let bg = self.t.tint(self.t.muted);
            self.chip(
                nx + nw + self.u * 2.0,
                ty + (self.lh_of(Font::Strong) - self.chip_h()) / 2.0,
                &ep.kind,
                fg,
                bg,
            );
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
                let cw = self.chip_w(model);
                if cx + cw > r.right() - pad {
                    break;
                }
                let fg = self.t.text;
                let bg = self.t.tint(self.t.ok);
                let c = self.chip(cx, my, model, fg, bg);
                cx = c.right() + self.u * 1.5;
            }
            y += row_h + gap;
        }
        self.pop_clip(old);
        self.scrollbar(area, content, off);
    }

    // -- intake -------------------------------------------------------------

    fn intake(&mut self, area: Rect) {
        let area = self.list_area(area);
        let md = self.md;
        let sources = &md.snap.sources;
        if sources.is_empty() {
            self.empty_state(
                area,
                "No intake yet",
                "Add a [[jira]] watch to sources.toml and store an API token with `tower auth jira <site>`. New issues then arrive as flights, and nothing is written back to Jira without your clearance.",
            );
            return;
        }
        let pad = self.u * 4.0;
        let gap = self.u * 3.0;
        let measure = self.push_clip(EMPTY);
        let heights: Vec<f32> = sources
            .iter()
            .map(|s| self.source_card(s, area.x, 0.0, area.w))
            .collect();
        self.pop_clip(measure);
        let content = heights.iter().sum::<f32>() + gap * heights.len() as f32;
        let off = self.scroll_region(ScrollId::Intake, area, content, false, false);
        let old = self.push_clip(area);
        let mut y = area.y - off;
        for (src, h) in sources.iter().zip(heights.iter()) {
            let r = Rect::new(area.x, y, area.w, *h);
            let radius = self.u * 2.4;
            self.shadow(r, radius);
            self.fill(r, self.t.card, radius);
            self.source_card(src, area.x, y, area.w);
            y += h + gap;
        }
        let _ = pad;
        self.pop_clip(old);
        self.scrollbar(area, content, off);
    }

    /// One intake watch; returns its height.
    fn source_card(&mut self, src: &super::proto::SourceStatus, x0: f32, y0: f32, w: f32) -> f32 {
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
        let kind = if src.kind == "jira" {
            "Jira"
        } else {
            src.kind.as_str()
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
        if src.auto_launch {
            line.push_str(" · launches automatically");
        }
        line.push_str(&format!(
            " · {} flight{} so far",
            src.flights,
            if src.flights == 1 { "" } else { "s" }
        ));
        self.text_fit(nx, y, &line, Font::Small, self.t.faint, right - nx);
        y += self.lh_of(Font::Small) + self.u * 2.0;
        if let Some(err) = &src.error {
            let c = self.t.bad;
            y += self.banner(nx, y, right - nx, err, c) + self.u * 2.0;
        }
        y + pad - self.u * 2.0 - y0
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

#[cfg(test)]
mod tests {
    use super::super::forms::Sheet;
    use super::super::model::Model;
    use super::super::proto::demo_snapshot;
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

    #[test]
    fn every_view_lays_out_inside_the_area() {
        let mut model = Model::new(demo_snapshot(1_000_000));
        for view in View::ALL.iter() {
            model.view = *view;
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

    #[test]
    fn board_has_a_card_per_flight_and_nav_for_every_view() {
        let model = Model::new(demo_snapshot(1_000_000));
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
        let mut model = Model::new(snap);
        model.view = View::Inbox;
        let scene = scene_for(&model);
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
        assert!(scene
            .cmds
            .iter()
            .any(|c| matches!(c, Cmd::Text { text, .. } if text == "No flights yet")));
        assert!(has(&scene, &Hit::Button(Action::NewFlight)));
        model.view = View::Inbox;
        let scene = scene_for(&model);
        assert!(scene
            .cmds
            .iter()
            .any(|c| matches!(c, Cmd::Text { text, .. } if text == "All clear")));
        let model = Model::new(super::super::model::empty_snapshot());
        let scene = scene_for(&model);
        assert!(scene
            .cmds
            .iter()
            .any(|c| matches!(c, Cmd::Text { text, .. } if text.starts_with("Connecting"))));
    }

    #[test]
    fn intake_shows_watches_errors_and_actions() {
        let mut snap = demo_snapshot(1_000_000);
        snap.config_dir = "/cfg".into();
        snap.sources[0].error = Some("HTTP 401: not authorized".into());
        let mut model = Model::new(snap);
        model.view = View::Intake;
        let scene = scene_for(&model);
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
        let scene = scene_for(&model);
        assert!(scene
            .cmds
            .iter()
            .any(|c| matches!(c, Cmd::Text { text, .. } if text == "No intake yet")));
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
        m.view = View::Models;
        let t = texts(&scene_for(&m));
        assert!(t.iter().any(|t| t.contains("ROUTING")), "{:?}", t);
        assert!(t.iter().any(|t| t == "ollama pull qwen3.6:27b"));
        m.view = View::Strips;
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
