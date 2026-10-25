//! Text flowing around a raised Changes panel, the way text in a document
//! flows around a picture.
//!
//! A program in a pane is told one column count and breaks its lines
//! itself, so a pane cannot be narrower beside the panel than below it. The
//! wrapping therefore happens on screen only: the program keeps the full
//! width, and a row that would run under the panel is cut short of it and
//! continued on the next screen row. Everything below moves down to make
//! room, and the top of the viewport gives way so the bottom (the prompt and
//! the cursor) stays in view.
//!
//! Painting, clicks and the IME cursor all go through one [`WrapLayout`], so
//! what is drawn where is also where a click lands.

use super::geometry::PanelGeometry;
use config::DiffPanelPosition;

/// Columns left free between the text and the panel's side.
pub const GAP_COLS: usize = 2;
/// Rows left free between the panel's bottom edge and full-width text. One:
/// a cell is about twice as tall as it is wide, so this matches
/// [`GAP_COLS`] by eye; two left so much room that a few pixels of drag
/// pulled an agent's input box into the wrapped zone.
pub const GAP_ROWS: usize = 1;
/// How far above the cursor [`input_top`] looks for the rule that opens an
/// agent's input box. A prompt typed taller than this is taken from the
/// cursor row alone.
pub const INPUT_SEARCH_ROWS: usize = 24;
/// Share of a row a panel edge may overlap without the row counting as
/// beside it: well under a pixel.
const ROW_EPSILON: f32 = 1e-3;
/// Fewest line-art cells a row needs to count as a horizontal rule.
const MIN_RULE_CELLS: usize = 8;
/// A row narrower than this is not worth wrapping into: the panel then
/// simply covers the pane, as it would without wrapping.
pub const MIN_WRAP_COLS: usize = 8;

/// Whether a cell showing `text` is padding or line art rather than text.
///
/// Programs draw rules (`────`), box borders and highlight backgrounds out
/// to their full width, which runs under the panel. Continuing those on the
/// next screen row puts a stub of a rule under every rule, so a row is only
/// wrapped as far as its last real text; filler after it is clipped to the
/// zone instead. Box drawing (U+2500..U+257F) counts as filler; block
/// elements do not, since a progress bar's tail is information.
pub fn is_wrap_filler(text: &str) -> bool {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (None, _) => true,
        (Some(c), None) => c == ' ' || ('\u{2500}'..='\u{257f}').contains(&c),
        _ => false,
    }
}

/// What one cell contributes to [`is_rule`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleCell {
    Padding,
    Art,
    Text,
}

/// Classifies a cell showing `text` for [`is_rule`].
pub fn rule_cell(text: &str) -> RuleCell {
    if !is_wrap_filler(text) {
        RuleCell::Text
    } else if text.chars().next().is_some_and(|c| c != ' ') {
        RuleCell::Art
    } else {
        RuleCell::Padding
    }
}

/// Whether a row made of `cells` is a horizontal rule: nothing but padding
/// and line art, with at least [`MIN_RULE_CELLS`] of the latter. Claude Code
/// opens and closes its input box with one.
pub fn is_rule(cells: impl IntoIterator<Item = RuleCell>) -> bool {
    let mut art = 0;
    for cell in cells {
        match cell {
            RuleCell::Text => return false,
            RuleCell::Art => art += 1,
            RuleCell::Padding => {}
        }
    }
    art >= MIN_RULE_CELLS
}

/// The first viewport row of the input the cursor is in, for keeping a
/// raised panel clear of it (`diff_panel.avoid_input`).
///
/// `rules[r]` says whether row `r` is a rule ([`is_rule`]). An agent's input
/// box starts at the nearest rule above the cursor (the rows between are the
/// prompt, which grows as it is typed into), so that rule is the top. With
/// no rule within [`INPUT_SEARCH_ROWS`] (a shell prompt), the cursor row is.
pub fn input_top(rules: &[bool], cursor_row: usize) -> usize {
    let cursor_row = cursor_row.min(rules.len().saturating_sub(1));
    let floor = cursor_row.saturating_sub(INPUT_SEARCH_ROWS);
    (floor..cursor_row)
        .rev()
        .find(|&row| rules[row])
        .unwrap_or(cursor_row)
}

