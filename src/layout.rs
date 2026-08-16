//! The geometry layer: everything about *where the black squares go*, with no knowledge of any
//! word list.
//!
//! The search state is a grid of three-valued cells (`Unknown`/`Open`/`Block`). Decisions are made
//! per *symmetry orbit* rather than per cell, so 180-degree symmetry holds by construction instead
//! of being tested afterwards.
//!
//! Every propagation rule here relies on the same monotonicity argument: a cell only ever moves
//! from `Unknown` to a decided state, so blocks are never removed and runs of non-block cells only
//! ever shrink. That is what makes "this cell can never be part of a long enough entry" a sound
//! conclusion rather than a guess.

use rand::prelude::*;
use rand::rngs::SmallRng;
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// The state of a single square during the layout search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cell {
    /// Not yet decided; the search may still make this open or a block.
    Unknown,
    /// A white square.
    Open,
    /// A black square.
    Block,
}

/// The direction of an entry, mirroring `ingrid_core`'s but kept local so the geometry layer
/// doesn't depend on the solver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Across,
    Down,
}

/// The symmetry the finished grid must obey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Symmetry {
    /// Standard American 180-degree rotational symmetry.
    Rotational180,
    /// No symmetry constraint; every cell is its own orbit.
    None,
}

/// The tunable constraints defining what counts as a legal grid.
#[derive(Debug, Clone)]
pub struct LayoutSettings {
    pub min_entry_length: usize,
    pub min_blocks: usize,
    pub max_blocks: usize,
    pub min_words: usize,
    pub max_words: usize,
    /// How many minimum-length entries the grid may contain. Three-letter entries are where the
    /// crosswordese lives, so this is a quality bound worth enforcing rather than merely scoring:
    /// grids are cheap to sample and expensive to fill, and it is better to never offer the solver
    /// a grid built out of threes.
    pub max_short_entries: usize,
    pub symmetry: Symmetry,
}

impl Default for LayoutSettings {
    fn default() -> Self {
        LayoutSettings {
            min_entry_length: 3,
            min_blocks: 30,
            max_blocks: 42,
            min_words: 0,
            max_words: 78,
            max_short_entries: 10,
            symmetry: Symmetry::Rotational180,
        }
    }
}

/// A theme entry inferred from a run of letters in the input template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeEntry {
    pub start: (usize, usize),
    pub direction: Direction,
    pub length: usize,
    pub answer: String,
}

/// Everything about the problem that is fixed for the whole search: dimensions, the theme letters,
/// the settings, and precomputed geometry.
#[derive(Debug, Clone)]
pub struct Problem {
    pub width: usize,
    pub height: usize,
    /// Theme letters by flat index, `None` for every non-theme square.
    pub letters: Vec<Option<char>>,
    pub settings: LayoutSettings,
    pub theme_entries: Vec<ThemeEntry>,
    /// Flat indices of every row followed by every column, precomputed once.
    lines: Vec<Vec<usize>>,
}

impl Problem {
    #[must_use]
    pub fn cell_count(&self) -> usize {
        self.width * self.height
    }

    /// The 180-degree rotational partner of a flat index. Under this symmetry the partner of `i` is
    /// simply `n - 1 - i`, and an odd-area grid's centre square is its own partner.
    #[must_use]
    pub fn partner(&self, idx: usize) -> usize {
        match self.settings.symmetry {
            Symmetry::Rotational180 => self.cell_count() - 1 - idx,
            Symmetry::None => idx,
        }
    }

    #[must_use]
    pub fn coord(&self, idx: usize) -> (usize, usize) {
        (idx % self.width, idx / self.width)
    }

    #[must_use]
    pub fn index(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }
}

/// A contradiction reached during propagation: this branch of the search is dead.
///
/// Carries which rule objected. The search ignores it, but when propagating the *fixed* squares
/// contradicts, that string is the whole explanation the constructor gets for why their theme
/// placement was rejected -- so it is worth carrying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contradiction(pub &'static str);

/// An optional word-list check the geometry search can consult, kept as a trait so this module
/// stays independent of any particular word list.
///
/// Without it the search happily builds grids whose long down entries thread through two or three
/// theme answers, producing patterns like `???s???n???d???` that nothing in the dictionary matches.
/// Those only get caught at the very end, after a whole grid has been built and handed to the
/// solver. Asking the question as soon as an entry's contents are pinned down kills that branch
/// while it is still a branch.
pub trait EntryViability {
    /// Could any word fill an entry with this pattern? `None` marks an empty square.
    ///
    /// Answering `true` is always safe -- it just means the check declines to prune.
    fn is_viable(&mut self, pattern: &[Option<char>]) -> bool;
}

type PropResult<T> = Result<T, Contradiction>;

