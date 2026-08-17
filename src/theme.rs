//! The theme layer: deciding *where* the answers go, before a single black square exists.
//!
//! This runs above [`crate::layout`] and, like it, knows nothing about word lists. Its output is
//! exactly the other layer's input -- a [`Template`] with letters in it -- so the two compose
//! without either knowing much about the other:
//!
//! ```text
//!   answers  ->  theme::place  ->  Template  ->  layout::build_problem  ->  layout::search
//! ```
//!
//! ## What symmetry means up here
//!
//! Down in the geometry layer, 180-degree symmetry is free: [`crate::layout::Layout::set`] writes
//! every square and its rotational partner together, so no arrangement it produces can be
//! asymmetric. That is *legal* symmetry, and it is not what a constructor means by the word.
//!
//! What they mean is that the theme answers themselves mirror each other -- a 13 in row 3 answered
//! by a 13 in row 11 -- because otherwise the mirror of a theme entry is an ordinary entry of the
//! same length, which is both a wasted theme slot and a hard one to fill. So that is the rule this
//! layer enforces by default: **every theme entry's rotational mirror is another theme entry, or
//! the entry is its own mirror.** Answers therefore pair up by length, and a length with an odd
//! number of answers has to put one of them dead centre.
//!
//! That is a real constraint and it does reject things. Three answers of three different lengths
//! all want the centre line at once, and there is no way to give it to all of them. Hence
//! [`ThemeSettings::require_pairing`]: turning it off lets an answer's mirror be an ordinary entry,
//! which is inelegant but is what a constructor does when the word lengths don't cooperate.
//!
//! ## Why it is a sampler
//!
//! For the same reason the block search is. Three answers in a 15x15 have hundreds of thousands of
//! legal homes, and we only want to try a dozen. So this uses the same randomised-restart shape:
//! one placement per descent, fresh randomness each time, which buys independent samples instead of
//! a dozen grids that differ by one row.

use rand::prelude::*;
use rand::rngs::SmallRng;
use std::collections::HashSet;
use std::time::Instant;

use crate::layout::{
    build_problem, theme_boundary_indices, Cell, Direction, Flow, Layout, LayoutSettings, Problem,
    Symmetry, Template, ThemeEntry,
};

/// Knobs for the placement search.
#[derive(Debug, Clone)]
pub struct ThemeSettings {
    /// Let answers run Down as well as Across. Off by default: almost all themes are Across, and
    /// allowing Down roughly doubles the branching for a kind of grid most people don't want.
    pub allow_down: bool,
    /// Require every theme entry's rotational mirror to be a theme entry too. See the module docs.
    pub require_pairing: bool,
    /// Allow two parallel theme answers on adjacent lines. Off by default -- stacked themes are a
    /// real thing, but they are a deliberate choice, and allowing them by accident produces grids
    /// with no room left for fill.
    pub allow_stacked: bool,
    /// Search nodes to spend before abandoning a restart, as in the block search.
    pub nodes_per_restart: usize,
    pub seed: u64,
    pub deadline: Option<Instant>,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        ThemeSettings {
            allow_down: false,
            require_pairing: true,
            allow_stacked: false,
            nodes_per_restart: 20_000,
            seed: 0,
            deadline: None,
        }
    }
}

/// One decided home for the whole theme, already carried through the geometry layer's opening move.
///
/// Holding the built `Problem` and propagated `root` rather than just the positions means the block
/// search can start from it directly, and means a placement that survives propagation is the only
/// kind that ever reaches the caller.
#[derive(Debug, Clone)]
pub struct Placement {
    pub problem: Problem,
    pub root: Layout,
    pub metrics: PlacementMetrics,
    pub score: f64,
}