/// A pane's cell grid, in window pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneRect {
    /// Left edge of column 0.
    pub x: f32,
    /// Top edge of row 0.
    pub y: f32,
    pub cols: usize,
    pub rows: usize,
    pub cell_width: f32,
    pub cell_height: f32,
}

/// The part of a pane beside the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrapZone {
    /// Screen rows, from the pane's top, beside the panel or in the gap
    /// below it.
    pub rows: usize,
    /// First column a wrapped row is drawn in.
    pub offset: usize,
    /// Columns a wrapped row is drawn in.
    pub width: usize,
}

/// The zone a floating panel cuts out of a pane, or `None` when the panel
/// does not reach the pane, or leaves too little beside it to wrap into.
pub fn wrap_zone(
    pane: PaneRect,
    panel: &PanelGeometry,
    position: DiffPanelPosition,
) -> Option<WrapZone> {
    if pane.cols == 0 || pane.rows == 0 || pane.cell_width <= 0. || pane.cell_height <= 0. {
        return None;
    }
    let zone_bottom = panel.bottom() + GAP_ROWS as f32 * pane.cell_height;
    // A panel ending exactly on a row boundary (as `avoid_input` places it)
    // must not pull the next row in through float rounding.
    let rows = ((zone_bottom - pane.y) / pane.cell_height - ROW_EPSILON).ceil();
    if !(rows > 0.) {
        return None;
    }
    let rows = (rows as usize).min(pane.rows);

    let (offset, width) = match position {
        DiffPanelPosition::Right => {
            let before_panel = ((panel.x - pane.x) / pane.cell_width).floor();
            let width = before_panel as isize - GAP_COLS as isize;
            if width >= pane.cols as isize {
                return None;
            }
            (0, width)
        }
        DiffPanelPosition::Left => {
            let covered = ((panel.right() - pane.x) / pane.cell_width).ceil();
            if !(covered > 0.) {
                return None;
            }
            let offset = covered as usize + GAP_COLS;
            (offset, pane.cols as isize - offset as isize)
        }
    };
    if width < MIN_WRAP_COLS as isize {
        return None;
    }
    Some(WrapZone {
        rows,
        offset,
        width: width as usize,
    })
}

/// Where one viewport row of the pane ended up on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowPlacement {
    /// Screen row of its first piece; negative when it scrolled off the top.
    pub top: isize,
    /// Screen rows it takes: 1 at full width, one per piece when wrapped.
    pub pieces: usize,
    /// Cut into [`WrapZone::width`] pieces drawn from [`WrapZone::offset`].
    pub wrapped: bool,
}

/// One piece of a row, as painted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    /// Viewport row it belongs to.
    pub row: usize,
    /// Screen row it is drawn on, always within the viewport.
    pub screen_row: usize,
    /// The row's cells it shows.
    pub cols: std::ops::Range<usize>,
    /// Screen column its first cell is drawn in.
    pub offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrapLayout {
    pub zone: WrapZone,
    pub cols: usize,
    pub screen_rows: usize,
    /// One per viewport row, in order.
    pub rows: Vec<RowPlacement>,
}