/// The mutable search state. Deliberately small (one byte per cell plus two counters) so that
/// cloning it per search node is cheaper than maintaining an undo trail.
#[derive(Debug, Clone)]
pub struct Layout {
    pub cells: Vec<Cell>,
    pub block_count: usize,
    pub unknown_count: usize,
    /// Per square, which of its two entries have already been checked against the word list:
    /// bit 0 for the across entry, bit 1 for the down entry. A determined entry can never change,
    /// so re-asking about it on the way down the tree is pure waste -- and without this the check
    /// dominates the whole run.
    ///
    /// Two bits rather than one because a square belongs to an across entry *and* a down entry, and
    /// checking one says nothing about the other.
    ///
    /// Not part of the grid's identity, so it is excluded from `PartialEq`.
    viability_checked: Vec<u8>,
}

impl PartialEq for Layout {
    fn eq(&self, other: &Self) -> bool {
        self.cells == other.cells
    }
}

impl Eq for Layout {}

impl Layout {
    #[must_use]
    pub fn new(problem: &Problem) -> Layout {
        Layout {
            cells: vec![Cell::Unknown; problem.cell_count()],
            block_count: 0,
            unknown_count: problem.cell_count(),
            viability_checked: vec![0; problem.cell_count()],
        }
    }

    /// Assign a cell (and, under rotational symmetry, its partner). Assigning a value a cell
    /// already holds is a no-op; assigning a conflicting value is a contradiction.
    ///
    /// Returns whether anything actually changed.
    pub fn set(&mut self, problem: &Problem, idx: usize, value: Cell) -> PropResult<bool> {
        debug_assert!(value != Cell::Unknown);

        let mut changed = false;
        for target in [idx, problem.partner(idx)] {
            match self.cells[target] {
                Cell::Unknown => {
                    self.cells[target] = value;
                    self.unknown_count -= 1;
                    if value == Cell::Block {
                        self.block_count += 1;
                    }
                    changed = true;
                }
                existing if existing == value => {}
                _ => return Err(Contradiction("a square and its 180-degree partner need opposite values")),
            }
        }
        Ok(changed)
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unknown_count == 0
    }

