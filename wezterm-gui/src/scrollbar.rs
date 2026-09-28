use mux::renderable::RenderableDimensions;
use wezterm_term::StableRowIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollHit {
    /// Offset from the top of the rail the thumb travels along, in pixels.
    pub top: usize,
    /// Height of the thumb, in pixels.
    pub height: usize,
}

impl ScrollHit {
    /// Compute the y-coordinate for the top of the scrollbar thumb
    /// and the height of the thumb and return them.
    pub fn for_dimensions(
        render_dims: &RenderableDimensions,
        viewport: Option<StableRowIndex>,
        max_thumb_height: usize,
        min_thumb_size: usize,
    ) -> Self {
        let scroll_top = render_dims
            .physical_top
            .saturating_sub(viewport.unwrap_or(render_dims.physical_top))
            as f32;

        let scroll_size = render_dims.scrollback_rows as f32;

        let thumb_size = (render_dims.viewport_rows as f32 / scroll_size) * max_thumb_height as f32;

        let min_thumb_size = min_thumb_size as f32;
        let thumb_size = if thumb_size < min_thumb_size {
            min_thumb_size
        } else {
            thumb_size
        }
        .ceil() as usize;
        // A minimum larger than the whole track would otherwise push the
        // thumb past its end.
        let thumb_size = thumb_size.min(max_thumb_height);

        let scrollable_rows = render_dims.physical_top - render_dims.scrollback_top;
        let scroll_percent = if scrollable_rows > 0 {
            1.0 - (scroll_top / scrollable_rows as f32)
        } else {
            1.0
        };
        let thumb_top =
            (scroll_percent * (max_thumb_height.saturating_sub(thumb_size)) as f32).ceil() as usize;

        Self {
            top: thumb_top,
            height: thumb_size,
        }
    }

    /// Given a new thumb top coordinate (produced by dragging the thumb),
    /// compute the equivalent viewport offset.
    pub fn scroll_top_for_dimensions(
        thumb_top: usize,
        render_dims: &RenderableDimensions,
        viewport: Option<StableRowIndex>,
        max_thumb_height: usize,
        min_thumb_size: usize,
    ) -> StableRowIndex {
        let thumb = Self::for_dimensions(render_dims, viewport, max_thumb_height, min_thumb_size);
        // A thumb as tall as its track has nowhere to go: that is "scrolled
        // to the bottom", not a division by zero.
        let available_height = max_thumb_height.saturating_sub(thumb.height);
        if available_height == 0 {
            return render_dims.physical_top;
        }
        let scroll_percent = thumb_top.min(available_height) as f32 / available_height as f32;

        render_dims.scrollback_top.saturating_add(
            ((render_dims.physical_top - render_dims.scrollback_top) as f32 * scroll_percent)
                as StableRowIndex,
        )
    }
}

/// The vertical extent of the pane scrollbar, before any thumb is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollBarLayout {
    /// First window pixel row the scrollbar may use (below a top tab bar and
    /// the window border).
    pub track_top: usize,
    /// One past the last window pixel row it may use.
    pub track_bottom: usize,
    /// Empty space kept at each end of the track, so the pill never touches
    /// the window corners.
    pub inset: usize,
    /// Smallest thumb that is drawn and grabbed.
    pub min_thumb: usize,
    /// Extra pixels above and below the drawn thumb that still count as
    /// grabbing it.
    pub slop: usize,
}

/// Where the pane scrollbar's thumb is, in window pixels.
///
/// Painting, hit-testing and dragging all derive from this one value. They
/// used to compute the track independently: paint inset it by 16-32px at each
/// end and ignored a top tab bar, hit-testing did neither. So the clickable
/// thumb sat up to ~48px above the drawn pill, and once a long scrollback had
/// shrunk the pill to a few pixels, a click on it always landed in the
/// page-down zone below the real thumb -- the pill "could not be clicked" and
/// the view "skipped around".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollBarGeometry {
    /// Window y of the top of the rail the thumb travels along.
    pub rail_top: usize,
    /// Height of that rail.
    pub rail_height: usize,
    /// First and one-past-last window rows of the whole track.
    pub track_top: usize,
    pub track_bottom: usize,
    /// Grab slop, see [`ScrollBarLayout::slop`].
    pub slop: usize,
    /// The thumb, with `top` relative to `rail_top`.
    pub thumb: ScrollHit,
}

impl ScrollBarGeometry {
    /// `None` when there is nothing to scroll (the pane's whole history fits
    /// in its viewport) or the window is too short to hold a rail.
    pub fn compute(
        layout: ScrollBarLayout,
        dims: &RenderableDimensions,
        viewport: Option<StableRowIndex>,
    ) -> Option<Self> {
        if dims.scrollback_rows <= dims.viewport_rows {
            return None;
        }
        let track = layout.track_bottom.checked_sub(layout.track_top)?;
        let rail_height = track
            .checked_sub(layout.inset * 2)
            .filter(|height| *height > 0)?;
        let thumb = ScrollHit::for_dimensions(
            dims,
            viewport,
            rail_height,
            layout.min_thumb.min(rail_height),
        );
        Some(Self {
            rail_top: layout.track_top + layout.inset,
            rail_height,
            track_top: layout.track_top,
            track_bottom: layout.track_bottom,
            // The slop must fit inside the inset, or the grab box would
            // spill past the track ends.
            slop: layout.slop.min(layout.inset),
            thumb,
        })
    }

