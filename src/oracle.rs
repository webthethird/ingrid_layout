//! The only module that knows about word lists. It answers one question about a finished topology:
//! can `ingrid_core` actually fill it?
//!
//! The word list is loaded once and reused for every candidate. That matters a lot -- loading
//! Spread the Wordlist takes seconds, and we may ask about hundreds of grids.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ingrid_core::backtracking_search::{find_fill, FillFailure};
use ingrid_core::grid_config::{
    generate_slot_configs, generate_slot_options, generate_slots_from_template_string, render_grid,
    sort_slot_options, GridConfig,
};
use ingrid_core::types::{GlyphId, WordId};
use ingrid_core::word_list::WordList;

use crate::layout::{EntryViability, Layout, Problem};

/// Why a topology was rejected, or what it cost to accept it.
#[derive(Debug, Clone)]
pub enum Verdict {
    Filled(FillReport),
    /// Some slot has no matching word at all, so no search was needed. Almost always a theme
    /// answer stranding an awkward letter in a crossing entry.
    NoWordForSlot { slot: String, pattern: String },
    /// `ingrid_core` proved the grid unfillable (its initial arc consistency wiped out a domain).
    Unfillable,
    /// We ran out of time before the solver decided either way.
    TimedOut,
}

/// A successful fill and what it cost.
#[derive(Debug, Clone)]
pub struct FillReport {
    pub grid: String,
    /// Mean score of the words used, a rough proxy for fill quality.
    pub mean_word_score: f64,
    /// Lowest score among the words used, which is usually what makes a fill feel bad.
    pub min_word_score: u16,
    pub states: usize,
    pub retries: usize,
    pub elapsed: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct OracleStats {
    pub attempts: usize,
    pub filled: usize,
    pub rejected_by_empty_slot: usize,
    pub unfillable: usize,
    pub timed_out: usize,
    pub options_cache_hits: usize,
    pub options_cache_misses: usize,
    pub total_time: Duration,
}

/// The pattern of a slot: its per-square contents, `None` for an empty square. Two slots with the
/// same pattern always have the same candidate words, so this is what we cache on.
type PatternKey = Vec<Option<GlyphId>>;

pub struct Oracle {
    word_list: WordList,
    min_score: u16,
    /// Candidate words per slot pattern.
    ///
    /// This is the difference between a usable tool and an unusable one. Without it every candidate
    /// grid rescans whole length buckets (STWL has ~20k five-letter and ~32k seven-letter words) for
    /// each of its ~78 slots. With it, every *empty* slot of a given length shares one entry, so the
    /// scan happens once per length for the whole run.
    ///
    /// The cache stays correct as the word list grows: the only words we ever add are the hidden
    /// ones minted for fully-specified theme entries, and `generate_slot_options` excludes hidden
    /// words from any slot that isn't already complete.
    options_cache: HashMap<PatternKey, Vec<WordId>>,
    pub stats: OracleStats,
}

impl Oracle {
    #[must_use]
    pub fn new(word_list: WordList, min_score: u16) -> Oracle {
        Oracle {
            word_list,
            min_score,
            options_cache: HashMap::new(),
            stats: OracleStats::default(),
        }
    }

    #[must_use]
    pub fn word_list(&self) -> &WordList {
        &self.word_list
    }

    /// Ask whether this topology can be filled.
    pub fn evaluate(&mut self, problem: &Problem, layout: &Layout, timeout: Duration) -> Verdict {
        let start = Instant::now();
        self.stats.attempts += 1;
        let verdict = self.evaluate_inner(problem, layout, timeout);
        self.stats.total_time += start.elapsed();

        match &verdict {
            Verdict::Filled(_) => self.stats.filled += 1,
            Verdict::NoWordForSlot { .. } => self.stats.rejected_by_empty_slot += 1,
            Verdict::Unfillable => self.stats.unfillable += 1,
            Verdict::TimedOut => self.stats.timed_out += 1,
        }

        verdict
    }