    /// Run every propagation rule to a fixpoint.
    pub fn propagate(
        &mut self,
        problem: &Problem,
        viability: Option<&mut (dyn EntryViability + '_)>,
    ) -> PropResult<()> {
        loop {
            self.check_block_bounds(problem)?;

            let mut changed = self.rule_min_entry_length(problem)?;
            changed |= self.rule_forced_open(problem)?;

            if !changed {
                break;
            }
        }

        self.check_block_bounds(problem)?;
        self.check_optimistic_connectivity(problem)?;

        // Last, because these rules only reject -- they can't force anything -- and one of them
        // costs a dictionary lookup.
        self.check_determined_entries(problem, viability)?;

        Ok(())
    }

    /// Everything we can conclude from entries whose extent is already pinned down.
    ///
    /// An entry is "determined" once it is a run of open squares with a block or an edge at each
    /// end: no future decision can change its length or which squares it covers. Determined entries
    /// only ever accumulate, so counts taken over them are lower bounds on the finished grid, and
    /// exceeding a maximum is final. That turns the word-count and short-entry bounds from things we
    /// discover at the leaf into things that prune the branch.
    ///
    /// Also checks every determined entry against the word list. Entries with theme letters are the
    /// interesting case, but empty ones matter too: Spread the Wordlist tops out at 15 letters, so
    /// in a 21x21 any 16-square entry is unfillable no matter what crosses it. Their answers depend
    /// only on length, so they cost one cache entry per length for the whole run.
    fn check_determined_entries(
        &mut self,
        problem: &Problem,
        mut viability: Option<&mut (dyn EntryViability + '_)>,
    ) -> PropResult<()> {
        let mut determined_entries = 0;
        let mut determined_short_entries = 0;

        for line_idx in 0..problem.lines.len() {
            let line = &problem.lines[line_idx];
            let checked_bit: u8 = if line_idx < problem.height { 1 } else { 2 };
            let mut start = 0;
            while start < line.len() {
                if self.cells[line[start]] != Cell::Open {
                    start += 1;
                    continue;
                }
                let mut end = start;
                while end < line.len() && self.cells[line[end]] == Cell::Open {
                    end += 1;
                }

                let run = &line[start..end];
                // Both ends have to be pinned. A run that merely stops at an undecided square can
                // still grow, and a longer pattern may well be viable when the short one isn't.
                let determined = (start == 0 || self.cells[line[start - 1]] == Cell::Block)
                    && (end == line.len() || self.cells[line[end]] == Cell::Block);

                if determined && run.len() > 1 {
                    determined_entries += 1;
                    if run.len() == problem.settings.min_entry_length {
                        determined_short_entries += 1;
                    }
                    if determined_entries > problem.settings.max_words {
                        return Err(Contradiction("too many entries"));
                    }
                    if determined_short_entries > problem.settings.max_short_entries {
                        return Err(Contradiction("too many minimum-length entries"));
                    }

                    let already_checked = self.viability_checked[run[0]] & checked_bit != 0;

                    if let Some(viability) = viability.as_deref_mut() {
                        if !already_checked {
                            let pattern: Vec<Option<char>> =
                                run.iter().map(|&idx| problem.letters[idx]).collect();
                            if !viability.is_viable(&pattern) {
                                return Err(Contradiction(
                                    "an entry matches no word in the list",
                                ));
                            }
                            for &idx in run {
                                self.viability_checked[idx] |= checked_bit;
                            }
                        }
                    }
                }

                start = end;
            }
        }

        Ok(())
    }

    /// Every non-block square must be able to end up in an across entry *and* a down entry of at
    /// least `min_entry_length`. Since runs only shrink, a square whose maximal reachable run is
    /// already too short can never be open, so we can force it black outright.
    ///
    /// This is the workhorse rule: it turns `#?##...` into `####...` rather than branching on it.
    fn rule_min_entry_length(&mut self, problem: &Problem) -> PropResult<bool> {
        let min_len = problem.settings.min_entry_length;
        let mut forced: Vec<usize> = vec![];

        for line in &problem.lines {
            let mut run_start = 0;
            while run_start < line.len() {
                if self.cells[line[run_start]] == Cell::Block {
                    run_start += 1;
                    continue;
                }
                let mut run_end = run_start;
                while run_end < line.len() && self.cells[line[run_end]] != Cell::Block {
                    run_end += 1;
                }

                if run_end - run_start < min_len {
                    for &idx in &line[run_start..run_end] {
                        if self.cells[idx] == Cell::Open {
                            return Err(Contradiction(
                                "a white square is walled in too tightly to reach the minimum entry length",
                            ));
                        }
                        forced.push(idx);
                    }
                }

                run_start = run_end;
            }
        }

        let mut changed = false;
        for idx in forced {
            changed |= self.set(problem, idx, Cell::Block)?;
        }
        Ok(changed)
    }

    /// An open square's entry has to reach `min_entry_length`, and it can only grow into the
    /// unknown cells on either side of it within its non-block run. When one side doesn't have
    /// enough room, the shortfall is forced open on the other side.
    fn rule_forced_open(&mut self, problem: &Problem) -> PropResult<bool> {
        let min_len = problem.settings.min_entry_length;
        let mut forced: Vec<usize> = vec![];

        for line in &problem.lines {
            let mut run_start = 0;
            while run_start < line.len() {
                if self.cells[line[run_start]] == Cell::Block {
                    run_start += 1;
                    continue;
                }
                let mut run_end = run_start;
                while run_end < line.len() && self.cells[line[run_end]] != Cell::Block {
                    run_end += 1;
                }

                // Walk the maximal runs of `Open` cells inside this non-block run.
                let mut open_start = run_start;
                while open_start < run_end {
                    if self.cells[line[open_start]] != Cell::Open {
                        open_start += 1;
                        continue;
                    }
                    let mut open_end = open_start;
                    while open_end < run_end && self.cells[line[open_end]] == Cell::Open {
                        open_end += 1;
                    }

                    let open_len = open_end - open_start;
                    if open_len < min_len {
                        let needed = min_len - open_len;
                        let room_left = open_start - run_start;
                        let room_right = run_end - open_end;

                        if room_left + room_right < needed {
                            // `rule_min_entry_length` should have caught this already, but the
                            // rules run independently so we check rather than assume.
                            return Err(Contradiction("an entry can't reach the minimum length"));
                        }

                        let must_left = needed.saturating_sub(room_right);
                        let must_right = needed.saturating_sub(room_left);

                        forced.extend(&line[open_start - must_left..open_start]);
                        forced.extend(&line[open_end..open_end + must_right]);
                    }

                    open_start = open_end;
                }

                run_start = run_end;
            }
        }

        let mut changed = false;
        for idx in forced {
            changed |= self.set(problem, idx, Cell::Open)?;
        }
        Ok(changed)
    }

    /// Reject as soon as the block count can no longer land in range. Every remaining unknown could
    /// still become a block, so the reachable maximum is `block_count + unknown_count`.
    fn check_block_bounds(&self, problem: &Problem) -> PropResult<()> {
        if self.block_count > problem.settings.max_blocks
            || self.block_count + self.unknown_count < problem.settings.min_blocks
        {
            return Err(Contradiction("the block count can't land between --min-blocks and --max-blocks"));
        }
        Ok(())
    }

    /// If two open squares can't reach each other *even by travelling through unknown squares*,
    /// no future decision can reconnect them.
    fn check_optimistic_connectivity(&self, problem: &Problem) -> PropResult<()> {
        if self.flood_reaches_all_open(problem, |cell| cell != Cell::Block) {
            Ok(())
        } else {
            Err(Contradiction("the white squares are split into regions that can't reconnect"))
        }
    }

    /// Flood fill from the first open square across every cell satisfying `passable`, and report
    /// whether that reached every open square.
    fn flood_reaches_all_open(&self, problem: &Problem, passable: impl Fn(Cell) -> bool) -> bool {
        let Some(start) = self.cells.iter().position(|&c| c == Cell::Open) else {
            return true;
        };

        let mut seen = vec![false; self.cells.len()];
        let mut stack = vec![start];
        seen[start] = true;
        let mut reached_open = 0;

        while let Some(idx) = stack.pop() {
            if self.cells[idx] == Cell::Open {
                reached_open += 1;
            }
            let (x, y) = problem.coord(idx);
            let push = |nx: usize, ny: usize, stack: &mut Vec<usize>, seen: &mut Vec<bool>| {
                let n = problem.index(nx, ny);
                if !seen[n] && passable(self.cells[n]) {
                    seen[n] = true;
                    stack.push(n);
                }
            };
            if x > 0 {
                push(x - 1, y, &mut stack, &mut seen);
            }
            if x + 1 < problem.width {
                push(x + 1, y, &mut stack, &mut seen);
            }
            if y > 0 {
                push(x, y - 1, &mut stack, &mut seen);
            }
            if y + 1 < problem.height {
                push(x, y + 1, &mut stack, &mut seen);
            }
        }

        reached_open == self.cells.iter().filter(|&&c| c == Cell::Open).count()
    }

    /// Every entry in a finished grid, as (start coord, direction, length).
    #[must_use]
    pub fn entries(&self, problem: &Problem) -> Vec<((usize, usize), Direction, usize)> {
        let mut result = vec![];

        for (line_idx, line) in problem.lines.iter().enumerate() {
            let direction = if line_idx < problem.height {
                Direction::Across
            } else {
                Direction::Down
            };

            let mut start = 0;
            while start < line.len() {
                if self.cells[line[start]] != Cell::Open {
                    start += 1;
                    continue;
                }
                let mut end = start;
                while end < line.len() && self.cells[line[end]] == Cell::Open {
                    end += 1;
                }
                if end - start > 1 {
                    result.push((problem.coord(line[start]), direction, end - start));
                }
                start = end;
            }
        }

        result
    }

    /// Independent validation of a finished grid. The search is supposed to make this
    /// unconditionally true; the tests assert it on every emitted layout, and the search asserts it
    /// before emitting, so a bug in propagation shows up as a rejected grid rather than a bad one.
    pub fn validate(&self, problem: &Problem) -> Result<(), String> {
        if !self.is_complete() {
            return Err("grid still has undecided squares".into());
        }

        if problem.settings.symmetry == Symmetry::Rotational180 {
            for idx in 0..self.cells.len() {
                if self.cells[idx] != self.cells[problem.partner(idx)] {
                    return Err(format!("symmetry violated at {:?}", problem.coord(idx)));
                }
            }
        }

        if self.block_count < problem.settings.min_blocks
            || self.block_count > problem.settings.max_blocks
        {
            return Err(format!("block count {} out of range", self.block_count));
        }

        let entries = self.entries(problem);

        for &(start, direction, length) in &entries {
            if length < problem.settings.min_entry_length {
                return Err(format!(
                    "entry at {start:?} {direction:?} has length {length}"
                ));
            }
        }

        // Every open square must be checked, i.e. in both an across and a down entry.
        let mut in_across = vec![false; self.cells.len()];
        let mut in_down = vec![false; self.cells.len()];
        for &(start, direction, length) in &entries {
            for i in 0..length {
                let idx = match direction {
                    Direction::Across => problem.index(start.0 + i, start.1),
                    Direction::Down => problem.index(start.0, start.1 + i),
                };
                match direction {
                    Direction::Across => in_across[idx] = true,
                    Direction::Down => in_down[idx] = true,
                }
            }
        }
        for idx in 0..self.cells.len() {
            if self.cells[idx] == Cell::Open && !(in_across[idx] && in_down[idx]) {
                return Err(format!("unchecked square at {:?}", problem.coord(idx)));
            }
        }

        if entries.len() < problem.settings.min_words || entries.len() > problem.settings.max_words {
            return Err(format!("word count {} out of range", entries.len()));
        }

        let short_entries = entries
            .iter()
            .filter(|&&(_, _, length)| length == problem.settings.min_entry_length)
            .count();
        if short_entries > problem.settings.max_short_entries {
            return Err(format!("too many short entries: {short_entries}"));
        }

        if !self.flood_reaches_all_open(problem, |cell| cell == Cell::Open) {
            return Err("white squares are not all connected".into());
        }

        for theme in &problem.theme_entries {
            if !entries
                .iter()
                .any(|&(start, dir, len)| {
                    start == theme.start && dir == theme.direction && len == theme.length
                })
            {
                return Err(format!(
                    "theme entry {:?} at {:?} {:?} is not an entry in this grid",
                    theme.answer, theme.start, theme.direction
                ));
            }
        }

        Ok(())
    }

    /// Render as an `ingrid_core` template: `#` for blocks, `.` for empty white squares, and the
    /// theme letters in place.
    #[must_use]
    pub fn render(&self, problem: &Problem) -> String {
        (0..problem.height)
            .map(|y| {
                (0..problem.width)
                    .map(|x| {
                        let idx = problem.index(x, y);
                        match self.cells[idx] {
                            Cell::Block => '#',
                            Cell::Unknown => '?',
                            Cell::Open => problem.letters[idx].unwrap_or('.'),
                        }
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Parse an input template into a `Problem` and a propagated starting `Layout`.
///
/// | char | meaning |
/// | --- | --- |
/// | `?` | unknown, the search decides |
/// | `#` | forced block |
/// | `.` | forced open, empty |
/// | letter | forced open, fixed theme letter |
pub fn parse_problem(input: &str, settings: LayoutSettings) -> Result<(Problem, Layout), String> {
    let rows: Vec<Vec<char>> = input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.chars().collect())
        .collect();

    if rows.is_empty() {
        return Err("grid must have at least one row".into());
    }
    let width = rows[0].len();
    let height = rows.len();
    if rows.iter().any(|row| row.len() != width) {
        return Err("rows in grid must all be the same length".into());
    }
    if settings.min_entry_length < 2 {
        return Err("minimum entry length must be at least 2".into());
    }

    let lines: Vec<Vec<usize>> = (0..height)
        .map(|y| (0..width).map(|x| y * width + x).collect())
        .chain((0..width).map(|x| (0..height).map(|y| y * width + x).collect()))
        .collect();

    let mut letters: Vec<Option<char>> = vec![None; width * height];
    let mut initial: Vec<Option<Cell>> = vec![None; width * height];
    // Squares the constructor *wrote* as `.`. Only these count as opting out of theme-boundary
    // forcing; a square that merely ends up open for some other reason (symmetry, propagation) is a
    // conflict rather than an instruction.
    let mut explicitly_open: Vec<bool> = vec![false; width * height];

    for (y, row) in rows.iter().enumerate() {
        for (x, &ch) in row.iter().enumerate() {
            let idx = y * width + x;
            match ch {
                '?' => {}
                '#' => initial[idx] = Some(Cell::Block),
                '.' => {
                    initial[idx] = Some(Cell::Open);
                    explicitly_open[idx] = true;
                }
                ch if ch.is_alphanumeric() => {
                    initial[idx] = Some(Cell::Open);
                    letters[idx] = Some(ch.to_lowercase().next().unwrap());
                }
                other => {
                    return Err(format!(
                        "unexpected character {other:?} at ({x}, {y}); expected one of ? # . or a letter"
                    ))
                }
            }
        }
    }

    let mut problem = Problem {
        width,
        height,
        letters,
        settings,
        theme_entries: vec![],
        lines,
    };

    problem.theme_entries = infer_theme_entries(&problem)?;

    let mut layout = Layout::new(&problem);

    for (idx, initial_cell) in initial.iter().enumerate() {
        if let Some(cell) = *initial_cell {
            layout
                .set(&problem, idx, cell)
                .map_err(|_| conflict_message(&problem, idx))?;
        }
    }

    // A theme answer has to be a whole entry, not a substring of a longer one, so the squares just
    // past each end are blocks. We only force squares the constructor left as `?`: writing an
    // explicit `.` there is how you say "this run is part of a longer entry".
    let themes = std::mem::take(&mut problem.theme_entries);
    let mut enforced_themes = Vec::with_capacity(themes.len());

    for theme in themes {
        let boundaries = theme_boundary_indices(&problem, &theme);

        // Writing `.` just past the end of a run says "this run is part of a longer entry", so we
        // leave the boundary alone and stop treating the run as an entry of its own.
        if boundaries.iter().any(|&idx| explicitly_open[idx]) {
            continue;
        }

        for &idx in &boundaries {
            match layout.cells[idx] {
                Cell::Unknown => {
                    layout
                        .set(&problem, idx, Cell::Block)
                        .map_err(|_| conflict_message(&problem, idx))?;
                }
                Cell::Block => {}
                // The square has been forced open by something else -- most often symmetry, when
                // the boundary's rotational partner is a theme letter. The answer then cannot be an
                // entry in its own right no matter where the blocks go, so this is a dead end and
                // saying so beats silently filling around it.
                Cell::Open => {
                    return Err(format!(
                        "theme answer {:?} at {:?} {:?} can't be a self-contained entry: the square \
                         at {:?} just past it has to be open (its 180-degree partner {:?} is open), \
                         so the answer would run on into a longer entry. Move the answer, or write \
                         `.` there to say it really is part of a longer entry.",
                        theme.answer,
                        theme.start,
                        theme.direction,
                        problem.coord(idx),
                        problem.coord(problem.partner(idx)),
                    ));
                }
            }
        }

        enforced_themes.push(theme);
    }

    problem.theme_entries = enforced_themes;

    layout
        .propagate(&problem, None)
        .map_err(|Contradiction(reason)| {
            format!("this theme placement can't work with these settings: {reason}")
        })?;

    Ok((problem, layout))
}

fn conflict_message(problem: &Problem, idx: usize) -> String {
    format!(
        "the fixed squares are inconsistent at {:?} (its 180-degree partner {:?} demands the opposite)",
        problem.coord(idx),
        problem.coord(problem.partner(idx))
    )
}

/// The squares immediately before and after a theme entry, omitting any that fall off the grid.
fn theme_boundary_indices(problem: &Problem, theme: &ThemeEntry) -> Vec<usize> {
    let (x, y) = theme.start;
    let mut result = vec![];
    match theme.direction {
        Direction::Across => {
            if x > 0 {
                result.push(problem.index(x - 1, y));
            }
            if x + theme.length < problem.width {
                result.push(problem.index(x + theme.length, y));
            }
        }
        Direction::Down => {
            if y > 0 {
                result.push(problem.index(x, y - 1));
            }
            if y + theme.length < problem.height {
                result.push(problem.index(x, y + theme.length));
            }
        }
    }
    result
}

/// Work out which runs of letters the constructor meant as theme answers.
///
/// The obvious rule -- "every maximal run of two or more letters is an entry" -- breaks on stacked
/// theme answers, which are completely ordinary. Two full-width answers on adjacent rows leave a
/// two-letter *vertical* run in every column, and treating each of those as a down entry would wall
/// off the whole grid.
///
/// So a run is an artifact rather than an entry when every one of its squares already sits inside a
/// strictly longer run the other way: that run is fully explained by the answers crossing it. A run
/// of one letter is the degenerate case of the same idea.
fn infer_theme_entries(problem: &Problem) -> Result<Vec<ThemeEntry>, String> {
    // How long a run of letters each square sits in, across [0] and down [1]. Zero for squares with
    // no theme letter.
    let mut run_lengths = [vec![0usize; problem.cell_count()], vec![0usize; problem.cell_count()]];
    let mut runs: Vec<(usize, Vec<usize>)> = vec![];

    for (line_idx, line) in problem.lines.iter().enumerate() {
        let axis = usize::from(line_idx >= problem.height);

        let mut start = 0;
        while start < line.len() {
            if problem.letters[line[start]].is_none() {
                start += 1;
                continue;
            }
            let mut end = start;
            while end < line.len() && problem.letters[line[end]].is_some() {
                end += 1;
            }

            for &idx in &line[start..end] {
                run_lengths[axis][idx] = end - start;
            }
            runs.push((axis, line[start..end].to_vec()));

            start = end;
        }
    }

    let mut result = vec![];

    for (axis, run) in runs {
        let length = run.len();
        let crossing_axis = 1 - axis;

        // A lone letter is a pinned square, never an answer -- most often the crossing square of a
        // perpendicular answer, but it stays a pinned square even with nothing crossing it.
        if length < 2 {
            continue;
        }

        // Fully explained by longer answers crossing it, so not an answer in its own right.
        if run
            .iter()
            .all(|&idx| run_lengths[crossing_axis][idx] > length)
        {
            continue;
        }

        let answer: String = run
            .iter()
            .map(|&idx| problem.letters[idx].unwrap())
            .collect();

        if length < problem.settings.min_entry_length {
            return Err(format!(
                "theme answer {:?} at {:?} is only {} square{} long, but entries have to be at \
                 least {}. Lengthen it, or lower --min-entry-length.",
                answer,
                problem.coord(run[0]),
                length,
                if length == 1 { "" } else { "s" },
                problem.settings.min_entry_length,
            ));
        }

        result.push(ThemeEntry {
            start: problem.coord(run[0]),
            direction: if axis == 0 {
                Direction::Across
            } else {
                Direction::Down
            },
            length,
            answer,
        });
    }

    Ok(result)
}

/// What the caller wants the search to do after being handed a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

/// Knobs for the randomised search.
#[derive(Debug, Clone)]
pub struct SearchSettings {
    /// Search nodes to spend before abandoning a restart and starting over with fresh randomness.
    pub nodes_per_restart: usize,
    /// Candidates to accept from one restart before starting over.
    ///
    /// This wants to be small. Consecutive leaves of a single depth-first descent differ by a
    /// square or two, so letting one restart supply the whole pool produces hundreds of grids that
    /// are all the same grid -- and then ranking them is pointless. Restarting instead pays a full
    /// descent per grid and buys genuinely independent samples.
    pub candidates_per_restart: usize,
    pub seed: u64,
    /// When to give up. Needed inside the search itself, not just between candidates: a theme that
    /// yields no legal topology at all would otherwise restart forever without ever handing the
    /// caller a chance to stop.
    pub deadline: Option<Instant>,
    /// How often to call the progress callback. `None` never calls it.
    ///
    /// Progress has to be reported from inside the search for the same reason the deadline is
    /// checked there: the interesting case is a theme that emits nothing at all, and a callback that
    /// only fires on candidates says nothing precisely when you most want to hear something.
    pub progress_interval: Option<Duration>,
}

impl Default for SearchSettings {
    fn default() -> Self {
        SearchSettings {
            nodes_per_restart: 20_000,
            candidates_per_restart: 1,
            seed: 0,
            deadline: None,
            progress_interval: None,
        }
    }
}

/// Counters describing what the search actually did, so we can tell which regime we're in.
#[derive(Debug, Clone, Default)]
pub struct SearchStats {
    pub nodes: usize,
    pub restarts: usize,
    pub complete_grids: usize,
    pub duplicates: usize,
    pub rejected_by_validation: usize,
    /// Distinct topologies handed to `on_candidate`. The caller usually knows this already from its
    /// own pool, but the progress callback can't see that pool -- it is borrowed by the candidate
    /// callback -- so the count lives here.
    pub candidates: usize,
}

/// Sample legal topologies, calling `on_candidate` with each distinct one.
///
/// This is a sampler, not an enumerator: for a sparse theme the number of legal topologies is
/// astronomical, so we run randomised restarts and let the caller stop when it has enough.
///
/// `on_progress` is called roughly every `settings.progress_interval` while the search runs, so a
/// caller with a long deadline can tell a search that is grinding from one that is stuck.
pub fn search(
    problem: &Problem,
    root: &Layout,
    settings: &SearchSettings,
    mut viability: Option<&mut (dyn EntryViability + '_)>,
    on_candidate: &mut dyn FnMut(&Layout, &SearchStats) -> Flow,
    on_progress: Option<&mut dyn FnMut(&SearchStats)>,
) -> SearchStats {
    let mut state = SearchState {
        problem,
        rng: SmallRng::seed_from_u64(settings.seed),
        seen: HashSet::new(),
        stats: SearchStats::default(),
        node_budget: 0,
        deadline: settings.deadline,
        out_of_time: false,
        candidates_this_restart: 0,
        candidates_per_restart: settings.candidates_per_restart.max(1),
        on_candidate,
        progress_interval: settings.progress_interval,
        next_progress: settings
            .progress_interval
            .map(|interval| Instant::now() + interval),
        on_progress,
    };

    loop {
        state.node_budget = settings.nodes_per_restart;
        state.candidates_this_restart = 0;
        state.stats.restarts += 1;

        let flow = state.descend(root.clone(), viability.as_deref_mut());
        if flow == Flow::Stop || state.out_of_time {
            break;
        }

        // A restart that explored its whole subtree without exhausting its budget and without
        // hitting the per-restart candidate cap means the space really is finished, so there is
        // nothing left to restart into.
        if state.node_budget > 0 && state.candidates_this_restart < state.candidates_per_restart {
            break;
        }
    }

    state.stats
}

/// How often to consult the clock, in search nodes.
const DEADLINE_CHECK_INTERVAL: usize = 512;

/// Two lifetimes because the two callbacks come from two independent borrows at the call site.
/// `&mut` is invariant in its target, so a single `'a` shared by both would force the caller to
/// prove the two borrows live exactly as long as each other -- which they don't.
struct SearchState<'a, 'p> {
    problem: &'a Problem,
    rng: SmallRng,
    seen: HashSet<Vec<Cell>>,
    stats: SearchStats,
    node_budget: usize,
    deadline: Option<Instant>,
    out_of_time: bool,
    candidates_this_restart: usize,
    candidates_per_restart: usize,
    on_candidate: &'a mut dyn FnMut(&Layout, &SearchStats) -> Flow,
    progress_interval: Option<Duration>,
    /// When the next progress report is due, or `None` if progress isn't being reported.
    next_progress: Option<Instant>,
    on_progress: Option<&'p mut dyn FnMut(&SearchStats)>,
}

impl SearchState<'_, '_> {
    /// Should this restart wind up, whether because it ran out of budget or time or because it has
    /// already produced its share of candidates?
    fn abandoning_restart(&self) -> bool {
        self.node_budget == 0
            || self.out_of_time
            || self.candidates_this_restart >= self.candidates_per_restart
    }

    /// Call the progress callback if one is installed and its interval has elapsed.
    ///
    /// The destructuring is what makes this compile: `self.on_progress` has to be borrowed mutably
    /// to call the closure, and `self.stats` immutably to pass it, and the borrow checker only sees
    /// those as two separate borrows once the fields are named individually.
    fn report_progress(&mut self, now: Instant) {
        let Some(due) = self.next_progress else { return };
        if now < due {
            return;
        }
        // From `now` rather than `due`, so a slow callback or a long gap between clock checks can't
        // leave a backlog of reports that all fire at once.
        self.next_progress = self.progress_interval.map(|interval| now + interval);

        let SearchState {
            on_progress, stats, ..
        } = self;
        if let Some(on_progress) = on_progress.as_deref_mut() {
            on_progress(stats);
        }
    }

    /// Depth-first from an already-propagated layout.
    ///
    /// The viability checker is threaded through as a parameter rather than held in the struct so
    /// each level of the recursion can reborrow it for just as long as it needs.
    fn descend(&mut self, layout: Layout, mut viability: Option<&mut (dyn EntryViability + '_)>) -> Flow {
        if self.abandoning_restart() {
            return Flow::Continue;
        }
        self.node_budget -= 1;
        self.stats.nodes += 1;

        // One clock reading serves both the deadline and the heartbeat: `Instant::now` is cheap but
        // not free, and this runs on every node.
        if self.stats.nodes.is_multiple_of(DEADLINE_CHECK_INTERVAL) {
            let now = Instant::now();
            self.report_progress(now);
            if let Some(deadline) = self.deadline {
                if now >= deadline {
                    self.out_of_time = true;
                    return Flow::Continue;
                }
            }
        }

        let Some(orbit) = self.pick_orbit(&layout) else {
            return self.emit(&layout);
        };

        for value in self.value_order(&layout) {
            let mut child = layout.clone();
            if child.set(self.problem, orbit, value).is_err() {
                continue;
            }
            if child
                .propagate(self.problem, viability.as_deref_mut())
                .is_err()
            {
                continue;
            }
            if self.descend(child, viability.as_deref_mut()) == Flow::Stop {
                return Flow::Stop;
            }
            if self.abandoning_restart() {
                break;
            }
        }

        Flow::Continue
    }

    fn emit(&mut self, layout: &Layout) -> Flow {
        self.stats.complete_grids += 1;

        if let Err(reason) = layout.validate(self.problem) {
            // Propagation is supposed to make this unreachable except for the whole-grid counts,
            // which can't be enforced incrementally -- an entry's length isn't final until blocks
            // land on both sides of it. Anything else is a propagation bug, and we'd rather drop
            // the grid than emit an illegal one.
            debug_assert!(
                reason.starts_with("word count") || reason.starts_with("too many short entries"),
                "search produced an invalid grid: {reason}\n{}",
                layout.render(self.problem)
            );
            self.stats.rejected_by_validation += 1;
            return Flow::Continue;
        }

        if !self.seen.insert(layout.cells.clone()) {
            self.stats.duplicates += 1;
            return Flow::Continue;
        }

        // Only fresh grids count toward the cap, so a restart that turns up nothing but duplicates
        // still runs to completion and lets the caller notice the space is exhausted.
        self.candidates_this_restart += 1;
        self.stats.candidates += 1;

        (self.on_candidate)(layout, &self.stats)
    }

    /// Most-constrained-first: prefer the orbit whose squares already have the most decided
    /// neighbours, which keeps the search working outward from settled structure instead of
    /// scattering blocks. Ties are broken randomly so restarts explore different grids.
    fn pick_orbit(&mut self, layout: &Layout) -> Option<usize> {
        let mut best: Option<usize> = None;
        let mut best_score = 0;
        let mut ties = 0u32;

        for idx in 0..layout.cells.len() {
            if layout.cells[idx] != Cell::Unknown {
                continue;
            }
            let partner = self.problem.partner(idx);
            if partner < idx {
                continue; // Canonical representative of the orbit.
            }

            let score = self.decided_neighbours(layout, idx) + self.decided_neighbours(layout, partner);

            if best.is_none() || score > best_score {
                best = Some(idx);
                best_score = score;
                ties = 1;
            } else if score == best_score {
                ties += 1;
                if self.rng.random_range(0..ties) == 0 {
                    best = Some(idx);
                }
            }
        }

        best
    }

    fn decided_neighbours(&self, layout: &Layout, idx: usize) -> u32 {
        let (x, y) = self.problem.coord(idx);
        let mut count = 0;
        // Edges count as decided: a square against the wall is as constrained as one against a block.
        if x == 0 || layout.cells[idx - 1] != Cell::Unknown {
            count += 1;
        }
        if x + 1 == self.problem.width || layout.cells[idx + 1] != Cell::Unknown {
            count += 1;
        }
        if y == 0 || layout.cells[idx - self.problem.width] != Cell::Unknown {
            count += 1;
        }
        if y + 1 == self.problem.height || layout.cells[idx + self.problem.width] != Cell::Unknown {
            count += 1;
        }
        count
    }

    /// Try black or white first depending on how many blocks we still need, so the search drifts
    /// toward the requested density instead of having to backtrack into it.
    fn value_order(&mut self, layout: &Layout) -> [Cell; 2] {
        let target = (self.problem.settings.min_blocks + self.problem.settings.max_blocks) as f64 / 2.0;
        let remaining_needed = (target - layout.block_count as f64).max(0.0);
        let p_block = if layout.unknown_count == 0 {
            0.0
        } else {
            (remaining_needed / layout.unknown_count as f64).clamp(0.0, 1.0)
        };

        if self.rng.random::<f64>() < p_block {
            [Cell::Block, Cell::Open]
        } else {
            [Cell::Open, Cell::Block]
        }
    }
}
