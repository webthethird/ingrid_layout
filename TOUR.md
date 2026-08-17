# A tour of the code

This is a guided read-through of `ingrid_layout`, written for someone who is new to Rust. It maps
the parts of this program onto the Rust ideas they use, so you can learn the language from code that
does something you care about rather than from toy examples.

You don't need to know any Rust to start. You do need to be able to run `cargo test`.

Read it with the files open beside you. Everything is anchored by type and function name rather than
line number, so use your editor's "go to symbol" (or `grep -n "fn ensure_options" src/oracle.rs`).

---

## The map

About 2,300 lines of source and 700 of tests.

| file | lines | what it does | why read it |
| --- | --- | --- | --- |
| [src/lib.rs](src/lib.rs) | 14 | declares the modules | 30 seconds, start here |
| [src/score.rs](src/score.rs) | 260 | ranks finished grids | plain functions over data; the gentlest file |
| [src/layout.rs](src/layout.rs) | 1306 | the block-placement search | the heart of it; most of the Rust ideas live here |
| [src/theme.rs](src/theme.rs) | 750 | decides where the theme answers go | a second search built on the first; good for seeing a pattern reused |
| [src/oracle.rs](src/oracle.rs) | 254 | asks `ingrid_core` whether a grid can be filled | where the borrow checker gets interesting |
| [src/main.rs](src/main.rs) | 798 | the command-line program | argument parsing, wiring it all together |

The dependency arrow points one way: `layout` knows nothing about word lists or themes, `theme`
knows about `layout`, `oracle` knows about word lists. That separation is the main design idea in
the project, and it's also why the trait in "[Traits](#7-traits)" below exists.

## Suggested order

1. **[src/lib.rs](src/lib.rs)** — see how a crate is split into modules.
2. **[src/score.rs](src/score.rs)** — functions, structs, iterators. No lifetimes, no traits.
3. **The types at the top of [src/layout.rs](src/layout.rs)** — `Cell`, `Layout`, `Problem`.
4. **`Layout::set` and the propagation rules** — `Result`, `?`, pattern matching.
5. **[src/oracle.rs](src/oracle.rs)** — borrowing, caching, trait implementation.
6. **[src/theme.rs](src/theme.rs)** — read *after* `layout.rs`, because it's the same search shape
   applied to a different question, and the comparison is most of the value.
7. **[tests/layout.rs](tests/layout.rs)** — reads like a specification; each test is a small grid
   with a comment saying what should happen.

Skip [src/main.rs](src/main.rs) until last. It's the least interesting Rust.

---

## 1. Modules

[src/lib.rs](src/lib.rs) is the whole thing:

```rust
pub mod layout;
pub mod oracle;
pub mod score;
```

`mod layout;` means "there is a module called `layout`, find it in `layout.rs`". `pub` means code
outside this crate can use it. Without `pub`, the module would be visible only inside the crate.

This crate builds two things: a **library** (`lib.rs` and everything it declares) and a **binary**
(`main.rs`). The binary depends on the library like any outside user would — that's why `main.rs`
says `use ingrid_layout::layout::...` rather than reaching in directly. It's a useful discipline: if
the binary can do its job through the public API, so can anyone else.

## 2. Structs and `impl`

From [src/score.rs](src/score.rs):

```rust
pub struct Metrics {
    pub block_count: usize,
    pub word_count: usize,
    pub short_entries: usize,
    // ...
}
```

A `struct` is a record. `usize` is an unsigned integer sized to the machine's pointer width — it's
what Rust uses for counts and indices.

Methods live in a separate `impl` block rather than inside the struct:

```rust
impl Metrics {
    pub fn measure(problem: &Problem, layout: &Layout) -> Metrics { ... }
    pub fn geometric_score(&self, weights: &Weights) -> f64 { ... }
}
```

The difference between those two matters:

- `measure` has no `self`. It's an **associated function**, called as `Metrics::measure(...)`. Other
  languages would call it a static method or a constructor.
- `geometric_score` takes `&self`, so it's a **method**, called as `metrics.geometric_score(...)`.

Three ways a method can take itself:

| form | meaning | example |
| --- | --- | --- |
| `&self` | borrow it, read only | `Metrics::geometric_score` |
| `&mut self` | borrow it, may modify | `Layout::set` |
| `self` | take ownership, consume it | rare here |

If you see `&mut self`, the method changes something. That's a genuinely useful signal when reading
unfamiliar code — much more so than in languages where any method might mutate anything.

## 3. Ownership and borrowing

This is the idea Rust is famous for, and the one worth spending time on.

