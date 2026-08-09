# ingrid_layout

Given a crossword grid with only the theme answers placed, work out where the black squares go so
the finished grid is both legal and fillable.

[`ingrid_core`](https://github.com/rf-/ingrid_core) fills a *fixed* grid topology — you give it a
template, it derives the Across/Down slots and solves for the words. Block placement isn't one of its
variables. This crate sits on top: it searches over the geometry, and uses `ingrid_core` both as a
fillability oracle and as a running check on whether the entries it is creating can be filled at all.

It's a separate crate on purpose. It needs no changes to `ingrid_core`, so a fork of that library
stays clean.

## Install

```
cargo install --git https://github.com/webthethird/ingrid_layout
```

Or from a clone:

```
git clone https://github.com/webthethird/ingrid_layout
cd ingrid_layout
cargo install --path .
```

It pulls `ingrid_core` from [webthethird/ingrid_core](https://github.com/webthethird/ingrid_core) and
bundles its own copy of the word list, so a fresh clone builds with nothing else set up. To develop
the two crates side by side, point the dependency in `Cargo.toml` at a local path instead; to allow
rebus squares in theme answers, point its `branch` at `rebus-support`.

## Usage

```
$ cat theme.txt
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
???????????????

$ ingrid_layout theme.txt --count 3
```

Each cell of the input is one character:

| char | meaning |
| --- | --- |
| `?` | undecided — the tool chooses |
| `#` | a block you want to keep |
| `.` | a square you want to stay open |
| letter | a theme answer's letter |

Runs of letters are read as **complete entries**, so the squares just past each end are forced to be
blocks. Writing an explicit `.` there instead says "this run is part of some longer entry" and opts
out.

Two kinds of letter run are *not* read as answers, because they can't be:

- **A lone letter**, which is just the crossing square of a perpendicular answer.
- **A run fully explained by longer answers crossing it.** Stacked theme answers on adjacent rows
  leave a two-letter vertical run in every column; those are crossing squares, and forcing blocks
  around each one would wall off the grid.

A run that survives both tests but is shorter than `--min-entry-length` is reported as an error,
since it was evidently meant as an answer and can't legally be one.

Output is `--count` grids, best first, each with its block pattern, a completed fill, and the metrics
behind its score.

### Options

```
--min-blocks / --max-blocks         block count range
--max-words                         most entries allowed
--max-short-entries                 most three-letter entries; lower is better but yields fewer grids
--min-entry-length 3                shortest allowed entry
--no-symmetry                       drop the 180-degree rotational symmetry requirement
--count 5                           how many filled grids to return
--pool 500                          legal topologies to sample before ranking
--timeout 120                       overall budget
--fill-timeout                      per-grid fill budget
--wordlist PATH --min-score 50      word list and quality bar
--seed 0                            same seed, same grids
--emit-templates                    geometry only, no filling
-v                                  search and oracle counters on stderr
```

### Grid size

The four count limits and the fill budget default to values **scaled to your grid**, anchored on the
two standard American sizes:

| | 15×15 daily | 21×21 Sunday |
|---|---|---|
| blocks | 30–42 | 59–82 |
| entries | ≤78 | ≤140 |
| minimum-length entries | ≤10 | ≤18 |
| per-grid fill budget | 5s | 19s |

This matters more than it sounds. Hardcoded 15×15 limits make every larger grid *silently
impossible*: a 21×21 has 42 entries before you place a single block, so a cap of 78 rejects
everything and you get "found no legal block arrangement" with no hint as to why. Run with `-v` to
see the limits actually in force. Any of them can still be set explicitly.

Bigger grids are also much slower to fill — not proportionally but closer to quadratically in area —
so expect a 21×21 to want a longer `--timeout` as well.

## How it works

**Geometry search** (`layout.rs`, no knowledge of any word list). Squares are three-valued —
undecided, open, or block — and decisions are made per *symmetry orbit* rather than per square, so
180-degree symmetry holds by construction rather than being tested afterwards. Constraint propagation
then does most of the work; every rule rests on the same monotonicity argument, that a square only
ever moves from undecided to decided, so runs of non-block squares only ever shrink:

- **Minimum entry length.** Any square whose longest possible across *or* down run is already too
  short can never be white, so it is forced black. This also enforces that every white square is
  checked in both directions.
- **Forced open.** An open square's entry has to reach the minimum length; when one side lacks the
  room, the shortfall is forced open on the other.
- **Block count**, rejected as soon as the range becomes unreachable.
- **Connectivity**, checked optimistically: if two open squares can't reach each other even by
  travelling through undecided squares, nothing can reconnect them.
- **Word and short-entry counts**, counted over entries whose extent is already pinned down. Those
  counts only grow, so exceeding a maximum prunes the branch instead of being discovered at the leaf.

The search is a *sampler*, not an enumerator — for a sparse theme the number of legal topologies is
astronomical. It uses randomised restarts, one grid per restart, because consecutive leaves of a
single depth-first descent differ by a square or two and ranking hundreds of near-identical grids is
pointless.

**Word-list feedback during the search.** As soon as an entry's extent is pinned down, if it contains
theme letters it is checked against the word list. Without this the search happily builds grids whose
long down entries thread through two or three theme answers, needing patterns like `???s???n???d???`
that match nothing — and that only surfaces after a whole grid has been built. The geometry layer
stays word-list-agnostic; it asks through an `EntryViability` trait that the oracle implements.

**Ranking** (`score.rs`). Legality is settled by the time a grid gets here, so this is what separates
a usable grid from an ugly one: short-entry count, long entries, cheater squares (blocks whose removal
wouldn't change the word count), block clumping, word count, and entries crossing two or more theme
answers. Grids are ranked on geometry *first*, so the expensive fill attempts are spent on the most
promising ones.

**Filling** (`oracle.rs`). The only module that touches a word list. It builds a `GridConfig` and
calls `find_fill`. Two things make this affordable:

- The word list is loaded once and reused for every candidate.
- Candidate words are cached per slot pattern. An empty slot's options depend only on its length, so
  all ~70 empty slots in every grid share one entry per length. Without this, each candidate rescans
  whole length buckets (STWL has ~20k five-letter and ~32k seven-letter words) once per slot.

## Calibration

The tests are anchored on the grid in `ingrid_core`'s README — a real published puzzle. It scores
about **-5** on these metrics (32 blocks, 72 words, 6 three-letter entries, 0 cheaters). Grids this
tool generates for the same theme score roughly **-11 to -40**. So the scoring does rank a human's
grid above the tool's, and the gap is mostly three-letter entries and cheater squares.

Feeding that published grid back in exercises the whole pipeline as a validator: the geometry rules
accept it, and the oracle produces a fill byte-identical to the `ingrid_core` CLI's.

## Two things worth knowing

**Fill scores are flat at the default `--min-score`.** Spread the Wordlist scores words in bands of
0/10/20/30/40/50, so `--min-score 50` leaves exactly one band and every fill scores 50.0. The fill
component of the ranking then contributes nothing and grids are ordered on geometry alone. Lower
`--min-score` to make it discriminate — at the cost of letting weaker words in.

**Most sampled topologies don't fill.** Typically a few percent, and the survivors cluster near the
top of the geometric ranking, which is exactly why ranking happens before filling. If a theme yields
nothing, the useful knobs are `--max-short-entries` (up), the block range (wider), or moving the
theme answers. `-v` shows the breakdown between "no word for a slot" and "unfillable", and prints one
of the grids the solver rejected so you can see what it's up against.

## Where it struggles

Heavily themed 21×21s. A Sunday with four or five long theme rows pins letters in every column, and
with the bundled word list the sampled grids come back *proven* unfillable at arc consistency — not
timed out, but shown to have no fill at all. Two things are working against it:

- **Word list size.** Spread the Wordlist at `--min-score 50` is about 120k entries, and it contains
  nothing longer than 15 letters. Constructors building stacked Sundays typically work from lists
  several times that size. `--min-score 40` or `30` widens the pool considerably.
- **The sampler is undirected.** It generates legal grids and ranks them, but it doesn't reason
  backwards from "this corner is hard to fill" to "put a block here", which is what a human does.
  The more the theme constrains the grid, the more that gap costs.

For a 21×21, expect to need `--min-score 40`, a large `--pool`, and a `--timeout` in the several
minutes. It's a genuinely harder problem than a 15×15, not just a bigger one.

## Development

```
cargo test          # 24 tests; the geometry ones need no word list and run in well under a second
cargo clippy --all-targets
```

`tests/layout.rs` covers the geometry layer on its own, including a property test that samples
hundreds of grids and asserts every one satisfies symmetry, minimum entry length, checked squares,
connectivity, theme boundaries and the count bounds. `tests/pipeline.rs` covers scoring and filling
against the published grid.

## Acknowledgments

- [`ingrid_core`](https://github.com/rf-/ingrid_core) by Ryan Fitzgerald does all the actual
  crossword filling. This crate only decides where the black squares go.
- [Spread the Wordlist](https://www.spreadthewordlist.com) by Brooke Husic and Enrique Henestroza
  Anguiano is bundled as the default dictionary.
- The example and test grid is Ryan McCarty's puzzle, from `ingrid_core`'s README.

## License

MIT.
