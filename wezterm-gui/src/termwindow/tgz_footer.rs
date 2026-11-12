//! The sidebar footer as data: which buttons it has, in which rows.
//!
//! The footer is a list of rows, top to bottom, each a list of buttons that
//! share the row equally. Where the list comes from, in order:
//!
//! 1. `sidebar_footer` in the Lua config, when set. Lua wins, and the footer
//!    then cannot be rearranged by hand.
//! 2. The arrangement the user dragged together ("Customize buttons…"), kept
//!    in `tgz_ui_state`.
//! 3. [`default_footer_rows`].
//!
//! Everything here is pure; painting and the drag live in `render/sidebar.rs`
//! and `mouseevent.rs`.

use config::SidebarFooterItem as Item;
use std::collections::HashMap;
use termwiz::cell::unicode_column_width;

pub(crate) type FooterRows = Vec<Vec<Item>>;

/// How wide each button is relative to the others in its row. A button
/// without an entry weighs 1, so an empty map is equal shares.
pub(crate) type FooterWeights = HashMap<Item, f32>;

/// Most rows the footer may have: each one is a row the tab list loses.
pub(crate) const MAX_FOOTER_ROWS: usize = 5;

/// Most buttons a row may hold unless `sidebar_footer_max_per_row` says
/// otherwise: more than that and none of them is a comfortable target at
/// ordinary sidebar widths.
#[cfg(test)]
pub(crate) const MAX_ROW_ITEMS: usize = 5;

/// The range `sidebar_footer_max_per_row` is held to: a row needs a button,
/// and there are seven buttons in all.
pub(crate) fn clamp_max_row_items(configured: usize) -> usize {
    configured.clamp(1, ALL_ITEMS.len())
}

/// The weight of `item`; 1 when unset or nonsense.
pub(crate) fn weight_of(weights: &FooterWeights, item: Item) -> f32 {
    weights
        .get(&item)
        .copied()
        .filter(|weight| weight.is_finite() && *weight > 0.)
        .unwrap_or(1.)
}

/// An item by the name a person would write: the Lua variant (`NewTab`) or
/// the state-file name (`new_tab`), in any case.
pub(crate) fn item_from_loose_name(name: &str) -> Option<Item> {
    let wanted: String = name
        .chars()
        .filter(|c| *c != '_')
        .flat_map(char::to_lowercase)
        .collect();
    ALL_ITEMS.iter().copied().find(|item| {
        let variant = format!("{item:?}").to_lowercase();
        variant == wanted || item_name(*item).replace('_', "") == wanted
    })
}

/// Share `total` among `weights` in proportion, with nobody below `min`:
/// whoever would fall short gets `min` and the rest share what is left.
pub(crate) fn distribute(total: f32, weights: &[f32], min: f32) -> Vec<f32> {
    let mut out = vec![min; weights.len()];
    let mut clamped = vec![false; weights.len()];
    loop {
        let fixed = clamped.iter().filter(|c| **c).count() as f32 * min;
        let free = (total - fixed).max(0.);
        let open: f32 = weights
            .iter()
            .zip(&clamped)
            .filter(|(_, c)| !**c)
            .map(|(weight, _)| *weight)
            .sum();
        let mut changed = false;
        for idx in 0..weights.len() {
            if clamped[idx] {
                continue;
            }
            let share = if open > 0. {
                free * weights[idx] / open
            } else {
                0.
            };
            if share < min {
                clamped[idx] = true;
                out[idx] = min;
                changed = true;
            } else {
                out[idx] = share;
            }
        }
        if !changed {
            return out;
        }
    }
}

const ALL_ITEMS: [Item; 7] = [
    Item::Worktree,
    Item::Changes,
    Item::Toolbelt,
    Item::SecureInput,
    Item::Agent,
    Item::Ssh,
    Item::NewTab,
];

/// The footer as shipped: the view toggles on top, the agent and ssh
/// launchers as halves below, New Tab across the bottom edge.
pub(crate) fn default_footer_rows() -> FooterRows {
    vec![
        vec![
            Item::Worktree,
            Item::Changes,
            Item::Toolbelt,
            Item::SecureInput,
        ],
        vec![Item::Agent, Item::Ssh],
        vec![Item::NewTab],
    ]
}

