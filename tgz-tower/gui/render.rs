//! Replays a [`Scene`] as GPU quads.
//!
//! Everything goes into sub-layer 0 of the pane layer in painter's order, so
//! later Tower shapes cover earlier ones (the drawer over the board) and fork
//! overlays painted after the panes (menus, Changes panel) still win. Every
//! quad is clipped on the CPU first (see `clip.rs`).

use super::clip::{clip_textured, Rect};
use super::fonts::FontSet;
use super::layout::{Cmd, Scene};
use super::text::Font;
use super::theme::Color;
use crate::customglyph::BlockKey;
use crate::quad::{QuadTrait, TripleLayerQuadAllocator, TripleLayerQuadAllocatorTrait};
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::termwindow::TermWindow;
use window::bitmaps::TextureRect;
use window::color::LinearRgba;

const LAYER: usize = 0;

#[derive(Clone, Copy)]
enum Kind {
    Background,
    Mask,
    Glyph { has_color: bool },
}

fn linear(c: Color) -> LinearRgba {
    LinearRgba::with_srgba(
        (c.r * 255.0).round() as u8,
        (c.g * 255.0).round() as u8,
        (c.b * 255.0).round() as u8,
        (c.a * 255.0).round() as u8,
    )
}

pub struct Painter<'t, 'l, 'a> {
    tw: &'t TermWindow,
    layers: &'l mut TripleLayerQuadAllocator<'a>,
    fonts: &'t FontSet,
    half_w: f32,
    half_h: f32,
    filled_box: TextureRect,
}

impl<'t, 'l, 'a> Painter<'t, 'l, 'a> {
    pub fn new(
        tw: &'t TermWindow,
        layers: &'l mut TripleLayerQuadAllocator<'a>,
        fonts: &'t FontSet,
    ) -> Self {
        let filled_box = tw
            .render_state
            .as_ref()
            .map(|s| s.util_sprites.filled_box.texture_coords())
            .unwrap_or_default();
        Self {
            half_w: tw.dimensions.pixel_width as f32 / 2.0,
            half_h: tw.dimensions.pixel_height as f32 / 2.0,
            tw,
            layers,
            fonts,
            filled_box,
        }
    }

    pub fn scene(&mut self, scene: &Scene) -> anyhow::Result<()> {
        for cmd in &scene.cmds {
            match cmd {
                Cmd::Rect {
                    r,
                    color,
                    radius,
                    clip,
                } => self.rect(*r, *color, *radius, clip)?,
                Cmd::Text {
                    x,
                    y,
                    text,
                    font,
                    color,
                    clip,
                } => self.text(*x, *y, text, *font, *color, clip)?,
            }
        }
        Ok(())
    }

    fn quad(
        &mut self,
        dest: Rect,
        tex: TextureRect,
        clip: &Rect,
        color: LinearRgba,
        kind: Kind,
    ) -> anyhow::Result<()> {
        let t = (tex.min_x(), tex.max_x(), tex.min_y(), tex.max_y());
        let (d, (x1, x2, y1, y2)) = match clip_textured(dest, t, clip) {
            Some(v) => v,
            None => return Ok(()),
        };
        let mut q = self.layers.allocate(LAYER)?;
        q.set_position(
            d.x - self.half_w,
            d.y - self.half_h,
            d.right() - self.half_w,
            d.bottom() - self.half_h,
        );
        q.set_texture_discrete(x1, x2, y1, y2);
        q.set_fg_color(color);
        q.set_alt_color_and_mix_value(color, 0.0);
        q.set_hsv(None);
        match kind {
            Kind::Background => q.set_is_background(),
            // Grayscale: the shader multiplies the colour's alpha by the mask.
            // The plain glyph mode replaces it with the mask, which turned the
            // corners of translucent pills into solid caps.
            Kind::Mask => q.set_grayscale(),
            Kind::Glyph { has_color } => q.set_has_color(has_color),
        }
        Ok(())
    }

    fn solid(&mut self, r: Rect, color: LinearRgba, clip: &Rect) -> anyhow::Result<()> {
        let tex = self.filled_box;
        self.quad(r, tex, clip, color, Kind::Background)
    }

