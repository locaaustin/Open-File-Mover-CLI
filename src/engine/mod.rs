//! The transfer engine: decide what to do, then do it.
//!
//! Planning and execution are deliberately separate. The planner produces a
//! complete, inspectable list of operations without changing anything, which
//! is what makes `--dry-run` an exact preview rather than a guess.

pub mod exec;
pub mod filters;
pub mod plan;
