//! `ingrid_layout`: work out where a crossword's theme answers and black squares go.
//!
//! Three stages, each feeding the next. Decide where the theme answers sit (skipped when the grid
//! file already says); sample a pool of legal block topologies for each placement using geometry
//! alone; then rank the pool and spend the expensive fill attempts on the most promising grids,
//! best first.

use std::fmt::{Debug, Formatter};
use std::fs;
use std::time::{Duration, Instant};

use clap::Parser;

use ingrid_core::word_list::{WordList, WordListSourceConfig, WordListSourceConfigProvider};
use ingrid_layout::layout::{
    build_problem, parse_template, search, Flow, Layout, LayoutSettings, SearchSettings,
    SearchStats, Symmetry, Template,
};
use ingrid_layout::oracle::{Oracle, Verdict};
use ingrid_layout::score::{fill_score, Candidate, Metrics, Weights};
use ingrid_layout::theme::{
    parse_answers, place, Placement, PlacementStats, ThemeSettings, ThemeWeights,
};

/// Bundled copy of Spread the Wordlist, by Brooke Husic and Enrique Henestroza Anguiano. Vendored
/// so the tool works with no configuration and the crate builds from a fresh clone.
const STWL_RAW: &str = include_str!("../resources/spreadthewordlist.dict");

/// ingrid_layout: crossword block-placement tool
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the theme grid: `?` for undecided squares, `#` for blocks you want to keep, `.` for
    /// squares you want to stay open, and letters for the theme answers. Optional if you give
    /// `--answers` and `--size` instead
    grid_path: Option<String>,

    /// Path to a list of theme answers, one per line, for the tool to place itself. Blank lines and
    /// `#` comments are ignored, and spaces and punctuation are dropped
    #[arg(long)]
    answers: Option<String>,

    /// Grid size for `--answers` when there is no grid file, e.g. `15x15` or `15`
    #[arg(long)]
    size: Option<String>,

    /// How many theme placements to sample before ranking them
    #[arg(long, default_value_t = 12)]
    theme_pool: usize,

    /// Let theme answers run Down as well as Across
    #[arg(long, default_value_t = false)]
    theme_down: bool,

    /// Allow two theme answers on adjacent rows
    #[arg(long, default_value_t = false)]
    stacked_themes: bool,

    /// Allow a theme answer whose 180-degree mirror is an ordinary entry rather than another theme
    /// answer. Needed when the answer lengths can't pair up
    #[arg(long, default_value_t = false)]
    loose_theme_symmetry: bool,

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

    /// Overall time budget in seconds; 0 removes the limit and tries every topology sampled
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

    /// How often to print a progress line to stderr, in seconds; 0 turns progress off
    #[arg(long, default_value_t = 5)]
    progress: u64,
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

/// Consecutive fruitless restarts before a theme placement is abandoned, when there are others
/// waiting.
///
/// This is a **backstop, not the main bound** -- that job belongs to the per-placement deadline
/// slice, which is fair by construction and scales with `--timeout`. Set too low, this preempts the
/// slice and throws away placements that were merely slow: a placement needing 37 restarts to find
/// its first topology is entirely normal on a tight theme, so a limit of a dozen quietly discards
/// working answers and reports them as hopeless. It only wants to be small enough that a genuinely
/// empty search doesn't burn a long slice proving it.
const BARREN_RESTART_LIMIT: usize = 100;

/// What to do when placements come back with nothing.
///
/// The two causes want opposite fixes and the counters can't tell them apart, so this hands over the
/// one experiment that can: `--emit-templates` runs the identical geometry search with the word list
/// unplugged. Plenty of topologies means the geometry is fine and the dictionary is the wall; none
/// means the grid and the bounds can't be satisfied at all, and no `--min-score` will help.
const DIAGNOSE_BARREN: &str = "\
Run the same command with --emit-templates to find out which half is at fault -- it does the same \
geometry search without the word list, and takes seconds.

  * Lots of topologies there, none here: the word list is the wall. Entries crossing two theme \
answers often match nothing. Lower --min-score, or supply a bigger --wordlist.
  * Nothing there either: the geometry itself is over-constrained. Widen --min-blocks/--max-blocks \
