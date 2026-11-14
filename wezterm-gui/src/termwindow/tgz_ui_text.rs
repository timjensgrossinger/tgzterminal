//! Proportional text and round rings for the fork's own chrome.
//!
//! The terminal line renderer the sidebar used to draw its labels with puts
//! every glyph in a terminal cell. This module shapes a string with a chrome
//! font (`FontConfiguration::ui_font`), measures it by its real advances and
//! draws it into the quad allocator the caller already holds, so chrome text
//! keeps its place in the draw order of whatever is being painted.
//!
//! Widths are pixels, never columns. The pure parts (`fit_px`, `arc_points`,
//! `partial_path`) are unit tested; the rest needs a window.
//!
//! Rings and strokes are not stamped from squares: each is a small vector
//! path handed to the glyph cache, which rasterises it antialiased once and
//! keeps the sprite. A turning ring is a few dozen such sprites, one per step.

use crate::color::LinearRgba;
use crate::customglyph::{BlockAlpha, BlockCoord, BlockKey, Poly, PolyCommand, PolyStyle};
use crate::glyphcache::CachedGlyph;
use crate::quad::{QuadTrait, TripleLayerQuadAllocator, TripleLayerQuadAllocatorTrait};
use crate::termwindow::TermWindowNotif;
use crate::utilsprites::RenderMetrics;
use ::window::WindowOps;
use anyhow::anyhow;
use config::FontWeight;
use finl_unicode::grapheme_clusters::Graphemes;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{LazyLock, Mutex};
use termwiz::cell::grapheme_column_width;
use wezterm_font::{LoadedFont, LoadedFontId};

/// The text styles chrome uses. Sizes are relative to `sidebar_font_size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum UiFont {
    /// Body text.
    Regular,
    /// Body size, medium weight: a row's first line, button labels.
    Medium,
    /// A row's second line, hints.
    Small,
    /// Section and group headers.
    SmallMedium,
    /// The number inside a ring.
    Micro,
}

impl UiFont {
    fn scale(self) -> f64 {
        match self {
            Self::Regular | Self::Medium => 1.0,
            Self::Small | Self::SmallMedium => 12.0 / 13.0,
            Self::Micro => 10.0 / 13.0,
        }
    }

    fn weight(self) -> FontWeight {
        match self {
            Self::Regular | Self::Small => FontWeight::REGULAR,
            Self::Medium | Self::SmallMedium | Self::Micro => FontWeight::MEDIUM,
        }
    }
}

/// Where a string sits inside the width it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Align {
    Left,
    Center,
    Right,
}

/// A string shaped with one font.
pub(crate) struct ShapedLine {
    glyphs: Vec<Rc<CachedGlyph>>,
    advances: Vec<f32>,
    /// Sum of the advances, in pixels.
    pub width: f32,
}

/// How many shaped strings are kept before the cache starts over. Chrome has
/// a few hundred distinct strings at most; this only bounds a pathological run
/// (a title that changes every frame).
const MAX_CACHED_LINES: usize = 2048;

/// Shaped chrome strings and per-font metrics. Holds glyph textures, so it
/// must be cleared whenever the glyph atlas is.
#[derive(Default)]
pub(crate) struct UiTextCache {
    lines: HashMap<(LoadedFontId, String), Rc<ShapedLine>>,
    metrics: HashMap<LoadedFontId, RenderMetrics>,
}

impl UiTextCache {
    pub(crate) fn clear(&mut self) {
        self.lines.clear();
        self.metrics.clear();
    }
}

/// How many leading glyphs of a line fit in `max_px`, and whether an ellipsis
/// of width `ellipsis_px` follows them. A line that fits whole is never cut;
/// one that does not gives up glyphs until the ellipsis fits too, and drops
/// the ellipsis only when not even that fits.
pub(crate) fn fit_px(advances: &[f32], max_px: f32, ellipsis_px: f32) -> (usize, bool) {
    let total: f32 = advances.iter().sum();
    if total <= max_px {
        return (advances.len(), false);
    }
    if ellipsis_px > max_px {
        return (0, false);
    }
    let room = max_px - ellipsis_px;
    let mut used = 0.0;
    let mut count = 0;
    for advance in advances {
        if used + advance > room {
            break;
        }
        used += advance;
        count += 1;
    }
    (count, true)
}

