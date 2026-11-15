//! Pane layout presets and balancing for a tab (TGZTerminal).
//!
//! Both operations are pure tree arithmetic: a preset builds a fresh split
//! tree around the panes a tab already has, and balancing re-divides an
//! existing tree's space without touching its shape. Neither spawns or kills
//! anything; `Tab` resizes the panes afterwards. The functions are generic
//! over the leaf type so the arithmetic can be tested without real panes.

use crate::tab::{SplitDirection, SplitDirectionAndSize};
use config::keyassignment::PaneLayout;
use wezterm_term::TerminalSize;

type SizedTree<L> = bintree::Tree<L, SplitDirectionAndSize>;

/// The shape of a layout before it is given panes and sizes. `weights` are
/// the shares of the split's space its (first, second) children get.
enum Shape {
    Leaf,
    Split {
        direction: SplitDirection,
        weights: (f32, f32),
        first: Box<Shape>,
        second: Box<Shape>,
    },
}

/// `count` leaves in a row along `direction`, equal shares.
fn chain(direction: SplitDirection, count: usize) -> Shape {
    if count <= 1 {
        return Shape::Leaf;
    }
    Shape::Split {
        direction,
        weights: (1.0, (count - 1) as f32),
        first: Box::new(Shape::Leaf),
        second: Box::new(chain(direction, count - 1)),
    }
}

/// One main leaf taking `ratio` of the space, the rest chained across the
/// other axis beside it.
fn main_and_rest(direction: SplitDirection, ratio: f32, count: usize) -> Shape {
    if count <= 1 {
        return Shape::Leaf;
    }
    let rest = match direction {
        SplitDirection::Horizontal => SplitDirection::Vertical,
        SplitDirection::Vertical => SplitDirection::Horizontal,
    };
    Shape::Split {
        direction,
        weights: (ratio, 1.0 - ratio),
        first: Box::new(Shape::Leaf),
        second: Box::new(chain(rest, count - 1)),
    }
}

/// Columns of stacked leaves, as square as `count` allows. Earlier columns
/// take the remainder, so 3 panes are one column of two beside one of one.
fn grid(count: usize) -> Shape {
    let cols = (count as f32).sqrt().ceil().max(1.0) as usize;
    let heights: Vec<usize> = (0..cols)
        .map(|i| count / cols + usize::from(i < count % cols))
        .filter(|h| *h > 0)
        .collect();

    fn columns(heights: &[usize]) -> Shape {
        match heights {
            [] => Shape::Leaf,
            [only] => chain(SplitDirection::Vertical, *only),
            [first, rest @ ..] => Shape::Split {
                direction: SplitDirection::Horizontal,
                weights: (1.0, rest.len() as f32),
                first: Box::new(chain(SplitDirection::Vertical, *first)),
                second: Box::new(columns(rest)),
            },
        }
    }
    columns(&heights)
}

fn shape_for(layout: PaneLayout, count: usize) -> Shape {
    match layout {
        PaneLayout::EvenColumns => chain(SplitDirection::Horizontal, count),
        PaneLayout::EvenRows => chain(SplitDirection::Vertical, count),
        PaneLayout::MainLeft => main_and_rest(SplitDirection::Horizontal, 0.5, count),
        PaneLayout::MainTop => main_and_rest(SplitDirection::Vertical, 0.6, count),
        PaneLayout::AgentPlusShell => main_and_rest(SplitDirection::Horizontal, 0.6, count),
        PaneLayout::Grid => grid(count),
    }
}

/// How a split divides its space: the children's shares, and how many
/// dividers each child already holds along the split's direction (those are
/// cells that are not content, so they are left out of the sharing).
#[derive(Clone, Copy)]
struct Division {
    weights: (f32, f32),
    dividers: (usize, usize),
    min_lens: (usize, usize),
}

/// Divides `size` along `direction` as `division` says, leaving one cell for
/// the divider itself and at least the minimum length on either side where
/// the space allows.
fn divide(
    size: &TerminalSize,
    direction: SplitDirection,
    division: Division,
) -> SplitDirectionAndSize {
    let cell_width = size.pixel_width.checked_div(size.cols).unwrap_or(1);
    let cell_height = size.pixel_height.checked_div(size.rows).unwrap_or(1);
    let total = match direction {
        SplitDirection::Horizontal => size.cols,
        SplitDirection::Vertical => size.rows,
    };
    let avail = total.saturating_sub(1);
    let content = avail.saturating_sub(division.dividers.0 + division.dividers.1);
    let (w1, w2) = division.weights;
    let share = if w1 + w2 > 0.0 { w1 / (w1 + w2) } else { 0.5 };
    let wanted = (content as f32 * share).round() as usize + division.dividers.0;
    let (min_first, min_second) = division.min_lens;
    let upper = avail.saturating_sub(min_second).max(min_first.min(avail));
    let first_len = wanted.max(min_first).min(upper);
    let second_len = avail - first_len;

    let part = |len: usize| match direction {
        SplitDirection::Horizontal => TerminalSize {
            cols: len,
            rows: size.rows,
            pixel_width: len * cell_width,
            pixel_height: size.pixel_height,
            dpi: size.dpi,
        },
        SplitDirection::Vertical => TerminalSize {
            cols: size.cols,
            rows: len,
            pixel_width: size.pixel_width,
            pixel_height: len * cell_height,
            dpi: size.dpi,
        },
    };

    SplitDirectionAndSize {
        direction,
        first: part(first_len),
        second: part(second_len),
    }
}

