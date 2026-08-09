//! `ingrid_layout`: given a grid with only the theme answers placed, work out where the black
//! squares go.
//!
//! Runs in two stages. First sample a pool of legal topologies using geometry alone, then rank them
//! and spend the expensive fill attempts on the most promising ones, best first.

use std::fmt::{Debug, Formatter};
use std::fs;
use std::time::{Duration, Instant};

use clap::Parser;

use ingrid_core::word_list::{WordList, WordListSourceConfig, WordListSourceConfigProvider};
use ingrid_layout::layout::{
    parse_problem, search, Flow, Layout, LayoutSettings, SearchSettings, Symmetry,
};
use ingrid_layout::oracle::{Oracle, Verdict};
use ingrid_layout::score::{fill_score, Candidate, Metrics, Weights};

/// Bundled copy of Spread the Wordlist, by Brooke Husic and Enrique Henestroza Anguiano. Vendored
/// so the tool works with no configuration and the crate builds from a fresh clone.
const STWL_RAW: &str = include_str!("../resources/spreadthewordlist.dict");

/// ingrid_layout: crossword block-placement tool
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the theme grid: `?` for undecided squares, `#` for blocks you want to keep, `.` for
    /// squares you want to stay open, and letters for the theme answers
    grid_path: String,

    /// Minimum length of an entry
    #[arg(long, default_value_t = 3)]
    min_entry_length: usize,

    /// Fewest blocks the finished grid may have [default: scaled to the grid, 30 for a 15x15]
    #[arg(long)]
    min_blocks: Option<usize>,

    /// Most blocks the finished grid may have [default: scaled to the grid, 42 for a 15x15]
    #[arg(long)]
    max_blocks: Option<usize>,

    /// Most entries the finished grid may have [default: scaled to the grid, 78 for a 15x15 and 140
    /// for a 21x21]
    #[arg(long)]
    max_words: Option<usize>,

    /// Most minimum-length (usually three-letter) entries the finished grid may have. Lowering this
    /// gives better grids but fewer of them [default: scaled to the grid, 10 for a 15x15]
    #[arg(long)]
    max_short_entries: Option<usize>,

    /// Drop the 180-degree rotational symmetry requirement
    #[arg(long, default_value_t = false)]
    no_symmetry: bool,

    /// How many filled grids to return
    #[arg(long, default_value_t = 5)]
    count: usize,

    /// How many legal topologies to sample before ranking them
    #[arg(long, default_value_t = 500)]
    pool: usize,

    /// Overall time budget in seconds
    #[arg(long, default_value_t = 120)]
    timeout: u64,

    /// Time budget for filling a single grid, in seconds [default: scaled to the grid, 5 for a
    /// 15x15 and 45 for a 21x21]
    #[arg(long)]
    fill_timeout: Option<u64>,

    /// Print legal topologies without trying to fill them
    #[arg(long, default_value_t = false)]
    emit_templates: bool,

    /// Path to a scored wordlist file [default: (embedded copy of Spread the Wordlist)]
    #[arg(long)]
    wordlist: Option<String>,

    /// Minimum allowable word score
    #[arg(long, default_value_t = 50)]
    min_score: u16,

    /// Maximum shared substring length between entries [default: none]
    #[arg(long)]
    max_shared_substring: Option<usize>,

    /// Seed for the randomised search; the same seed gives the same grids
    #[arg(long, default_value_t = 0)]
    seed: u64,

    /// Print search and oracle counters to stderr
    #[arg(short, long, default_value_t = false)]
    verbose: bool,
}

/// Conventional limits for a grid of this many squares.
///
/// The two anchors are the standard American sizes: a 15x15 daily runs to 78 entries and roughly
/// 30-42 blocks, a 21x21 Sunday to 140 entries and roughly 59-82. Interpolating between them beats
/// hardcoding the 15x15 numbers, which silently make every larger grid impossible -- a 21x21 has 42
/// entries before a single block is placed, so a cap of 78 rejects everything.
struct SizeDefaults {
    min_blocks: usize,
    max_blocks: usize,
    max_words: usize,
    max_short_entries: usize,
    fill_timeout: u64,
}

impl SizeDefaults {
    fn for_grid(width: usize, height: usize) -> SizeDefaults {
        let cells = (width * height) as f64;

        // Block counts sit at a near-constant fraction of the grid across both standard sizes.
        let min_blocks = (cells * 0.133).round() as usize;
        let max_blocks = (cells * 0.187).round() as usize;

        // Entry counts don't: the line through (225, 78) and (441, 140).
        let max_words = (13.4 + 0.287 * cells).round() as usize;

        // Keep the same share of entries allowed to be minimum-length as a 15x15 gets.
        let max_short_entries = ((max_words as f64) * 10.0 / 78.0).round() as usize;

        // Fill difficulty grows much faster than area. 5s is comfortable for a 15x15; a 21x21 needs
        // tens of seconds, and measurements put the growth near the square of the area ratio.
        let ratio = cells / 225.0;
        let fill_timeout = ((5.0 * ratio * ratio).round() as u64).clamp(5, 120);

        SizeDefaults {
            min_blocks,
            max_blocks,
            max_words,
            max_short_entries,
            fill_timeout,
        }
    }
}

