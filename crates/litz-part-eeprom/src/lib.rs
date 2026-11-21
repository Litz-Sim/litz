//! A reference Litz part: a 24C02 I2C serial EEPROM.
//!
//! The register file is in [`eeprom`] so it runs and is tested on the host.
//! [`component`] is the only wasm-specific file: it maps the `litz:part@0.2.0`
//! exports onto that struct.
//!
//! The catalog entry is `litz-24c02`; its manifest declares `[buses.i2c0]`
//! with address 0x50 and points at the component `build.sh` writes.

// `wit-bindgen`'s generated export shims are `unsafe`; only the glue module
// opts back in, and the guest logic stays under `deny`.
#![deny(unsafe_code)]

pub mod eeprom;

#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod component;
