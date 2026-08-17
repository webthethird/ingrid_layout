//! Tests for the theme layer: where the answers go, before any black square exists.
//!
//! Like the geometry tests these need no word list. The one to read first is
//! `every_placement_is_a_legal_starting_point` -- the whole value of this layer is that its output
//! is something the block search can actually start from, so that is asserted over a sample rather
//! than eyeballed once.

use std::collections::HashSet;

use ingrid_layout::layout::{Direction, Flow, LayoutSettings, Symmetry, Template};
use ingrid_layout::theme::{parse_answers, place, Placement, ThemeSettings};

fn settings() -> LayoutSettings {
    LayoutSettings {
        min_entry_length: 3,
        min_blocks: 30,
        max_blocks: 44,
        min_words: 0,
        max_words: 80,
        max_short_entries: 24,
        symmetry: Symmetry::Rotational180,
    }
}

/// Collect up to `limit` placements for a blank grid of this size.
fn sample(
    width: usize,
    height: usize,
    answers: &[&str],
    theme: ThemeSettings,
    limit: usize,
) -> Vec<Placement> {
    let answers: Vec<String> = answers.iter().map(|s| (*s).to_string()).collect();
    let mut found = vec![];

    place(
        &Template::blank(width, height),
        &settings(),
        &answers,
        &theme,
        &mut |placement| {
            found.push(placement);
            if found.len() >= limit {
                Flow::Stop
            } else {
                Flow::Continue
            }
        },
    )
    .unwrap();

    found
}

#[test]
fn answers_are_normalised_and_comments_skipped() {
    let answers = parse_answers(
        "\
# a theme
Intimate Apparel

WISTERIA-LANE
",
    )
    .unwrap();

    assert_eq!(answers, vec!["intimateapparel", "wisterialane"]);
}

#[test]
fn a_line_with_no_letters_is_reported() {
    let err = parse_answers("intimateapparel\n---\n").unwrap_err();
    assert!(err.contains("no letters"), "unexpected error: {err}");
}

#[test]
fn two_answers_of_a_length_are_placed_as_each_others_mirror() {
    let found = sample(15, 15, &["sleightofhand", "defenestrated"], ThemeSettings::default(), 20);
    assert!(!found.is_empty(), "no placement found for two 13s in a 15x15");

    for placement in &found {
        let entries = placement.entries();
        assert_eq!(entries.len(), 2, "expected two theme entries, got {entries:?}");

        // Rotating one entry 180 degrees has to land exactly on the other: rows sum to H-1, and
        // columns to W - length. (Both at column 1 is the familiar case, but a 13 at columns 0-12
        // mirrored by one at columns 2-14 is just as symmetric.)
        let (a, b) = (&entries[0], &entries[1]);
        assert_eq!(a.start.1 + b.start.1, 14, "rows aren't mirrors: {entries:?}");
        assert_eq!(a.start.0 + b.start.0, 2, "columns aren't mirrors: {entries:?}");
        assert_eq!(placement.metrics.unpaired, 0);
    }
}

#[test]
fn a_lone_answer_has_to_take_the_centre_line() {
    // With pairing on and nothing to pair with, the only legal home for a single 13 in a 15x15 is
    // the position that is its own mirror: the middle row, centred.
    let found = sample(15, 15, &["sleightofhand"], ThemeSettings::default(), 10);
    assert!(!found.is_empty(), "no placement found for one 13 in a 15x15");

    for placement in &found {
        assert_eq!(placement.entries().len(), 1);
        assert_eq!(placement.entries()[0].start, (1, 7));
        assert_eq!(placement.entries()[0].direction, Direction::Across);
    }

    // And exactly one such placement exists, so the search should notice the space is finished
    // rather than restart forever.
    assert_eq!(found.len(), 1, "expected a single placement, got {}", found.len());
}

#[test]
fn an_even_length_answer_has_no_centred_home_in_an_odd_grid() {
    // A 12 in a 15-wide grid would need to start at column 1.5 to be its own mirror, so under
    // strict pairing there is nowhere for it to go at all.
    let found = sample(15, 15, &["twelvelettes"], ThemeSettings::default(), 10);
    assert!(found.is_empty(), "expected no placement, got {}", found.len());

    // Relaxing the pairing rule is exactly the escape hatch for this.
    let loose = ThemeSettings {
        require_pairing: false,
        ..ThemeSettings::default()
    };
    let found = sample(15, 15, &["twelvelettes"], loose, 5);
    assert!(!found.is_empty(), "loose pairing should find a home for a 12");
    for placement in &found {
        assert_eq!(placement.metrics.unpaired, 1);
    }
}