/// A view toggle (lit while its panel or mode is on), as opposed to a button
/// that opens something.
pub(crate) fn is_toggle(item: Item) -> bool {
    toggle_label(item).is_some()
}

/// Text beside a toggle's icon once its row is wide enough.
pub(crate) fn toggle_label(item: Item) -> Option<&'static str> {
    match item {
        Item::Worktree => Some("Files"),
        Item::Changes => Some("Diff"),
        Item::Toolbelt => Some("Tools"),
        Item::SecureInput => Some("Lock"),
        Item::Agent | Item::Ssh | Item::NewTab => None,
    }
}

/// Name of an item in the state file.
pub(crate) fn item_name(item: Item) -> &'static str {
    match item {
        Item::Worktree => "worktree",
        Item::Changes => "changes",
        Item::Toolbelt => "toolbelt",
        Item::SecureInput => "lock",
        Item::Agent => "agent",
        Item::Ssh => "ssh",
        Item::NewTab => "new_tab",
    }
}

pub(crate) fn item_from_name(name: &str) -> Option<Item> {
    ALL_ITEMS
        .iter()
        .copied()
        .find(|item| item_name(*item) == name)
}

pub(crate) fn rows_to_names(rows: &FooterRows) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|item| item_name(*item).to_string())
                .collect()
        })
        .collect()
}

/// Unknown names are dropped: a state file written by a newer build must not
/// take the footer down with it.
pub(crate) fn rows_from_names(names: &[Vec<String>]) -> FooterRows {
    names
        .iter()
        .map(|row| row.iter().filter_map(|name| item_from_name(name)).collect())
        .collect()
}

/// Make `rows` well formed: each button once (its first place wins), no empty
/// rows, at most [`MAX_FOOTER_ROWS`] (the overflow joins the last row).
///
/// `append_missing` adds the buttons that are not placed at all to the first
/// row. That is for an arrangement made by dragging, where nothing can be
/// removed, so a button this build added must not be lost. A Lua list is
/// taken at its word: what it leaves out is not shown.
#[cfg(test)]
pub(crate) fn normalize_footer_rows(rows: &[Vec<Item>], append_missing: bool) -> FooterRows {
    normalize_footer_rows_capped(rows, append_missing, MAX_ROW_ITEMS)
}

/// [`normalize_footer_rows`] with at most `max_row` buttons to a row.
pub(crate) fn normalize_footer_rows_capped(
    rows: &[Vec<Item>],
    append_missing: bool,
    max_row: usize,
) -> FooterRows {
    let max_row = max_row.max(1);
    let mut seen: Vec<Item> = Vec::new();
    let mut out: FooterRows = Vec::new();
    for row in rows {
        let mut kept = Vec::new();
        for item in row {
            if !seen.contains(item) {
                seen.push(*item);
                kept.push(*item);
            }
        }
        if kept.is_empty() {
            continue;
        }
        // A row over the cap continues on the next one.
        for chunk in kept.chunks(max_row) {
            if out.len() < MAX_FOOTER_ROWS {
                out.push(chunk.to_vec());
            } else if let Some(last) = out.last_mut() {
                last.extend_from_slice(chunk);
            }
        }
    }
    if append_missing {
        let missing: Vec<Item> = ALL_ITEMS
            .iter()
            .copied()
            .filter(|item| !seen.contains(item))
            .collect();
        if !missing.is_empty() {
            match out.first_mut() {
                Some(first) => first.extend(missing),
                None => out.push(missing),
            }
        }
    }
    out
}

/// The rows to draw: `rows` without the buttons that have nothing to offer
/// right now (ssh with no host, Changes when the panel is disabled) and
/// without the rows that leaves empty.
pub(crate) fn visible_footer_rows(
    rows: &[Vec<Item>],
    available: impl Fn(Item) -> bool,
) -> FooterRows {
    rows.iter()
        .map(|row| {
            row.iter()
                .copied()
                .filter(|item| available(*item))
                .collect::<Vec<_>>()
        })
        .filter(|row| !row.is_empty())
        .collect()
}

/// One button's share of its row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FooterSlot {
    pub item: Item,
    pub x: f32,
    pub w: f32,
}

