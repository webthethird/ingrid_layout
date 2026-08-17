//! Ranking candidate grids.
//!
//! Legality is a solved problem by the time a grid gets here -- the geometry search only ever emits
//! legal topologies, and most conventional 15x15s fill fine with a decent word list. What separates
//! a usable grid from an ugly one is everything in this module, so expect to tune the weights.
//!
//! Scores are "higher is better" and centred loosely on zero. They are only meaningful relative to
//! each other within a single run.

use crate::layout::{Cell, Direction, Layout, Problem};
use crate::oracle::FillReport;

/// Structural facts about a finished grid, independent of any fill.
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    pub block_count: usize,
    pub word_count: usize,
    /// Entries of the minimum length. A few are normal; a stack of them is the classic sign of a
    /// grid that was forced rather than designed.
    pub short_entries: usize,
    /// Entries of six or more squares, which is where the interesting fill lives.
    pub long_entries: usize,
    pub max_entry_length: usize,
    /// Blocks that could be removed without changing the word count. Constructors add these to make
    /// a corner fillable; a grid full of them looks padded.
    pub cheater_squares: usize,
    /// Squares in the largest orthogonally-connected clump of blocks.
    pub largest_block_clump: usize,
    /// Pairs of blocks sharing an edge, *excluding* those inside a finger.
    ///
    /// This measures how efficiently the block budget is being spent. A block earns its keep by
    /// cutting both a row and a column; two blocks side by side cut the same row twice and waste
    /// half of one. Counting shared edges scores the arrangements constructors rate, with no
    /// special cases: a diagonal line of blocks scores 0, scattered single blocks score 0, a
    /// straight line of n scores n-1, and a 2x2 square scores 4 for its four blocks. That is why
    /// straight lines and solid rectangles leave a grid heavily blacked-in and still hard to fill.
    ///
    /// Fingers are the exception, and they have to be, or this metric would fight the grid's own
    /// skeleton. A finger is a straight run of blocks growing inward from a wall; they are
    /// deliberate, they cannot be drawn diagonally, and a grid needs several. So a block run that
    /// reaches a wall is doing its job and isn't charged for touching.
    pub block_adjacencies: usize,
    /// Runs of blocks reaching in from the left or right wall -- what constructors call "fingers".
    ///
    /// Reported rather than scored. It is a genuine diagnostic (the rule of thumb is about three
    /// per side for a Sunday, and a grid with two per side is markedly harder to fill), but the
    /// right target clearly varies with grid size and this crate has one real puzzle to calibrate
    /// against, which is not enough to fix a weight with.
    pub side_fingers: usize,
    /// Non-theme entries that cross two or more theme entries. These are the squares where the fill
    /// is most likely to get ugly, because both of their crossing letters are already pinned.
    pub theme_crossing_hotspots: usize,
}

/// Weights for combining [`Metrics`] into a single number. The defaults are a starting point chosen
/// by inspection, not a tuned model -- the CLI prints the breakdown so you can see what drove a
/// ranking and adjust.
#[derive(Debug, Clone)]
pub struct Weights {
    pub short_entry: f64,
    pub long_entry: f64,
    pub cheater_square: f64,
    /// Applied to each block beyond `clump_tolerance` in the largest clump.
    pub block_clump: f64,
    pub clump_tolerance: usize,
    /// Applied to each pair of blocks sharing an edge. Small, because some are unavoidable -- the
    /// straight runs poking in from the side walls that constructors call "fingers" are wanted, and
    /// a grid needs a few. It is the difference between grids that this has to rank, not the
    /// absolute count.
    pub block_adjacency: f64,
    pub theme_crossing_hotspot: f64,
    /// Applied to the absolute difference between the word count and `target_word_count`.
    pub word_count_deviation: f64,
    pub target_word_count: usize,
    /// Applied to `mean_word_score - 50`, i.e. how far above average the fill's words are.
    pub mean_word_score: f64,
    /// Applied to `min_word_score - 50`. A single weak entry is what people notice.
    pub min_word_score: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Weights {
            short_entry: -1.5,
            long_entry: 1.0,
            cheater_square: -3.0,
            block_clump: -1.5,
            clump_tolerance: 4,
            block_adjacency: -0.35,
            theme_crossing_hotspot: -1.0,
            word_count_deviation: -0.5,
            target_word_count: 76,
            mean_word_score: 0.4,
            min_word_score: 0.2,
        }
    }
}