/// Lays the viewport's rows out around the zone.
///
/// `lens[r]` is how many leading cells of row `r` hold text or the cursor
/// (trailing [`is_wrap_filler`] cells excluded); a wrapped row is cut into as
/// many pieces as that needs. `keep` is how many rows must stay in view:
/// everything up to the last row showing anything at all, filler included. Rows after that are blank and
/// are the first to be pushed off the bottom; only then does the top give
/// way, by as few screen rows as it takes.
pub fn layout(zone: WrapZone, cols: usize, lens: &[usize], keep: usize) -> WrapLayout {
    let screen_rows = lens.len();
    let keep = keep.min(screen_rows);
    let width = zone.width.max(1);

    let place = |drop: isize| -> (Vec<RowPlacement>, isize) {
        let mut rows = Vec::with_capacity(lens.len());
        let mut y = -drop;
        let mut kept_end = 0;
        for (r, &len) in lens.iter().enumerate() {
            let wrapped = y < zone.rows as isize;
            let pieces = if wrapped {
                len.min(cols).div_ceil(width).max(1)
            } else {
                1
            };
            rows.push(RowPlacement {
                top: y,
                pieces,
                wrapped,
            });
            y += pieces as isize;
            if r < keep {
                kept_end = y;
            }
        }
        (rows, kept_end)
    };

    // Pushing rows up can move another one into the zone, where it may need
    // more pieces, so the fit is searched rather than computed. Each drop
    // step frees at most one screen row, so the search is bounded by how
    // many pieces wrapping can add at most.
    let most_extra: usize = lens
        .iter()
        .take(keep)
        .map(|&len| len.min(cols).div_ceil(width).max(1) - 1)
        .sum();
    let mut best = place(0);
    for drop in 1..=most_extra as isize {
        if best.1 <= screen_rows as isize {
            break;
        }
        best = place(drop);
    }
    WrapLayout {
        zone,
        cols,
        screen_rows,
        rows: best.0,
    }
}

impl WrapLayout {
    /// The pieces to paint, top to bottom, clipped to the viewport.
    pub fn pieces(&self) -> Vec<Piece> {
        let mut out = Vec::with_capacity(self.screen_rows);
        for (row, placement) in self.rows.iter().enumerate() {
            for piece in 0..placement.pieces {
                let screen_row = placement.top + piece as isize;
                if screen_row < 0 {
                    continue;
                }
                if screen_row >= self.screen_rows as isize {
                    return out;
                }
                let (cols, offset) = if placement.wrapped {
                    let start = (piece * self.zone.width).min(self.cols);
                    let end = (start + self.zone.width).min(self.cols);
                    (start..end, self.zone.offset)
                } else {
                    (0..self.cols, 0)
                };
                out.push(Piece {
                    row,
                    screen_row: screen_row as usize,
                    cols,
                    offset,
                });
            }
        }
        out
    }

    /// The viewport cell under a screen cell, for clicks and selection.
    pub fn hit(&self, screen_row: isize, screen_col: usize) -> (isize, usize) {
        let last_col = self.cols.saturating_sub(1);
        for (row, placement) in self.rows.iter().enumerate() {
            let end = placement.top + placement.pieces as isize;
            if screen_row < placement.top || screen_row >= end {
                continue;
            }
            if !placement.wrapped {
                return (row as isize, screen_col.min(last_col));
            }
            let piece = (screen_row - placement.top) as usize;
            let within = screen_col
                .saturating_sub(self.zone.offset)
                .min(self.zone.width.saturating_sub(1));
            return (
                row as isize,
                (piece * self.zone.width + within).min(last_col),
            );
        }
        // Above everything still shown, or in the blank rows pushed off the
        // bottom: keep the distance, so a drag past the edge still extends a
        // selection the way it would without wrapping.
        match (self.rows.first(), self.rows.last()) {
            (Some(first), _) if screen_row < first.top => {
                (screen_row - first.top, screen_col.min(last_col))
            }
            _ => (
                (self.rows.len() as isize - 1).max(0),
                screen_col.min(last_col),
            ),
        }
    }

