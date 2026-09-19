//! Reorganising the archive into `YYYY/YYYY-MM-DD_событие/`.
//!
//! This stage runs after deduplication and never on its own initiative: it
//! decides only where a file should live, not whether it should exist. Every
//! move is a rename within one filesystem, recorded in the journal with both
//! paths, so the whole reorganisation can be walked backwards.

pub mod date;
pub mod events;
pub mod plan;

pub use date::{Dated, Precision, Source};
pub use plan::{compute, Move, Options, Plan, Refusal};
