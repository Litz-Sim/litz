//! Guest SDK for Litz parts.
//!
//! A part crate depends on this instead of invoking `wit-bindgen` itself. The
//! generated bindings live in [`abi`]; re-exports below flatten the names a
//! device needs. A reference guest then looks like:
//!
//! ```ignore
//! use litz_part_sdk::Guest;
//!
//! struct Part;
//!
//! impl Guest for Part {
//!     fn info() -> litz_part_sdk::PartInfo { /* ... */ }
//!     // ... one method per `litz:part@0.2.0` export
//! }
//!
//! litz_part_sdk::export!(Part with_types_in litz_part_sdk::abi);
//! ```
//!
//! The bindings compile on the host too, so a guest can keep its logic in a
//! normal module with unit tests and compile the export glue only for
//! `wasm32-unknown-unknown`. Nothing here adds a runtime: the component world
//! imports only `log` and `rom`, and no WASI is involved.
#![deny(unsafe_code)]

/// The `litz:part@0.2.0` world, generated from the WIT contract.
///
/// The module is allowed to contain `unsafe` because `wit-bindgen`'s canonical
/// ABI shims are `no_mangle` entry points; the rest of the crate stays under
/// `deny`.
#[allow(unsafe_code)]
pub mod abi {
    wit_bindgen::generate!({
        path: "../litz-part-wit/wit",
        world: "component",
        pub_export_macro: true,
    });
}

pub use abi::exports::litz::part::part::Guest;
pub use abi::litz::part::types::{
    AttributeDef, AttributeValue, Damage, Drive, FramebufferInfo, Level, PartInfo, PartState,
    Strength, Tone,
};

/// The macro that installs a [`Guest`] implementation as the component's
/// exports.
pub use abi::export;

/// The host services the contract permits.
pub mod host {
    /// Append one diagnostic line to the host's run log.
    pub fn log(message: &str) {
        crate::abi::litz::part::host::log(message);
    }

    /// The read-only image the host attached at load; empty when it supplied
    /// none. Call it once (tests, `info`) rather than on every transaction.
    pub fn rom() -> Vec<u8> {
        crate::abi::litz::part::host::rom()
    }
}