    fn evaluate_inner(
        &mut self,
        problem: &Problem,
        layout: &Layout,
        timeout: Duration,
    ) -> Verdict {
        let template = layout.render(problem);

        // Deriving the slots with ingrid_core's own template parser rather than our own means our
        // notion of an entry can't drift from the solver's.
        let slot_specs = generate_slots_from_template_string(&template);
        let (slot_configs, crossing_count) = generate_slot_configs(&slot_specs);

        let fill: Vec<Option<GlyphId>> = problem
            .letters
            .iter()
            .map(|letter| letter.map(|ch| self.word_list.glyph_id_for_char(ch)))
            .collect();

        let mut slot_options: Vec<Vec<WordId>> = Vec::with_capacity(slot_configs.len());

        for slot in &slot_configs {
            let pattern = slot.fill(&fill, problem.width);
            let options = self.ensure_options(&pattern).to_vec();

            // A slot that matches nothing at all kills the grid outright, with no search. Usually
            // that's a theme answer stranding an awkward letter -- a four-square slot needing
            // `??q?`, say -- but an empty slot can hit it too if the list has no words that long.
            if options.is_empty() {
                return Verdict::NoWordForSlot {
                    slot: slot.slot_key(),
                    pattern: render_pattern(&self.word_list, &pattern),
                };
            }

            slot_options.push(options);
        }

        sort_slot_options(&self.word_list, &slot_configs, &mut slot_options);

        let config = GridConfig {
            word_list: &self.word_list,
            fill: &fill,
            slot_configs: &slot_configs,
            slot_options: &slot_options,
            width: problem.width,
            height: problem.height,
            crossing_count,
            abort: None,
        };

        // Always pass a timeout: with `None`, `find_fill` retries forever with a growing backtrack
        // limit and never concludes a hard grid is hopeless.
        let start = Instant::now();
        match find_fill(&config, Some(timeout), None) {
            Ok(success) => {
                // Skip the theme entries. They're pinned by the constructor, and an answer that
                // isn't in the dictionary gets a hidden word minted for it with a score of zero --
                // which would otherwise drag every grid's fill score down by the same amount and
                // make `min_word_score` permanently zero.
                let scores: Vec<u16> = success
                    .choices
                    .iter()
                    .filter(|choice| {
                        slot_configs[choice.slot_id]
                            .complete_fill(&fill, problem.width)
                            .is_none()
                    })
                    .map(|choice| {
                        let length = slot_configs[choice.slot_id].length;
                        self.word_list.words[length][choice.word_id].score
                    })
                    .collect();

                Verdict::Filled(FillReport {
                    // `render_grid` writes `.` for any square it has no letter for. Every square of
                    // a finished fill has a letter, so the ones left over are exactly the blocks.
                    grid: render_grid(&config, &success.choices).replace('.', "#"),
                    mean_word_score: if scores.is_empty() {
                        0.0
                    } else {
                        f64::from(scores.iter().map(|&s| u32::from(s)).sum::<u32>())
                            / scores.len() as f64
                    },
                    min_word_score: scores.iter().copied().min().unwrap_or(0),
                    states: success.statistics.states,
                    retries: success.statistics.retries,
                    elapsed: start.elapsed(),
                })
            }
            Err(FillFailure::Timeout | FillFailure::Abort) => Verdict::TimedOut,
            Err(FillFailure::HardFailure | FillFailure::ExceededBacktrackLimit(_)) => {
                Verdict::Unfillable
            }
        }
    }

    /// Candidate words for a slot pattern, computed once per distinct pattern.
    ///
    /// Returned by reference so the viability check, which only wants to know whether the list is
    /// empty, doesn't pay to copy a bucket of tens of thousands of word ids.
    fn ensure_options(&mut self, pattern: &[Option<GlyphId>]) -> &[WordId] {
        if self.options_cache.contains_key(pattern) {
            self.stats.options_cache_hits += 1;
        } else {
            self.stats.options_cache_misses += 1;
            let options =
                generate_slot_options(&mut self.word_list, pattern, self.min_score, None, None);
            self.options_cache.insert(pattern.to_vec(), options);
        }

        &self.options_cache[pattern]
    }
}

/// Lets the geometry search consult the word list while it is still choosing blocks, instead of
/// discovering at the very end that a theme answer stranded an impossible crossing.
///
/// Every answer goes through the same cache the fill attempts use, so a pattern the search has
/// already asked about costs a hash lookup.
impl EntryViability for Oracle {
    fn is_viable(&mut self, pattern: &[Option<char>]) -> bool {
        let pattern: PatternKey = pattern
            .iter()
            .map(|cell| cell.map(|ch| self.word_list.glyph_id_for_char(ch)))
            .collect();

        if pattern.len() >= self.word_list.words.len() {
            // No words that long are loaded at all.
            return false;
        }

        !self.ensure_options(&pattern).is_empty()
    }
}

/// Render a slot pattern for error messages, e.g. `??q?`.
fn render_pattern(word_list: &WordList, pattern: &[Option<GlyphId>]) -> String {
    pattern
        .iter()
        .map(|cell| cell.map_or('?', |glyph| word_list.glyphs[glyph]))
        .collect()
}
