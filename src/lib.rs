//! A meta-solver above `ingrid_core`.
//!
//! `ingrid_core` fills a *fixed* grid topology: block placement is not one of its variables. This
//! crate searches over the geometry instead -- given a grid with only the theme answers placed, it
//! works out where the black squares can go -- and uses `ingrid_core`'s solver as an oracle for the
//! question "can this topology actually be filled?".
//!
//! The two layers are deliberately independent. [`layout`] knows nothing about word lists and can
//! be tested on its own; [`oracle`] is the only place that touches the solver.

pub mod layout;
pub mod oracle;
pub mod score;