impl Placement {
    /// Wrap an already-built problem, for the path where the constructor placed the answers
    /// themselves and there was nothing for this layer to decide.
    #[must_use]
    pub fn new(
        problem: Problem,
        root: Layout,
        pre_fixed_blocks: usize,
        weights: &ThemeWeights,
    ) -> Placement {
        let metrics = PlacementMetrics::measure(&problem, &root, pre_fixed_blocks);
        let score = metrics.score(weights);
        Placement {
            problem,
            root,
            metrics,
            score,
        }
    }

    /// The theme answers as this layer placed them, best-effort ordered for display.
    #[must_use]
    pub fn entries(&self) -> &[ThemeEntry] {
        &self.problem.theme_entries
    }
}

/// Structural facts about a theme placement, independent of any block pattern.
#[derive(Debug, Clone, Default)]
pub struct PlacementMetrics {
    /// Theme entries whose rotational mirror is not itself a theme entry. Zero is the conventional
    /// answer; anything else means some ordinary entry has to carry a theme-sized slot.
    pub unpaired: usize,
    /// Theme entries in the outermost *two* lines at either end -- rows 1 and 2 of a grid, counting
    /// the way a constructor does, and their mirrors.
    ///
    /// Two rules in one, because they point the same way. An answer in the very first row is
    /// crossed on one side only and reads as an accident. An answer in the second row is legal and
    /// occasionally done, but rare, because every down entry crossing it has to start within a
    /// square of the wall -- so the whole top band of the grid is pinned before the block search
    /// has any say.
    pub outer_band_entries: usize,
    /// Squares shared by two theme entries. Each one pins a letter in both directions at once, so
    /// they are where the fill gets hardest.
    pub crossings: usize,
    pub theme_squares: usize,
    /// Squares an answer's position wastes *along its own line*: the leftovers between its boundary
    /// block and the grid edge, when there are too few of them to be an entry.
    ///
    /// A 13 in a 15-wide grid shows the idea. At columns 1-13 its two boundary blocks are the edge
    /// columns and nothing is left over. Shift it one either way and the far end has two squares to
    /// play with -- one becomes the boundary, the other is walled in against the edge.
    ///
    /// Measured but *not* scored by default, because it turned out to be the small half of the
    /// effect: see `forced_blocks`.
    pub stranded_squares: usize,
    /// Theme entries that neither span their whole line nor sit flush against one end of it.
    ///
    /// Constructors place theme answers hard against alternating walls -- one flush left, the next
    /// flush right -- and the reason is block economy. An answer against a wall needs a boundary
    /// block on one side only, and whatever is left over past it forms a straight run of blocks
    /// growing inward from the *other* wall, which is a useful structure (a "finger"). The same
    /// answer moved one square inward needs boundary blocks on both sides, and the isolated one now
    /// cuts the edge column in half for no benefit at all.
    ///
    /// Under 180-degree symmetry the alternation comes free: the mirror of a flush-left answer is a
    /// flush-right one. So this only has to count answers that are flush against *neither* wall,
    /// and the pairing rule takes care of making them alternate.
    pub floating_entries: usize,
    /// How many squares are already black once the placement has been propagated.
    ///
    /// This is the metric that earns its keep, and it replaced a pile of hand-rolled geometry.
    /// Every proxy for "how much does this placement cost the grid" was really trying to
    /// approximate what propagation computes exactly and for free: a theme answer forces boundary
    /// blocks, those blocks cut the perpendicular entries, the stubs left over are too short to be
    /// entries, so they go black too, and so on until it settles.
    ///
    /// The effect is much bigger than it sounds. A 12-letter answer on row 2 of a 15x15 piles its
    /// boundary blocks into a corner and strands the whole 3x2 patch outside them -- six squares
    /// spent before the block search has made a single choice. The same answer on row 5 has room
    /// above and below and strands nothing.
    pub forced_blocks: usize,
    /// The tightest stretch of non-theme lines anywhere in the grid: between two parallel theme
    /// entries, or between one and the edge it faces.
    ///
    /// Counting the edges is the whole point. Theme rows at 1/7/13 of a 15 and at 3/7/11 have the
    /// same gaps *between* them, but the first leaves a single row above and below, so every down
    /// entry in the grid has to thread a theme letter within one square of starting. Measured
    /// against the published grid for this crate's example theme, that difference is the
    /// difference between a topology pool that fills and one that doesn't.
    pub min_free_lines: usize,
}