    /// Window y of the drawn thumb's top edge.
    pub fn thumb_top(&self) -> usize {
        self.rail_top + self.thumb.top
    }

    /// Window y and height of the box that grabs the thumb: the drawn thumb
    /// plus `slop` above and below.
    pub fn thumb_hit(&self) -> (usize, usize) {
        (
            self.thumb_top() - self.slop,
            self.thumb.height + 2 * self.slop,
        )
    }

    /// How far the thumb can travel within the rail.
    pub fn travel(&self) -> usize {
        self.rail_height.saturating_sub(self.thumb.height)
    }

    /// The thumb's new offset within the rail after a drag that grabbed the
    /// hit box at window y `hit_top_at_press` and has since moved `delta_y`.
    pub fn dragged_thumb_top(&self, hit_top_at_press: usize, delta_y: isize) -> usize {
        let top_at_press = (hit_top_at_press + self.slop).saturating_sub(self.rail_top) as isize;
        (top_at_press + delta_y).clamp(0, self.travel() as isize) as usize
    }

    /// The viewport top that puts the thumb at `thumb_top` within the rail.
    pub fn scroll_top_for(
        &self,
        thumb_top: usize,
        dims: &RenderableDimensions,
        viewport: Option<StableRowIndex>,
    ) -> StableRowIndex {
        ScrollHit::scroll_top_for_dimensions(
            thumb_top,
            dims,
            viewport,
            self.rail_height,
            self.thumb.height,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dims(total_rows: usize, viewport_rows: usize) -> RenderableDimensions {
        RenderableDimensions {
            cols: 80,
            viewport_rows,
            scrollback_rows: total_rows,
            physical_top: (total_rows - viewport_rows) as StableRowIndex,
            scrollback_top: 0,
            ..Default::default()
        }
    }

    fn layout() -> ScrollBarLayout {
        ScrollBarLayout {
            track_top: 30,
            track_bottom: 830,
            inset: 16,
            min_thumb: 24,
            slop: 6,
        }
    }

    #[test]
    fn nothing_to_scroll_has_no_scrollbar() {
        assert_eq!(
            ScrollBarGeometry::compute(layout(), &dims(40, 40), None),
            None
        );
        let too_short = ScrollBarLayout {
            track_bottom: 60,
            ..layout()
        };
        assert_eq!(
            ScrollBarGeometry::compute(too_short, &dims(1000, 40), None),
            None
        );
    }

    #[test]
    fn a_huge_scrollback_still_gets_a_grabbable_thumb_inside_the_rail() {
        let geometry = ScrollBarGeometry::compute(layout(), &dims(200_000, 40), None).unwrap();
        assert_eq!(geometry.rail_top, 46);
        assert_eq!(geometry.rail_height, 800 - 32);
        assert_eq!(geometry.thumb.height, 24);
        // At the bottom of the scrollback the thumb sits at the rail's end.
        assert_eq!(
            geometry.thumb_top() + geometry.thumb.height,
            geometry.rail_top + geometry.rail_height
        );
        // The grab box surrounds the drawn thumb and stays inside the track.
        let (hit_top, hit_height) = geometry.thumb_hit();
        assert!(hit_top <= geometry.thumb_top());
        assert!(hit_top + hit_height >= geometry.thumb_top() + geometry.thumb.height);
        assert!(hit_top >= geometry.track_top);
        assert!(hit_top + hit_height <= geometry.track_bottom);
    }

    #[test]
    fn dragging_moves_the_viewport_by_the_distance_dragged() {
        let dims = dims(100_000, 40);
        let geometry = ScrollBarGeometry::compute(layout(), &dims, None).unwrap();
        let (hit_top, _) = geometry.thumb_hit();

        // No movement: the thumb stays put, so the view stays at the bottom.
        let top = geometry.dragged_thumb_top(hit_top, 0);
        assert_eq!(top, geometry.thumb.top);
        assert_eq!(geometry.scroll_top_for(top, &dims, None), dims.physical_top);

        // Dragging to the very top scrolls to the start of the scrollback,
        // and overshooting is clamped rather than wrapping.
        let top = geometry.dragged_thumb_top(hit_top, -10_000);
        assert_eq!(top, 0);
        assert_eq!(
            geometry.scroll_top_for(top, &dims, None),
            dims.scrollback_top
        );

        // Halfway up the rail is halfway up the scrollback.
        let half = geometry.travel() / 2;
        let row = geometry.scroll_top_for(half, &dims, None);
        let expected = dims.physical_top / 2;
        assert!(
            (row - expected).abs() <= dims.physical_top / 500,
            "{row} vs {expected}"
        );
    }

    #[test]
    fn thumb_position_round_trips_through_the_viewport() {
        let dims = dims(100_000, 40);
        let geometry = ScrollBarGeometry::compute(layout(), &dims, None).unwrap();
        for thumb_top in [0, 1, 100, 377, geometry.travel()] {
            let row = geometry.scroll_top_for(thumb_top, &dims, None);
            let placed = ScrollBarGeometry::compute(layout(), &dims, Some(row)).unwrap();
            assert!(
                placed.thumb.top.abs_diff(thumb_top) <= 1,
                "thumb_top {thumb_top} came back as {}",
                placed.thumb.top
            );
        }
    }

    #[test]
    fn a_thumb_filling_its_track_does_not_divide_by_zero() {
        let dims = dims(41, 40);
        assert_eq!(
            ScrollHit::scroll_top_for_dimensions(5, &dims, None, 20, 40),
            dims.physical_top
        );
    }
}