    /// The screen cell a viewport cell is drawn at, for the IME cursor;
    /// `None` when that row scrolled out of view.
    pub fn locate(&self, row: usize, col: usize) -> Option<(usize, usize)> {
        let placement = self.rows.get(row)?;
        let (screen_row, screen_col) = if placement.wrapped {
            let width = self.zone.width.max(1);
            let piece = (col / width).min(placement.pieces - 1);
            (
                placement.top + piece as isize,
                self.zone.offset + col - piece * width,
            )
        } else {
            (placement.top, col)
        };
        if screen_row < 0 || screen_row >= self.screen_rows as isize {
            return None;
        }
        Some((screen_row as usize, screen_col))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_panel::geometry::PanelSnap;

    fn pane() -> PaneRect {
        PaneRect {
            x: 100.,
            y: 50.,
            cols: 100,
            rows: 40,
            cell_width: 10.,
            cell_height: 20.,
        }
    }

    fn panel(x: f32, width: f32, height: f32) -> PanelGeometry {
        PanelGeometry {
            x,
            y: 50.,
            width,
            height,
            snap: PanelSnap::Raised,
            reserved_width: 0.,
            strip_extra_width: 0.,
        }
    }

    #[test]
    fn a_right_panel_leaves_two_columns_and_one_row_free() {
        // Panel starts at column 70 and covers rows 0..10.
        let z = wrap_zone(pane(), &panel(800., 300., 200.), DiffPanelPosition::Right).unwrap();
        assert_eq!(
            z,
            WrapZone {
                rows: 11,
                offset: 0,
                width: 68
            }
        );
    }

    #[test]
    fn a_left_panel_pushes_wrapped_rows_right() {
        // Covers columns 0..30 of the pane.
        let z = wrap_zone(pane(), &panel(50., 350., 200.), DiffPanelPosition::Left).unwrap();
        assert_eq!(z.offset, 32);
        assert_eq!(z.width, 68);
    }

    #[test]
    fn panes_the_panel_does_not_reach_are_left_alone() {
        // A left split ending before the panel.
        let mut left = pane();
        left.cols = 50;
        assert_eq!(
            wrap_zone(left, &panel(800., 300., 200.), DiffPanelPosition::Right),
            None
        );
        // A bottom split starting below the panel and its gap.
        let mut below = pane();
        below.y = 50. + 11. * 20.;
        assert_eq!(
            wrap_zone(below, &panel(800., 300., 200.), DiffPanelPosition::Right),
            None
        );
    }

    #[test]
    fn a_pane_almost_entirely_under_the_panel_is_not_wrapped() {
        let z = wrap_zone(pane(), &panel(150., 300., 200.), DiffPanelPosition::Right);
        assert_eq!(z, None);
    }

    fn zone() -> WrapZone {
        WrapZone {
            rows: 3,
            offset: 0,
            width: 10,
        }
    }

    #[test]
    fn rows_beside_the_panel_wrap_and_the_rest_moves_down() {
        // 6 screen rows; row 0 needs 25 cells, so 3 pieces.
        let l = layout(zone(), 30, &[25, 5, 30, 0, 0, 0], 3);
        assert_eq!(
            l.rows[..3],
            [
                RowPlacement {
                    top: 0,
                    pieces: 3,
                    wrapped: true
                },
                // Starts at screen row 3: past the zone, full width.
                RowPlacement {
                    top: 3,
                    pieces: 1,
                    wrapped: false
                },
                RowPlacement {
                    top: 4,
                    pieces: 1,
                    wrapped: false
                },
            ]
        );
        let pieces = l.pieces();
        assert_eq!(pieces[0].cols, 0..10);
        assert_eq!(pieces[2].cols, 20..30);
        assert_eq!(
            pieces[3],
            Piece {
                row: 1,
                screen_row: 3,
                cols: 0..30,
                offset: 0
            }
        );
        // Six screen rows, six pieces: trailing blank rows fall off the end.
        assert_eq!(pieces.len(), 6);
    }

    #[test]
    fn the_top_gives_way_so_the_last_content_row_stays_in_view() {
        // Every row is full: wrapping row 0 into 3 pieces overflows by 2.
        let l = layout(zone(), 30, &[30, 30, 30, 30], 4);
        assert!(l.rows[3].top + l.rows[3].pieces as isize <= 4);
        // The cursor row (3) is drawn on the last screen row.
        assert_eq!(l.locate(3, 0), Some((3, 0)));
        // The first row is (at least partly) gone.
        assert!(l.rows[0].top < 0);
    }

    #[test]
    fn an_empty_row_still_takes_one_screen_row() {
        let l = layout(zone(), 30, &[0, 0], 2);
        assert_eq!(l.rows[0].pieces, 1);
        assert_eq!(l.rows[1].top, 1);
    }

    #[test]
    fn hit_and_locate_agree() {
        let l = layout(zone(), 30, &[25, 5, 30, 0, 0, 0], 3);
        // Screen row 1, column 4 is the second piece of row 0.
        assert_eq!(l.hit(1, 4), (0, 14));
        assert_eq!(l.locate(0, 14), Some((1, 4)));
        // Past the piece's end, beside the panel: clamps to the piece.
        assert_eq!(l.hit(1, 25), (0, 19));
        // Full-width row below the zone.
        assert_eq!(l.hit(3, 22), (1, 22));
        assert_eq!(l.locate(1, 22), Some((3, 22)));
    }

    #[test]
    fn hit_with_an_offset_subtracts_it() {
        let mut z = zone();
        z.offset = 12;
        let l = layout(z, 30, &[25, 0, 0, 0], 1);
        assert_eq!(l.hit(1, 15), (0, 13));
        assert_eq!(l.locate(0, 13), Some((1, 15)));
        // Under the panel, left of the text: the piece's first cell.
        assert_eq!(l.hit(1, 2), (0, 10));
    }

    /// The whole chain a paint runs, from the panel's dragged height to the
    /// rows drawn, for a full-screen agent (Claude Code runs on the
    /// alternate screen): header text, blank middle, a full-width rule and
    /// status line at the bottom. Whatever the height, the terminal keeps
    /// its columns, and every row drawn below the panel's gap is drawn
    /// whole at full width; shrinking the panel gives rows back.
    #[test]
    fn every_raised_height_gives_the_rows_below_the_panel_their_full_width() {
        use crate::diff_panel::geometry::{panel_geometry, PanelArea, PanelRequest};
        use config::DiffPanelRaisedMode;

        let pane = pane();
        let area = PanelArea {
            left: pane.x,
            right: pane.x + pane.cols as f32 * pane.cell_width,
            top: pane.y,
            bottom: pane.y + pane.rows as f32 * pane.cell_height,
            strip_height: 0.,
        };
        let mut lens = vec![60; 10];
        lens.extend([0; 25]);
        lens.extend([100; 5]);
        let keep = lens.len();

        let mut height = 100.;
        while height < 760. {
            let geometry = panel_geometry(
                area,
                PanelRequest {
                    position: DiffPanelPosition::Right,
                    raised_mode: DiffPanelRaisedMode::Wrap,
                    width: 300.,
                    height: Some(height),
                    snap: 30.,
                    min_width: 200.,
                    min_height: 100.,
                    max_bottom: None,
                },
            )
            .unwrap();
            assert_eq!(geometry.reserved_width, 0., "height {height}");

            let zone = wrap_zone(pane, &geometry, DiffPanelPosition::Right).unwrap();
            let expected =
                ((geometry.bottom() - pane.y) / pane.cell_height).ceil() as usize + GAP_ROWS;
            assert_eq!(zone.rows, expected.min(pane.rows), "height {height}");
            let layout = layout(zone, pane.cols, &lens, keep);
            for (row, placement) in layout.rows.iter().enumerate() {
                if placement.top >= zone.rows as isize {
                    assert!(!placement.wrapped, "height {height}, row {row}");
                    assert_eq!(placement.pieces, 1, "height {height}, row {row}");
                }
            }
            for piece in layout.pieces() {
                if piece.screen_row >= zone.rows {
                    assert_eq!(piece.cols, 0..pane.cols, "height {height}");
                    assert_eq!(piece.offset, 0, "height {height}");
                }
            }
            height += 7.;
        }
        // Walk back down: the zone shrinks with the panel, never sticks.
        let mut previous = 0;
        let mut height = 760.;
        while height >= 100. {
            let geometry = panel_geometry(
                area,
                PanelRequest {
                    position: DiffPanelPosition::Right,
                    raised_mode: DiffPanelRaisedMode::Wrap,
                    width: 300.,
                    height: Some(height),
                    snap: 30.,
                    min_width: 200.,
                    min_height: 100.,
                    max_bottom: None,
                },
            )
            .unwrap();
            let zone = wrap_zone(pane, &geometry, DiffPanelPosition::Right).unwrap();
            if previous > 0 {
                assert!(zone.rows <= previous, "height {height}");
            }
            previous = zone.rows;
            height -= 7.;
        }
    }

    #[test]
    fn rules_and_padding_are_filler_and_text_is_not() {
        for filler in ["", " ", "─", "━", "═", "│", "╭", "╯", "┼"] {
            assert!(is_wrap_filler(filler), "{filler:?}");
        }
        for text in ["a", "-", "█", "░", "⏺", "✳", "e\u{301}"] {
            assert!(!is_wrap_filler(text), "{text:?}");
        }
    }

    #[test]
    fn a_panel_ending_part_way_into_a_row_frees_the_row_after_the_next() {
        // Bottom at 255px: 5px into row 10, which is beside the panel; row
        // 11 is the gap; row 12 is full width again.
        let z = wrap_zone(pane(), &panel(800., 300., 205.), DiffPanelPosition::Right).unwrap();
        assert_eq!(z.rows, 12);
    }

    #[test]
    fn rules_are_rows_of_line_art_only() {
        let rule = |cells: &[&str]| is_rule(cells.iter().map(|text| rule_cell(text)));
        assert!(rule(&["─"; 40]));
        let mut padded = vec![" "; 4];
        padded.extend(["─"; 20]);
        assert!(rule(&padded));
        assert!(!rule(&["─"; 7]));
        assert!(!rule(&[" "; 40]));
        let mut labelled = vec!["─"; 20];
        labelled.push("a");
        assert!(!rule(&labelled));
    }

    #[test]
    fn the_input_starts_at_the_rule_above_the_cursor() {
        // Output, the box's top rule (3), three prompt rows, its bottom rule
        // (7), status.
        let rules = [false, false, false, true, false, false, false, true, false];
        assert_eq!(input_top(&rules, 6), 3);
        assert_eq!(input_top(&rules, 4), 3);
        // A rule on the cursor row itself is not above it.
        assert_eq!(input_top(&rules, 3), 3);
    }

    #[test]
    fn with_no_rule_above_the_input_is_the_cursor_row() {
        assert_eq!(input_top(&[false; 6], 4), 4);
        // Too far above to be this input's rule.
        let mut rules = vec![false; INPUT_SEARCH_ROWS + 5];
        rules[0] = true;
        let cursor = INPUT_SEARCH_ROWS + 3;
        assert_eq!(input_top(&rules, cursor), cursor);
        // A cursor past the end is clamped, not a panic.
        assert_eq!(input_top(&[false, true, false], 10), 1);
        assert_eq!(input_top(&[], 3), 0);
    }

    #[test]
    fn rows_scrolled_off_the_top_are_not_located() {
        let l = layout(zone(), 30, &[30, 30, 30, 30], 4);
        // Row 0 ended above the viewport.
        assert!(l.rows[0].top + l.rows[0].pieces as isize <= 0);
        assert_eq!(l.locate(0, 0), None);
    }
}
