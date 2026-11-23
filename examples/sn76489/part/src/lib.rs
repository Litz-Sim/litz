//! The SN76489 programmable sound generator as a Litz wasm part.
//!
//! The chip's registers, counters and mixer live in [`psg`], where the host
//! can test them; [`component`] is the wasm-only glue that maps the
//! `litz:part` contract onto that model. This crate is the worked example
//! behind `docs/parts.md`: a device with a parallel write bus, a clock of its
//! own and an output a scope can see.

// `wit-bindgen`'s generated export shims are `unsafe`, so only the generated
// glue opts back in; the hand-written logic stays under `deny`.
#![deny(unsafe_code)]

pub mod psg;

#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod component;