#[test]
fn answers_of_mismatched_lengths_both_want_the_centre() {
    // A 15 and a 13 each have nothing to pair with, so strict pairing sends both to the middle row
    // and they collide. This is the case the `--loose-theme-symmetry` escape hatch exists for.
    let strict = sample(15, 15, &["intimateapparel", "sleightofhand"], ThemeSettings::default(), 5);
    assert!(strict.is_empty(), "expected a collision, got {}", strict.len());

    let loose = ThemeSettings {
        require_pairing: false,
        ..ThemeSettings::default()
    };
    let found = sample(15, 15, &["intimateapparel", "sleightofhand"], loose, 5);
    assert!(!found.is_empty(), "loose pairing should place a 15 and a 13");
}

/// The smallest gap between any two theme rows in a placement.
fn closest_theme_rows(placement: &Placement) -> usize {
    let rows: Vec<usize> = placement.entries().iter().map(|e| e.start.1).collect();
    let mut closest = usize::MAX;
    for (i, &a) in rows.iter().enumerate() {
        for &b in &rows[i + 1..] {
            closest = closest.min(a.abs_diff(b));
        }
    }
    closest
}

#[test]
fn stacked_answers_are_rejected_unless_asked_for() {
    // It takes three answers to test this. A *mirrored pair* sits in rows r and H-1-r, which differ
    // by an even number, so two paired answers can never be adjacent however hard they try. Three
    // 13s give a pair plus a centred one, and the centred one at row 7 can end up flush against a
    // pair at rows 6 and 8.
    let answers = &["sleightofhand", "defenestrated", "gerrymandered"];

    for placement in sample(15, 15, answers, ThemeSettings::default(), 30) {
        assert!(
            closest_theme_rows(&placement) > 1,
            "adjacent theme rows should need --stacked-themes: {:?}",
            placement.entries()
        );
    }

    let stacked = ThemeSettings {
        allow_stacked: true,
        seed: 5,
        ..ThemeSettings::default()
    };
    let any_adjacent = sample(15, 15, answers, stacked, 150)
        .iter()
        .any(|placement| closest_theme_rows(placement) == 1);
    assert!(any_adjacent, "--stacked-themes should allow adjacent theme rows");
}

#[test]
fn spacing_counts_the_room_to_the_edges_too() {
    // Theme rows at 1/7/13 and at 3/7/11 have the same gaps *between* them, so a metric that only
    // looked at those would call them equally good. The first leaves one row above and below, which
    // in practice is the difference between a theme that fills and one that doesn't -- so the
    // ranking has to separate them, and this is the test that says so.
    let found = sample(
        15,
        15,
        &["sleightofhand", "defenestrated", "intimateapparel"],
        ThemeSettings::default(),
        60,
    );

    let rows_of = |placement: &Placement| {
        let mut rows: Vec<usize> = placement.entries().iter().map(|e| e.start.1).collect();
        rows.sort_unstable();
        rows
    };

    // The best-scoring placement for each row set, so the comparison isn't confounded by the
    // column offset -- both row sets can reach the same column offsets, and those carry their own
    // penalty.
    let best_with = |wanted: Vec<usize>| {
        found
            .iter()
            .filter(|p| rows_of(p) == wanted)
            .max_by(|a, b| a.score.total_cmp(&b.score))
            .unwrap_or_else(|| panic!("rows {wanted:?} should be reachable"))
    };

    let airy = best_with(vec![3, 7, 11]);
    let cramped = best_with(vec![1, 7, 13]);

    assert_eq!(airy.metrics.min_free_lines, 3);
    assert_eq!(cramped.metrics.min_free_lines, 1);
    assert!(
        airy.score > cramped.score,
        "3/7/11 ({}) should outrank 1/7/13 ({})",
        airy.score,
        cramped.score
    );
}

/// The theme of the puzzle in `ingrid_core`'s README, as four bare answers. Two 15s and two 12s, so
/// it pairs up cleanly with nothing needing the centre.
const README_ANSWERS: [&str; 4] = [
    "intimateapparel",
    "wisterialane",
    "cremebrulees",
    "goingintodetail",
];

