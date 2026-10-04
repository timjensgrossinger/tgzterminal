//! Where the Changes panel sits, and what it takes from the terminal.
//!
//! One pure function answers both, so layout (`resize.rs`), painting and
//! hit-testing can never disagree about where the panel is.

use config::{DiffPanelPosition, DiffPanelRaisedMode};

/// The widest the panel may grow, as a share of the area it docks into.
const MAX_WIDTH_SHARE: f32 = 0.7;

/// In `Auto` mode, the share of the area's height up to which a raised panel
/// floats. Taller than this it covers too much to sit on top of the text.
const AUTO_FLOAT_SHARE: f32 = 0.4;

/// The window region the terminal and the panel share, in pixels.
///
/// Independent of the terminal's own size: it is what is left of the window
/// after the OS border, the sidebar and the tab bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelArea {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    /// Bottom of the usable region, *including* the docked input strip.
    pub bottom: f32,
    /// Height of the docked input strip at the bottom, 0 when not shown.
    pub strip_height: f32,
}

/// What the user (config, drags, persisted state) asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelRequest {
    pub position: DiffPanelPosition,
    pub raised_mode: DiffPanelRaisedMode,
    pub width: f32,
    /// Desired height from the top of the area; `None` means "to the bottom".
    pub height: Option<f32>,
    pub snap: f32,
    pub min_width: f32,
    pub min_height: f32,
}

/// What the panel's bottom edge is attached to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelSnap {
    /// Runs to the bottom of the area, beside the docked input strip.
    Bottom,
    /// Sits on top of the docked input strip, which spans the full width.
    Strip,
    /// Ends wherever it was dragged to.
    Raised,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub snap: PanelSnap,
    /// Pixels of width the terminal gives up for the panel.
    pub reserved_width: f32,
    /// Extra width the docked input strip gains, beyond the terminal's own,
    /// because the panel does not reach down beside it.
    pub strip_extra_width: f32,
}

impl PanelGeometry {
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// True when the panel is drawn over terminal cells rather than beside
    /// them.
    pub fn floats(&self) -> bool {
        self.reserved_width <= 0.
    }
}

