//! Library face of the CLI, so the scan orchestration and its safety gates
//! can be exercised by integration tests rather than only through the binary.

pub mod families;
pub mod format;
pub mod index;
pub mod permit;
pub mod read;
pub mod scan;