/// [`footer_row_layout_weighted`] with equal shares.
#[cfg(test)]
pub(crate) fn footer_row_layout(
    item_x: f32,
    item_w: f32,
    min_side: f32,
    gap: f32,
    row: &[Item],
) -> Vec<FooterSlot> {
    footer_row_layout_weighted(item_x, item_w, min_side, gap, row, &FooterWeights::new())
}

/// Lay `row` out across `item_w` from `item_x`: shares in proportion to the
/// buttons' weights, one `gap` apart, the last taking what rounding leaves
/// so the row reaches its right edge. No button is narrower than `min_side`;
/// the ones that do not fit at that size are dropped from the end.
pub(crate) fn footer_row_layout_weighted(
    item_x: f32,
    item_w: f32,
    min_side: f32,
    gap: f32,
    row: &[Item],
    weights: &FooterWeights,
) -> Vec<FooterSlot> {
    let right = item_x + item_w;
    let min_side = min_side.min(item_w).max(1.);
    let fits = (((item_w + gap) / (min_side + gap)).floor() as usize).max(1);
    let count = row.len().min(fits);
    if count == 0 {
        return Vec::new();
    }
    let row_weights: Vec<f32> = row
        .iter()
        .take(count)
        .map(|item| weight_of(weights, *item))
        .collect();
    let widths = distribute(
        (item_w - gap * (count as f32 - 1.)).max(1.),
        &row_weights,
        min_side,
    );
    let mut x = item_x;
    row.iter()
        .take(count)
        .zip(widths)
        .enumerate()
        .map(|(idx, (item, share))| {
            let w = if idx + 1 == count {
                (right - x).max(1.)
            } else {
                share.max(1.)
            };
            let slot = FooterSlot { item: *item, x, w };
            x += w + gap;
            slot
        })
        .collect()
}

/// Whether the toggles of a row show text beside their icons. All or
/// nothing, so a row never mixes labelled and bare toggles: every label has
/// to fit the narrowest toggle. `chrome` is what a toggle spends on padding
/// and the gap between icon and label.
pub(crate) fn toggles_labelled(slots: &[FooterSlot], cell_width: f32, chrome: f32) -> bool {
    let toggles: Vec<&FooterSlot> = slots.iter().filter(|slot| is_toggle(slot.item)).collect();
    let Some(narrowest) = toggles
        .iter()
        .map(|slot| slot.w)
        .min_by(|a, b| a.total_cmp(b))
    else {
        return false;
    };
    let room = narrowest - chrome - cell_width;
    toggles.iter().all(|slot| {
        toggle_label(slot.item)
            .is_some_and(|label| unicode_column_width(label, None) as f32 * cell_width <= room)
    })
}

/// A button as it was painted, for the drag to aim at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PaintedSlot {
    pub item: Item,
    /// Which drawn row, from the top.
    pub row: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Where a dragged button goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FooterTarget {
    Before(Item),
    After(Item),
    /// A row of its own above the others.
    NewTopRow,
}

/// Where the button being dragged belongs with the pointer at `pointer`:
/// beside the button under the pointer, or in a new row when the pointer is
/// above the footer. `None` leaves things as they are.
pub(crate) fn footer_drag_target(
    slots: &[PaintedSlot],
    rows: &[Vec<Item>],
    dragged: Item,
    pointer: (f32, f32),
) -> Option<FooterTarget> {
    let top = slots
        .iter()
        .map(|slot| slot.y)
        .min_by(|a, b| a.total_cmp(b))?;
    let (px, py) = pointer;
    if py < top {
        return Some(FooterTarget::NewTopRow);
    }
    let under = slots.iter().find(|slot| {
        px >= slot.x && px < slot.x + slot.w && py >= slot.y && py < slot.y + slot.h
    })?;
    if under.item == dragged {
        return None;
    }
    let place = |item: Item| {
        rows.iter().enumerate().find_map(|(row_idx, row)| {
            row.iter()
                .position(|candidate| *candidate == item)
                .map(|idx| (row_idx, idx))
        })
    };
    let (target_row, target_idx) = place(under.item)?;
    let after = match place(dragged) {
        // Within a row the two swap sides: a button dragged rightwards lands
        // after the one it passes.
        Some((row, idx)) if row == target_row => idx < target_idx,
        // From another row it lands on the side of the button the pointer
        // is on.
        _ => px >= under.x + under.w * 0.5,
    };
    Some(if after {
        FooterTarget::After(under.item)
    } else {
        FooterTarget::Before(under.item)
    })
}

