//! Loading a Litz project, booting it and deciding pass or fail.
//!
//! This crate is the part a host embeds. A command-line runner is one host:
//! it parses flags into [`runner::Options`], calls [`runner::execute_with`] and
//! prints the transcript. A browser session is another: it hands
//! [`machine::boot_c3`] the diagram and images it already holds in memory. The
//! library owns project loading, boot, the run loop, the assertions and the
//! probe records; the host owns arguments, files and presentation.

#[cfg(not(target_family = "wasm"))]
pub mod board;
pub mod config;
pub mod decode;
pub mod image;
pub mod lock;
pub mod machine;
pub mod python;
pub mod runner;
pub mod scenario;

/// Probes, their records and the measurements over them, re-exported so a
/// host that decodes or draws a scope needs one dependency, not two.
pub use litz_board::probe;