/// Weights for turning [`PlacementMetrics`] into one number, in the same "higher is better,
/// centred near zero" convention as [`crate::score::Weights`].
#[derive(Debug, Clone)]
pub struct ThemeWeights {
    pub unpaired: f64,
    pub outer_band_entry: f64,
    pub crossing: f64,
    pub stranded_square: f64,
    /// Applied to each theme entry flush against neither wall. See `floating_entries` -- this is
    /// the constructor's "alternate hugging opposite walls" rule, and it is the single biggest
    /// lever this layer has on whether the grids it proposes can be filled.
    pub floating_entry: f64,
    /// Applied to every square propagation has already blackened. Negative, but gently: a grid
    /// needs blocks eventually, and this only says that spending them before the search starts is
    /// spending them without choosing them.
    pub forced_block: f64,
    /// Applied to `min_free_lines`, capped at `free_lines_cap`. The cap is what stops a theme with
    /// one answer in it from winning on emptiness alone; three free lines is already all the room a
    /// 15x15 with three theme rows can offer.
    pub free_lines: f64,
    pub free_lines_cap: usize,
}

impl Default for ThemeWeights {
    fn default() -> Self {
        ThemeWeights {
            unpaired: -6.0,
            outer_band_entry: -3.0,
            crossing: -1.0,
            stranded_square: 0.0,
            floating_entry: -4.0,
            forced_block: -0.5,
            free_lines: 2.0,
            free_lines_cap: 3,
        }
    }
}

impl PlacementMetrics {
    #[must_use]
    /// `pre_fixed_blocks` is how many blocks the constructor had already written into the grid, so
    /// that `forced_blocks` counts only what *this placement* cost. Without it, handing in a grid
    /// that is already fully blocked charges the placement for every square the constructor chose
    /// deliberately, which is both wrong and enough to rank a real published grid below the tool's
    /// own output.
    pub fn measure(problem: &Problem, root: &Layout, pre_fixed_blocks: usize) -> PlacementMetrics {
        let themes = &problem.theme_entries;

        let unpaired = themes
            .iter()
            .filter(|theme| !has_theme_mirror(problem, theme))
            .count();

        let outer_band_entries = themes
            .iter()
            .filter(|theme| {
                let (line, _) = theme.line_and_span();
                let last = match theme.direction {
                    Direction::Across => problem.height - 1,
                    Direction::Down => problem.width - 1,
                };
                line <= 1 || line + 1 >= last
            })
            .count();

        let mut covered = vec![0u8; problem.cell_count()];
        for theme in themes {
            for idx in theme.cells(problem) {
                covered[idx] = covered[idx].saturating_add(1);
            }
        }

        // An answer flush against either wall of its line, or spanning the whole thing, is
        // "anchored"; anything else floats in the middle and pays for boundary blocks twice.
        let floating_entries = themes
            .iter()
            .filter(|theme| {
                let (_, span) = theme.line_and_span();
                let line_length = match theme.direction {
                    Direction::Across => problem.width,
                    Direction::Down => problem.height,
                };
                span.start != 0 && span.end != line_length
            })
            .count();

        let min_entry_length = problem.settings.min_entry_length;
        let stranded_squares: usize = themes
            .iter()
            .map(|theme| {
                let (_, span) = theme.line_and_span();
                let line_length = match theme.direction {
                    Direction::Across => problem.width,
                    Direction::Down => problem.height,
                };
                // One square each side goes to the boundary block; whatever is left over past it
                // has to reach `min_entry_length` or it can never be an entry.
                [span.start, line_length - span.end]
                    .iter()
                    .map(|&room| match room.checked_sub(1) {
                        Some(leftover) if leftover > 0 && leftover < min_entry_length => leftover,
                        _ => 0,
                    })
                    .sum::<usize>()
            })
            .sum();

        // Room to the edge the entry faces, on both sides...
        let mut min_free_lines = usize::MAX;
        for theme in themes {
            let (line, _) = theme.line_and_span();
            let last = match theme.direction {
                Direction::Across => problem.height - 1,
                Direction::Down => problem.width - 1,
            };
            min_free_lines = min_free_lines.min(line).min(last - line);
        }
        // ...and room between any two that shadow each other. A gap of `n` lines has `n - 1` free
        // ones between them, which is why the two measurements are comparable at all.
        for (i, a) in themes.iter().enumerate() {
            for b in &themes[i + 1..] {
                if let Some(gap) = parallel_gap(a, b) {
                    min_free_lines = min_free_lines.min(gap - 1);
                }
            }
        }

        PlacementMetrics {
            unpaired,
            outer_band_entries,
            crossings: covered.iter().filter(|&&n| n > 1).count(),
            theme_squares: covered.iter().filter(|&&n| n > 0).count(),
            stranded_squares,
            floating_entries,
            forced_blocks: root.block_count.saturating_sub(pre_fixed_blocks),
            // A grid with no theme entries at all has nothing constraining it anywhere.
            min_free_lines: if themes.is_empty() { 0 } else { min_free_lines },
        }
    }

