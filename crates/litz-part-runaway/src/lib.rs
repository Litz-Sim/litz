//! Test fixture: a part whose `drive` never returns.
//!
//! The host must turn the infinite loop into a fuel trap rather than hang.
//! Nothing else about the part matters; it exists only for
//! `litz-part-wasm`'s sandbox test, and is not part of the catalog.

#![deny(unsafe_code)]

#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod component {
    use litz_part_sdk::{
        AttributeDef, AttributeValue, Damage, Drive, FramebufferInfo, Guest, Level, PartInfo,
        PartState,
    };
    use std::hint::black_box;

    struct Part;

    impl Guest for Part {
        fn info() -> PartInfo {
            PartInfo {
                name: "runaway".into(),
                description: "never returns from drive()".into(),
                pins: vec!["P".into()],
            }
        }

        fn set_pin(_pin: u16, _level: Level, _at_ns: u64) {}
        fn advance(_to_ns: u64) {}
        fn next_event() -> Option<u64> {
            None
        }

        fn drive(_pin: u16) -> Drive {
            loop {
                black_box(0u32);
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
        fn spi_transfer(tx: Vec<u8>, _rx_len: u32) -> Vec<u8> {
            tx
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
}