/// A candidate grid with everything we know about it.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub template: String,
    pub metrics: Metrics,
    pub geometric_score: f64,
    pub fill: Option<FillReport>,
    pub total_score: f64,
}

impl Metrics {
    #[must_use]
    pub fn measure(problem: &Problem, layout: &Layout) -> Metrics {
        let entries = layout.entries(problem);
        let min_len = problem.settings.min_entry_length;

        let theme_cells: Vec<bool> = {
            let mut flags = vec![false; layout.cells.len()];
            for theme in &problem.theme_entries {
                for i in 0..theme.length {
                    let idx = match theme.direction {
                        Direction::Across => problem.index(theme.start.0 + i, theme.start.1),
                        Direction::Down => problem.index(theme.start.0, theme.start.1 + i),
                    };
                    flags[idx] = true;
                }
            }
            flags
        };

        let is_theme_entry = |start: (usize, usize), dir: Direction, len: usize| {
            problem.theme_entries.iter().any(|theme| {
                theme.start == start && theme.direction == dir && theme.length == len
            })
        };

        let mut theme_crossing_hotspots = 0;
        for &(start, dir, len) in &entries {
            if is_theme_entry(start, dir, len) {
                continue;
            }
            let crossings = (0..len)
                .filter(|&i| {
                    let idx = match dir {
                        Direction::Across => problem.index(start.0 + i, start.1),
                        Direction::Down => problem.index(start.0, start.1 + i),
                    };
                    theme_cells[idx]
                })
                .count();
            if crossings >= 2 {
                theme_crossing_hotspots += 1;
            }
        }

        Metrics {
            block_count: layout.block_count,
            word_count: entries.len(),
            short_entries: entries.iter().filter(|&&(_, _, len)| len == min_len).count(),
            long_entries: entries.iter().filter(|&&(_, _, len)| len >= 6).count(),
            max_entry_length: entries.iter().map(|&(_, _, len)| len).max().unwrap_or(0),
            cheater_squares: count_cheater_squares(problem, layout, entries.len()),
            largest_block_clump: largest_block_clump(problem, layout),
            block_adjacencies: block_adjacencies(problem, layout),
            side_fingers: side_fingers(problem, layout),
            theme_crossing_hotspots,
        }
    }

    /// The part of the score we can compute without running the solver, used to decide which grids
    /// are worth the cost of a fill attempt.
    #[must_use]
    pub fn geometric_score(&self, weights: &Weights) -> f64 {
        let clump_excess = self
            .largest_block_clump
            .saturating_sub(weights.clump_tolerance);
        let word_deviation = self.word_count.abs_diff(weights.target_word_count);

        weights.short_entry * self.short_entries as f64
            + weights.long_entry * self.long_entries as f64
            + weights.cheater_square * self.cheater_squares as f64
            + weights.block_clump * clump_excess as f64
            + weights.block_adjacency * self.block_adjacencies as f64
            + weights.theme_crossing_hotspot * self.theme_crossing_hotspots as f64
            + weights.word_count_deviation * word_deviation as f64
    }
}

/// The score contributed by an actual fill: how good the words are, not how fast it was found.
/// Fill *time* deliberately doesn't count -- a grid that took a while but ended up clean is better
/// than one that filled instantly with junk.
#[must_use]
pub fn fill_score(report: &FillReport, weights: &Weights) -> f64 {
    weights.mean_word_score * (report.mean_word_score - 50.0)
        + weights.min_word_score * (f64::from(report.min_word_score) - 50.0)
}

