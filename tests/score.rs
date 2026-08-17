//! Tests for the ranking metrics, built by placing blocks directly rather than by searching.
//!
//! These need no word list and no propagation, which is the point: `block_adjacencies` is a claim
//! about *shapes*, and the clearest way to state a claim about shapes is to draw them.

use ingrid_layout::layout::{Cell, Layout, LayoutSettings, Problem, Symmetry};
use ingrid_layout::score::Metrics;

/// A grid with blocks exactly where asked, bypassing the geometry rules.
///
/// `Layout::set` with `Symmetry::None` writes one square and keeps the counters straight, so these
/// arrangements can be illegal as crosswords -- an isolated white square, say -- without troubling
/// anything the metrics look at.
fn grid(width: usize, height: usize, blocks: &[(usize, usize)]) -> (Problem, Layout) {
    let settings = LayoutSettings {
        min_entry_length: 2,
        symmetry: Symmetry::None,
        ..LayoutSettings::default()
    };
    let problem = Problem::new(width, height, settings).unwrap();
    let mut layout = Layout::new(&problem);

    for &(x, y) in blocks {
        layout.set(&problem, problem.index(x, y), Cell::Block).unwrap();
    }
    for idx in 0..problem.cell_count() {
        if layout.cells[idx] == Cell::Unknown {
            layout.set(&problem, idx, Cell::Open).unwrap();
        }
    }

    (problem, layout)
}

fn adjacencies(width: usize, height: usize, blocks: &[(usize, usize)]) -> usize {
    let (problem, layout) = grid(width, height, blocks);
    Metrics::measure(&problem, &layout).block_adjacencies
}

#[test]
fn scattered_blocks_share_no_edges() {
    // Free-floating blocks are the most efficient thing you can do with the budget: each one cuts a
    // row and a column and pays for neither twice.
    assert_eq!(adjacencies(7, 7, &[(2, 2), (4, 4), (2, 5)]), 0);
}

#[test]
fn a_diagonal_line_costs_nothing_either() {
    // The whole reason constructors prefer diagonals. Four blocks, no shared edges, and every one
    // of them still cutting a distinct row and column.
    assert_eq!(adjacencies(7, 7, &[(2, 2), (3, 3), (4, 4), (5, 5)]), 0);
}

#[test]
fn a_straight_interior_line_pays_for_every_join() {
    // n blocks in a row share n-1 edges, and the same holds vertically.
    assert_eq!(adjacencies(7, 7, &[(2, 3), (3, 3), (4, 3)]), 2);
    assert_eq!(adjacencies(7, 7, &[(3, 2), (3, 3), (3, 4)]), 2);
}

#[test]
fn a_solid_rectangle_is_the_worst_shape_per_block() {
    // Four blocks buying two rows and two columns, at a cost of four shared edges -- against a
    // diagonal's four blocks, four rows, four columns and nothing to pay.
    assert_eq!(adjacencies(7, 7, &[(2, 2), (3, 2), (2, 3), (3, 3)]), 4);
}

#[test]
fn a_finger_reaching_in_from_a_wall_is_free() {
    // Fingers have to be straight -- there is no such thing as a diagonal finger -- and a grid
    // wants several, so charging them for being straight would have this metric fighting the
    // structure it is supposed to reward.
    assert_eq!(adjacencies(7, 7, &[(0, 3), (1, 3), (2, 3)]), 0);
    assert_eq!(adjacencies(7, 7, &[(4, 3), (5, 3), (6, 3)]), 0);

    // Move that same run one square clear of the wall and it is an ordinary interior line again.
    assert_eq!(adjacencies(7, 7, &[(1, 3), (2, 3), (3, 3)]), 2);
}

#[test]
fn fingers_are_counted_per_side() {
    let (problem, layout) = grid(
        7,
        7,
        &[(0, 1), (1, 1), (5, 3), (6, 3), (0, 5), (1, 5), (3, 3)],
    );
    let metrics = Metrics::measure(&problem, &layout);

    // Two reaching in from the left, one from the right. The lone block at (3,3) touches no wall.
    assert_eq!(metrics.side_fingers, 3);
}

#[test]
fn a_single_block_against_a_wall_is_not_a_finger() {
    let (problem, layout) = grid(7, 7, &[(0, 3)]);
    assert_eq!(Metrics::measure(&problem, &layout).side_fingers, 0);
}
