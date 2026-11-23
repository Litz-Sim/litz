//! The component glue: the `litz:part@0.2.0` exports over [`crate::psg`].
//!
//! The chip's logic is in `psg.rs` and is testable on the host. This file only
//! maps the contract's types onto it. A guest that does not implement the bus,
//! attribute or framebuffer groups answers them with their empty defaults.

use crate::psg::{AUDIO, Level, PINS, Sn76489};
use litz_part_sdk::{
    AttributeDef, AttributeValue, Damage, Drive, FramebufferInfo, Guest, Level as WitLevel,
    PartInfo, PartState, Strength, Tone,
};
use std::cell::RefCell;

thread_local! {
    /// One chip per component instance. `thread_local` gives each instance its
    /// own state without `unsafe`, which is the shape the 74hc595 reference
    /// part uses.
    static CHIP: RefCell<Sn76489> = RefCell::new(Sn76489::new());
}

fn with_chip<T>(f: impl FnOnce(&mut Sn76489) -> T) -> T {
    CHIP.with(|chip| f(&mut chip.borrow_mut()))
}

fn level_of(level: WitLevel) -> Level {
    match level {
        WitLevel::Low => Level::Low,
        WitLevel::High => Level::High,
        WitLevel::Floating => Level::Floating,
    }
}

struct Part;

impl Guest for Part {
    fn info() -> PartInfo {
        PartInfo {
            name: "sn76489".into(),
            description: "Three-tone plus noise programmable sound generator".into(),
            pins: PINS.iter().map(|name| (*name).to_string()).collect(),
        }
    }

    fn set_pin(pin: u16, level: WitLevel, at_ns: u64) {
        with_chip(|chip| chip.set_pin(pin, level_of(level), at_ns));
    }

    fn advance(to_ns: u64) {
        with_chip(|chip| chip.advance(to_ns));
    }

    fn next_event() -> Option<u64> {
        with_chip(|chip| chip.next_event())
    }

    fn drive(pin: u16) -> Drive {
        if pin != AUDIO {
            return Drive {
                level: WitLevel::Floating,
                strength: Strength::Weak,
            };
        }
        let high = with_chip(|chip| chip.output_high());
        Drive {
            level: if high { WitLevel::High } else { WitLevel::Low },
            strength: Strength::Strong,
        }
    }

    fn state() -> PartState {
        let (sounding, hz) = with_chip(|chip| chip.state());
        PartState::Tone(Tone { sounding, hz })
    }

    fn press(_pressed: bool) -> bool {
        false
    }

    fn set_control(_permille: u16) -> bool {
        false
    }

    fn analog(pin: u16) -> Option<f32> {
        (pin == AUDIO).then(|| with_chip(|chip| chip.analog_volts()))
    }

    fn attributes() -> Vec<AttributeDef> {
        // The clock is a constant in `psg.rs`. An attribute would offer a
        // setting the guest has no way to apply, so there is none.
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
    fn spi_transfer(_tx: Vec<u8>, rx_len: u32) -> Vec<u8> {
        vec![0xff; rx_len as usize]
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

litz_part_sdk::export!(Part with_types_in litz_part_sdk::abi);