/// Count blocks whose removal wouldn't change the word count.
fn count_cheater_squares(problem: &Problem, layout: &Layout, baseline_words: usize) -> usize {
    let mut cells = layout.cells.clone();
    let mut count = 0;

    for idx in 0..cells.len() {
        if cells[idx] != Cell::Block {
            continue;
        }
        cells[idx] = Cell::Open;
        if count_entries(problem, &cells) == baseline_words {
            count += 1;
        }
        cells[idx] = Cell::Block;
    }

    count
}

/// Number of entries (runs of two or more open squares) in a grid.
fn count_entries(problem: &Problem, cells: &[Cell]) -> usize {
    let mut count = 0;

    for y in 0..problem.height {
        count += runs_in(problem.width, |x| cells[problem.index(x, y)]);
    }
    for x in 0..problem.width {
        count += runs_in(problem.height, |y| cells[problem.index(x, y)]);
    }

    count
}

fn runs_in(len: usize, cell_at: impl Fn(usize) -> Cell) -> usize {
    let mut count = 0;
    let mut run = 0;
    for i in 0..len {
        if cell_at(i) == Cell::Open {
            run += 1;
        } else {
            if run > 1 {
                count += 1;
            }
            run = 0;
        }
    }
    if run > 1 {
        count += 1;
    }
    count
}

/// Pairs of blocks sharing an edge, not counting runs that reach a wall.
///
/// Walking maximal runs rather than testing pairs individually is what makes the finger exemption
/// fall out for free: a run of n blocks has n-1 shared edges along its own axis, and whether it is
/// a finger is just a question about where the run starts and ends.
fn block_adjacencies(problem: &Problem, layout: &Layout) -> usize {
    let mut count = 0;

    for y in 0..problem.height {
        count += interior_run_edges(problem.width, |x| layout.cells[problem.index(x, y)]);
    }
    for x in 0..problem.width {
        count += interior_run_edges(problem.height, |y| layout.cells[problem.index(x, y)]);
    }

    count
}

/// Shared edges inside maximal block runs along one line, skipping any run that touches either end.
fn interior_run_edges(len: usize, cell_at: impl Fn(usize) -> Cell) -> usize {
    let mut count = 0;
    let mut start = 0;

    while start < len {
        if cell_at(start) != Cell::Block {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < len && cell_at(end) == Cell::Block {
            end += 1;
        }
        if start > 0 && end < len {
            count += end - start - 1;
        }
        start = end;
    }

    count
}

/// Runs of two or more blocks reaching in from the left or right wall.
fn side_fingers(problem: &Problem, layout: &Layout) -> usize {
    let mut count = 0;

    for y in 0..problem.height {
        let block_at = |x: usize| layout.cells[problem.index(x, y)] == Cell::Block;

        for (wall, step) in [(0usize, 1isize), (problem.width - 1, -1)] {
            if !block_at(wall) {
                continue;
            }
            let mut length = 1;
            let mut x = wall as isize + step;
            while x >= 0 && (x as usize) < problem.width && block_at(x as usize) {
                length += 1;
                x += step;
            }
            if length >= 2 {
                count += 1;
            }
        }
    }

    count
}

/// Size of the largest orthogonally-connected group of blocks.
fn largest_block_clump(problem: &Problem, layout: &Layout) -> usize {
    let mut seen = vec![false; layout.cells.len()];
    let mut largest = 0;

    for start in 0..layout.cells.len() {
        if seen[start] || layout.cells[start] != Cell::Block {
            continue;
        }

        let mut size = 0;
        let mut stack = vec![start];
        seen[start] = true;

        while let Some(idx) = stack.pop() {
            size += 1;
            let (x, y) = problem.coord(idx);
            let mut neighbours = vec![];
            if x > 0 {
                neighbours.push(problem.index(x - 1, y));
            }
            if x + 1 < problem.width {
                neighbours.push(problem.index(x + 1, y));
            }
            if y > 0 {
                neighbours.push(problem.index(x, y - 1));
            }
            if y + 1 < problem.height {
                neighbours.push(problem.index(x, y + 1));
            }
            for n in neighbours {
                if !seen[n] && layout.cells[n] == Cell::Block {
                    seen[n] = true;
                    stack.push(n);
                }
            }
        }

        largest = largest.max(size);
    }

    largest
}