struct Error(String);

impl Debug for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0) // Print the error unquoted.
    }
}

fn main() -> Result<(), Error> {
    let args = Args::parse();
    let start = Instant::now();
    let deadline = start + Duration::from_secs(args.timeout);

    if args.count == 0 {
        return Err(Error("--count must be at least 1".into()));
    }

    let raw_grid = fs::read_to_string(&args.grid_path)
        .map_err(|_| Error(format!("Couldn't read file '{}'", args.grid_path)))?;

    // Size the conventional limits to the grid in front of us. `parse_problem` re-derives and
    // validates the dimensions; this only needs them well enough to pick defaults.
    let rough_height = raw_grid.lines().filter(|line| !line.trim().is_empty()).count();
    let rough_width = raw_grid
        .lines()
        .map(|line| line.trim().chars().count())
        .max()
        .unwrap_or(0);
    let defaults = SizeDefaults::for_grid(rough_width, rough_height);

    let min_blocks = args.min_blocks.unwrap_or(defaults.min_blocks);
    let max_blocks = args.max_blocks.unwrap_or(defaults.max_blocks);
    let fill_timeout = Duration::from_secs(args.fill_timeout.unwrap_or(defaults.fill_timeout));

    if min_blocks > max_blocks {
        return Err(Error("--min-blocks can't exceed --max-blocks".into()));
    }

    let settings = LayoutSettings {
        min_entry_length: args.min_entry_length,
        min_blocks,
        max_blocks,
        min_words: 0,
        max_words: args.max_words.unwrap_or(defaults.max_words),
        max_short_entries: args.max_short_entries.unwrap_or(defaults.max_short_entries),
        symmetry: if args.no_symmetry {
            Symmetry::None
        } else {
            Symmetry::Rotational180
        },
    };

    let (problem, root) = parse_problem(&raw_grid, settings).map_err(Error)?;

    if args.verbose {
        eprintln!(
            "{}x{} grid, {} theme {} to place",
            problem.width,
            problem.height,
            problem.theme_entries.len(),
            if problem.theme_entries.len() == 1 {
                "answer"
            } else {
                "answers"
            }
        );
        eprintln!(
            "  limits: {}-{} blocks, <={} entries, <={} minimum-length entries, {:?} per fill",
            problem.settings.min_blocks,
            problem.settings.max_blocks,
            problem.settings.max_words,
            problem.settings.max_short_entries,
            fill_timeout,
        );
        for theme in &problem.theme_entries {
            eprintln!(
                "  {:>16} at {:?} {:?}",
                theme.answer, theme.start, theme.direction
            );
        }
    }

    // The word list is loaded before the search, not after, because the search consults it: a grid
    // whose long down entry has to match `???s???n???d???` is worth abandoning while it is still
    // half-built rather than after it has been finished and handed to the solver.
    let mut oracle = if args.emit_templates {
        None
    } else {
        Some(load_oracle(&args, &problem)?)
    };

    if args.verbose && oracle.is_some() {
        eprintln!("word list loaded at {:?}", start.elapsed());
    }

    // Reserve part of the budget for filling. Sampling topologies is cheap by comparison, so it
    // should never be what uses up the clock.
    let search_deadline = if args.emit_templates {
        deadline
    } else {
        start + Duration::from_secs(args.timeout).mul_f64(0.35)
    };

    let mut pool: Vec<Layout> = vec![];
    let search_stats = search(
        &problem,
        &root,
        &SearchSettings {
            nodes_per_restart: 40_000,
            candidates_per_restart: 1,
            seed: args.seed,
            deadline: Some(search_deadline),
        },
        oracle
            .as_mut()
            .map(|oracle| oracle as &mut dyn ingrid_layout::layout::EntryViability),
        &mut |layout, _stats| {
            pool.push(layout.clone());
            if pool.len() >= args.pool {
                Flow::Stop
            } else {
                Flow::Continue
            }
        },
    );

    if args.verbose {
        eprintln!(
            "sampled {} topologies in {:?} ({} search nodes, {} restarts)",
            pool.len(),
            start.elapsed(),
            search_stats.nodes,
            search_stats.restarts
        );
    }

    if pool.is_empty() {
        return Err(Error(
            "Found no legal block arrangement for this theme. Try widening --min-blocks/--max-blocks \
             or --max-words, or moving the theme answers."
                .into(),
        ));
    }

    let weights = Weights::default();

    // Rank on geometry first so the expensive fill attempts go to the best-looking grids.
    let mut ranked: Vec<(f64, Layout, Metrics)> = pool
        .into_iter()
        .map(|layout| {
            let metrics = Metrics::measure(&problem, &layout);
            let score = metrics.geometric_score(&weights);
            (score, layout, metrics)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));

    if args.emit_templates {
        for (score, layout, metrics) in ranked.iter().take(args.count) {
            println!("# geometry score {score:.1}  {}", describe(metrics));
            println!("{}\n", layout.render(&problem));
        }
        return Ok(());
    }

    let mut oracle = oracle.expect("the oracle is only skipped when emitting templates");
    let mut results: Vec<Candidate> = vec![];
    let mut printed_unfillable_example = false;

    for (geometric_score, layout, metrics) in ranked {
        if results.len() >= args.count || Instant::now() >= deadline {
            break;
        }

        match oracle.evaluate(&problem, &layout, fill_timeout) {
            Verdict::Filled(report) => {
                let total_score = geometric_score + fill_score(&report, &weights);
                results.push(Candidate {
                    template: layout.render(&problem),
                    metrics,
                    geometric_score,
                    fill: Some(report),
                    total_score,
                });
            }
            Verdict::NoWordForSlot { slot, pattern } if args.verbose => {
                eprintln!("  rejected: slot {slot} needs a word matching {pattern}");
            }
            // When nothing is filling, the single most useful thing to see is one of the grids that
            // didn't, so you can tell whether the block pattern or the theme is at fault.
            Verdict::Unfillable if args.verbose && !printed_unfillable_example => {
                printed_unfillable_example = true;
                eprintln!(
                    "  first grid the solver proved unfillable:\n{}",
                    layout.render(&problem)
                );
            }
            _ => {}
        }
    }

    results.sort_by(|a, b| b.total_score.total_cmp(&a.total_score));

    if args.verbose {
        let stats = &oracle.stats;
        eprintln!(
            "tried {} topologies in {:?}: {} filled, {} unfillable, {} timed out, {} had a slot with no word",
            stats.attempts,
            stats.total_time,
            stats.filled,
            stats.unfillable,
            stats.timed_out,
            stats.rejected_by_empty_slot,
        );
        eprintln!(
            "slot option cache: {} hits, {} misses",
            stats.options_cache_hits, stats.options_cache_misses
        );
        eprintln!("total {:?}", start.elapsed());
    }

    if results.is_empty() {
        return Err(Error(format!(
            "Sampled {} legal topologies but none of them filled. Try a lower --min-score, a longer \
             --fill-timeout, or a larger --pool.",
            oracle.stats.attempts
        )));
    }

    for (rank, candidate) in results.iter().enumerate() {
        print_candidate(rank + 1, results.len(), candidate);
    }

    if results.len() < args.count {
        eprintln!(
            "\nOnly found {} fillable {} of the {} requested within the time budget.",
            results.len(),
            if results.len() == 1 { "grid" } else { "grids" },
            args.count
        );
    }

    Ok(())
}