/// How many cells a subtree needs along `direction`: one per leaf in a run of
/// same-direction splits plus their dividers.
fn min_len<L>(tree: &SizedTree<L>, direction: SplitDirection) -> usize {
    match tree {
        bintree::Tree::Node {
            left,
            right,
            data: Some(data),
        } if data.direction == direction => {
            min_len(left, direction) + min_len(right, direction) + 1
        }
        bintree::Tree::Node { left, right, .. } => {
            min_len(left, direction).max(min_len(right, direction))
        }
        _ => 1,
    }
}

fn shape_min_len(shape: &Shape, direction: SplitDirection) -> usize {
    match shape {
        Shape::Leaf => 1,
        Shape::Split {
            direction: d,
            first,
            second,
            ..
        } if *d == direction => {
            shape_min_len(first, direction) + shape_min_len(second, direction) + 1
        }
        Shape::Split { first, second, .. } => {
            shape_min_len(first, direction).max(shape_min_len(second, direction))
        }
    }
}

fn shape_units(shape: &Shape, direction: SplitDirection) -> usize {
    match shape {
        Shape::Split {
            direction: d,
            first,
            second,
            ..
        } if *d == direction => shape_units(first, direction) + shape_units(second, direction),
        _ => 1,
    }
}

fn build<L>(
    shape: &Shape,
    leaves: &mut impl Iterator<Item = L>,
    size: &TerminalSize,
) -> SizedTree<L> {
    match shape {
        Shape::Leaf => match leaves.next() {
            Some(leaf) => bintree::Tree::Leaf(leaf),
            None => bintree::Tree::Empty,
        },
        Shape::Split {
            direction,
            weights,
            first,
            second,
        } => {
            let data = divide(
                size,
                *direction,
                Division {
                    weights: *weights,
                    dividers: (
                        shape_units(first, *direction) - 1,
                        shape_units(second, *direction) - 1,
                    ),
                    min_lens: (
                        shape_min_len(first, *direction),
                        shape_min_len(second, *direction),
                    ),
                },
            );
            let left = build(first, leaves, &data.first);
            let right = build(second, leaves, &data.second);
            bintree::Tree::Node {
                left: Box::new(left),
                right: Box::new(right),
                data: Some(data),
            }
        }
    }
}

/// A tree arranging `leaves`, in order, as `layout` within `size`. Every
/// leaf is placed: the shape is sized to however many there are.
pub(crate) fn build_layout<L>(
    layout: PaneLayout,
    leaves: Vec<L>,
    size: &TerminalSize,
) -> SizedTree<L> {
    let shape = shape_for(layout, leaves.len());
    build(&shape, &mut leaves.into_iter(), size)
}

/// Whether `count` panes arranged as `layout` get at least one cell each
/// within `size`.
pub(crate) fn layout_fits(layout: PaneLayout, count: usize, size: &TerminalSize) -> bool {
    let shape = shape_for(layout, count);
    shape_min_len(&shape, SplitDirection::Horizontal) <= size.cols
        && shape_min_len(&shape, SplitDirection::Vertical) <= size.rows
}

/// How many leaves of a run of `direction` splits a subtree contributes:
/// the unit that balancing shares space by.
fn units<L>(tree: &SizedTree<L>, direction: SplitDirection) -> usize {
    match tree {
        bintree::Tree::Node {
            left,
            right,
            data: Some(data),
        } if data.direction == direction => units(left, direction) + units(right, direction),
        _ => 1,
    }
}