/// `None` when the area is too small to host a panel at all.
pub fn panel_geometry(area: PanelArea, request: PanelRequest) -> Option<PanelGeometry> {
    let area_width = area.right - area.left;
    let area_height = area.bottom - area.top;
    if !(area_width.is_finite() && area_height.is_finite()) {
        return None;
    }
    let max_width = (area_width * MAX_WIDTH_SHARE).floor();
    if max_width < request.min_width || area_height < request.min_height {
        return None;
    }
    let width = request.width.clamp(request.min_width, max_width).floor();

    let strip_height = area.strip_height.clamp(0., area_height);
    let strip_top = area.bottom - strip_height;
    let has_strip = strip_height > 0.;

    let wanted_bottom = match request.height {
        None => area.bottom,
        Some(height) => area.top + height.max(request.min_height),
    };

    let (bottom, snap) = if wanted_bottom >= area.bottom - request.snap {
        (area.bottom, PanelSnap::Bottom)
    } else if has_strip && wanted_bottom >= strip_top - request.snap {
        // Anything from just above the strip down to the bottom snap zone
        // rests on the strip: a panel cannot end part-way down beside it.
        (strip_top, PanelSnap::Strip)
    } else {
        (wanted_bottom, PanelSnap::Raised)
    };
    // A strip so tall that resting on it leaves no room: fall back to the
    // bottom rather than produce a sliver.
    let (bottom, snap) = if bottom - area.top < request.min_height {
        (area.bottom, PanelSnap::Bottom)
    } else {
        (bottom, snap)
    };

    let reserves = match (snap, request.raised_mode) {
        (PanelSnap::Bottom, _) => true,
        (_, DiffPanelRaisedMode::Reserve) => true,
        (_, DiffPanelRaisedMode::Float) => false,
        (_, DiffPanelRaisedMode::Auto) => bottom - area.top > area_height * AUTO_FLOAT_SHARE,
    };
    let reserved_width = if reserves { width } else { 0. };
    // The strip is as wide as the terminal. It only needs widening when the
    // terminal was narrowed for a panel that stops above it.
    let strip_extra_width = if has_strip && reserves && snap != PanelSnap::Bottom {
        width
    } else {
        0.
    };

    let x = match request.position {
        DiffPanelPosition::Left => area.left,
        DiffPanelPosition::Right => area.right - width,
    };

    Some(PanelGeometry {
        x,
        y: area.top,
        width,
        height: bottom - area.top,
        snap,
        reserved_width,
        strip_extra_width,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(strip_height: f32) -> PanelArea {
        PanelArea {
            left: 100.,
            right: 1100.,
            top: 20.,
            bottom: 820.,
            strip_height,
        }
    }

    fn request(height: Option<f32>, raised_mode: DiffPanelRaisedMode) -> PanelRequest {
        PanelRequest {
            position: DiffPanelPosition::Right,
            raised_mode,
            width: 300.,
            height,
            snap: 30.,
            min_width: 200.,
            min_height: 100.,
        }
    }

    #[test]
    fn a_panel_with_no_height_runs_to_the_bottom_and_reserves_its_width() {
        for mode in [DiffPanelRaisedMode::Float, DiffPanelRaisedMode::Reserve] {
            let g = panel_geometry(area(0.), request(None, mode)).unwrap();
            assert_eq!(g.snap, PanelSnap::Bottom);
            assert_eq!((g.x, g.y, g.width, g.height), (800., 20., 300., 800.));
            assert_eq!(g.reserved_width, 300.);
            assert_eq!(g.strip_extra_width, 0.);
        }
    }

    #[test]
    fn a_raised_floating_panel_takes_nothing_from_the_terminal() {
        let g = panel_geometry(area(0.), request(Some(400.), DiffPanelRaisedMode::Float)).unwrap();
        assert_eq!(g.snap, PanelSnap::Raised);
        assert_eq!(g.height, 400.);
        assert_eq!(g.reserved_width, 0.);
        assert!(g.floats());
    }

    #[test]
    fn a_raised_reserving_panel_keeps_its_columns() {
        let g =
            panel_geometry(area(0.), request(Some(400.), DiffPanelRaisedMode::Reserve)).unwrap();
        assert_eq!(g.snap, PanelSnap::Raised);
        assert_eq!(g.reserved_width, 300.);
        assert!(!g.floats());
    }

    #[test]
    fn in_auto_mode_a_short_panel_floats_and_a_tall_one_takes_its_columns() {
        // The area is 800 tall, so the switch is at 320.
        let short =
            panel_geometry(area(0.), request(Some(300.), DiffPanelRaisedMode::Auto)).unwrap();
        assert!(short.floats());
        let tall =
            panel_geometry(area(0.), request(Some(340.), DiffPanelRaisedMode::Auto)).unwrap();
        assert_eq!(tall.reserved_width, 300.);
        let snapped = panel_geometry(area(0.), request(None, DiffPanelRaisedMode::Auto)).unwrap();
        assert_eq!(snapped.reserved_width, 300.);
    }

    #[test]
    fn dragging_near_the_bottom_snaps_onto_it() {
        // Area is 800 tall and the snap zone is 30: 771 is inside, 769 is not.
        let snapped =
            panel_geometry(area(0.), request(Some(771.), DiffPanelRaisedMode::Float)).unwrap();
        assert_eq!(snapped.snap, PanelSnap::Bottom);
        assert_eq!(snapped.height, 800.);
        assert_eq!(snapped.reserved_width, 300.);

        let raised =
            panel_geometry(area(0.), request(Some(769.), DiffPanelRaisedMode::Float)).unwrap();
        assert_eq!(raised.snap, PanelSnap::Raised);
    }

    #[test]
    fn a_panel_near_the_strip_rests_on_it_and_the_strip_goes_full_width() {
        // Strip occupies 720..820.
        let g = panel_geometry(
            area(100.),
            request(Some(690.), DiffPanelRaisedMode::Reserve),
        )
        .unwrap();
        assert_eq!(g.snap, PanelSnap::Strip);
        assert_eq!(g.bottom(), 720.);
        assert_eq!(g.reserved_width, 300.);
        assert_eq!(g.strip_extra_width, 300.);

        // Floating: the terminal, and so the strip, is already full width.
        let g =
            panel_geometry(area(100.), request(Some(690.), DiffPanelRaisedMode::Float)).unwrap();
        assert_eq!(g.snap, PanelSnap::Strip);
        assert_eq!(g.reserved_width, 0.);
        assert_eq!(g.strip_extra_width, 0.);
    }

    #[test]
    fn a_panel_cannot_end_part_way_down_beside_the_strip() {
        let g =
            panel_geometry(area(100.), request(Some(740.), DiffPanelRaisedMode::Float)).unwrap();
        assert_eq!(g.snap, PanelSnap::Strip);
        assert_eq!(g.bottom(), 720.);
    }

    #[test]
    fn snapped_to_the_bottom_the_strip_stays_beside_the_panel() {
        let g = panel_geometry(area(100.), request(None, DiffPanelRaisedMode::Float)).unwrap();
        assert_eq!(g.snap, PanelSnap::Bottom);
        assert_eq!(g.bottom(), 820.);
        assert_eq!(g.strip_extra_width, 0.);
    }

    #[test]
    fn the_left_position_docks_at_the_left_edge() {
        let mut req = request(None, DiffPanelRaisedMode::Float);
        req.position = DiffPanelPosition::Left;
        let g = panel_geometry(area(0.), req).unwrap();
        assert_eq!(g.x, 100.);
        assert_eq!(g.right(), 400.);
    }

    #[test]
    fn width_and_height_are_clamped_to_the_area() {
        let mut req = request(Some(10.), DiffPanelRaisedMode::Float);
        req.width = 5000.;
        let g = panel_geometry(area(0.), req).unwrap();
        assert_eq!(g.width, 700.);
        assert_eq!(g.height, 100.);

        req.width = 1.;
        let g = panel_geometry(area(0.), req).unwrap();
        assert_eq!(g.width, 200.);
    }

    #[test]
    fn an_area_too_small_for_the_minimum_panel_hosts_none() {
        let tiny = PanelArea {
            left: 0.,
            right: 250.,
            top: 0.,
            bottom: 800.,
            strip_height: 0.,
        };
        assert!(panel_geometry(tiny, request(None, DiffPanelRaisedMode::Float)).is_none());

        let flat = PanelArea {
            left: 0.,
            right: 1000.,
            top: 0.,
            bottom: 60.,
            strip_height: 0.,
        };
        assert!(panel_geometry(flat, request(None, DiffPanelRaisedMode::Float)).is_none());
    }

    #[test]
    fn a_strip_that_leaves_no_room_above_it_sends_the_panel_to_the_bottom() {
        let squat = PanelArea {
            left: 0.,
            right: 1000.,
            top: 0.,
            bottom: 150.,
            strip_height: 100.,
        };
        let g = panel_geometry(squat, request(Some(40.), DiffPanelRaisedMode::Float)).unwrap();
        assert_eq!(g.snap, PanelSnap::Bottom);
        assert_eq!(g.height, 150.);
    }
}