Every value has exactly one **owner**. When the owner goes out of scope, the value is freed — no
garbage collector, no manual `free`. Instead of passing values around and copying them, you pass
**references**: `&T` to read, `&mut T` to modify.

The rule the compiler enforces: at any moment you may have **either** any number of `&T` **or**
exactly one `&mut T`, never both. That single rule is what prevents whole categories of bug.

You can see it shape the code. Notice that nearly every function in [src/layout.rs](src/layout.rs)
takes `problem: &Problem`:

```rust
fn rule_min_entry_length(&mut self, problem: &Problem) -> PropResult<bool>
```

`Problem` holds the grid dimensions, the theme letters and precomputed geometry — it never changes
during the search. So it's passed as `&Problem`, a shared read-only borrow, and many things can hold
it at once. `Layout`, which *does* change, is the `&mut self`.

That split isn't decoration. It's why the search can hold one `Problem` and clone thousands of
`Layout`s cheaply, and the compiler guarantees nothing accidentally mutates the shared part.

### Clone

In `SearchState::descend`:

```rust
let mut child = layout.clone();
```

`.clone()` makes an independent copy. It's explicit in Rust — nothing is deep-copied behind your
back. Here it's a deliberate choice: the search could instead record every change and undo it on the
way back up, but `Layout` is about 500 bytes, and copying it is simpler and fast enough. The comment
on the struct says exactly that. Cloning isn't a failure; unconsidered cloning is.

## 4. Enums and `match`

Rust enums are much stronger than the integer constants of C or Java. Each variant can be a distinct
shape, and the compiler forces you to handle every one.

```rust
pub enum Cell {
    Unknown,
    Open,
    Block,
}
```

The payoff is `match`:

```rust
match self.cells[target] {
    Cell::Unknown => { /* assign it */ }
    existing if existing == value => {}          // already this value: fine
    _ => return Err(Contradiction("...")),       // conflicting value
}
```

Three things to notice:

- `match` is **exhaustive**. Leave out a variant and it won't compile. Add a fourth variant to `Cell`
  later, and the compiler lists every place that needs updating. This is the single best thing about
  Rust for refactoring — see the exercises.
- `existing if existing == value` is a **match guard**: the arm applies only when the condition
  holds.
- `_` is the catch-all.

Enums carry data too. From [src/oracle.rs](src/oracle.rs):

```rust
pub enum Verdict {
    Filled(FillReport),
    NoWordForSlot { slot: String, pattern: String },
    Unfillable,
    TimedOut,
}
```

One value that is exactly one of four things, each carrying what it needs. A grid that filled has a
report; one that didn't, doesn't — and you cannot read a report that isn't there, because getting at
it requires matching on `Filled` first.

## 5. `Option` and `Result`

Rust has no `null`. Something that might be absent is `Option<T>`:

```rust
pub letters: Vec<Option<char>>,
```

One entry per square: `Some('a')` for a theme letter, `None` for an empty square. You can't
accidentally use the letter of a square that has none — you have to unwrap the `Option`, and the
compiler makes you say how.

Something that might fail is `Result<T, E>` — either `Ok(T)` or `Err(E)`. This codebase uses an
alias:

```rust
type PropResult<T> = Result<T, Contradiction>;
```

so `PropResult<bool>` means "a `bool` if it worked, a `Contradiction` if this branch of the search is
dead."

### The `?` operator

This is the piece of syntax you'll see most:

```rust
let mut changed = self.rule_min_entry_length(problem)?;
```

`?` means: if this is `Err`, return that error from the enclosing function right now; otherwise
unwrap the `Ok` value and carry on. It turns error handling from nested `if`s into ordinary-looking
straight-line code, while keeping every failure explicit in the type.

Look at `Layout::propagate` to see the effect — the happy path reads top to bottom, and every `?` is
a place the search can give up.

### Why `set` returns `bool`

```rust
pub fn set(&mut self, problem: &Problem, idx: usize, value: Cell) -> PropResult<bool>
```

The `bool` is "did this actually change anything?" `propagate` loops its rules until nothing changes
— a **fixpoint**. Without that flag it wouldn't know when to stop. So the return type carries two
different things: whether it failed (`Result`) and whether it did anything (`bool`).

## 6. Iterators and closures

Rust loops are usually written as iterator chains. From `Metrics::measure`:

```rust
short_entries: entries.iter().filter(|&&(_, _, len)| len == min_len).count(),
```

Reading it right to left: take the entries, keep the ones whose length equals `min_len`, count them.