fn load_oracle(args: &Args, problem: &ingrid_layout::layout::Problem) -> Result<Oracle, Error> {
    let word_list = WordList::new(
        vec![match &args.wordlist {
            Some(path) => WordListSourceConfig {
                id: "0".into(),
                enabled: true,
                provider: WordListSourceConfigProvider::File { path: path.into() },
                normalization: None,
            },
            None => WordListSourceConfig {
                id: "0".into(),
                enabled: true,
                provider: WordListSourceConfigProvider::FileContents { contents: STWL_RAW },
                normalization: None,
            },
        }],
        None,
        Some(problem.width.max(problem.height)),
        args.max_shared_substring,
    );

    if let Some(errors) = word_list.get_source_errors().get("0") {
        if !errors.is_empty() {
            let mut message = String::new();
            for error in errors {
                message.push_str(&format!("\n- {error}"));
            }
            return Err(Error(message));
        }
    }
    if word_list.word_id_by_string.is_empty() {
        return Err(Error("Word list is empty".into()));
    }

    Ok(Oracle::new(word_list, args.min_score))
}

fn describe(metrics: &Metrics) -> String {
    format!(
        "blocks {}  words {}  short {}  long {}  cheaters {}  clump {}  hotspots {}",
        metrics.block_count,
        metrics.word_count,
        metrics.short_entries,
        metrics.long_entries,
        metrics.cheater_squares,
        metrics.largest_block_clump,
        metrics.theme_crossing_hotspots,
    )
}

fn print_candidate(rank: usize, total: usize, candidate: &Candidate) {
    println!("=== grid {rank} of {total} ===");
    print!("score {:.1}", candidate.total_score);
    if let Some(fill) = &candidate.fill {
        println!(
            "  (geometry {:.1}, fill {:.1})",
            candidate.geometric_score,
            candidate.total_score - candidate.geometric_score
        );
        println!("{}", describe(&candidate.metrics));
        println!(
            "word score: mean {:.1}, worst {}   filled in {:?} after {} {}",
            fill.mean_word_score,
            fill.min_word_score,
            fill.elapsed,
            fill.retries,
            if fill.retries == 1 { "retry" } else { "retries" }
        );
        println!("\n{}\n", candidate.template);
        println!("{}\n", fill.grid);
    } else {
        println!();
        println!("{}", describe(&candidate.metrics));
        println!("\n{}\n", candidate.template);
    }
}