    #[must_use]
    pub fn score(&self, weights: &ThemeWeights) -> f64 {
        weights.unpaired * self.unpaired as f64
            + weights.outer_band_entry * self.outer_band_entries as f64
            + weights.crossing * self.crossings as f64
            + weights.stranded_square * self.stranded_squares as f64
            + weights.floating_entry * self.floating_entries as f64
            + weights.forced_block * self.forced_blocks as f64
            + weights.free_lines * self.min_free_lines.min(weights.free_lines_cap) as f64
    }
}

/// Counters describing what the placement search did.
#[derive(Debug, Clone, Default)]
pub struct PlacementStats {
    pub nodes: usize,
    pub restarts: usize,
    /// Complete placements whose geometry the block layer then rejected -- most often because the
    /// forced boundary blocks strand a square that can't reach the minimum entry length.
    pub rejected_by_geometry: usize,
    pub duplicates: usize,
    pub placements: usize,
}

/// Read a list of theme answers, one per line.
///
/// Blank lines and `#` comments are skipped, and everything that isn't a letter or digit is dropped
/// so `"Intimate Apparel"` and `"intimateapparel"` mean the same thing -- which is how people
/// actually write a theme down.
pub fn parse_answers(input: &str) -> Result<Vec<String>, String> {
    let mut answers = vec![];

    for (line_no, line) in input.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let normalized: String = line
            .chars()
            .filter(|ch| ch.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();

        if normalized.is_empty() {
            return Err(format!(
                "line {} ({line:?}) has no letters in it",
                line_no + 1
            ));
        }
        answers.push(normalized);
    }

    if answers.is_empty() {
        return Err("no theme answers found".into());
    }

    Ok(answers)
}

