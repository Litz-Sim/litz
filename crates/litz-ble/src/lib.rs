//! Bluetooth LE belongs to the circuit, not to the chip adapter.
//!
//! A radio backend owns the LE link a chip hears, exactly as a Wi-Fi backend
//! owns its 802.11 link. The devices around a scanning station — their
//! addresses, names, services and signal — therefore belong to the parts the
//! circuit places, not to one adapter. Keeping them here means the same
//! diagram is heard by an ESP32-C3 or any later backend, and the addresses and
//! bytes a test asserts on do not depend on which backend carried them. It is
//! the same seam as `litz-net`, one register lower: `litz-net` describes the
//! network behind an access point, this crate describes the advertisers around
//! a station.
//!
//! Nothing here reads a clock or a random source. An address the circuit does
//! not name is derived from the advertising bytes and carries the random-static
//! type bits, so it is the same on every run and in every process. A backend
//! only translates a [`Beacon`] into its own peer type; `litz-esp` does that
//! for esp32sim.
//!
//! The crate is `std` because an advertising payload is held in a `Vec`, but it
//! has no other dependencies.

#![forbid(unsafe_code)]

pub mod ad;
pub mod beacon;
pub mod config;
pub mod gatt;

pub use ad::{
    AD_FLAGS, AD_NAME_COMPLETE, AD_NAME_SHORT, AD_UUID16_COMPLETE, AD_UUID16_INCOMPLETE,
    AdStructure, advertising_data,
};
pub use beacon::{
    Beacon, MAX_ADVERTISING_DATA, MAX_NAME, address_text, derive_address, parse_address,
};
pub use config::BleConfig;
pub use gatt::{
    FLAG_NOTIFY, FLAG_READ, FLAG_WRITE, GattCharacteristic, GattNotification, GattService,
    GattWrite, MAX_CHARACTERISTICS, MAX_SERVICES, MAX_VALUE,
};
