//! A reference Litz part: a 74HC595 serial-in/parallel-out shift register.
//!
//! The logic is here rather than in the component glue so it is testable on
//! the host. [`component`] is the only wasm-specific file: it maps the
//! `litz:part` contract onto this struct.
//!
//! The part is deliberately digital and stateless in the electrical sense:
//! `DS` is sampled on `SHCP`'s rising edge, `STCP`'s rising edge copies the
//! shift register to the output latch, `OE` is active-low and releases the
//! outputs, and `MR` is active-low and clears the shift register without
//! touching the latch. That is the 595's behaviour in the two-level model the
//! board resolves; nothing else is modelled.

// `wit-bindgen`'s generated export shims are `unsafe` (they are `no_mangle`
// ABI entry points), and a crate-level `forbid` cannot be relaxed in a child
// module. The hand-written logic stays under `deny`, and only the generated
// glue opts back in; `litz-wasm` uses the same pattern at its C ABI boundary.
#![deny(unsafe_code)]

pub mod shift;

#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod component;