/// Sample theme placements, calling `on_placement` with each distinct one.
///
/// `base` is whatever the constructor already fixed -- a blank grid, or a template with some blocks
/// and letters already in it. Answers are placed into whatever it leaves undecided.
pub fn place(
    base: &Template,
    settings: &LayoutSettings,
    answers: &[String],
    theme: &ThemeSettings,
    on_placement: &mut dyn FnMut(Placement) -> Flow,
) -> Result<PlacementStats, String> {
    let geometry = Problem::new(base.width, base.height, settings.clone())?;
    validate_answers(&geometry, answers, theme)?;

    // Pairing is a statement about rotational mirrors, so without rotational symmetry there is
    // nothing to pair against and every answer is placed on its own.
    let pairing = theme.require_pairing && settings.symmetry != Symmetry::None;

    let pre_fixed_blocks = base
        .fixed
        .iter()
        .filter(|cell| **cell == Some(Cell::Block))
        .count();

    let mut placer = Placer {
        geometry,
        pre_fixed_blocks,
        base,
        settings: settings.clone(),
        theme,
        answers,
        pairing,
        rng: SmallRng::seed_from_u64(theme.seed),
        letters: base.letters.clone(),
        placed: vec![],
        seen: HashSet::new(),
        stats: PlacementStats::default(),
        node_budget: 0,
        out_of_time: false,
        found_this_restart: 0,
    };

    loop {
        placer.node_budget = theme.nodes_per_restart;
        placer.found_this_restart = 0;
        placer.stats.restarts += 1;

        let units = placer.build_units();
        let flow = placer.descend(&units, 0, on_placement);

        if flow == Flow::Stop || placer.out_of_time {
            break;
        }
        // A restart that walked its whole tree without running out of budget and still found
        // nothing new means there is nothing left to find, so restarting again would only repeat it.
        if placer.node_budget > 0 && placer.found_this_restart == 0 {
            break;
        }
    }

    Ok(placer.stats)
}

/// Reject answers that can never be placed, with a message that says which one and why. Doing this
/// before the search means "no placement found" is only ever about the *arrangement*.
fn validate_answers(
    problem: &Problem,
    answers: &[String],
    theme: &ThemeSettings,
) -> Result<(), String> {
    let longest_line = if theme.allow_down {
        problem.width.max(problem.height)
    } else {
        problem.width
    };

    for answer in answers {
        let length = answer.chars().count();

        if length < problem.settings.min_entry_length {
            return Err(format!(
                "theme answer {answer:?} is only {length} square{} long, but entries have to be at \
                 least {}. Lengthen it, or lower --min-entry-length.",
                if length == 1 { "" } else { "s" },
                problem.settings.min_entry_length,
            ));
        }
        if length > longest_line {
            return Err(format!(
                "theme answer {answer:?} is {length} squares long, which doesn't fit in a \
                 {}x{} grid{}.",
                problem.width,
                problem.height,
                if theme.allow_down {
                    ""
                } else {
                    " across (add --theme-down to let answers run downward)"
                },
            ));
        }
    }

    Ok(())
}

/// One decision the search makes at a time.
///
/// A `Pair` is two answers of the same length placed as each other's mirrors -- one choice of
/// position settles both, which is what makes the paired search so much smaller than placing every
/// answer independently.
#[derive(Debug, Clone, Copy)]
enum Unit {
    Pair(usize, usize),
    Single(usize),
}

impl Unit {
    fn is_pair(self) -> bool {
        matches!(self, Unit::Pair(..))
    }
}

struct Placer<'a> {
    /// Blocks the constructor had already written into `base`, so they aren't charged to the
    /// placement. See [`PlacementMetrics::measure`].
    pre_fixed_blocks: usize,
    /// Dimensions and symmetry only; its `letters` stay empty. Used for `coord`/`index`/`partner`,
    /// which the search needs long before it has a letter grid worth building a `Problem` from.
    geometry: Problem,
    base: &'a Template,
    settings: LayoutSettings,
    theme: &'a ThemeSettings,
    answers: &'a [String],
    pairing: bool,
    rng: SmallRng,
    letters: Vec<Option<char>>,
    placed: Vec<ThemeEntry>,
    seen: HashSet<Vec<Option<char>>>,
    stats: PlacementStats,
    node_budget: usize,
    out_of_time: bool,
    found_this_restart: usize,
}

/// How often to consult the clock, in search nodes.
const DEADLINE_CHECK_INTERVAL: usize = 256;

