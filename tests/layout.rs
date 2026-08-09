//! Tests for the geometry layer, which is deliberately independent of any word list.
//!
//! The important one is `every_sampled_grid_is_legal`: the search's whole value depends on it never
//! emitting an illegal topology, so we sample a few hundred and check all of them rather than
//! eyeballing one.

use ingrid_layout::layout::{
    parse_problem, search, Cell, Direction, Flow, Layout, LayoutSettings, Problem, SearchSettings,
    Symmetry,
};

/// Settings with the block-count and word-count bounds opened up, so tests of the *shape* rules
/// aren't accidentally rejected by a count bound.
fn permissive(min_entry_length: usize) -> LayoutSettings {
    LayoutSettings {
        min_entry_length,
        min_blocks: 0,
        max_blocks: usize::MAX / 2,
        min_words: 0,
        max_words: usize::MAX / 2,
        max_short_entries: usize::MAX / 2,
        symmetry: Symmetry::None,
    }
}

fn parse(input: &str, settings: LayoutSettings) -> Result<(Problem, Layout), String> {
    parse_problem(input, settings)
}

/// Render only the cell states, ignoring theme letters, so propagation results are easy to compare.
fn shape(problem: &Problem, layout: &Layout) -> String {
    (0..problem.height)
        .map(|y| {
            (0..problem.width)
                .map(|x| match layout.cells[problem.index(x, y)] {
                    Cell::Block => '#',
                    Cell::Open => '.',
                    Cell::Unknown => '?',
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn min_length_rule_blocks_trapped_squares() {
    // The single square between the blocks can never reach three letters across, so it must be
    // black -- and once it is, its column is decided too.
    let (problem, layout) = parse("#?#\n???\n???", permissive(3)).unwrap();
    assert_eq!(shape(&problem, &layout).lines().next().unwrap(), "###");
}

#[test]
fn min_length_rule_kills_a_too_short_gap() {
    // Two white squares walled in between blocks can never grow to three.
    let result = parse("##..#??????", permissive(3));
    assert!(result.is_err(), "expected infeasible, got {result:?}");
}

#[test]
fn min_length_rule_applies_to_columns_too() {
    // A one-row grid means every column run has length 1, so nothing can be open.
    let result = parse(".....", permissive(3));
    assert!(result.is_err(), "expected infeasible, got {result:?}");
}

#[test]
fn forced_open_rule_extends_a_short_entry() {
    // The open square has only one direction to grow in, so the two squares to its right are
    // forced open rather than branched on.
    let (problem, layout) = parse("#.???\n?????\n?????\n?????\n?????", permissive(3)).unwrap();
    let first_row: String = shape(&problem, &layout).lines().next().unwrap().to_string();
    assert_eq!(&first_row[..4], "#...");
}

#[test]
fn forced_open_rule_splits_the_deficit_when_neither_side_suffices() {
    // The run between the two blocks is exactly three wide, so the open square in the middle of it
    // forces both of its neighbours open.
    let (problem, layout) = parse("#?.?#\n?????\n?????", permissive(3)).unwrap();
    let first_row: String = shape(&problem, &layout).lines().next().unwrap().to_string();
    assert_eq!(first_row, "#...#");
}

#[test]
fn symmetry_forces_the_rotational_partner() {
    let settings = LayoutSettings {
        symmetry: Symmetry::Rotational180,
        ..permissive(3)
    };
    let (problem, layout) = parse("#????\n?????\n?????\n?????\n?????", settings).unwrap();
    assert_eq!(
        layout.cells[problem.index(4, 4)],
        Cell::Block,
        "block at (0,0) should force a block at (4,4)"
    );
}

#[test]
fn symmetry_forces_open_partners_too() {
    // Symmetry maps blocks to blocks, so it maps white squares to white squares: a theme letter
    // pins its partner open.
    let settings = LayoutSettings {
        symmetry: Symmetry::Rotational180,
        ..permissive(3)
    };
    let (problem, layout) = parse("?????\n?????\n?????\n?????\n????a", settings).unwrap();
    assert_eq!(layout.cells[problem.index(0, 0)], Cell::Open);
}

#[test]
fn conflicting_fixed_squares_are_reported() {
    let settings = LayoutSettings {
        symmetry: Symmetry::Rotational180,
        ..permissive(3)
    };
    // (0,0) is a block but its partner (4,4) is a fixed letter.
    let err = parse("#????\n?????\n?????\n?????\n????a", settings).unwrap_err();
    assert!(err.contains("inconsistent"), "unexpected error: {err}");
}

#[test]
fn theme_boundaries_are_inferred_from_letter_runs() {
    let (problem, layout) = parse("??cat??\n???????\n???????", permissive(3)).unwrap();

    assert_eq!(
        problem.theme_entries.len(),
        1,
        "expected one theme entry, got {:?}",
        problem.theme_entries
    );
    assert_eq!(problem.theme_entries[0].answer, "cat");
    assert_eq!(problem.theme_entries[0].direction, Direction::Across);

    assert_eq!(layout.cells[problem.index(1, 0)], Cell::Block);
    assert_eq!(layout.cells[problem.index(5, 0)], Cell::Block);
}

#[test]
fn an_explicit_dot_opts_out_of_boundary_forcing() {
    // Writing `.` next to the run says "this run is part of a longer entry", so no block is forced
    // and the run stops being tracked as a theme entry.
    let (problem, layout) = parse("?.cat??\n???????\n???????", permissive(3)).unwrap();
    assert_eq!(layout.cells[problem.index(1, 0)], Cell::Open);
    assert!(
        problem.theme_entries.is_empty(),
        "run with an opted-out boundary should not be tracked as an entry: {:?}",
        problem.theme_entries
    );
}

#[test]
fn single_letters_do_not_force_boundaries() {
    // A lone letter is ambiguous -- it's the crossing square of some perpendicular entry -- so it
    // must not force blocks around itself.
    let (problem, layout) = parse("???????\n???a???\n???????", permissive(3)).unwrap();
    assert!(problem.theme_entries.is_empty());
    assert_eq!(layout.cells[problem.index(2, 1)], Cell::Unknown);
    assert_eq!(layout.cells[problem.index(4, 1)], Cell::Unknown);
}

#[test]
fn crossing_theme_answers_are_both_recognised() {
    // `dog` across shares its `o` with `coops` down.
    let crossing = "\
??c??
??o??
?dog?
??p??
??s??";
    let (problem, _) = parse(crossing, permissive(3)).unwrap();
    let mut answers: Vec<&str> = problem
        .theme_entries
        .iter()
        .map(|entry| entry.answer.as_str())
        .collect();
    answers.sort_unstable();
    assert_eq!(answers, vec!["coops", "dog"]);
}

#[test]
fn a_theme_answer_boxed_in_perpendicular_is_infeasible() {
    // The blocks above and below `c` leave it in a one-square down entry, so no arrangement of the
    // remaining squares can work.
    let input = "\
?#???
?cat?
?#???
?????
?????";
    let result = parse(input, permissive(3));
    assert!(result.is_err(), "expected infeasible, got {result:?}");
}

#[test]
fn stacked_theme_answers_do_not_create_spurious_down_entries() {
    // Two answers on adjacent rows leave a two-letter vertical run in every column. Those are
    // crossing squares, not entries -- treating them as entries would force blocks above and below
    // each one and wall off the grid. Real stacked Sunday themes look exactly like this.
    let input = "\
???????
?abcd??
?efgh??
???????
???????
???????
???????";
    let (problem, layout) = parse(input, permissive(3)).unwrap();

    let answers: Vec<&str> = problem
        .theme_entries
        .iter()
        .map(|entry| entry.answer.as_str())
        .collect();
    assert_eq!(answers, vec!["abcd", "efgh"], "got {answers:?}");

    // No block should have been forced above or below the stack.
    assert_eq!(layout.cells[problem.index(1, 0)], Cell::Unknown);
    assert_eq!(layout.cells[problem.index(1, 3)], Cell::Unknown);
}

#[test]
fn a_theme_run_too_short_to_be_an_entry_is_reported() {
    // Nothing crosses `ab`, so it really was meant as an entry -- and a two-square entry is illegal.
    let err = parse("???????\n??ab???\n???????\n???????", permissive(3)).unwrap_err();
    assert!(err.contains("only 2 squares long"), "unexpected error: {err}");
}

#[test]
fn disconnected_white_regions_are_rejected() {
    // The middle column is entirely black, so the two halves can never join.
    let input = "\
...#...
...#...
...#...
...#...
...#...
...#...
...#...";
    let result = parse(input, permissive(3));
    assert!(result.is_err(), "expected infeasible, got {result:?}");
}

/// A conventional themed 15x15: three across theme entries on rows 3, 7 and 11. The 13-letter
/// answers sit at columns 1-13, so their boundary blocks at columns 0 and 14 are each other's
/// rotational partners.
const THEME_15: &str = "\
???????????????
???????????????
???????????????
?sleightofhand?
???????????????
???????????????
???????????????
intimateapparel
???????????????
???????????????
???????????????
?defenestrated?
???????????????
???????????????
???????????????";

#[test]
fn every_sampled_grid_is_legal() {
    let settings = LayoutSettings {
        min_entry_length: 3,
        min_blocks: 34,
        max_blocks: 44,
        min_words: 0,
        max_words: 80,
        max_short_entries: 24,
        symmetry: Symmetry::Rotational180,
    };

    let (problem, root) = parse(THEME_15, settings).unwrap();

    let mut found = 0;
    let mut checked = 0;
    let stats = search(
        &problem,
        &root,
        &SearchSettings {
            nodes_per_restart: 40_000,
            candidates_per_restart: 1,
            seed: 7,
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(60)),
        },
        None,
        &mut |layout, _stats| {
            checked += 1;
            // `validate` is an independent reimplementation of the rules, not a replay of
            // propagation, so this genuinely cross-checks the search.
            if let Err(reason) = layout.validate(&problem) {
                panic!("illegal grid emitted: {reason}\n{}", layout.render(&problem));
            }

            // Spot-check the properties that matter most, directly rather than via `validate`.
            for idx in 0..layout.cells.len() {
                assert_eq!(
                    layout.cells[idx],
                    layout.cells[problem.partner(idx)],
                    "asymmetric grid\n{}",
                    layout.render(&problem)
                );
            }
            assert!((34..=44).contains(&layout.block_count));
            for &(_, _, length) in &layout.entries(&problem) {
                assert!(length >= 3);
            }

            found += 1;
            if found >= 200 {
                Flow::Stop
            } else {
                Flow::Continue
            }
        },
    );

    assert!(
        found >= 200,
        "only sampled {found} grids in the time budget ({stats:?})"
    );
    assert_eq!(checked, found);
}

#[test]
fn sampled_grids_are_distinct() {
    let settings = LayoutSettings {
        min_blocks: 34,
        max_blocks: 44,
        max_words: 80,
        max_short_entries: 24,
        ..LayoutSettings::default()
    };
    let (problem, root) = parse(THEME_15, settings).unwrap();

    let mut seen = std::collections::HashSet::new();
    search(
        &problem,
        &root,
        &SearchSettings {
            nodes_per_restart: 40_000,
            candidates_per_restart: 1,
            seed: 11,
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(60)),
        },
        None,
        &mut |layout, _| {
            assert!(
                seen.insert(layout.render(&problem)),
                "search emitted a duplicate grid"
            );
            if seen.len() >= 50 {
                Flow::Stop
            } else {
                Flow::Continue
            }
        },
    );

    assert_eq!(seen.len(), 50);
}

#[test]
fn theme_entries_survive_into_every_emitted_grid() {
    let settings = LayoutSettings {
        min_blocks: 34,
        max_blocks: 44,
        max_words: 80,
        max_short_entries: 24,
        ..LayoutSettings::default()
    };
    let (problem, root) = parse(THEME_15, settings).unwrap();
    assert_eq!(problem.theme_entries.len(), 3);

    let mut count = 0;
    search(
        &problem,
        &root,
        &SearchSettings {
            nodes_per_restart: 40_000,
            candidates_per_restart: 1,
            seed: 3,
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(60)),
        },
        None,
        &mut |layout, _| {
            let entries = layout.entries(&problem);
            for theme in &problem.theme_entries {
                assert!(
                    entries.iter().any(|&(start, dir, len)| {
                        start == theme.start && dir == theme.direction && len == theme.length
                    }),
                    "theme {:?} is not a whole entry in\n{}",
                    theme.answer,
                    layout.render(&problem)
                );
            }
            count += 1;
            if count >= 25 {
                Flow::Stop
            } else {
                Flow::Continue
            }
        },
    );
    assert_eq!(count, 25);
}

#[test]
fn an_impossible_theme_placement_fails_fast() {
    // The theme's left boundary block lands right next to a square the constructor pinned open,
    // stranding it in a one-letter across entry.
    let input = "\
.?cat??
???????
???????";
    let result = parse(input, permissive(3));
    assert!(result.is_err(), "expected infeasible, got {result:?}");
}

#[test]
fn a_theme_whose_boundary_is_forced_open_by_symmetry_is_reported() {
    // `cat`'s right boundary at (4,0) is the rotational partner of (2,6), which `dog` pins open, so
    // `cat` can never be an entry of its own. That has to be an error, not a silently longer entry.
    let input = "\
?cat???
???????
???????
???????
???????
???????
??dog??";
    let settings = LayoutSettings {
        symmetry: Symmetry::Rotational180,
        ..permissive(3)
    };
    let err = parse(input, settings).unwrap_err();
    assert!(
        err.contains("self-contained entry"),
        "unexpected error: {err}"
    );
}