The `|...| ...` is a **closure** — an anonymous function that can capture variables from around it
(`min_len` here). The `|&&(_, _, len)|` looks cryptic; it's destructuring. Each entry is a tuple
`((usize, usize), Direction, usize)`, `filter` hands the closure a reference to a reference, the
`&&` peels both off, and `_` discards the parts we don't want, binding only the length.

Closures can be parameters. From `Layout::flood_reaches_all_open`:

```rust
fn flood_reaches_all_open(&self, problem: &Problem, passable: impl Fn(Cell) -> bool) -> bool
```

`impl Fn(Cell) -> bool` means "any function-like thing taking a `Cell` and returning a `bool`". It
lets one flood-fill serve two purposes:

```rust
self.flood_reaches_all_open(problem, |cell| cell != Cell::Block)   // optimistic: unknowns passable
self.flood_reaches_all_open(problem, |cell| cell == Cell::Open)    // strict: only white squares
```

That's generics. The compiler generates a specialised copy for each closure, so it costs nothing at
runtime.

## 7. Traits

A trait is a set of behaviours a type can implement — roughly an interface.

Some come from the standard library. `impl Default for LayoutSettings` gives you
`LayoutSettings::default()`, and lets you write:

```rust
LayoutSettings { min_blocks: 30, ..LayoutSettings::default() }
```

`..` means "the rest from there". Very handy in tests.

`#[derive(Debug, Clone)]` above a struct asks the compiler to write some traits for you — `Debug`
enables `{:?}` printing, `Clone` enables `.clone()`.

Sometimes you write one by hand for a reason. `Layout` implements `PartialEq` manually:

```rust
impl PartialEq for Layout {
    fn eq(&self, other: &Self) -> bool {
        self.cells == other.cells
    }
}
```

because `Layout` carries a `viability_checked` cache that isn't part of what the grid *is*. Two
grids with the same squares are the same grid, whatever each happens to have checked. Derive would
have compared it and called them different.

### The interesting one

```rust
pub trait EntryViability {
    fn is_viable(&mut self, pattern: &[Option<char>]) -> bool;
}
```

This exists to solve a design problem. The geometry search needs to ask "could any word fill this
entry?" — but [src/layout.rs](src/layout.rs) is deliberately ignorant of word lists. So it declares
the *question* as a trait, and [src/oracle.rs](src/oracle.rs) supplies the *answer*:

```rust
impl EntryViability for Oracle { ... }
```

`layout` still doesn't know `Oracle` exists. It could be given a different implementation — a
different dictionary, or a stub in a test — without changing a line. This is how Rust does
dependency inversion.

## 8. Trait objects, and one genuinely ugly signature

```rust
fn descend(&mut self, layout: Layout, mut viability: Option<&mut (dyn EntryViability + '_)>) -> Flow
```

Unpacking it:

- `Option<...>` — the checker is optional; `--emit-templates` runs without a word list.
- `&mut` — `is_viable` takes `&mut self` (it updates a cache), so we need a mutable borrow.
- `dyn EntryViability` — a **trait object**. `impl Trait` (section 6) picks the concrete type at
  compile time; `dyn Trait` decides at runtime through a pointer. Needed here because the type must
  be nameable in a struct field and threaded through recursion.
- `+ '_` — and this one is worth understanding, because it cost real time.

In `&'a mut dyn Trait`, the trait object gets an implicit lifetime bound, and it **defaults to `'a`**
— the lifetime of the reference itself. That ties the two together. When `descend` recurses:

```rust
self.descend(child, viability.as_deref_mut())
```

it needs to hand down a *shorter* borrow than the one it holds. With the default bound the compiler
refuses: the borrow is required to last for the original lifetime, so the second iteration of the
loop collides with the first. `+ '_` says "give the trait object its own lifetime, independent of the
reference's", which lets the reborrow shrink.

The error you get is `cannot borrow 'viability' as mutable more than once at a time` — which does not
obviously point at the fix. Worth remembering: **when a reborrow through recursion won't compile,
suspect the default object lifetime bound.**

## 9. Lifetimes

Lifetimes are how the compiler proves a reference never outlives what it points at. Most of the time
they're inferred and invisible. You write them when a struct *holds* a reference:

```rust
struct SearchState<'a, 'p> {
    problem: &'a Problem,
    on_candidate: &'a mut dyn FnMut(&Layout, &SearchStats) -> Flow,
    on_progress: Option<&'p mut dyn FnMut(&SearchStats)>,
    // ...
}
```