impl Placer<'_> {
    /// Group the answers into the units this restart will place.
    ///
    /// The grouping is randomised, and that matters: with three 13-letter answers, *which* two of
    /// them pair up and which goes in the centre is a real choice, and shuffling before chunking is
    /// how successive restarts get to make it differently.
    fn build_units(&mut self) -> Vec<Unit> {
        let mut by_length: Vec<(usize, Vec<usize>)> = vec![];
        for (i, answer) in self.answers.iter().enumerate() {
            let length = answer.chars().count();
            match by_length.iter_mut().find(|(len, _)| *len == length) {
                Some((_, group)) => group.push(i),
                None => by_length.push((length, vec![i])),
            }
        }

        let mut units = vec![];
        for (_, group) in &mut by_length {
            if self.pairing {
                group.shuffle(&mut self.rng);
                let mut rest = group.as_slice();
                while let [a, b, tail @ ..] = rest {
                    units.push(Unit::Pair(*a, *b));
                    rest = tail;
                }
                units.extend(rest.iter().map(|&i| Unit::Single(i)));
            } else {
                units.extend(group.iter().map(|&i| Unit::Single(i)));
            }
        }

        // Longest first, pairs before singles: both are more constrained, and a search that fails
        // should fail as high up the tree as possible.
        units.sort_by_key(|unit| {
            let length = match *unit {
                Unit::Pair(a, _) | Unit::Single(a) => self.answers[a].chars().count(),
            };
            (std::cmp::Reverse(length), !unit.is_pair())
        });

        units
    }

    fn descend(
        &mut self,
        units: &[Unit],
        depth: usize,
        on_placement: &mut dyn FnMut(Placement) -> Flow,
    ) -> Flow {
        if self.node_budget == 0 || self.out_of_time || self.found_this_restart > 0 {
            return Flow::Continue;
        }
        self.node_budget -= 1;
        self.stats.nodes += 1;

        if self.stats.nodes.is_multiple_of(DEADLINE_CHECK_INTERVAL) {
            if let Some(deadline) = self.theme.deadline {
                if Instant::now() >= deadline {
                    self.out_of_time = true;
                    return Flow::Continue;
                }
            }
        }

        let Some(&unit) = units.get(depth) else {
            return self.emit(on_placement);
        };

        for mv in self.moves_for(unit) {
            // A move is one or two entries and either both go down or neither does, so the undo
            // has to span the whole move rather than each entry separately.
            let Some(written) = self.try_move(&mv) else {
                continue;
            };

            let flow = self.descend(units, depth + 1, on_placement);
            self.undo_move(&written, mv.len());

            if flow == Flow::Stop {
                return Flow::Stop;
            }
            if self.node_budget == 0 || self.out_of_time || self.found_this_restart > 0 {
                break;
            }
        }

        Flow::Continue
    }

    fn emit(&mut self, on_placement: &mut dyn FnMut(Placement) -> Flow) -> Flow {
        if !self.seen.insert(self.letters.clone()) {
            self.stats.duplicates += 1;
            return Flow::Continue;
        }

        let mut template = self.base.clone();
        for (idx, letter) in self.letters.iter().enumerate() {
            if let Some(ch) = *letter {
                template.letters[idx] = Some(ch);
                template.fixed[idx] = Some(Cell::Open);
            }
        }

        // The block layer has the final say. Everything up to here is cheap local checking; this is
        // full propagation, and it is what catches the placements that are individually fine but
        // jointly leave a square that can never reach the minimum entry length.
        let Ok((problem, root)) = build_problem(&template, self.settings.clone()) else {
            self.stats.rejected_by_geometry += 1;
            return Flow::Continue;
        };

        self.found_this_restart += 1;
        self.stats.placements += 1;

        on_placement(Placement::new(
            problem,
            root,
            self.pre_fixed_blocks,
            &ThemeWeights::default(),
        ))
    }

    /// Every position this unit could take, in the order to try them.
    fn moves_for(&mut self, unit: Unit) -> Vec<Vec<ThemeEntry>> {
        let directions: &[Direction] = if self.theme.allow_down {
            &[Direction::Across, Direction::Down]
        } else {
            &[Direction::Across]
        };

        match unit {
            Unit::Pair(a, b) => {
                let mut moves = vec![];
                for &direction in directions {
                    for entry in self.positions(a, direction) {
                        let Some(mirror) = self.mirror_of(&entry, b) else {
                            continue;
                        };
                        // A position whose mirror overlaps it isn't two entries, it's one confused
                        // one. The self-mirroring case belongs to `Single`.
                        let cells: HashSet<usize> = entry.cells(&self.geometry).into_iter().collect();
                        if mirror
                            .cells(&self.geometry)
                            .iter()
                            .any(|idx| cells.contains(idx))
                        {
                            continue;
                        }
                        moves.push(vec![entry, mirror]);
                    }
                }
                moves.shuffle(&mut self.rng);
                moves
            }
            Unit::Single(a) => {
                // Self-mirroring positions first. When pairing is on they are the only ones allowed;
                // when it is off they are still the better answer, so they are still worth first
                // refusal.
                let (mut central, mut loose): (Vec<_>, Vec<_>) = directions
                    .iter()
                    .flat_map(|&direction| self.positions(a, direction))
                    .partition(|entry| self.is_self_mirrored(entry));

                central.shuffle(&mut self.rng);
                if self.pairing {
                    return central.into_iter().map(|entry| vec![entry]).collect();
                }

                loose.shuffle(&mut self.rng);
                central
                    .into_iter()
                    .chain(loose)
                    .map(|entry| vec![entry])
                    .collect()
            }
        }
    }

    /// Every in-bounds position for one answer in one direction.
    fn positions(&self, answer: usize, direction: Direction) -> Vec<ThemeEntry> {
        let text = &self.answers[answer];
        let length = text.chars().count();

        // The answer runs along one axis, so that axis loses `length - 1` starting positions; the
        // other one is unconstrained. `checked_sub` returning `None` is the answer not fitting this
        // way round at all.
        let (last_x, last_y) = match direction {
            Direction::Across => (
                self.geometry.width.checked_sub(length),
                Some(self.geometry.height - 1),
            ),
            Direction::Down => (
                Some(self.geometry.width - 1),
                self.geometry.height.checked_sub(length),
            ),
        };
        let (Some(last_x), Some(last_y)) = (last_x, last_y) else {
            return vec![];
        };

        let mut result = vec![];
        for y in 0..=last_y {
            for x in 0..=last_x {
                result.push(ThemeEntry {
                    start: (x, y),
                    direction,
                    length,
                    answer: text.clone(),
                });
            }
        }
        result
    }

    /// Where a second answer has to go for it to be `entry`'s rotational mirror.
    ///
    /// The mirror of an entry's *last* square is the mirrored entry's *first*, which is true in both
    /// directions and saves writing the coordinate algebra out twice.
    fn mirror_of(&self, entry: &ThemeEntry, answer: usize) -> Option<ThemeEntry> {
        let cells = entry.cells(&self.geometry);
        let start = self.geometry.coord(self.geometry.partner(*cells.last()?));
        Some(ThemeEntry {
            start,
            direction: entry.direction,
            length: entry.length,
            answer: self.answers[answer].clone(),
        })
    }

    fn is_self_mirrored(&self, entry: &ThemeEntry) -> bool {
        let cells = entry.cells(&self.geometry);
        self.geometry.partner(cells[cells.len() - 1]) == cells[0]
    }

    /// Write a whole move, or back out entirely. Returns the squares it newly filled.
    ///
    /// All-or-nothing because a `Pair`'s two entries only make sense together: leaving the first one
    /// down after the second is refused would put the search in a state with an unmirrored answer,
    /// which is exactly what pairing exists to prevent.
    fn try_move(&mut self, entries: &[ThemeEntry]) -> Option<Vec<usize>> {
        let mut written = vec![];

        for (placed_so_far, entry) in entries.iter().enumerate() {
            match self.try_place(entry) {
                Some(cells) => written.extend(cells),
                None => {
                    self.undo_move(&written, placed_so_far);
                    return None;
                }
            }
        }

        Some(written)
    }

    /// Take back what `try_move` wrote. `entry_count` is how many entries actually made it onto
    /// `placed`, which is the whole move on the way back up the search and a prefix of it when a
    /// move was refused halfway through.
    fn undo_move(&mut self, written: &[usize], entry_count: usize) {
        for &idx in written {
            self.letters[idx] = None;
        }
        self.placed.truncate(self.placed.len() - entry_count);
    }

    /// Can this answer go here? Every check is local to the entry and its immediate surroundings;
    /// anything that needs to reason about the whole grid is left to propagation in `emit`.
    fn try_place(&mut self, entry: &ThemeEntry) -> Option<Vec<usize>> {
        let problem = &self.geometry;
        let cells = entry.cells(problem);

        // Two entries running the same way may cross nothing at all -- they'd merge into one run.
        for other in &self.placed {
            if other.direction != entry.direction {
                continue;
            }
            if other.cells(problem).iter().any(|idx| cells.contains(idx)) {
                return None;
            }
            if !self.theme.allow_stacked && parallel_gap(entry, other) == Some(1) {
                return None;
            }
        }

        let mut written = vec![];
        for (&idx, ch) in cells.iter().zip(entry.answer.chars()) {
            // A theme square is open, so symmetry pins its partner open too. A `#` on either side
            // of that pairing is a straight contradiction.
            if self.base.fixed[idx] == Some(Cell::Block)
                || self.base.fixed[problem.partner(idx)] == Some(Cell::Block)
            {
                return None;
            }
            match self.letters[idx] {
                // Crossing an answer already down is fine, as long as they agree on the letter.
                Some(existing) if existing != ch => return None,
                Some(_) => {}
                None => written.push((idx, ch)),
            }
        }

        for idx in theme_boundary_indices(problem, entry) {
            // The boundary has to become a block, so anything that makes it open kills the
            // position: a letter of its own, a letter at its rotational partner (symmetry would
            // force it open), or an explicit `.` the constructor wrote there.
            if self.letters[idx].is_some()
                || self.letters[problem.partner(idx)].is_some()
                || self.base.explicitly_open[idx]
                || self.base.fixed[idx] == Some(Cell::Open)
                || self.base.fixed[problem.partner(idx)] == Some(Cell::Open)
            {
                return None;
            }
        }

        // Nothing above this point mutates, so a refusal needs no undo of its own.
        for &(idx, ch) in &written {
            self.letters[idx] = Some(ch);
        }
        self.placed.push(entry.clone());

        Some(written.into_iter().map(|(idx, _)| idx).collect())
    }
}

/// Whether `theme`'s rotational mirror is also a theme entry (possibly itself).
fn has_theme_mirror(problem: &Problem, theme: &ThemeEntry) -> bool {
    let cells = theme.cells(problem);
    let mirror_start = problem.coord(problem.partner(cells[cells.len() - 1]));

    problem.theme_entries.iter().any(|other| {
        other.start == mirror_start
            && other.direction == theme.direction
            && other.length == theme.length
    })
}

/// How many lines apart two parallel theme entries are, when their spans overlap. `None` when they
/// run in different directions or don't shadow each other at all.
fn parallel_gap(a: &ThemeEntry, b: &ThemeEntry) -> Option<usize> {
    if a.direction != b.direction {
        return None;
    }
    let (line_a, span_a) = a.line_and_span();
    let (line_b, span_b) = b.line_and_span();

    if span_a.start >= span_b.end || span_b.start >= span_a.end {
        return None;
    }
    Some(line_a.abs_diff(line_b))
}
