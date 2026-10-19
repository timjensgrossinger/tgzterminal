//! The only file that touches `TermWindow`. Everything that depends on fork
//! or upstream internals lives here, so an upstream merge breaks one file.

use super::client;
use super::clip::Rect;
use super::fonts::{font_set, GpuMeasure};
use super::layout::layout;
use super::model::{Effect, Frame};
use super::pane::{now_ms, TowerPane};
use super::render::Painter;
use super::theme::{Color, Theme};
use crate::quad::TripleLayerQuadAllocator;
use crate::termwindow::TermWindow;
use mux::pane::{CachePolicy, Pane};
use mux::tab::{PositionedPane, Tab};
use mux::Mux;
use std::sync::Arc;
use std::time::{Duration, Instant};
use window::WindowOps;

pub const OPEN_EVENT: &str = "tgz.tower.open";

pub fn is_tower_pane(pane: &Arc<dyn Pane>) -> bool {
    pane.downcast_ref::<TowerPane>().is_some()
}

pub fn on_emit_event(tw: &mut TermWindow, _pane: &Arc<dyn Pane>, name: &str) -> bool {
    if name != OPEN_EVENT {
        return false;
    }
    open(tw);
    true
}

/// Focus this window's Tower tab, creating it if there is none.
pub fn open(tw: &mut TermWindow) {
    if let Err(err) = open_impl(tw) {
        log::error!("tgz_tower: open failed: {:#}", err);
    }
    if let Some(w) = tw.window.as_ref() {
        w.invalidate();
    }
}

fn open_impl(tw: &mut TermWindow) -> anyhow::Result<()> {
    let mux = Mux::get();
    let existing = mux.get_window(tw.mux_window_id).and_then(|window| {
        window.iter_tabs().position(|tab| {
            tab.iter_panes_ignoring_zoom()
                .iter()
                .any(|p| is_tower_pane(&p.pane))
        })
    });
    if let Some(idx) = existing {
        return tw.activate_tab(idx as isize);
    }

    let active = mux.get_active_tab_for_window(tw.mux_window_id);
    let size = active.as_ref().map(|t| t.get_size()).unwrap_or_default();
    // New splits next to Tower start where you were working.
    let cwd = active
        .as_ref()
        .and_then(|t| t.get_active_pane())
        .and_then(|p| p.get_current_working_dir(CachePolicy::AllowStale));
    let domain_id = mux
        .get_domain_by_name("local")
        .unwrap_or_else(|| mux.default_domain())
        .domain_id();

    let tower = Arc::new(TowerPane::new(domain_id, size, cwd));
    let model = Arc::clone(&tower.model);
    let pane: Arc<dyn Pane> = tower;
    let pane_id = pane.pane_id();
    let tab = Arc::new(Tab::new(&size));
    tab.assign_pane(&pane);
    mux.add_tab_and_active_pane(&tab)?;
    mux.add_tab_to_window(&tab, tw.mux_window_id)?;
    let count = mux
        .get_window(tw.mux_window_id)
        .map(|w| w.count_tabs())
        .unwrap_or(1);
    tw.activate_tab(count.saturating_sub(1) as isize)?;
    client::subscribe(pane_id, &model);
    Ok(())
}