`'a` is a name for "some lifetime". This says: a `SearchState<'a, 'p>` holds references that live at
least as long as those lifetimes, so the compiler can reject any use of it after the `Problem` is
gone.

You'll also see `impl SearchState<'_, '_>` — `'_` means "there's a lifetime here, infer it, I don't
need to name it."

**Why two lifetimes rather than one.** The two callbacks are two separate `&mut` borrows made at the
call site, and `&mut T` is *invariant* in `T`: the compiler will not quietly shorten one to match the
other, the way it would for a shared `&T`. Forcing both into a single `'a` therefore asks the caller
to prove its two borrows live exactly as long as each other, and `search`'s caller in
[src/main.rs](src/main.rs) can't — one closure borrows the candidate pool, the other doesn't. A
second lifetime parameter costs nothing and says what is actually true: the two borrows are
unrelated. The error to recognise is `lifetime may not live long enough ... requirement occurs
because of a mutable reference`.

### Splitting borrows

From `Oracle::ensure_options`:

```rust
fn ensure_options(&mut self, pattern: &[Option<GlyphId>]) -> &[WordId] {
    if self.options_cache.contains_key(pattern) {
        self.stats.options_cache_hits += 1;
    } else {
        self.stats.options_cache_misses += 1;
        let options = generate_slot_options(&mut self.word_list, pattern, self.min_score, None, None);
        self.options_cache.insert(pattern.to_vec(), options);
    }

    &self.options_cache[pattern]
}
```

Two things here are worth noticing.

**It returns a reference derived from `&mut self`.** The returned `&[WordId]` borrows from
`self.options_cache`, so as long as the caller holds it, the whole `Oracle` stays borrowed. That's
why the caller writes `.to_vec()` when it needs an owned copy, and calls it directly when it only
needs `.is_empty()`. Returning `&[WordId]` instead of `Vec<WordId>` is what stops the viability check
from copying tens of thousands of word IDs on every call.

**It touches `self.word_list`, `self.options_cache` and `self.stats` in one function.** The compiler
tracks borrows of *individual fields*, so mutating two different fields at once is fine. But once you
call a *method* on `self`, the whole struct is borrowed — which is a common source of confusion, and
why this function pokes at fields directly rather than calling helpers.

`SearchState::report_progress` needs the same trick in a form that isn't optional:

```rust
let SearchState { on_progress, stats, .. } = self;
if let Some(on_progress) = on_progress.as_deref_mut() {
    on_progress(stats);
}
```

Calling the closure needs `self.on_progress` mutably; passing it the counters needs `self.stats`
immutably. Written as `self.on_progress...(&self.stats)` that's a mutable and a shared borrow of the
same `self` at once, and it's rejected. Destructuring names the two fields separately, and the
compiler can then see they don't overlap. `..` means "and ignore the rest".

---

## 10. Two searches with the same shape

[src/theme.rs](src/theme.rs) decides where the theme answers go, and [src/layout.rs](src/layout.rs)
decides where the black squares go. They're different problems, but read them side by side and the
skeleton is identical:

| | `layout::search` | `theme::place` |
| --- | --- | --- |
| what it decides | one symmetry orbit at a time | one answer (or mirrored pair) at a time |
| how it recurses | `SearchState::descend` | `Placer::descend` |
| how it undoes | clones the `Layout` per node | writes letters, then `undo_move` |
| when it gives up | node budget, then restart | node budget, then restart |
| how it reports | `FnMut(&Layout, &SearchStats) -> Flow` | `FnMut(Placement) -> Flow` |

Two things are worth noticing about that.

**The `Flow` callback is the same type in both.** Neither search collects its results into a `Vec`
and returns it; both hand each result to a closure that says `Continue` or `Stop`. That's what lets
[src/main.rs](src/main.rs) stop a search the moment it has enough without either module knowing what
"enough" means. It's also why the caller can hold state — the pool — that the search never sees.

**They undo differently, on purpose.** `layout` clones its whole `Layout` at each node, because
that's about 500 bytes and simpler than an undo trail. `theme` writes into a shared letter grid and
takes the writes back, because its nodes are *pairs* of answers and a half-applied pair would leave
the search in a state that violates its own invariant. Read `Placer::try_move` for the comment
version of that argument: the all-or-nothing behaviour is the point, and the `Vec<usize>` of written
squares is the whole undo log.

### Where the layering shows up in the types

```rust
pub fn build_problem(template: &Template, settings: LayoutSettings) -> Result<(Problem, Layout), String>
```