/// `text` shortened from its start until it fits `max_px` as `measure` sees
/// it, led by an ellipsis: the end of a path is the part that names the
/// file. Text that fits comes back whole; when not even the ellipsis fits,
/// nothing does.
pub(crate) fn elide_start_px(text: &str, max_px: f32, measure: impl Fn(&str) -> f32) -> String {
    if measure(text) <= max_px {
        return text.to_string();
    }
    let starts: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
    // The fewest leading characters to drop. Dropping more never makes the
    // rest wider, so the answer can be searched for.
    let (mut low, mut high) = (1, starts.len());
    while low < high {
        let mid = (low + high) / 2;
        if measure(&format!("\u{2026}{}", &text[starts[mid]..])) <= max_px {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    if low >= starts.len() {
        return if measure("\u{2026}") <= max_px {
            "\u{2026}".to_string()
        } else {
            String::new()
        };
    }
    format!("\u{2026}{}", &text[starts[low]..])
}

/// The drawn part of one glyph: where its bitmap sits from the pen position
/// at the start of the line (`left`) and from the baseline (`top`, negative
/// above it), and the bitmap's size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GlyphInk {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

/// The box around everything a line draws, as `(left, top, right, bottom)`
/// from the line's start and baseline. `None` for a line that draws nothing.
///
/// Centring a short label by its line box puts it off-centre: digits have no
/// descender, so they sit high, and a glyph's advance includes the space
/// beside it. The ink box is what the eye centres.
pub(crate) fn ink_box(glyphs: &[GlyphInk]) -> Option<(f32, f32, f32, f32)> {
    glyphs
        .iter()
        .filter(|glyph| glyph.width > 0.0 && glyph.height > 0.0)
        .map(|glyph| {
            (
                glyph.left,
                glyph.top,
                glyph.left + glyph.width,
                glyph.top + glyph.height,
            )
        })
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
}

/// Steps a turning ring is drawn in: one sprite per step and size.
pub(crate) const RING_TURN_STEPS: u32 = 48;

/// Path coordinates are kept as `n / PATH_GRID` of the box side.
const PATH_GRID: i8 = 120;

fn grid(value: f32) -> i8 {
    (value * PATH_GRID as f32)
        .round()
        .clamp(0.0, PATH_GRID as f32) as i8
}

/// Points along an arc of a circle centred in a unit box, as fractions of
/// the box side.
///
/// `radius` is a fraction of the side too. `start_turn` and `sweep` are in
/// turns (1.0 = full circle); the arc starts at 12 o'clock plus `start_turn`
/// and runs clockwise.
pub(crate) fn arc_points(radius: f32, start_turn: f32, sweep: f32) -> Vec<(f32, f32)> {
    let sweep = sweep.clamp(0.0, 1.0);
    if radius <= 0.0 || sweep <= 0.0 {
        return vec![];
    }
    // A segment every 1/48 turn is well under a pixel off the true circle at
    // the sizes chrome draws rings in.
    let steps = ((sweep * 48.0).ceil() as usize).max(1);
    (0..=steps)
        .map(|i| {
            let turn = start_turn + sweep * (i as f32 / steps as f32);
            let angle = turn * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            (0.5 + radius * angle.cos(), 0.5 + radius * angle.sin())
        })
        .collect()
}

/// The first `progress` (0..=1, by length) of a polyline.
pub(crate) fn partial_path(path: &[(f32, f32)], progress: f32) -> Vec<(f32, f32)> {
    let progress = progress.clamp(0.0, 1.0);
    if path.len() < 2 || progress <= 0.0 {
        return vec![];
    }
    let lengths: Vec<f32> = path
        .windows(2)
        .map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt())
        .collect();
    let target = lengths.iter().sum::<f32>() * progress;
    let mut out = vec![path[0]];
    let mut walked = 0.0;
    for (w, len) in path.windows(2).zip(&lengths) {
        if *len <= 0.0 {
            continue;
        }
        if walked + len <= target + 1e-6 {
            out.push(w[1]);
            walked += len;
            continue;
        }
        let t = ((target - walked) / len).clamp(0.0, 1.0);
        if t > 0.0 {
            out.push((
                w[0].0 + (w[1].0 - w[0].0) * t,
                w[0].1 + (w[1].1 - w[0].1) * t,
            ));
        }
        break;
    }
    if out.len() < 2 {
        return vec![];
    }
    out
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum ShapeKey {
    Circle(i8),
    Stroke(Vec<(i8, i8)>),
}

/// Shapes handed to the glyph cache must be `'static`; each distinct one is
/// built once and kept for the life of the process. The set is small and
/// bounded: coordinates sit on a coarse grid and a ring turns in fixed steps.
static SHAPES: LazyLock<Mutex<HashMap<ShapeKey, &'static [Poly]>>> =
    LazyLock::new(Default::default);

fn shape(key: ShapeKey) -> &'static [Poly] {
    let mut shapes = SHAPES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(polys) = shapes.get(&key) {
        return polys;
    }
    let frac = |value: i8| BlockCoord::Frac(value, PATH_GRID);
    let path: Vec<PolyCommand> = match &key {
        ShapeKey::Circle(radius) => vec![PolyCommand::Circle {
            center: (frac(PATH_GRID / 2), frac(PATH_GRID / 2)),
            radius: frac(*radius),
        }],
        ShapeKey::Stroke(points) => points
            .iter()
            .enumerate()
            .map(|(i, (x, y))| {
                if i == 0 {
                    PolyCommand::MoveTo(frac(*x), frac(*y))
                } else {
                    PolyCommand::LineTo(frac(*x), frac(*y))
                }
            })
            .collect(),
    };
    let polys: &'static [Poly] = Box::leak(Box::new([Poly {
        path: Box::leak(path.into_boxed_slice()),
        intensity: BlockAlpha::Full,
        style: PolyStyle::Outline,
    }]));
    shapes.insert(key, polys);
    polys
}