/// What dragging the boundary on a button's right does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FooterResize {
    /// The row's buttons and their new widths, left to right.
    Widths(Vec<(Item, f32)>),
    /// The buttons to the right cannot get any smaller: this one, the last
    /// of the row, has to move to the row above to make room.
    Overflow(Item),
    Nothing,
}

/// Drag the right edge of `item` to `pointer_x`. `row` is the buttons of its
/// row as painted, left to right.
///
/// The button takes the width asked for. The ones left of it keep theirs;
/// the ones right of it share what remains in their old proportions, none
/// going below `min_side`. Once they are all at that size, growing further
/// pushes the row's last button out.
pub(crate) fn footer_resize(
    row: &[PaintedSlot],
    item: Item,
    pointer_x: f32,
    min_side: f32,
    gap: f32,
) -> FooterResize {
    let Some(idx) = row.iter().position(|slot| slot.item == item) else {
        return FooterResize::Nothing;
    };
    let after = row.len() - idx - 1;
    // The last button's right edge is the row's: nothing to drag against.
    let (Some(last), true) = (row.last(), after > 0) else {
        return FooterResize::Nothing;
    };
    let right = last.x + last.w;
    let width = (pointer_x - row[idx].x).max(min_side);
    let remaining = right - (row[idx].x + width) - gap * after as f32;
    if remaining < after as f32 * min_side {
        return FooterResize::Overflow(last.item);
    }
    let old: Vec<f32> = row[idx + 1..].iter().map(|slot| slot.w).collect();
    let shares = distribute(remaining, &old, min_side);
    FooterResize::Widths(
        row[..idx]
            .iter()
            .map(|slot| (slot.item, slot.w))
            .chain(std::iter::once((item, width)))
            .chain(row[idx + 1..].iter().map(|slot| slot.item).zip(shares))
            .collect(),
    )
}

/// `rows` with `item` moved to the end of the row above its own, or to a new
/// row there when that one is full or there is none. Unchanged when neither
/// is possible.
#[cfg(test)]
pub(crate) fn footer_move_up(rows: &[Vec<Item>], item: Item) -> FooterRows {
    footer_move_up_capped(rows, item, MAX_ROW_ITEMS)
}

/// [`footer_move_up`] with at most `max_row` buttons to a row.
pub(crate) fn footer_move_up_capped(rows: &[Vec<Item>], item: Item, max_row: usize) -> FooterRows {
    let Some(row_idx) = rows.iter().position(|row| row.contains(&item)) else {
        return rows.to_vec();
    };
    let mut out = rows.to_vec();
    if row_idx > 0 && out[row_idx - 1].len() < max_row {
        out[row_idx].retain(|other| *other != item);
        out[row_idx - 1].push(item);
    } else if out.len() < MAX_FOOTER_ROWS && out[row_idx].len() > 1 {
        out[row_idx].retain(|other| *other != item);
        out.insert(row_idx, vec![item]);
    }
    out.retain(|row| !row.is_empty());
    out
}

/// Give `item` the average weight of the buttons it now shares a row with,
/// so that a button joining a row neither dwarfs it nor vanishes in it.
pub(crate) fn settle_weight(weights: &mut FooterWeights, rows: &[Vec<Item>], item: Item) {
    let others: Vec<f32> = rows
        .iter()
        .find(|row| row.contains(&item))
        .map(|row| {
            row.iter()
                .filter(|other| **other != item)
                .map(|other| weight_of(weights, *other))
                .collect()
        })
        .unwrap_or_default();
    if others.is_empty() {
        weights.remove(&item);
    } else {
        weights.insert(item, others.iter().sum::<f32>() / others.len() as f32);
    }
}

/// `rows` with `item` moved to `target`. A row the move empties is removed.
/// A move that cannot be made (an unknown target, a row already holding
/// [`MAX_ROW_ITEMS`], a new row beyond [`MAX_FOOTER_ROWS`]) returns `rows`
/// unchanged.
#[cfg(test)]
pub(crate) fn footer_move(rows: &[Vec<Item>], item: Item, target: FooterTarget) -> FooterRows {
    footer_move_capped(rows, item, target, MAX_ROW_ITEMS)
}

