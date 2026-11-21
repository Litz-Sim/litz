//! Litz — an open-source, modular firmware simulator.
//!
//! One core, two exports: a native binary for CI and a WebAssembly module for
//! the browser and for WASI. Boards are described by a `diagram.json`, so a
//! project is a set of files rather than a database row.
//!
//! ```text
//! litz-core    time, pins, Bus / Mcu / Part traits
//! litz-mcu     memory maps, clock domains, interrupt controllers
//! litz-esp     ESP32-family chips
//! litz-wasm    the WebAssembly surface
//! litz-cli     `litz run` / `litz test`
//! ```
//!
//! The umbrella re-exports the family seams (`litz::esp`, `litz::mcu`) so a
//! user picks a chip, not a crate.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

/// Time, pin and bus primitives every other crate shares.
pub use litz_core as core;

/// Chip-family building blocks: memory maps, clock domains, interrupt routing.
pub use litz_mcu as mcu;

/// ESP32-family chips. This is the module a user picks a chip from.
pub use litz_esp as esp;

/// Circuits: parse `diagram.json`, resolve wires into nets, validate against a catalog.
pub use litz_diagram as diagram;

/// Catalog entries: `part.toml` and `board.toml`.
pub use litz_catalog as catalog;
