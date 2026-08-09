//! Tests that involve the word list: the scoring metrics and the fill oracle.
//!
//! These are anchored on a real published grid -- the example from `ingrid_core`'s README, which is
//! Ryan McCarty's puzzle. Using a grid a human actually built and a solver actually filled means
//! these tests check the tool against reality rather than against its own opinions.

use std::time::Duration;

use ingrid_core::word_list::{WordList, WordListSourceConfig, WordListSourceConfigProvider};
use ingrid_layout::layout::{parse_problem, LayoutSettings, Symmetry};
use ingrid_layout::oracle::{Oracle, Verdict};
use ingrid_layout::score::{Metrics, Weights};

const STWL_RAW: &str = include_str!("../resources/spreadthewordlist.dict");

/// The published grid, with the theme answers in place and every other square already decided.
const PUBLISHED_GRID: &str = "\
....#.....#....
....#.....#....
intimateapparel
......##.......
###.....#......
wisterialane###
.....#.....#...
....#.....#....
...#.....#.....
###cremebrulees
......#.....###
.......##......
goingintodetail
....#.....#....
....#.....#....";

/// The same theme, but with every non-theme square left for the tool to decide.
const THEME_ONLY: &str = "\
???????????????
???????????????
intimateapparel
???????????????
???????????????
wisterialane???
???????????????
???????????????
???????????????
???cremebrulees
???????????????
???????????????
goingintodetail
???????????????
???????????????";

fn permissive_settings() -> LayoutSettings {
    LayoutSettings {
        min_entry_length: 3,
        min_blocks: 0,
        max_blocks: 99,
        min_words: 0,
        max_words: 99,
        max_short_entries: 99,
        symmetry: Symmetry::Rotational180,
    }
}

fn oracle() -> Oracle {
    let word_list = WordList::new(
        vec![WordListSourceConfig {
            id: "0".into(),
            enabled: true,
            provider: WordListSourceConfigProvider::FileContents { contents: STWL_RAW },
            normalization: None,
        }],
        None,
        Some(15),
        None,
    );
    Oracle::new(word_list, 50)
}

#[test]
fn the_published_grid_is_accepted_by_the_geometry_rules() {
    // Every square is already decided, so this exercises the rules as a validator: if any of them
    // is too strict, a grid that ran in a newspaper gets rejected.
    let (problem, layout) = parse_problem(PUBLISHED_GRID, permissive_settings())
        .expect("the published grid should be legal");

    assert!(layout.is_complete());
    layout
        .validate(&problem)
        .expect("the published grid should pass validation");
    assert_eq!(problem.theme_entries.len(), 4);
}

#[test]
fn metrics_match_the_published_grid() {
    let (problem, layout) = parse_problem(PUBLISHED_GRID, permissive_settings()).unwrap();
    let metrics = Metrics::measure(&problem, &layout);

    assert_eq!(metrics.block_count, 32);
    assert_eq!(metrics.word_count, 72);
    assert_eq!(metrics.short_entries, 6);
    assert_eq!(metrics.max_entry_length, 15);

    // A well-built grid shouldn't be padded with squares that carry no structural weight. Getting
    // zero here on a real grid is the main evidence that the cheater metric means what it claims.
    assert_eq!(metrics.cheater_squares, 0);

    // And it should comfortably beat what our own search produces, which scores around -11 to -40.
    let score = metrics.geometric_score(&Weights::default());
    assert!(score > -10.0, "published grid scored {score}");
}

#[test]
fn the_oracle_fills_the_published_grid() {
    let (problem, layout) = parse_problem(PUBLISHED_GRID, permissive_settings()).unwrap();

    match oracle().evaluate(&problem, &layout, Duration::from_secs(30)) {
        Verdict::Filled(report) => {
            assert_eq!(report.grid.lines().count(), 15);
            // The theme answers have to survive into the finished fill.
            assert!(report.grid.contains("intimateapparel"), "{}", report.grid);
            assert!(report.grid.contains("goingintodetail"), "{}", report.grid);
            // Theme entries are excluded from the scores, so a run of hidden zero-score words can't
            // drag this down.
            assert!(
                report.min_word_score >= 50,
                "min word score was {}",
                report.min_word_score
            );
        }
        other => panic!("the published grid should fill, got {other:?}"),
    }
}

#[test]
fn the_oracle_reports_which_entry_has_no_word() {
    // A block pattern that forces a 13-square down entry through three theme answers. Nothing in
    // any dictionary matches `i??t???c??n??`, and the oracle should say so rather than searching.
    let grid = "\
...#...#.......
...#...#.......
intimateapparel
...............
###.....##.....
wisterialane###
....#.....#....
....#.....#....
....#.....#....
###cremebrulees
.....##.....###
...............
goingintodetail
.......#...#...
.......#...#...";

    let (problem, layout) = parse_problem(grid, permissive_settings()).unwrap();

    match oracle().evaluate(&problem, &layout, Duration::from_secs(30)) {
        Verdict::NoWordForSlot { pattern, .. } => {
            assert!(pattern.contains('?'), "pattern was {pattern}");
        }
        other => panic!("expected a no-word rejection, got {other:?}"),
    }
}

#[test]
fn the_search_rediscovers_a_fillable_grid_for_the_published_theme() {
    use ingrid_layout::layout::{search, EntryViability, Flow, SearchSettings};

    // The end-to-end claim: given only the theme answers, the tool finds block placements that
    // actually fill. We know at least one exists, because a human found one for this theme.
    let settings = LayoutSettings {
        min_blocks: 30,
        max_blocks: 42,
        max_words: 78,
        max_short_entries: 12,
        ..LayoutSettings::default()
    };
    let (problem, root) = parse_problem(THEME_ONLY, settings).unwrap();

    let mut oracle = oracle();
    let mut pool = vec![];
    search(
        &problem,
        &root,
        &SearchSettings {
            nodes_per_restart: 40_000,
            candidates_per_restart: 1,
            seed: 0,
            deadline: Some(std::time::Instant::now() + Duration::from_secs(30)),
        },
        Some(&mut oracle as &mut dyn EntryViability),
        &mut |layout, _| {
            pool.push(layout.clone());
            if pool.len() >= 60 {
                Flow::Stop
            } else {
                Flow::Continue
            }
        },
    );

    assert!(!pool.is_empty(), "the search found no legal topology");

    // Rank the way the CLI does, so this also checks that the prescore puts fillable grids first.
    let weights = Weights::default();
    let mut ranked: Vec<_> = pool
        .into_iter()
        .map(|layout| {
            let score = Metrics::measure(&problem, &layout).geometric_score(&weights);
            (score, layout)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));

    let filled = ranked.iter().take(40).any(|(_, layout)| {
        matches!(
            oracle.evaluate(&problem, layout, Duration::from_secs(5)),
            Verdict::Filled(_)
        )
    });

    assert!(
        filled,
        "none of the best-ranked topologies filled ({:?})",
        oracle.stats
    );
}