impl crate::TermWindow {
    /// The loaded font for a chrome text style.
    pub(crate) fn ui_font(&self, which: UiFont) -> anyhow::Result<Rc<LoadedFont>> {
        self.fonts.ui_font(
            self.config.sidebar_font_size.max(1.0) * which.scale(),
            which.weight(),
        )
    }

    fn ui_metrics(&self, font: &Rc<LoadedFont>) -> RenderMetrics {
        *self
            .ui_text_cache
            .borrow_mut()
            .metrics
            .entry(font.id())
            .or_insert_with(|| RenderMetrics::with_font_metrics(&font.metrics()))
    }

    /// Height of one line of a chrome text style, in pixels.
    pub(crate) fn ui_line_height(&self, which: UiFont) -> f32 {
        match self.ui_font(which) {
            Ok(font) => self.ui_metrics(&font).cell_size.height as f32,
            Err(_) => self.render_metrics.cell_size.height as f32,
        }
    }

    fn ui_shape(&self, font: &Rc<LoadedFont>, text: &str) -> anyhow::Result<Rc<ShapedLine>> {
        let key = (font.id(), text.to_string());
        if let Some(line) = self.ui_text_cache.borrow().lines.get(&key) {
            return Ok(Rc::clone(line));
        }

        let window = self
            .window
            .as_ref()
            .ok_or_else(|| anyhow!("no window"))?
            .clone();
        let infos = font.shape(
            text,
            move || window.notify(TermWindowNotif::InvalidateShapeCache),
            BlockKey::filter_out_synthetic,
            None,
            wezterm_bidi::Direction::LeftToRight,
            None,
            None,
        )?;
        let metrics = self.ui_metrics(font);
        let style = font.style();
        let render_state = self
            .render_state
            .as_ref()
            .ok_or_else(|| anyhow!("no render state"))?;
        let mut glyph_cache = render_state.glyph_cache.borrow_mut();

        let mut glyphs = Vec::with_capacity(infos.len());
        let mut advances = Vec::with_capacity(infos.len());
        let mut width = 0.0;
        for info in &infos {
            let rest = text.get(info.cluster as usize..).unwrap_or("");
            let mut graphemes = Graphemes::new(rest).peekable();
            let grapheme = graphemes.next().unwrap_or(" ");
            let followed_by_space = graphemes.peek() == Some(&" ");
            let num_cells = grapheme_column_width(grapheme, None).max(1);
            let glyph = glyph_cache.cached_glyph(
                info,
                style,
                followed_by_space,
                font,
                &metrics,
                num_cells as u8,
            )?;
            let advance = glyph.x_advance.get() as f32;
            width += advance;
            advances.push(advance);
            glyphs.push(glyph);
        }
        drop(glyph_cache);

        let line = Rc::new(ShapedLine {
            glyphs,
            advances,
            width,
        });
        let mut cache = self.ui_text_cache.borrow_mut();
        if cache.lines.len() >= MAX_CACHED_LINES {
            cache.lines.clear();
        }
        cache.lines.insert(key, Rc::clone(&line));
        Ok(line)
    }

