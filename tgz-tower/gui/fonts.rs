//! Proportional fonts at several sizes, without touching wezterm-font.
//!
//! wezterm-font only exposes a handful of purpose fonts (window frame, command
//! palette, ...), each at one configured size. A `FontConfiguration` created at
//! a scaled DPI yields the same font at a different pixel size, so each Tower
//! text style is "the window-frame (bold) or command-palette (regular) font at
//! dpi * target / configured". They are cached per thread by
//! `(dpi, config generation)`.

use super::text::{Font, Measure};
use crate::customglyph::BlockKey;
use crate::utilsprites::RenderMetrics;
use config::ConfigHandle;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use wezterm_font::{FontConfiguration, LoadedFont};

pub struct Slot {
    pub font: Rc<LoadedFont>,
    pub metrics: RenderMetrics,
}

pub struct FontSet {
    key: (usize, usize),
    /// Kept alive: `LoadedFont` only holds a weak reference to its config,
    /// which it needs to resolve fallback fonts.
    _configs: Vec<Rc<FontConfiguration>>,
    slots: Vec<(Font, Slot)>,
    widths: RefCell<HashMap<(Font, String), f32>>,
}

/// (style, size in pt, bold)
const SPECS: [(Font, f64, bool); 5] = [
    (Font::Heading, 17.0, true),
    (Font::Strong, 12.5, true),
    (Font::Body, 12.5, false),
    (Font::Small, 11.0, false),
    (Font::SmallStrong, 10.5, true),
];

/// Set when a shaping call had to wait for a fallback font; widths measured
/// before it arrived are wrong and get dropped on the next frame.
static FALLBACK_ARRIVED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static CURRENT: RefCell<Option<Rc<FontSet>>> = RefCell::new(None);
}

impl FontSet {
    fn build(config: &ConfigHandle, dpi: usize) -> anyhow::Result<Self> {
        let title_base =
            config
                .window_frame
                .font_size
                .unwrap_or(if cfg!(windows) { 10.0 } else { 12.0 });
        let palette_base = config.command_palette_font_size.max(1.0);
        let mut configs: Vec<(usize, Rc<FontConfiguration>)> = Vec::new();
        let mut slots = Vec::new();
        for (font, pt, bold) in SPECS.iter() {
            let base = if *bold { title_base } else { palette_base };
            let scaled = ((dpi as f64) * pt / base).round().max(1.0) as usize;
            let cfg = match configs.iter().find(|(d, _)| *d == scaled) {
                Some((_, cfg)) => Rc::clone(cfg),
                None => {
                    let cfg = Rc::new(FontConfiguration::new(Some(config.clone()), scaled)?);
                    configs.push((scaled, Rc::clone(&cfg)));
                    cfg
                }
            };
            let loaded = if *bold {
                cfg.title_font()?
            } else {
                cfg.command_palette_font()?
            };
            let metrics = RenderMetrics::with_font_metrics(&loaded.metrics());
            slots.push((
                *font,
                Slot {
                    font: loaded,
                    metrics,
                },
            ));
        }
        Ok(Self {
            key: (dpi, config.generation()),
            _configs: configs.into_iter().map(|(_, c)| c).collect(),
            slots,
            widths: RefCell::new(HashMap::new()),
        })
    }

    pub fn slot(&self, font: Font) -> &Slot {
        self.slots
            .iter()
            .find(|(f, _)| *f == font)
            .map(|(_, s)| s)
            .expect("every Font has a slot")
    }

    pub fn shape(
        &self,
        font: Font,
        text: &str,
    ) -> anyhow::Result<Vec<wezterm_font::shaper::GlyphInfo>> {
        self.slot(font).font.shape(
            text,
            || FALLBACK_ARRIVED.store(true, Ordering::Relaxed),
            BlockKey::filter_out_synthetic,
            None,
            wezterm_bidi::Direction::LeftToRight,
            None,
            None,
        )
    }
}

/// The font set for this window's DPI and the current config.
pub fn font_set(config: &ConfigHandle, dpi: usize) -> anyhow::Result<Rc<FontSet>> {
    CURRENT.with(|cur| {
        let mut cur = cur.borrow_mut();
        if let Some(set) = cur.as_ref() {
            if set.key == (dpi, config.generation()) {
                if FALLBACK_ARRIVED.swap(false, Ordering::Relaxed) {
                    set.widths.borrow_mut().clear();
                }
                return Ok(Rc::clone(set));
            }
        }
        let started = std::time::Instant::now();
        let set = Rc::new(FontSet::build(config, dpi)?);
        log::debug!(
            "tgz_tower: font set for dpi {} built in {:?}",
            dpi,
            started.elapsed()
        );
        *cur = Some(Rc::clone(&set));
        Ok(set)
    })
}

/// [`Measure`] over real shaped glyphs, memoized per string.
pub struct GpuMeasure<'a> {
    pub set: &'a FontSet,
}

impl<'a> Measure for GpuMeasure<'a> {
    fn width(&mut self, text: &str, font: Font) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let key = (font, text.to_string());
        if let Some(w) = self.set.widths.borrow().get(&key) {
            return *w;
        }
        let w = match self.set.shape(font, text) {
            Ok(infos) => infos.iter().map(|i| i.x_advance.get() as f32).sum(),
            Err(_) => text.chars().count() as f32 * self.line_height(font) * 0.5,
        };
        let mut widths = self.set.widths.borrow_mut();
        if widths.len() > 8192 {
            widths.clear();
        }
        widths.insert(key, w);
        w
    }

    fn line_height(&self, font: Font) -> f32 {
        self.set.slot(font).metrics.cell_size.height as f32
    }
}