`Template` is the handover point. `parse_template` produces one by reading characters; `theme::place`
produces one by *choosing* where answers go. Both then go through `build_problem`, so the two entry
paths cannot drift apart — the rule about what a theme answer's boundary looks like is written down
once.

That refactor is worth studying as a refactor. Before the theme layer existed, `parse_problem` did
both jobs in one function, and there was no reason to separate them. Splitting it was the *whole* of
what `layout.rs` needed to change to support a new layer above it. When a new feature needs almost no
change to the code it sits on, the boundary was in the right place.

## Five things that look odd, and why

**`for target in [idx, problem.partner(idx)]`** in `Layout::set`. Iterating a two-element array is
just a tidy way to run the same code for a square and its symmetric partner without duplicating it.

**`usize::from(line_idx >= problem.height)`** in `infer_theme_entries`. Converts `bool` to `0` or
`1`. Rust won't do that implicitly, which is a good thing, but it makes the explicit form look
peculiar.

**`self.stats.nodes.is_multiple_of(DEADLINE_CHECK_INTERVAL)`** rather than `% N == 0`. Same thing;
clippy prefers the named method. `cargo clippy` is worth running — it teaches idiom as you go.

**`debug_assert!`** in `Layout::set`. Checked in debug builds, compiled out of release builds. Use it
for invariants that should be impossible, where you don't want to pay for the check in production.

**`#[must_use]`** on functions like `Metrics::measure`. Warns if you call it and throw the result
away. Appropriate for pure functions where discarding the result means you've made a mistake.

---

## Exercises

Do these in order; each teaches something the next relies on. Run `cargo test` after each.

### 1. Add a metric (structs, iterators)

In [src/score.rs](src/score.rs), add a `long_entries_over_ten: usize` field to `Metrics`, count
entries of length 10 or more in `measure`, and print it from `describe` in
[src/main.rs](src/main.rs).

You'll learn: the compiler will refuse to build until you initialise the new field everywhere
`Metrics` is constructed. That's the pattern — the compiler as a to-do list.

### 2. Break a rule on purpose (Result, tests)

In [src/layout.rs](src/layout.rs), change `rule_min_entry_length` to use `min_len + 1`. Run
`cargo test`. Watch which tests fail and read their comments to see exactly which property you broke.
Change it back.

You'll learn: what the tests are actually pinning down, and how to read a Rust test failure.

### 3. Add a fourth `Cell` variant (exhaustive matching)

Add `Reserved` to the `Cell` enum. Don't change anything else. Run `cargo build`.

The compiler will list every `match` that no longer covers all cases. Read that list — it's a precise
map of everywhere cell state is inspected, which is genuinely hard to obtain in most languages.
Then `git checkout src/layout.rs` to undo.

**This is the exercise I'd do first if I only did one.**

### 4. Add a command-line flag (Option, clap)

Add `--max-block-clump <N>` to the `Args` struct in [src/main.rs](src/main.rs), and use it to drop
candidates whose `metrics.largest_block_clump` exceeds it, just after the `ranked` list is sorted.
Copy the shape of an existing optional flag like `--max-words`.

You'll learn: how `Option<T>` models "the user didn't say anything", and how `unwrap_or` supplies a
default.

A note on why it goes there rather than in `Layout::validate`: `largest_block_clump` lives in
[src/score.rs](src/score.rs), which depends on [src/layout.rs](src/layout.rs). Enforcing it inside
`validate` would make `layout` depend on `score` and the two would depend on each other. Rust permits
that within a crate, but it's a design smell — and noticing it is the more valuable lesson than the
flag.

### 5. Make the search report something new (borrowing)

Add a counter to `SearchStats` for how many candidates were rejected by connectivity, increment it in
the right place, and print it in verbose mode.

You'll learn: why `SearchState` holds `stats` by value while other things are borrowed.

---

## Where to go next

- **[The Rust Book](https://doc.rust-lang.org/book/)** — chapters 4 (ownership), 6 (enums and
  matching) and 10 (traits and lifetimes) cover most of what's used here.
- **[Rust by Example](https://doc.rust-lang.org/rust-by-example/)** — good when you want a small
  runnable illustration of one idea.
- `cargo clippy` — a linter that explains idiom. This crate is clean under it; keep it that way and
  it'll teach you as you write.
- `cargo doc --open` — renders the doc comments (`///`) in this codebase as browsable documentation.

The one thing worth internalising early: **fighting the borrow checker usually means the design is
unclear about who owns what.** The `+ '_` in section 8 is the exception that proves it — a genuine
wart — but nine times out of ten, the compiler is pointing at a real ambiguity, not being difficult.