    /// Width of `text` in a chrome text style, in pixels. Zero when the font
    /// cannot be loaded, so a caller's layout degrades instead of failing.
    pub(crate) fn ui_text_width(&self, which: UiFont, text: &str) -> f32 {
        self.ui_font(which)
            .and_then(|font| self.ui_shape(&font, text))
            .map(|line| line.width)
            .unwrap_or(0.0)
    }

    /// Draws a short label so that what it draws is centred on `(cx, cy)`:
    /// a number in a ring, a mark in a tile. See [`ink_box`].
    pub(crate) fn paint_ui_text_centered(
        &self,
        layers: &mut TripleLayerQuadAllocator,
        which: UiFont,
        text: &str,
        cx: f32,
        cy: f32,
        color: LinearRgba,
    ) -> anyhow::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let font = self.ui_font(which)?;
        let line = self.ui_shape(&font, text)?;
        let metrics = self.ui_metrics(&font);
        let mut pen = 0.0;
        let inks: Vec<GlyphInk> = line
            .glyphs
            .iter()
            .zip(line.advances.iter())
            .filter_map(|(glyph, advance)| {
                let start = pen;
                pen += advance;
                let texture = glyph.texture.as_ref()?;
                Some(GlyphInk {
                    left: start + (glyph.x_offset + glyph.bearing_x).get() as f32,
                    top: -((glyph.y_offset + glyph.bearing_y).get() as f32),
                    width: texture.coords.size.width as f32 * glyph.scale as f32,
                    height: texture.coords.size.height as f32 * glyph.scale as f32,
                })
            })
            .collect();
        let Some((left, top, right, bottom)) = ink_box(&inks) else {
            return Ok(());
        };
        let baseline = metrics.cell_size.height as f32 + metrics.descender.get() as f32;
        let x = cx - (left + right) * 0.5;
        let y = (cy - (top + bottom) * 0.5 - baseline).round();
        self.paint_ui_text(
            layers,
            which,
            text,
            x,
            y,
            line.width + 2.0,
            color,
            Align::Left,
        )?;
        Ok(())
    }

    /// Draws one line of chrome text.
    ///
    /// `x`/`y` are the top-left corner of the line box in window pixels and
    /// `max_px` its width; text that is wider ends in an ellipsis. Returns the
    /// width actually drawn. Glyphs go on the top sub-layer of `layers`, so
    /// no fill of the same pass can cover them.
    pub(crate) fn paint_ui_text(
        &self,
        layers: &mut TripleLayerQuadAllocator,
        which: UiFont,
        text: &str,
        x: f32,
        y: f32,
        max_px: f32,
        color: LinearRgba,
        align: Align,
    ) -> anyhow::Result<f32> {
        if text.is_empty() || max_px <= 0.0 {
            return Ok(0.0);
        }
        let font = self.ui_font(which)?;
        let line = self.ui_shape(&font, text)?;
        let metrics = self.ui_metrics(&font);

        let (count, ellipsis) = if line.width <= max_px {
            (line.glyphs.len(), None)
        } else {
            let dots = self.ui_shape(&font, "\u{2026}")?;
            let (count, cut) = fit_px(&line.advances, max_px, dots.width);
            (count, cut.then_some(dots))
        };
        let drawn: f32 = line.advances[..count].iter().sum::<f32>()
            + ellipsis.as_ref().map(|dots| dots.width).unwrap_or(0.0);

        let mut pos_x = match align {
            Align::Left => x,
            Align::Center => x + ((max_px - drawn) * 0.5).max(0.0),
            Align::Right => x + (max_px - drawn).max(0.0),
        }
        .round();
        let left = self.dimensions.pixel_width as f32 / -2.0;
        let top = self.dimensions.pixel_height as f32 / -2.0;
        let baseline = metrics.cell_size.height as f32 + metrics.descender.get() as f32;

        let tail = ellipsis.as_ref();
        let glyphs = line.glyphs[..count]
            .iter()
            .zip(line.advances[..count].iter())
            .chain(
                tail.into_iter()
                    .flat_map(|dots| dots.glyphs.iter().zip(dots.advances.iter())),
            );
        for (glyph, advance) in glyphs {
            if let Some(texture) = glyph.texture.as_ref() {
                let glyph_x = pos_x + (glyph.x_offset + glyph.bearing_x).get() as f32;
                let glyph_y = y + baseline - (glyph.y_offset + glyph.bearing_y).get() as f32;
                let width = texture.coords.size.width as f32 * glyph.scale as f32;
                let height = texture.coords.size.height as f32 * glyph.scale as f32;
                let mut quad = layers.allocate(2)?;
                quad.set_position(
                    glyph_x + left,
                    glyph_y + top,
                    glyph_x + left + width,
                    glyph_y + top + height,
                );
                quad.set_fg_color(color);
                quad.set_alt_color_and_mix_value(color, 0.0);
                quad.set_texture(texture.texture_coords());
                quad.set_has_color(glyph.has_color);
                quad.set_hsv(None);
            }
            pos_x += advance;
        }
        Ok(drawn)
    }

    /// Strokes a polyline, given as fractions of a `side` × `side` box at
    /// `x`/`y`, with a line `stroke` pixels wide. Antialiased.
    pub(crate) fn paint_stroke(
        &self,
        layers: &mut TripleLayerQuadAllocator,
        layer_num: usize,
        x: f32,
        y: f32,
        side: f32,
        stroke: f32,
        points: &[(f32, f32)],
        color: LinearRgba,
    ) -> anyhow::Result<()> {
        let mut path: Vec<(i8, i8)> = points.iter().map(|(x, y)| (grid(*x), grid(*y))).collect();
        path.dedup();
        if path.len() < 2 || side < 2.0 {
            return Ok(());
        }
        self.paint_shape(
            layers,
            layer_num,
            x,
            y,
            side,
            stroke,
            ShapeKey::Stroke(path),
            color,
        )
    }

    fn paint_shape(
        &self,
        layers: &mut TripleLayerQuadAllocator,
        layer_num: usize,
        x: f32,
        y: f32,
        side: f32,
        stroke: f32,
        key: ShapeKey,
        color: LinearRgba,
    ) -> anyhow::Result<()> {
        let side = side.round().max(2.0);
        self.poly_quad(
            layers,
            layer_num,
            euclid::point2(x.round(), y.round()),
            shape(key),
            stroke.round().max(1.0) as isize,
            euclid::size2(side, side),
            color,
        )?;
        Ok(())
    }

    /// Draws a round ring in the `side` × `side` box at `x`/`y`: an optional
    /// full `track`, and on top of it any number of `arcs`, each
    /// `(colour, start turn, sweep)`. See [`arc_points`] for the angles.
    pub(crate) fn paint_ring(
        &self,
        layers: &mut TripleLayerQuadAllocator,
        layer_num: usize,
        x: f32,
        y: f32,
        side: f32,
        stroke: f32,
        track: Option<LinearRgba>,
        arcs: &[(LinearRgba, f32, f32)],
    ) -> anyhow::Result<()> {
        let side = side.round().max(2.0);
        let stroke = stroke.round().max(1.0);
        // Half a pixel in from the edge so the antialiased rim is not clipped.
        let radius = ((side - stroke) * 0.5 - 0.5).max(1.0) / side;
        if let Some(color) = track {
            self.paint_shape(
                layers,
                layer_num,
                x,
                y,
                side,
                stroke,
                ShapeKey::Circle(grid(radius)),
                color,
            )?;
        }
        for (color, start, sweep) in arcs {
            if *sweep >= 1.0 {
                self.paint_shape(
                    layers,
                    layer_num,
                    x,
                    y,
                    side,
                    stroke,
                    ShapeKey::Circle(grid(radius)),
                    *color,
                )?;
                continue;
            }
            // Snapped to the turn steps so a turning ring reuses its sprites.
            let steps = RING_TURN_STEPS as f32;
            let start = (start.rem_euclid(1.0) * steps).round() / steps;
            let points = arc_points(radius, start, *sweep);
            self.paint_stroke(layers, layer_num, x, y, side, stroke, &points, *color)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_path_keeps_its_end() {
        // Every character ten pixels wide.
        let measure = |text: &str| text.chars().count() as f32 * 10.0;
        assert_eq!(elide_start_px("src/main.rs", 200.0, measure), "src/main.rs");
        assert_eq!(elide_start_px("src/main.rs", 110.0, measure), "src/main.rs");
        assert_eq!(
            elide_start_px("src/main.rs", 80.0, measure),
            "\u{2026}main.rs"
        );
        assert_eq!(elide_start_px("src/main.rs", 20.0, measure), "\u{2026}s");
        assert_eq!(elide_start_px("src/main.rs", 10.0, measure), "\u{2026}");
        assert_eq!(elide_start_px("src/main.rs", 5.0, measure), "");
        // Cut between characters, never inside one.
        assert_eq!(
            elide_start_px("\u{e4}\u{f6}\u{fc}/x", 30.0, measure),
            "\u{2026}/x"
        );
    }

    #[test]
    fn the_ink_box_ignores_descender_room_and_side_bearings() {
        // Two digits: bitmaps start one pixel in from the pen and stand
        // wholly above the baseline.
        let digits = [
            GlyphInk {
                left: 1.0,
                top: -7.0,
                width: 4.0,
                height: 7.0,
            },
            GlyphInk {
                left: 7.0,
                top: -7.0,
                width: 4.0,
                height: 7.0,
            },
        ];
        assert_eq!(ink_box(&digits), Some((1.0, -7.0, 11.0, 0.0)));
        // A space draws nothing and does not widen the box.
        let blank = GlyphInk {
            left: 20.0,
            top: 0.0,
            width: 0.0,
            height: 0.0,
        };
        assert_eq!(ink_box(&[digits[0], blank]), Some((1.0, -7.0, 5.0, 0.0)));
        assert_eq!(ink_box(&[blank]), None);
    }

    #[test]
    fn a_line_that_fits_is_not_cut() {
        assert_eq!(fit_px(&[5.0, 5.0, 5.0], 15.0, 6.0), (3, false));
        assert_eq!(fit_px(&[], 0.0, 6.0), (0, false));
    }

    #[test]
    fn a_long_line_gives_up_glyphs_until_the_ellipsis_fits() {
        // 4 × 5 = 20 px into 16 px with a 6 px ellipsis: two glyphs + ellipsis.
        assert_eq!(fit_px(&[5.0; 4], 16.0, 6.0), (2, true));
        // Exactly enough room for one glyph and the ellipsis.
        assert_eq!(fit_px(&[5.0; 4], 11.0, 6.0), (1, true));
    }

    #[test]
    fn no_room_for_the_ellipsis_draws_nothing() {
        assert_eq!(fit_px(&[5.0; 4], 4.0, 6.0), (0, false));
        // Room for the ellipsis alone still says "there was more".
        assert_eq!(fit_px(&[5.0; 4], 6.0, 6.0), (0, true));
    }

    #[test]
    fn arc_points_start_at_twelve_and_run_clockwise() {
        let points = arc_points(0.4, 0.0, 0.25);
        let (first_x, first_y) = points[0];
        assert!((first_x - 0.5).abs() < 1e-4, "{first_x}");
        assert!((first_y - 0.1).abs() < 1e-4, "{first_y}");
        // A quarter turn clockwise ends at 3 o'clock.
        let (last_x, last_y) = *points.last().unwrap();
        assert!((last_x - 0.9).abs() < 1e-4, "{last_x}");
        assert!((last_y - 0.5).abs() < 1e-4, "{last_y}");
        for (x, y) in &points {
            let r = ((x - 0.5).powi(2) + (y - 0.5).powi(2)).sqrt();
            assert!((r - 0.4).abs() < 1e-4, "{r}");
        }
    }

    #[test]
    fn a_degenerate_arc_has_no_points() {
        assert!(arc_points(0.0, 0.0, 1.0).is_empty());
        assert!(arc_points(0.4, 0.0, 0.0).is_empty());
    }

    #[test]
    fn partial_path_cuts_by_length() {
        let path = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)];
        assert!(partial_path(&path, 0.0).is_empty());
        assert_eq!(partial_path(&path, 0.25), vec![(0.0, 0.0), (0.5, 0.0)]);
        assert_eq!(partial_path(&path, 0.5), vec![(0.0, 0.0), (1.0, 0.0)]);
        assert_eq!(
            partial_path(&path, 0.75),
            vec![(0.0, 0.0), (1.0, 0.0), (1.0, 0.5)]
        );
        assert_eq!(partial_path(&path, 1.0), path.to_vec());
        assert!(partial_path(&path[..1], 1.0).is_empty());
    }

    #[test]
    fn equal_shapes_share_one_sprite_key() {
        let a = shape(ShapeKey::Stroke(vec![(1, 2), (30, 40)]));
        let b = shape(ShapeKey::Stroke(vec![(1, 2), (30, 40)]));
        assert!(std::ptr::eq(a, b));
        let c = shape(ShapeKey::Circle(50));
        assert!(!std::ptr::eq(a, c));
    }
}
