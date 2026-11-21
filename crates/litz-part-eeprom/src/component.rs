//! The component glue: the `litz:part@0.2.0` exports over [`crate::eeprom`].

use crate::eeprom::Eeprom;
use litz_part_sdk::{
    AttributeDef, AttributeValue, Damage, Drive, FramebufferInfo, Guest, Level, PartInfo,
    PartState, Strength,
};
use std::cell::RefCell;
use std::collections::BTreeMap;

thread_local! {
    static EEPROM: RefCell<Eeprom> = RefCell::new(Eeprom::new());
    /// Values written with `set-attribute`, applied over the defaults when
    /// `attributes()` is next read.
    static ATTRIBUTES: RefCell<BTreeMap<String, AttributeValue>> = RefCell::new(BTreeMap::new());
}

fn with_eeprom<T>(f: impl FnOnce(&mut Eeprom) -> T) -> T {
    EEPROM.with(|part| f(&mut part.borrow_mut()))
}

fn defaults() -> Vec<AttributeDef> {
    vec![
        AttributeDef {
            name: "size".into(),
            description: "bytes of storage".into(),
            value: AttributeValue::Number(crate::eeprom::SIZE as f64),
        },
        AttributeDef {
            name: "page-size".into(),
            description: "bytes per write page".into(),
            value: AttributeValue::Number(crate::eeprom::PAGE as f64),
        },
        AttributeDef {
            name: "address".into(),
            description: "fixed 7-bit I2C address".into(),
            value: AttributeValue::Number(0x50 as f64),
        },
    ]
}

struct Part;

impl Guest for Part {
    fn info() -> PartInfo {
        // The host fixed the factory image before instantiation, so this is
        // the one moment it is worth reading.
        let rom = litz_part_sdk::host::rom();
        if !rom.is_empty() {
            with_eeprom(|part| part.load_rom(&rom));
        }
        litz_part_sdk::host::log(&format!(
            "24c02: {} bytes, ROM {} bytes",
            crate::eeprom::SIZE,
            rom.len()
        ));
        PartInfo {
            name: "24c02".into(),
            description: "256-byte I2C serial EEPROM with 8-byte pages".into(),
            pins: ["A0", "A1", "A2", "GND", "SCL", "SDA", "VCC"]
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
        }
    }

    fn set_pin(_pin: u16, _level: Level, _at_ns: u64) {}
    fn advance(_to_ns: u64) {}
    fn next_event() -> Option<u64> {
        None
    }

    fn drive(_pin: u16) -> Drive {
        Drive {
            level: Level::Floating,
            strength: Strength::Weak,
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
        let mut defs = defaults();
        ATTRIBUTES.with(|attributes| {
            for (name, value) in attributes.borrow().iter() {
                match defs.iter_mut().find(|def| def.name == *name) {
                    Some(def) => def.value = value.clone(),
                    None => defs.push(AttributeDef {
                        name: name.clone(),
                        description: String::new(),
                        value: value.clone(),
                    }),
                }
            }
        });
        defs
    }

    fn set_attribute(name: String, value: AttributeValue) {
        ATTRIBUTES.with(|attributes| {
            attributes.borrow_mut().insert(name, value);
        });
    }

    fn i2c_connect(addr: u16, read: bool) {
        if addr & 0x7f != 0x50 {
            litz_part_sdk::host::log(&format!("24c02: addressed at 0x{addr:02x}"));
        }
        with_eeprom(|part| part.connect(read));
    }

    fn i2c_read() -> u8 {
        with_eeprom(|part| part.read())
    }

    fn i2c_write(byte: u8) -> bool {
        with_eeprom(|part| part.write(byte))
    }

    fn i2c_stop() {
        with_eeprom(|part| part.stop());
    }

    fn spi_select(_selected: bool) {}

    fn spi_transfer(tx: Vec<u8>, rx_len: u32) -> Vec<u8> {
        with_eeprom(|part| part.spi_transfer(&tx, rx_len))
    }

    fn uart_receive(bytes: Vec<u8>, _at_ns: u64) {
        with_eeprom(|part| part.uart_receive(&bytes));
    }

    fn uart_poll() -> Vec<u8> {
        with_eeprom(|part| part.uart_poll())
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