/// Re-divides every split of `tree` within `size` so that each run of
/// same-direction splits gives its members equal shares, as tmux's even
/// layouts do. The shape is unchanged.
pub(crate) fn balance<L>(tree: &mut SizedTree<L>, size: &TerminalSize) {
    if let bintree::Tree::Node {
        left,
        right,
        data: Some(data),
    } = tree
    {
        let direction = data.direction;
        let first_units = units(left, direction);
        let second_units = units(right, direction);
        *data = divide(
            size,
            direction,
            Division {
                weights: (first_units as f32, second_units as f32),
                dividers: (first_units - 1, second_units - 1),
                min_lens: (min_len(left, direction), min_len(right, direction)),
            },
        );
        let (first, second) = (data.first, data.second);
        balance(left, &first);
        balance(right, &second);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn size(cols: usize, rows: usize) -> TerminalSize {
        TerminalSize {
            rows,
            cols,
            pixel_width: cols * 10,
            pixel_height: rows * 20,
            dpi: 96,
        }
    }

    fn leaves_in_order(tree: &SizedTree<usize>, out: &mut Vec<usize>) {
        match tree {
            bintree::Tree::Leaf(l) => out.push(*l),
            bintree::Tree::Node { left, right, .. } => {
                leaves_in_order(left, out);
                leaves_in_order(right, out);
            }
            bintree::Tree::Empty => {}
        }
    }

    /// The (cols, rows) of each leaf, in order.
    fn leaf_sizes(tree: &SizedTree<usize>, size: TerminalSize, out: &mut Vec<(usize, usize)>) {
        match tree {
            bintree::Tree::Leaf(_) => out.push((size.cols, size.rows)),
            bintree::Tree::Node {
                left,
                right,
                data: Some(data),
            } => {
                leaf_sizes(left, data.first, out);
                leaf_sizes(right, data.second, out);
            }
            _ => {}
        }
    }

    fn assert_fits(tree: &SizedTree<usize>, size: &TerminalSize) {
        if let bintree::Tree::Node {
            left,
            right,
            data: Some(data),
        } = tree
        {
            assert_eq!(data.width(), size.cols, "{data:?} in {size:?}");
            assert_eq!(data.height(), size.rows, "{data:?} in {size:?}");
            assert_fits(left, &data.first);
            assert_fits(right, &data.second);
        }
    }

    #[test]
    fn every_layout_places_every_pane_in_order() {
        let size = size(120, 40);
        for layout in PaneLayout::ALL {
            for count in 1..=6 {
                let tree = build_layout(layout, (0..count).collect(), &size);
                let mut order = vec![];
                leaves_in_order(&tree, &mut order);
                assert_eq!(order, (0..count).collect::<Vec<_>>(), "{layout:?} x{count}");
                assert_fits(&tree, &size);
            }
        }
    }

    #[test]
    fn even_columns_are_equal() {
        let s = size(119, 30);
        let tree = build_layout(PaneLayout::EvenColumns, vec![0, 1, 2, 3], &s);
        let mut sizes = vec![];
        leaf_sizes(&tree, s, &mut sizes);
        // 119 cols minus 3 dividers leaves 116, so 29 each.
        assert_eq!(sizes, vec![(29, 30); 4]);
    }

    #[test]
    fn grid_of_four_is_two_by_two() {
        let s = size(81, 41);
        let tree = build_layout(PaneLayout::Grid, vec![0, 1, 2, 3], &s);
        let mut sizes = vec![];
        leaf_sizes(&tree, s, &mut sizes);
        assert_eq!(sizes, vec![(40, 20); 4]);
    }

    #[test]
    fn agent_plus_shell_gives_the_main_pane_sixty_percent() {
        let s = size(101, 40);
        let tree = build_layout(PaneLayout::AgentPlusShell, vec![0, 1, 2], &s);
        let mut sizes = vec![];
        leaf_sizes(&tree, s, &mut sizes);
        assert_eq!(sizes[0], (60, 40));
        assert_eq!(sizes[1].0, 40);
        assert_eq!(sizes[2].0, 40);
    }

    #[test]
    fn balance_evens_out_a_lopsided_chain() {
        let s = size(119, 30);
        let mut tree = build_layout(PaneLayout::EvenColumns, vec![0, 1, 2], &s);
        // Skew the outer split, then check balancing gives all three equal
        // widths again.
        if let bintree::Tree::Node {
            data: Some(data), ..
        } = &mut tree
        {
            *data = divide(
                &s,
                SplitDirection::Horizontal,
                Division {
                    weights: (9.0, 1.0),
                    dividers: (0, 1),
                    min_lens: (1, 3),
                },
            );
        }
        balance(&mut tree, &s);
        let mut sizes = vec![];
        leaf_sizes(&tree, s, &mut sizes);
        assert_eq!(sizes, vec![(39, 30); 3]);
        assert_fits(&tree, &s);
    }

    #[test]
    fn layouts_are_found_by_lua_and_cli_names() {
        assert_eq!(PaneLayout::from_name("MainLeft"), Some(PaneLayout::MainLeft));
        assert_eq!(PaneLayout::from_name("main-left"), Some(PaneLayout::MainLeft));
        assert_eq!(PaneLayout::from_name("GRID"), Some(PaneLayout::Grid));
        assert_eq!(
            PaneLayout::from_name("agent_plus_shell"),
            Some(PaneLayout::AgentPlusShell)
        );
        assert_eq!(PaneLayout::from_name("bogus"), None);
    }

    #[test]
    fn tiny_tabs_are_refused() {
        assert!(!layout_fits(PaneLayout::EvenColumns, 6, &size(5, 3)));
        assert!(!layout_fits(PaneLayout::EvenRows, 3, &size(80, 4)));
        assert!(layout_fits(PaneLayout::EvenRows, 3, &size(80, 5)));
        for layout in PaneLayout::ALL {
            assert!(layout_fits(layout, 6, &size(120, 40)), "{layout:?}");
        }
    }
}