/// The pane's content rectangle in window pixels, and its background
/// rectangle, which bleeds to the window edges like a terminal pane's does.
fn pane_rects(tw: &TermWindow, pos: &PositionedPane) -> anyhow::Result<(Rect, Rect)> {
    let (padding_left, padding_top) = tw.padding_left_top();
    let border = tw.get_os_border();
    let tab_bar_height = if tw.show_tab_bar && !tw.sidebar_is_active() {
        tw.tab_bar_pixel_height()?
    } else {
        0.0
    };
    let top_bar = if tw.config.tab_bar_at_bottom {
        0.0
    } else {
        tab_bar_height
    };
    let cw = tw.render_metrics.cell_size.width as f32;
    let ch = tw.render_metrics.cell_size.height as f32;
    let x0 = padding_left + border.left.get() as f32 + pos.left as f32 * cw;
    let y0 = top_bar + padding_top + border.top.get() as f32 + pos.top as f32 * ch;
    let content = Rect::new(x0, y0, pos.width as f32 * cw, pos.height as f32 * ch);

    let rightmost = pos.left + pos.width >= tw.terminal_size.cols as usize;
    let bottommost = pos.top + pos.height >= tw.terminal_size.rows as usize;
    let bx = if pos.left == 0 {
        x0 - padding_left.min(cw * 0.5)
    } else {
        x0 - cw * 0.5
    };
    let by = if pos.top == 0 {
        y0 - padding_top
    } else {
        y0 - ch * 0.5
    };
    let br = if rightmost {
        tw.dimensions.pixel_width as f32 - border.right.get() as f32
    } else {
        content.right() + cw * 0.5
    };
    let bb = if bottommost {
        tw.dimensions.pixel_height as f32
            - tw.docked_input_pixel_height()
            - border.bottom.get() as f32
    } else {
        content.bottom() + ch * 0.5
    };
    let background = Rect::new(bx, by, (br - bx).max(0.0), (bb - by).max(0.0));
    Ok((content, background))
}

pub fn paint(
    tw: &mut TermWindow,
    pos: &PositionedPane,
    layers: &mut TripleLayerQuadAllocator,
) -> anyhow::Result<()> {
    let tower = match pos.pane.downcast_ref::<TowerPane>() {
        Some(t) => t,
        None => return Ok(()),
    };
    let (area, background) = pane_rects(tw, pos)?;
    let palette = tw.palette().clone();
    let bg = palette.background;
    let fg = palette.foreground;
    let theme = Theme::from_palette(
        Color::rgba(bg.0, bg.1, bg.2, 1.0),
        Color::rgba(fg.0, fg.1, fg.2, 1.0),
    );
    let fonts = font_set(&tw.config, tw.dimensions.dpi)?;
    let now = now_ms();

    let scene = {
        let mut model = tower.model.lock();
        model.tick(now);
        model.preedit = match tw.composition_status() {
            window::DeadKeyStatus::Composing(s) if !s.is_empty() && model.focus.is_some() => {
                Some(s.clone())
            }
            _ => None,
        };
        let mut measure = GpuMeasure { set: &fonts };
        let scene = layout(&model, &theme, &mut measure, area, now / 1000);
        model.frame = Frame {
            hits: scene.hits.clone(),
            scrolls: scene.scrolls.clone(),
            origin: (area.x, area.y),
            cell: (
                tw.render_metrics.cell_size.width as f32,
                tw.render_metrics.cell_size.height as f32,
            ),
            wheel_step: scene.wheel_step,
        };
        let (cw, ch) = model.frame.cell;
        model.caret_cell = scene.caret.and_then(|(x, y)| {
            if cw <= 0.0 || ch <= 0.0 {
                return None;
            }
            Some((
                ((x - area.x) / cw).max(0.0) as usize,
                ((y - area.y) / ch).max(0.0) as isize,
            ))
        });
        if let Some((_, until)) = &model.toast {
            let wait = until.saturating_sub(now);
            tw.update_next_frame_time(Some(Instant::now() + Duration::from_millis(wait)));
        }
        scene
    };

    {
        let mut painter = Painter::new(tw, layers, &fonts);
        // Fill the bleed around the cell grid so the GUI meets the splits.
        painter.scene(&super::layout::Scene {
            cmds: vec![super::layout::Cmd::Rect {
                r: background,
                color: theme.bg,
                radius: 0.0,
                clip: background,
            }],
            ..Default::default()
        })?;
        painter.scene(&scene)?;
    }

    let pane_id = tower.pane_id();
    for effect in tower.take_effects() {
        match effect {
            Effect::Send(msg) => client::send(msg),
            Effect::Split { argv, cwd } => client::open_split(pane_id, argv, cwd),
            Effect::FocusPane(p) => client::focus(p),
            Effect::Open(path) => wezterm_open_url::open_url(&path),
            Effect::Repaint => {}
        }
    }
    Ok(())
}