/// [`footer_move`] with at most `max_row` buttons to a row.
pub(crate) fn footer_move_capped(
    rows: &[Vec<Item>],
    item: Item,
    target: FooterTarget,
    max_row: usize,
) -> FooterRows {
    let mut out: FooterRows = rows
        .iter()
        .map(|row| row.iter().copied().filter(|other| *other != item).collect())
        .collect();
    match target {
        FooterTarget::NewTopRow => {
            out.retain(|row: &Vec<Item>| !row.is_empty());
            if out.len() >= MAX_FOOTER_ROWS {
                return rows.to_vec();
            }
            out.insert(0, vec![item]);
        }
        FooterTarget::Before(anchor) | FooterTarget::After(anchor) => {
            let Some((row_idx, idx)) = out.iter().enumerate().find_map(|(row_idx, row)| {
                row.iter()
                    .position(|candidate| *candidate == anchor)
                    .map(|idx| (row_idx, idx))
            }) else {
                return rows.to_vec();
            };
            if out[row_idx].len() >= max_row {
                return rows.to_vec();
            }
            let at = if matches!(target, FooterTarget::After(_)) {
                idx + 1
            } else {
                idx
            };
            out[row_idx].insert(at, item);
            out.retain(|row| !row.is_empty());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use Item::*;

    #[test]
    fn normalize_keeps_each_button_once_and_drops_empty_rows() {
        let rows = vec![vec![NewTab, Agent], vec![], vec![Agent, Ssh], vec![NewTab]];
        assert_eq!(
            normalize_footer_rows(&rows, false),
            vec![vec![NewTab, Agent], vec![Ssh]]
        );
    }

    #[test]
    fn a_dragged_arrangement_never_loses_a_button_but_lua_may_omit_one() {
        let rows = vec![vec![NewTab], vec![Agent]];
        // Lua: what is left out is not shown.
        assert_eq!(normalize_footer_rows(&rows, false), rows);
        // Dragged: the rest joins the first row.
        let full = normalize_footer_rows(&rows, true);
        assert_eq!(full.len(), 2);
        for item in ALL_ITEMS.iter() {
            assert_eq!(full.iter().flatten().filter(|i| *i == item).count(), 1);
        }
        // Nothing at all still yields every button.
        assert_eq!(
            normalize_footer_rows(&[], true).concat().len(),
            ALL_ITEMS.len()
        );
        assert_eq!(
            normalize_footer_rows(&default_footer_rows(), true),
            default_footer_rows()
        );
    }

    #[test]
    fn rows_beyond_the_cap_join_the_last_row() {
        let rows: Vec<Vec<Item>> = ALL_ITEMS.iter().map(|item| vec![*item]).collect();
        let capped = normalize_footer_rows(&rows, false);
        assert_eq!(capped.len(), MAX_FOOTER_ROWS);
        assert_eq!(capped.concat().len(), ALL_ITEMS.len());
    }

    #[test]
    fn unavailable_buttons_and_the_rows_they_empty_are_not_drawn() {
        let visible = visible_footer_rows(&default_footer_rows(), |item| item != Ssh);
        assert_eq!(visible[1], vec![Agent]);
        let no_launchers =
            visible_footer_rows(&default_footer_rows(), |item| !matches!(item, Agent | Ssh));
        assert_eq!(no_launchers.len(), 2);
    }

    #[test]
    fn state_file_names_round_trip_and_ignore_unknown_ones() {
        let rows = default_footer_rows();
        assert_eq!(rows_from_names(&rows_to_names(&rows)), rows);
        let names = vec![vec!["new_tab".to_string(), "hologram".to_string()]];
        assert_eq!(rows_from_names(&names), vec![vec![NewTab]]);
    }

    #[test]
    fn a_row_tiles_its_width_and_drops_what_does_not_fit_from_the_end() {
        let row = [Worktree, Changes, Toolbelt, SecureInput];
        let mut width = 140.;
        while width <= 800. {
            let slots = footer_row_layout(8., width, 28., 4., &row);
            assert_eq!(slots[0].x, 8.);
            for pair in slots.windows(2) {
                assert!((pair[0].x + pair[0].w + 4. - pair[1].x).abs() < 0.01);
                assert!((pair[0].w - pair[1].w).abs() < 0.01);
            }
            let last = slots.last().unwrap();
            assert!((last.x + last.w - (8. + width)).abs() < 0.01);
            width += 4.;
        }
        // 100px at a 38px minimum: two fit, the first two.
        let narrow = footer_row_layout(8., 100., 38., 4., &row);
        assert_eq!(
            narrow.iter().map(|s| s.item).collect::<Vec<_>>(),
            [Worktree, Changes]
        );
        // The first button survives any width.
        let tiny = footer_row_layout(8., 20., 38., 4., &row);
        assert_eq!(tiny.len(), 1);
        assert!(tiny[0].w <= 20.);
        assert!(footer_row_layout(8., 200., 28., 4., &[]).is_empty());
    }

    #[test]
    fn toggle_labels_are_all_or_nothing() {
        let row = [Worktree, Changes, Toolbelt, SecureInput];
        let wide = footer_row_layout(8., 784., 28., 4., &row);
        assert!(toggles_labelled(&wide, 14., 18.));
        let default = footer_row_layout(8., 264., 28., 4., &row);
        assert!(!toggles_labelled(&default, 14., 18.));
        // A row without toggles has nothing to label.
        let launch = footer_row_layout(8., 784., 28., 4., &[Agent, Ssh]);
        assert!(!toggles_labelled(&launch, 14., 18.));
        // A lone toggle beside a launcher has half the row to itself.
        let mixed = footer_row_layout(8., 400., 28., 4., &[Worktree, NewTab]);
        assert!(toggles_labelled(&mixed, 14., 18.));
    }

    fn painted(rows: &FooterRows) -> Vec<PaintedSlot> {
        // 300px wide rows of 30px, stacked from y = 100.
        rows.iter()
            .enumerate()
            .flat_map(|(row_idx, row)| {
                footer_row_layout(0., 300., 28., 0., row)
                    .into_iter()
                    .map(move |slot| PaintedSlot {
                        item: slot.item,
                        row: row_idx,
                        x: slot.x,
                        y: 100. + row_idx as f32 * 30.,
                        w: slot.w,
                        h: 30.,
                    })
            })
            .collect()
    }

    #[test]
    fn weights_set_the_shares_and_nobody_goes_below_the_minimum() {
        assert_eq!(distribute(300., &[1., 1., 1.], 28.), vec![100., 100., 100.]);
        assert_eq!(distribute(300., &[2., 1.], 28.), vec![200., 100.]);
        // The light one is held at the minimum; the others share the rest.
        assert_eq!(
            distribute(300., &[10., 10., 0.1], 30.),
            vec![135., 135., 30.]
        );
        assert!(distribute(100., &[], 28.).is_empty());

        let mut weights = FooterWeights::new();
        weights.insert(Agent, 3.);
        let slots = footer_row_layout_weighted(0., 400., 28., 0., &[Agent, Ssh], &weights);
        assert_eq!((slots[0].w, slots[1].w), (300., 100.));
        assert_eq!(slots[1].x, 300.);
        // Nonsense weights count as 1.
        weights.insert(Ssh, f32::NAN);
        weights.insert(Agent, -2.);
        let slots = footer_row_layout_weighted(0., 400., 28., 0., &[Agent, Ssh], &weights);
        assert_eq!((slots[0].w, slots[1].w), (200., 200.));
    }

    #[test]
    fn widening_a_button_squeezes_the_ones_after_it_then_pushes_the_last_out() {
        // Three buttons of 100px, no gap, 30px minimum.
        let rows: FooterRows = vec![vec![Worktree, Changes, Toolbelt]];
        let row = painted(&rows);
        // Worktree's edge to 160: the other two share the remaining 140.
        assert_eq!(
            footer_resize(&row, Worktree, 160., 30., 0.),
            FooterResize::Widths(vec![(Worktree, 160.), (Changes, 70.), (Toolbelt, 70.)])
        );
        // The middle one: the first keeps its width.
        assert_eq!(
            footer_resize(&row, Changes, 250., 30., 0.),
            FooterResize::Widths(vec![(Worktree, 100.), (Changes, 150.), (Toolbelt, 50.)])
        );
        // Never narrower than the minimum itself.
        assert_eq!(
            footer_resize(&row, Worktree, 5., 30., 0.),
            FooterResize::Widths(vec![(Worktree, 30.), (Changes, 135.), (Toolbelt, 135.)])
        );
        // To 240 the others are exactly at their minimum; beyond, one goes.
        assert!(matches!(
            footer_resize(&row, Worktree, 240., 30., 0.),
            FooterResize::Widths(_)
        ));
        assert_eq!(
            footer_resize(&row, Worktree, 250., 30., 0.),
            FooterResize::Overflow(Toolbelt)
        );
        // The last button has no edge of its own to drag.
        assert_eq!(
            footer_resize(&row, Toolbelt, 250., 30., 0.),
            FooterResize::Nothing
        );
        assert_eq!(
            footer_resize(&row, Agent, 250., 30., 0.),
            FooterResize::Nothing
        );
    }

    #[test]
    fn a_pushed_out_button_joins_the_row_above_or_starts_one() {
        let rows = default_footer_rows();
        // Ssh leaves the launch row for the end of the view strip.
        let moved = footer_move_up(&rows, Ssh);
        assert_eq!(
            moved[0],
            vec![Worktree, Changes, Toolbelt, SecureInput, Ssh]
        );
        assert_eq!(moved[1], vec![Agent]);
        // That row is now full: the next one starts a row of its own.
        let rows = vec![moved[0].clone(), vec![Agent, NewTab]];
        let moved = footer_move_up(&rows, NewTab);
        assert_eq!(moved, vec![rows[0].clone(), vec![NewTab], vec![Agent]]);
        // From the top row: a new row above it.
        let rows = vec![vec![Agent, Ssh]];
        assert_eq!(footer_move_up(&rows, Ssh), vec![vec![Ssh], vec![Agent]]);
        // Alone in the top row there is nowhere to go.
        let rows = vec![vec![Agent]];
        assert_eq!(footer_move_up(&rows, Agent), rows);
    }

    #[test]
    fn a_row_holds_five_buttons_at_most() {
        let full = vec![
            vec![Worktree, Changes, Toolbelt, SecureInput, Agent],
            vec![Ssh, NewTab],
        ];
        assert_eq!(footer_move(&full, Ssh, FooterTarget::After(Agent)), full);
        // Within the full row a button still moves.
        assert_eq!(
            footer_move(&full, Agent, FooterTarget::Before(Worktree))[0],
            vec![Agent, Worktree, Changes, Toolbelt, SecureInput]
        );
        // A longer list is cut into rows of five.
        let long = vec![ALL_ITEMS.to_vec()];
        let rows = normalize_footer_rows(&long, false);
        assert_eq!(rows.iter().map(Vec::len).collect::<Vec<_>>(), [5, 2]);
    }

    #[test]
    fn the_row_cap_is_adjustable() {
        let long = vec![ALL_ITEMS.to_vec()];
        let rows = normalize_footer_rows_capped(&long, false, 3);
        assert_eq!(rows.iter().map(Vec::len).collect::<Vec<_>>(), [3, 3, 1]);
        let all = normalize_footer_rows_capped(&long, false, 7);
        assert_eq!(all.len(), 1);
        // Two to a row: a third is refused, and a pushed-out one starts a row.
        let rows = vec![vec![Agent, Ssh], vec![NewTab, Worktree]];
        assert_eq!(
            footer_move_capped(&rows, NewTab, FooterTarget::After(Ssh), 2),
            rows
        );
        assert_eq!(
            footer_move_up_capped(&rows, Worktree, 2),
            vec![vec![Agent, Ssh], vec![Worktree], vec![NewTab]]
        );
        assert_eq!(clamp_max_row_items(0), 1);
        assert_eq!(clamp_max_row_items(5), 5);
        assert_eq!(clamp_max_row_items(99), 7);
    }

    #[test]
    fn a_button_joining_a_row_takes_its_average_weight() {
        let rows = vec![vec![Agent, Ssh, NewTab]];
        let mut weights = FooterWeights::new();
        weights.insert(Agent, 0.5);
        weights.insert(Ssh, 1.5);
        weights.insert(NewTab, 9.);
        settle_weight(&mut weights, &rows, NewTab);
        assert_eq!(weight_of(&weights, NewTab), 1.);
        // Alone in a row a weight means nothing and is dropped.
        settle_weight(&mut weights, &[vec![Agent]], Agent);
        assert!(!weights.contains_key(&Agent));
    }

    #[test]
    fn loose_names_accept_lua_and_state_spellings() {
        assert_eq!(item_from_loose_name("NewTab"), Some(NewTab));
        assert_eq!(item_from_loose_name("new_tab"), Some(NewTab));
        assert_eq!(item_from_loose_name("SECUREINPUT"), Some(SecureInput));
        assert_eq!(item_from_loose_name("lock"), Some(SecureInput));
        assert_eq!(item_from_loose_name("hologram"), None);
    }

    #[test]
    fn dragging_within_a_row_swaps_with_the_button_passed() {
        let rows = default_footer_rows();
        let slots = painted(&rows);
        // Agent (left half of row 1) dragged over Ssh (right half).
        let target = footer_drag_target(&slots, &rows, Agent, (200., 140.)).unwrap();
        assert_eq!(target, FooterTarget::After(Ssh));
        assert_eq!(footer_move(&rows, Agent, target)[1], vec![Ssh, Agent]);
        // And back.
        let rows = footer_move(&rows, Agent, target);
        let slots = painted(&rows);
        let target = footer_drag_target(&slots, &rows, Agent, (20., 140.)).unwrap();
        assert_eq!(target, FooterTarget::Before(Ssh));
        assert_eq!(footer_move(&rows, Agent, target)[1], vec![Agent, Ssh]);
    }

    #[test]
    fn dragging_into_another_row_lands_on_the_pointers_side() {
        let rows = default_footer_rows();
        let slots = painted(&rows);
        // New Tab onto the left part of Agent: before it.
        let target = footer_drag_target(&slots, &rows, NewTab, (20., 140.)).unwrap();
        assert_eq!(target, FooterTarget::Before(Agent));
        let moved = footer_move(&rows, NewTab, target);
        // New Tab's own row is gone with it.
        assert_eq!(moved.len(), 2);
        assert_eq!(moved[1], vec![NewTab, Agent, Ssh]);
        // Onto the right part of Ssh: after it.
        let target = footer_drag_target(&slots, &rows, NewTab, (290., 140.)).unwrap();
        assert_eq!(target, FooterTarget::After(Ssh));
    }

    #[test]
    fn dragging_above_the_footer_makes_a_row_and_settles() {
        let rows = default_footer_rows();
        let slots = painted(&rows);
        let target = footer_drag_target(&slots, &rows, Ssh, (150., 90.)).unwrap();
        assert_eq!(target, FooterTarget::NewTopRow);
        let moved = footer_move(&rows, Ssh, target);
        assert_eq!(moved[0], vec![Ssh]);
        assert_eq!(moved.len(), 4);
        // Still above the footer: the same answer changes nothing more.
        assert_eq!(footer_move(&moved, Ssh, FooterTarget::NewTopRow), moved);
    }

    #[test]
    fn nothing_moves_over_itself_over_a_gap_or_past_the_row_cap() {
        let rows = default_footer_rows();
        let slots = painted(&rows);
        assert_eq!(footer_drag_target(&slots, &rows, Agent, (20., 140.)), None);
        // Right of every button.
        assert_eq!(footer_drag_target(&slots, &rows, Agent, (400., 140.)), None);
        assert_eq!(footer_drag_target(&[], &rows, Agent, (20., 140.)), None);

        let full: FooterRows = vec![
            vec![Worktree],
            vec![Changes],
            vec![Toolbelt],
            vec![SecureInput],
            vec![Agent, Ssh, NewTab],
        ];
        assert_eq!(footer_move(&full, Ssh, FooterTarget::NewTopRow), full);
        // A button alone in its row may still go to the top: the count holds.
        let moved = footer_move(&full, SecureInput, FooterTarget::NewTopRow);
        assert_eq!(moved[0], vec![SecureInput]);
        assert_eq!(moved.len(), MAX_FOOTER_ROWS);
    }
}