(a partial grid that already pins blocks eats into that range), raise --max-words and \
--max-short-entries, or free up squares in the grid file.

Either way, a smaller --theme-pool gives each placement more of the budget, which is the better \
trade when placements are this hard.";

struct Error(String);

impl Debug for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0) // Print the error unquoted.
    }
}

fn main() -> Result<(), Error> {
    let args = Args::parse();
    let start = Instant::now();

    // `--timeout 0` means "no overall limit". What still bounds the run is `--pool` (the sampler
    // stops once it has that many topologies), `--count`, and `--fill-timeout` per grid; only the
    // wall clock over the whole run goes away.
    let deadline = (args.timeout > 0).then(|| start + Duration::from_secs(args.timeout));

    if args.count == 0 {
        return Err(Error("--count must be at least 1".into()));
    }

    // Either the grid file says how big the puzzle is, or `--size` does.
    let template = match (&args.grid_path, &args.size) {
        (Some(path), _) => {
            let raw = fs::read_to_string(path)
                .map_err(|_| Error(format!("Couldn't read file '{path}'")))?;
            parse_template(&raw).map_err(Error)?
        }
        (None, Some(size)) => {
            let (width, height) = parse_size(size)?;
            Template::blank(width, height)
        }
        (None, None) => {
            return Err(Error(
                "Give a grid file, or --answers with --size (e.g. --answers theme.txt --size 15x15)."
                    .into(),
            ))
        }
    };

    let answers = match &args.answers {
        Some(path) => {
            let raw = fs::read_to_string(path)
                .map_err(|_| Error(format!("Couldn't read file '{path}'")))?;
            Some(parse_answers(&raw).map_err(Error)?)
        }
        None => None,
    };

    // Size the conventional limits to the grid in front of us.
    let defaults = SizeDefaults::for_grid(template.width, template.height);

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

    // Progress goes to stderr on its own, without `--verbose`: the whole point of it is to be there
    // during a long run, and the person watching a long run isn't necessarily the person who wanted
    // the counters. Piping stdout to a file still gets you a clean file.
    let progress_interval = match args.progress {
        0 => None,
        secs => Some(Duration::from_secs(secs)),
    };

    if args.verbose {
        eprintln!("{}x{} grid", template.width, template.height);
        eprintln!(
            "  limits: {}-{} blocks, <={} entries, <={} minimum-length entries, {:?} per fill",
            settings.min_blocks,
            settings.max_blocks,
            settings.max_words,
            settings.max_short_entries,
            fill_timeout,
        );
    }

    // Stage one: where do the theme answers go? Before the word list loads, because that takes
    // seconds and a theme with nowhere to go should say so immediately.
    let theme_weights = ThemeWeights::default();
    let placements = resolve_placements(
        &args,
        &template,
        &settings,
        answers.as_deref(),
        &theme_weights,
        deadline,
    )?;
    let placement_count = placements.len();

    if args.verbose {
        eprintln!(
            "{} theme placement{} to try; best one (theme score {:.1}: {}):",
            placements.len(),
            if placements.len() == 1 { "" } else { "s" },
            placements[0].score,
            describe_placement(&placements[0].metrics),
        );
        for theme in placements[0].entries() {
            eprintln!(
                "  {:>18} at {:?} {:?}",
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
        // Loading Spread the Wordlist takes seconds, and it happens before anything else prints.
        if progress_interval.is_some() {
            eprintln!("{} loading word list", stamp(start.elapsed()));
        }
        Some(load_oracle(&args, template.width, template.height)?)
    };

    if args.verbose && oracle.is_some() {
        eprintln!("word list loaded at {:?}", start.elapsed());
    }

    // Reserve part of the budget for filling. Sampling topologies is cheap by comparison, so it
    // should never be what uses up the clock. With no overall deadline there is nothing to divide,
    // and the sampler runs until `--pool` is full.
    let search_deadline = match deadline {
        Some(deadline) if args.emit_templates => Some(deadline),
        Some(_) => Some(start + Duration::from_secs(args.timeout).mul_f64(0.35)),
        None => None,
    };

    // Stage two: block topologies. Each theme placement gets an equal share of the pool, so a
    // single lucky placement can't crowd the others out of the ranking before they are tried.
    let mut pool: Vec<(usize, Layout)> = vec![];
    let mut barren_placements = 0;
    let per_placement = args.pool.div_ceil(placements.len());
    let mut search_stats = SearchStats::default();

    for (index, placement) in placements.iter().enumerate() {
        if pool.len() >= args.pool
            || search_deadline.is_some_and(|deadline| Instant::now() >= deadline)
        {
            break;
        }

        // Its own slice of what's left, rather than the whole remaining budget. A theme placement
        // whose topologies are rare makes the sampler grind -- the viability check rejects branch
        // after branch -- and on a shared deadline the first such placement swallows the entire
        // budget and the other eleven never run at all. Slicing what remains by how many are left
        // keeps that local: a slow placement costs its own turn and nobody else's, and placements
        // that finish early hand their unused time to the ones after them.
        let placements_left = (placement_count - index) as u32;
        let slice_deadline = search_deadline.map(|deadline| {
            let now = Instant::now();
            now + deadline.saturating_duration_since(now) / placements_left
        });

        let mut found_here = 0;
        let stats = search(
            &placement.problem,
            &placement.root,
            &SearchSettings {
                nodes_per_restart: 40_000,
                candidates_per_restart: 1,
                // A different seed per placement, so two placements that happen to have the same
                // shape don't explore it in the same order and hand back the same grids.
                seed: args.seed.wrapping_add(index as u64),
                deadline: slice_deadline,
                // Only when there is more than one placement to get through. With a single one
                // there is nothing else to spend the time on, so restarting until the deadline is
                // exactly right; with twelve, a barren one has to be cut loose.
                barren_restart_limit: (placement_count > 1).then_some(BARREN_RESTART_LIMIT),
                progress_interval,
            },
            oracle
                .as_mut()
                .map(|oracle| oracle as &mut dyn ingrid_layout::layout::EntryViability),
            &mut |layout, _stats| {
                pool.push((index, layout.clone()));
                found_here += 1;
                if found_here >= per_placement || pool.len() >= args.pool {
                    Flow::Stop
                } else {
                    Flow::Continue
                }
            },
            // Passed unconditionally; `progress_interval: None` is what turns it off. Reports
            // `stats.candidates` rather than `pool.len()` because `pool` is already borrowed by the
            // candidate callback above, and two closures can't hold it mutably at once.
            Some(&mut |stats: &SearchStats| {
                eprintln!(
                    "{} sampling: theme placement {} of {}, {} of {} topologies, {} nodes, \
                     {} restarts, {} duplicates",
                    stamp(start.elapsed()),
                    index + 1,
                    placement_count,
                    stats.candidates,
                    per_placement,
                    stats.nodes,
                    stats.restarts,
                    stats.duplicates,
                );
            }),
        );

        search_stats.nodes += stats.nodes;
        search_stats.restarts += stats.restarts;
        search_stats.duplicates += stats.duplicates;

        if found_here == 0 {
            barren_placements += 1;
        }
        if args.verbose {
            eprintln!(
                "{} theme placement {} of {}: {} topolog{} in {} restarts{}",
                stamp(start.elapsed()),
                index + 1,
                placement_count,
                found_here,
                if found_here == 1 { "y" } else { "ies" },
                stats.restarts,
                if stats.gave_up_barren {
                    ", gave up (nothing legal turning up)"
                } else {
                    ""
                },
            );
        }
    }

    if args.verbose {
        eprintln!(
            "sampled {} topologies across {} theme placement{} in {:?} ({} search nodes, {} restarts)",
            pool.len(),
            placement_count,
            if placement_count == 1 { "" } else { "s" },
            start.elapsed(),
            search_stats.nodes,
            search_stats.restarts
        );
    } else if progress_interval.is_some() {
        // The handover between the two phases, and the one number that explains a disappointing
        // run better than any other: how big the pool the filling has to work with actually is.
        eprintln!(
            "{} sampled {} of {} topologies, now filling",
            stamp(start.elapsed()),
            pool.len(),
            args.pool,
        );
    }

    if pool.is_empty() {
        return Err(Error(format!(
            "Found no legal block arrangement for any of the {placement_count} theme placement{}.\n\n\
             {}",
            if placement_count == 1 { "" } else { "s" },
            DIAGNOSE_BARREN,
        )));
    }

    // Barren placements are the loudest signal this run has to offer and they are otherwise only
    // visible as progress lines scrolling past with a topology count stuck at zero. What makes them
    // worth a line of their own is that the usual cause isn't the geometry -- it's the word list
    // rejecting every entry the geometry builds, and `--min-score` is the knob for that, which
    // nothing else in the output would point you towards.
    if barren_placements > 0 && !args.emit_templates {
        eprintln!(
            "\n{barren_placements} of {placement_count} theme placements produced no legal grid at \
             all.\n\n{DIAGNOSE_BARREN}\n"
        );
    }

    let weights = Weights::default();

    // Rank on geometry first so the expensive fill attempts go to the best-looking grids. The theme
    // placement's own score rides along, so grids from a better-shaped theme start ahead -- which
    // is the only thing that makes one ranking over several placements meaningful.
    let mut ranked: Vec<(f64, usize, Layout, Metrics)> = pool
        .into_iter()
        .map(|(index, layout)| {
            let metrics = Metrics::measure(&placements[index].problem, &layout);
            let score = metrics.geometric_score(&weights) + placements[index].score;
            (score, index, layout, metrics)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));

    if args.emit_templates {
        for (score, index, layout, metrics) in ranked.iter().take(args.count) {
            println!("# geometry score {score:.1}  {}", describe(metrics));
            println!("{}\n", layout.render(&placements[*index].problem));
        }
        return Ok(());
    }

    let mut oracle = oracle.expect("the oracle is only skipped when emitting templates");
    let mut results: Vec<Candidate> = vec![];
    let mut printed_unfillable_example = false;
    let attempts_available = ranked.len();
    // A single fill attempt can run for the whole `--fill-timeout`, so the heartbeat can only fire
    // between attempts. That still tells you the run is moving, and how the verdicts are trending.
    let mut next_progress = progress_interval.map(|interval| Instant::now() + interval);

    for (attempt, (geometric_score, index, layout, metrics)) in ranked.into_iter().enumerate() {
        if results.len() >= args.count || deadline.is_some_and(|deadline| Instant::now() >= deadline)
        {
            break;
        }

        let problem = &placements[index].problem;
        let verdict = oracle.evaluate(problem, &layout, fill_timeout);

        // One line per attempt under `--verbose`, and a throttled summary otherwise, so the two
        // don't say the same thing twice.
        if args.verbose {
            eprintln!(
                "{} grid {} of {} (geometry {:.1}): {}",
                stamp(start.elapsed()),
                attempt + 1,
                attempts_available,
                geometric_score,
                match &verdict {
                    Verdict::Filled(report) => format!(
                        "filled in {:.1}s, mean word score {:.1}",
                        report.elapsed.as_secs_f64(),
                        report.mean_word_score
                    ),
                    Verdict::NoWordForSlot { slot, pattern } =>
                        format!("slot {slot} needs a word matching {pattern}"),
                    Verdict::Unfillable => "the solver proved it unfillable".into(),
                    Verdict::TimedOut => "gave up at the fill timeout".into(),
                },
            );
        } else if let Some(due) = next_progress {
            if Instant::now() >= due {
                next_progress = progress_interval.map(|interval| Instant::now() + interval);
                let stats = &oracle.stats;
                eprintln!(
                    "{} filling: found {} of {} grids, tried {} of {} topologies \
                     ({} unfillable, {} timed out, {} with an impossible slot)",
                    stamp(start.elapsed()),
                    // `oracle.stats` already counts the attempt that just finished; `results` won't
                    // until the match below pushes it.
                    stats.filled,
                    args.count,
                    stats.attempts,
                    attempts_available,
                    stats.unfillable,
                    stats.timed_out,
                    stats.rejected_by_empty_slot,
                );
            }
        }

        match verdict {
            Verdict::Filled(report) => {
                let total_score = geometric_score + fill_score(&report, &weights);
                results.push(Candidate {
                    template: layout.render(problem),
                    metrics,
                    geometric_score,
                    fill: Some(report),
                    total_score,
                });
            }
            // A timeout is the one verdict that isn't an answer: the solver never proved anything
            // about this grid, it just ran out of clock (see the `Verdict` docs). So print it as it
            // happens rather than dropping it -- it is the one worth a longer budget or a human.
            // Goes to stderr with the rest of the running commentary, so a redirected stdout still
            // holds nothing but finished grids.
            // Printed unconditionally, unlike the heartbeat: `--progress` sets how often to repeat a
            // running summary, and a timeout isn't that. It happens once, and silencing a periodic
            // ticker shouldn't throw away the one grid the solver couldn't answer for. `2>/dev/null`
            // if you don't want them.
            Verdict::TimedOut => {
                eprintln!(
                    "{} grid {} of {} timed out after {:?} -- unproven, so it may still fill. \
                     Fill it by hand, or save it and rerun with a longer --fill-timeout:\n\n{}\n",
                    stamp(start.elapsed()),
                    attempt + 1,
                    attempts_available,
                    fill_timeout,
                    layout.render(problem),
                );
            }
            // When nothing is filling, the single most useful thing to see is one of the grids that
            // didn't, so you can tell whether the block pattern or the theme is at fault.
            Verdict::Unfillable if args.verbose && !printed_unfillable_example => {
                printed_unfillable_example = true;
                eprintln!(
                    "  first grid the solver proved unfillable:\n{}",
                    layout.render(problem)
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
        // Which limit actually stopped us is the useful part: raising `--pool` and raising
        // `--timeout` fix different runs, and with `--timeout 0` only one of them is even possible.
        let limit = if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            "within the time budget".to_string()
        } else {
            format!("in the {attempts_available} topologies sampled")
        };
        eprintln!(
            "\nOnly found {} fillable {} of the {} requested {limit}.",
            results.len(),
            if results.len() == 1 { "grid" } else { "grids" },
            args.count,
        );
    }

    Ok(())
}

/// Parse a `--size` argument: `15x15`, or `15` for a square grid.
fn parse_size(text: &str) -> Result<(usize, usize), Error> {
    let bad = || Error(format!("Couldn't read --size {text:?}; try `15x15` or `15`."));

    let (width, height) = match text.to_lowercase().split_once('x') {
        Some((w, h)) => (w.trim().to_string(), h.trim().to_string()),
        None => (text.trim().to_string(), text.trim().to_string()),
    };

    let width: usize = width.parse().map_err(|_| bad())?;
    let height: usize = height.parse().map_err(|_| bad())?;

    if width == 0 || height == 0 {
        return Err(bad());
    }
    Ok((width, height))
}

/// Stage one: decide where the theme answers go, or take the constructor's word for it.
///
/// Both arms come back as `Placement`s so the rest of the program doesn't have to care which
/// happened -- a hand-placed theme is just a pool of exactly one.
fn resolve_placements(
    args: &Args,
    template: &Template,
    settings: &LayoutSettings,
    answers: Option<&[String]>,
    weights: &ThemeWeights,
    deadline: Option<Instant>,
) -> Result<Vec<Placement>, Error> {
    let Some(answers) = answers else {
        let (problem, root) = build_problem(template, settings.clone()).map_err(Error)?;
        let pre_fixed = template
            .fixed
            .iter()
            .filter(|cell| **cell == Some(ingrid_layout::layout::Cell::Block))
            .count();
        return Ok(vec![Placement::new(problem, root, pre_fixed, weights)]);
    };

    if args.theme_pool == 0 {
        return Err(Error("--theme-pool must be at least 1".into()));
    }

    let theme_settings = ThemeSettings {
        allow_down: args.theme_down,
        require_pairing: !args.loose_theme_symmetry,
        allow_stacked: args.stacked_themes,
        nodes_per_restart: 20_000,
        seed: args.seed,
        // Placement is cheap next to filling, so it gets a small slice of the budget. Bounded all
        // the same: a theme with no legal home at all would otherwise restart forever.
        deadline: deadline.map(|deadline| {
            let now = Instant::now();
            now + (deadline - now).mul_f64(0.1)
        }),
    };

    // Sample well past what we'll keep, then take the best. A placement costs a few hundred search
    // nodes to find and nothing to score, while each one we keep costs a share of `--pool` and a
    // run of fill attempts -- so it is much cheaper to be picky here than to be picky later.
    //
    // The multiplier is large because the space is small enough to nearly cover and the ranking is
    // only as good as what it gets to choose from. Four answers in a 15x15 have some hundreds of
    // legal arrangements; sampling 120 of them missed the one a human picked, and it missed it
    // despite that arrangement scoring *highest* on our own metrics. Under-sampling looks exactly
    // like a bad metric from the outside, which is what makes it worth spending nodes to rule out.
    let sample_target = args.theme_pool.saturating_mul(40).max(600);

    let mut found: Vec<Placement> = vec![];
    let stats: PlacementStats = place(
        template,
        settings,
        answers,
        &theme_settings,
        &mut |placement| {
            found.push(placement);
            if found.len() >= sample_target {
                Flow::Stop
            } else {
                Flow::Continue
            }
        },
    )
    .map_err(Error)?;

    if args.verbose {
        eprintln!(
            "theme placement: {} found, {} nodes, {} restarts, {} rejected by geometry",
            found.len(),
            stats.nodes,
            stats.restarts,
            stats.rejected_by_geometry,
        );
    }

    if found.is_empty() {
        return Err(Error(no_placement_message(args, stats)));
    }

    // Best-shaped theme first, then keep only as many as asked for: every placement kept costs a
    // share of `--pool`, and a thin slice of topologies on a mediocre placement finds nothing.
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    found.truncate(args.theme_pool);
    Ok(found)
}

/// The advice to give when the theme has nowhere to go. Which knob to reach for depends on *how* it
/// failed, and the counters say: placements that propagation rejected mean the arrangement is too
/// tight, while none even being attempted means the pairing rule ruled everything out up front.
fn no_placement_message(args: &Args, stats: PlacementStats) -> String {
    let mut message = String::from("Found nowhere to put these theme answers.");

    if !args.loose_theme_symmetry {
        message.push_str(
            "\n\nMost likely the answer lengths can't pair up: with 180-degree symmetry, two \
             answers mirror each other only if they're the same length, and a length with an odd \
             number of answers has to put one of them dead centre — which two answers can't both \
             do. Try --loose-theme-symmetry to let an answer's mirror be an ordinary entry.",
        );
    }
    if stats.rejected_by_geometry > 0 {
        message.push_str(&format!(
            "\n\n{} arrangement{} were found and then rejected by the block rules, so the answers \
             do fit but leave squares that can't reach --min-entry-length. Try --stacked-themes, \
             or wider --min-blocks/--max-blocks.",
            stats.rejected_by_geometry,
            if stats.rejected_by_geometry == 1 {
                ""
            } else {
                "s"
            },
        ));
    }
    if !args.theme_down {
        message.push_str("\n\n--theme-down lets answers run downward as well as across.");
    }

    message
}

fn load_oracle(args: &Args, width: usize, height: usize) -> Result<Oracle, Error> {
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
        Some(width.max(height)),
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

/// A short elapsed-time prefix for progress lines, e.g. `[  12s]`. `Duration`'s own `Debug` is too
/// precise to scan down a column of them.
fn stamp(elapsed: Duration) -> String {
    format!("[{:>4}s]", elapsed.as_secs())
}

fn describe_placement(metrics: &ingrid_layout::theme::PlacementMetrics) -> String {
    format!(
        "unpaired {}  outer-band {}  crossings {}  floating {}  stranded {}  forced blocks {}  free lines {}",
        metrics.unpaired,
        metrics.outer_band_entries,
        metrics.crossings,
        metrics.floating_entries,
        metrics.stranded_squares,
        metrics.forced_blocks,
        metrics.min_free_lines,
    )
}

fn describe(metrics: &Metrics) -> String {
    format!(
        "blocks {}  words {}  short {}  long {}  cheaters {}  clump {}  adj {}  fingers {}  hotspots {}",
        metrics.block_count,
        metrics.word_count,
        metrics.short_entries,
        metrics.long_entries,
        metrics.cheater_squares,
        metrics.largest_block_clump,
        metrics.block_adjacencies,
        metrics.side_fingers,
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
