//! The component glue: the `litz:part` exports over [`crate::shift`].
//!
//! Compiled only for `wasm32-unknown-unknown`, because the generated exports
//! name symbols that exist inside a component and nowhere else. The register
//! underneath is testable on the host without this file.

mod bindings {
    wit_bindgen::generate!({
        path: "wit-legacy",
        world: "component",
    });
}

use crate::shift::{Level, PINS, ShiftRegister};
use bindings::exports::litz::part::part::Guest;
use bindings::litz::part::types::{Drive, Level as WitLevel, PartInfo, PartState, Strength};
use std::cell::RefCell;

thread_local! {
    /// One register per component instance. `thread_local` rather than a
    /// `static mut` because a component has one thread and a second instance
    /// in the same thread would get its own, which is what a host running two
    /// parts would want.
    static REGISTER: RefCell<ShiftRegister> = RefCell::new(ShiftRegister::new());
}

struct Part;

fn with_register<T>(f: impl FnOnce(&mut ShiftRegister) -> T) -> T {
    REGISTER.with(|register| f(&mut register.borrow_mut()))
}

fn level_of(level: WitLevel) -> Level {
    match level {
        WitLevel::Low => Level::Low,
        WitLevel::High => Level::High,
        WitLevel::Floating => Level::Floating,
    }
}

impl Guest for Part {
    fn info() -> PartInfo {
        PartInfo {
            name: "74hc595".into(),
            description: "8-bit serial-in/parallel-out shift register".into(),
            pins: PINS.iter().map(|name| (*name).to_string()).collect(),
        }
    }

    fn set_pin(pin: u16, level: WitLevel, at_ns: u64) {
        with_register(|register| register.set_pin(pin, level_of(level), at_ns));
    }

    fn advance(_to_ns: u64) {}

    fn next_event() -> Option<u64> {
        None
    }

    fn drive(pin: u16) -> Drive {
        match with_register(|register| register.output(pin)) {
            Some(Level::High) => Drive {
                level: WitLevel::High,
                strength: Strength::Strong,
            },
            Some(Level::Low) => Drive {
                level: WitLevel::Low,
                strength: Strength::Strong,
            },
            // Not a Q pin, or OE has released the outputs.
            _ => Drive {
                level: WitLevel::Floating,
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
}

bindings::export!(Part with_types_in bindings);