    fn rect(&mut self, r: Rect, color: Color, radius: f32, clip: &Rect) -> anyhow::Result<()> {
        if !(r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite())
            || r.is_empty()
        {
            return Ok(());
        }
        // Snap to whole pixels: the corner sprites are integer-sized, so
        // fractional edges would leave seams or double-blended overlaps
        // between the pieces, which shows on translucent fills.
        let x0 = r.x.round();
        let y0 = r.y.round();
        let r = Rect::new(
            x0,
            y0,
            (r.right().round() - x0).max(0.0),
            (r.bottom().round() - y0).max(0.0),
        );
        if r.is_empty() {
            return Ok(());
        }
        let lin = linear(color);
        let rad = radius.min(r.w / 2.0).min(r.h / 2.0).floor();
        if rad < 1.0 {
            return self.solid(r, lin, clip);
        }
        // Pieces tile without overlapping: middle band, then the top and
        // bottom bands between the corners.
        self.solid(
            Rect::new(r.x, r.y + rad, r.w, (r.h - 2.0 * rad).max(0.0)),
            lin,
            clip,
        )?;
        let band_w = (r.w - 2.0 * rad).max(0.0);
        self.solid(Rect::new(r.x + rad, r.y, band_w, rad), lin, clip)?;
        self.solid(
            Rect::new(r.x + rad, r.bottom() - rad, band_w, rad),
            lin,
            clip,
        )?;

        let corners = [
            (TOP_LEFT_ROUNDED_CORNER, r.x, r.y),
            (TOP_RIGHT_ROUNDED_CORNER, r.right() - rad, r.y),
            (BOTTOM_LEFT_ROUNDED_CORNER, r.x, r.bottom() - rad),
            (
                BOTTOM_RIGHT_ROUNDED_CORNER,
                r.right() - rad,
                r.bottom() - rad,
            ),
        ];
        for (polys, x, y) in corners.iter() {
            let tex = {
                let gl_state = match self.tw.render_state.as_ref() {
                    Some(s) => s,
                    None => return Ok(()),
                };
                let sprite = gl_state.glyph_cache.borrow_mut().cached_block(
                    BlockKey::PolyWithCustomMetrics {
                        polys,
                        underline_height: 1,
                        cell_size: euclid::size2(rad as isize, rad as isize),
                    },
                    &self.tw.render_metrics,
                )?;
                sprite.texture_coords()
            };
            self.quad(Rect::new(*x, *y, rad, rad), tex, clip, lin, Kind::Mask)?;
        }
        Ok(())
    }

    fn text(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        font: Font,
        color: Color,
        clip: &Rect,
    ) -> anyhow::Result<()> {
        let fonts = self.fonts;
        let slot = fonts.slot(font);
        let infos = fonts.shape(font, text)?;
        let lin = linear(color);
        let baseline =
            y + slot.metrics.cell_size.height as f32 + slot.metrics.descender.get() as f32;
        let mut pen = x;
        let gl_state = match self.tw.render_state.as_ref() {
            Some(s) => s,
            None => return Ok(()),
        };
        for info in &infos {
            let glyph = gl_state.glyph_cache.borrow_mut().cached_glyph(
                info,
                slot.font.style(),
                false,
                &slot.font,
                &slot.metrics,
                1,
            )?;
            if let Some(tex) = glyph.texture.as_ref() {
                let gx = (pen + (glyph.x_offset + glyph.bearing_x).get() as f32).round();
                let gy = (baseline - (glyph.y_offset + glyph.bearing_y).get() as f32).round();
                let w = tex.coords.size.width as f32 * glyph.scale as f32;
                let h = tex.coords.size.height as f32 * glyph.scale as f32;
                self.quad(
                    Rect::new(gx, gy, w, h),
                    tex.texture_coords(),
                    clip,
                    lin,
                    Kind::Glyph {
                        has_color: glyph.has_color,
                    },
                )?;
            }
            pen += glyph.x_advance.get() as f32;
        }
        Ok(())
    }
}
