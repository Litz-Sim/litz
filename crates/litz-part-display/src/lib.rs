//! A reference Litz part: a small RGBA framebuffer display.
//!
//! [`panel`] holds the pixels and damage tracking and is tested on the host.
//! [`component`] maps the `litz:part@0.2.0` exports onto it. The catalog entry
//! is `litz-rgb-panel`; it answers I2C at 0x3c and advertises an 8x8 RGBA8888
//! framebuffer.

#![deny(unsafe_code)]

pub mod panel;

#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod component;