#[test]
fn the_published_theme_placement_ranks_at_the_top() {
    // The published grid puts its theme rows at 2, 5, 9 and 12. This is the calibration test for
    // the whole ranking: if a human's arrangement doesn't come out near the top of ours, the
    // metrics are measuring the wrong thing.
    //
    // Sample generously. These four answers have some hundreds of legal arrangements, and a ranking
    // can only be as good as what it is given to choose from -- an earlier version of this test
    // sampled 120 and failed, not because the metrics were wrong but because the arrangement they
    // would have picked was never generated. Under-sampling is indistinguishable from a bad metric
    // from the outside, so rule it out here rather than debug it later.
    let found = sample(15, 15, &README_ANSWERS, ThemeSettings::default(), 500);
    assert!(found.len() >= 200, "only sampled {} placements", found.len());

    let best = found
        .iter()
        .max_by(|a, b| a.score.total_cmp(&b.score))
        .expect("sampled at least one");

    let mut rows: Vec<usize> = best.entries().iter().map(|e| e.start.1).collect();
    rows.sort_unstable();
    assert_eq!(rows, vec![2, 5, 9, 12], "best placement was {:?}", best.entries());

    assert_eq!(best.metrics.unpaired, 0);
    assert_eq!(best.metrics.outer_band_entries, 0);
}

#[test]
fn a_flush_answer_strands_squares_against_the_far_edge() {
    // A 12 in a 15-wide grid can't avoid leftovers: one square each side goes to a boundary block,
    // and the three spare columns can't split evenly. Columns 1-12 and 2-13 waste one square;
    // columns 0-11 and 3-14 waste two.
    let found = sample(15, 15, &README_ANSWERS, ThemeSettings::default(), 120);

    let stranded_for = |start_col: usize| {
        found
            .iter()
            .filter(|p| {
                p.entries()
                    .iter()
                    .any(|e| e.length == 12 && e.start.0 == start_col)
            })
            .map(|p| p.metrics.stranded_squares)
            .min()
    };

    let (Some(tight), Some(flush)) = (stranded_for(1), stranded_for(0)) else {
        panic!("expected 12-letter answers at both columns 0 and 1 in the sample");
    };
    assert!(
        tight < flush,
        "columns 1-12 ({tight} stranded) should beat columns 0-11 ({flush})"
    );
}

#[test]
fn every_placement_is_a_legal_starting_point() {
    // The point of this layer: whatever it emits, the block search can start from. `build_problem`
    // has already run inside `place`, so the propagated root is here to be checked directly.
    let found = sample(
        15,
        15,
        &["sleightofhand", "defenestrated", "intimateapparel"],
        ThemeSettings::default(),
        40,
    );
    assert!(found.len() >= 10, "only sampled {} placements", found.len());

    let mut seen = HashSet::new();

    for placement in &found {
        let problem = &placement.problem;
        let root = &placement.root;

        assert!(seen.insert(root.render(problem)), "duplicate placement emitted");
        assert_eq!(problem.theme_entries.len(), 3, "lost an answer: {:?}", problem.theme_entries);
        assert_eq!(placement.metrics.unpaired, 0, "strict pairing was not honoured");

        // The 15 is its own mirror, so it has to be the centre row; the two 13s mirror each other.
        let fifteen = problem
            .theme_entries
            .iter()
            .find(|e| e.length == 15)
            .expect("the 15-letter answer should still be an entry");
        assert_eq!(fifteen.start, (0, 7));

        // Symmetry holds square by square even though only some squares are decided yet.
        for idx in 0..root.cells.len() {
            assert_eq!(
                root.cells[idx],
                root.cells[problem.partner(idx)],
                "asymmetric root:\n{}",
                root.render(problem)
            );
        }

        // Boundary blocks are in, and the answers survived as whole entries.
        for theme in &problem.theme_entries {
            assert!(
                theme.answer.chars().count() == theme.length,
                "answer and entry length disagree: {theme:?}"
            );
        }
    }
}

#[test]
fn a_template_with_blocks_constrains_where_answers_can_go() {
    // Row 7 is walled off at its middle, so the 15 that would otherwise have to sit there can't,
    // and no placement exists.
    let mut base = Template::blank(15, 15);
    base.fixed[7 * 15 + 7] = Some(ingrid_layout::layout::Cell::Block);

    let answers = vec!["intimateapparel".to_string()];
    let mut found = 0;
    place(
        &base,
        &settings(),
        &answers,
        &ThemeSettings::default(),
        &mut |_| {
            found += 1;
            Flow::Stop
        },
    )
    .unwrap();

    assert_eq!(found, 0, "a block in the centre row should rule the 15 out");
}

