//! A minimal Litz part: pin `OUT` mirrors the level on pin `IN`.
//!
//! The catalog entry is `part.toml`; `build.sh` compiles this crate for
//! `wasm32-unknown-unknown` and wraps the module as the component the manifest
//! names. Pins are indices into the table in `info()`; the catalog orders pin
//! names alphabetically, so `IN` is 0 and `OUT` is 1.

use litz_part_sdk::{
    AttributeDef, AttributeValue, Damage, Drive, FramebufferInfo, Guest, Level, PartInfo,
    PartState, Strength,
};
use std::cell::RefCell;

thread_local! {
    /// The last level seen on `IN`. A component instance has one thread, so
    /// two instances in one process would each keep their own.
    static INPUT: RefCell<Level> = const { RefCell::new(Level::Floating) };
}

struct Mirror;

impl Guest for Mirror {
    fn info() -> PartInfo {
        PartInfo {
            name: "example-mirror".into(),
            description: "an output pin that follows an input pin".into(),
            pins: ["IN", "OUT"]
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
        }
    }

    fn set_pin(pin: u16, level: Level, _at_ns: u64) {
        if pin == 0 {
            INPUT.with(|input| *input.borrow_mut() = level);
        }
    }

    fn advance(_to_ns: u64) {}

    fn next_event() -> Option<u64> {
        None
    }

    fn drive(pin: u16) -> Drive {
        match (pin, INPUT.with(|input| *input.borrow())) {
            (1, Level::High) => Drive {
                level: Level::High,
                strength: Strength::Strong,
            },
            (1, Level::Low) => Drive {
                level: Level::Low,
                strength: Strength::Strong,
            },
            _ => Drive {
                level: Level::Floating,
                strength: Strength::Weak,
            },
        }
    }

    fn state() -> PartState {
        PartState::None
    }

    fn press(_pressed: bool) -> bool {
        false
    }

    fn set_control(_permille: u16) -> bool {
        false
    }

    fn analog(_pin: u16) -> Option<f32> {
        None
    }

    fn attributes() -> Vec<AttributeDef> {
        Vec::new()
    }

    fn set_attribute(_name: String, _value: AttributeValue) {}

    fn i2c_connect(_addr: u16, _read: bool) {}

    fn i2c_read() -> u8 {
        0
    }

    fn i2c_write(_byte: u8) -> bool {
        false
    }

    fn i2c_stop() {}

    fn spi_select(_selected: bool) {}

    fn spi_transfer(tx: Vec<u8>, rx_len: u32) -> Vec<u8> {
        let mut out = tx;
        out.resize(rx_len as usize, 0);
        out
    }

    fn uart_receive(_bytes: Vec<u8>, _at_ns: u64) {}

    fn uart_poll() -> Vec<u8> {
        Vec::new()
    }

    fn framebuffer() -> Option<FramebufferInfo> {
        None
    }

    fn draw(_offset: u32, _pixels: Vec<u8>) {}

    fn damage() -> Option<Damage> {
        None
    }
}

litz_part_sdk::export!(Mirror with_types_in litz_part_sdk::abi);
