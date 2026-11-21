//! The `litz` command: run a firmware image against a diagram, check the
//! diagram without running anything, or serve the browser simulator.
//!
//! The engine — project loading, boot, the run loop, assertions, probes — lives
//! in [`litz_engine`]. What stays here is what a command line owns: argument
//! parsing, file output, report formats, the static file server and the
//! project's build recipe. The re-exports below keep older paths such as
//! `litz_cli::runner::Options` working for callers of the binary crate.

pub mod build;
pub mod catalog;
pub mod heap;
pub mod report;
pub mod serve;
pub mod toolchain;

pub use litz_engine::{config, image, lock, runner, scenario};