#[test]
fn an_answer_that_cannot_fit_is_reported_before_any_search() {
    let answers = vec!["averylongthemeanswerindeed".to_string()];
    let err = place(
        &Template::blank(15, 15),
        &settings(),
        &answers,
        &ThemeSettings::default(),
        &mut |_| Flow::Stop,
    )
    .unwrap_err();

    assert!(err.contains("doesn't fit"), "unexpected error: {err}");
    assert!(err.contains("--theme-down"), "should suggest the escape hatch: {err}");
}

#[test]
fn an_answer_shorter_than_the_minimum_is_reported() {
    let answers = vec!["ab".to_string()];
    let err = place(
        &Template::blank(15, 15),
        &settings(),
        &answers,
        &ThemeSettings::default(),
        &mut |_| Flow::Stop,
    )
    .unwrap_err();

    assert!(err.contains("only 2 squares long"), "unexpected error: {err}");
}

#[test]
fn answers_are_placed_flush_against_alternating_walls() {
    // Constructors put theme answers hard against opposite walls in turn, because an answer against
    // a wall needs a boundary block on one side only and the leftovers past it form a finger. An
    // answer floating in the middle of its row pays for two boundary blocks and cuts both edge
    // columns for nothing.
    //
    // Under 180-degree symmetry the alternation is free -- the mirror of a flush-left answer is a
    // flush-right one -- so the ranking only has to prefer being flush at all.
    let found = sample(15, 15, &README_ANSWERS, ThemeSettings::default(), 200);

    let best = found
        .iter()
        .max_by(|a, b| a.score.total_cmp(&b.score))
        .expect("sampled at least one");

    assert_eq!(
        best.metrics.floating_entries, 0,
        "best placement should anchor every answer: {:?}",
        best.entries()
    );

    // And the 12s should end up on opposite walls, which is what "alternating" buys.
    let twelves: Vec<&ingrid_layout::layout::ThemeEntry> = best
        .entries()
        .iter()
        .filter(|entry| entry.length == 12)
        .collect();
    assert_eq!(twelves.len(), 2);
    let flush_left = twelves.iter().filter(|e| e.start.0 == 0).count();
    let flush_right = twelves.iter().filter(|e| e.start.0 + e.length == 15).count();
    assert_eq!(
        (flush_left, flush_right),
        (1, 1),
        "expected one against each wall: {twelves:?}"
    );
}

#[test]
fn a_floating_answer_ranks_below_an_anchored_one() {
    let found = sample(15, 15, &README_ANSWERS, ThemeSettings::default(), 200);

    let best_anchored = found
        .iter()
        .filter(|p| p.metrics.floating_entries == 0)
        .max_by(|a, b| a.score.total_cmp(&b.score))
        .expect("some placement anchors everything");
    let best_floating = found
        .iter()
        .filter(|p| p.metrics.floating_entries > 0)
        .max_by(|a, b| a.score.total_cmp(&b.score))
        .expect("some placement floats an answer");

    assert!(
        best_anchored.score > best_floating.score,
        "anchored ({}) should outrank floating ({})",
        best_anchored.score,
        best_floating.score
    );
}

#[test]
fn the_outermost_two_lines_are_avoided() {
    // Counting rows the way a constructor does, theme answers rarely appear in rows 1 or 2 -- the
    // first is crossed on one side only, and the second pins every down entry within a square of
    // the wall. Both are indices 0 and 1 here, plus their mirrors.
    let found = sample(15, 15, &README_ANSWERS, ThemeSettings::default(), 500);

    let best = found
        .iter()
        .max_by(|a, b| a.score.total_cmp(&b.score))
        .expect("sampled at least one");
    assert_eq!(best.metrics.outer_band_entries, 0);

    // And the rule has to bite, not merely be measured: a placement using the outer band should
    // rank below an otherwise comparable one that doesn't.
    let crowded = found
        .iter()
        .filter(|p| p.metrics.outer_band_entries > 0)
        .max_by(|a, b| a.score.total_cmp(&b.score))
        .expect("some placement uses the outer band");
    assert!(
        best.score > crowded.score,
        "outer-band placement ({}) should rank below {}",
        crowded.score,
        best.score
    );

    for placement in &found {
        let rows: Vec<usize> = placement.entries().iter().map(|e| e.start.1).collect();
        let counted = rows.iter().filter(|&&r| r <= 1 || r >= 13).count();
        assert_eq!(
            placement.metrics.outer_band_entries, counted,
            "outer-band count disagrees with rows {rows:?}"
        );
    }
}
