//! The component glue: the `litz:part@0.2.0` exports over [`crate::panel`].
//!
//! The I2C path is a small framebuffer protocol: a write phase's first byte is
//! a byte offset into the panel, later bytes are RGBA data. A controller that
//! only uses `draw()` never needs it, but it makes the module a real I2C
//! device rather than a display that happens to expose a framebuffer.

use crate::panel::{Panel, SIDE};
use litz_part_sdk::{
    AttributeDef, AttributeValue, Damage, Drive, FramebufferInfo, Guest, Level, PartInfo,
    PartState, Strength,
};
use std::cell::RefCell;
use std::collections::BTreeMap;

thread_local! {
    static PANEL: RefCell<Panel> = RefCell::new(Panel::new());
    static ATTRIBUTES: RefCell<BTreeMap<String, AttributeValue>> = RefCell::new(BTreeMap::new());
    /// `None` before a write phase's offset byte; `Some(offset)` after it.
    static CURSOR: RefCell<Option<u32>> = const { RefCell::new(None) };
}

fn with_panel<T>(f: impl FnOnce(&mut Panel) -> T) -> T {
    PANEL.with(|panel| f(&mut panel.borrow_mut()))
}

fn defaults() -> Vec<AttributeDef> {
    vec![
        AttributeDef {
            name: "width".into(),
            description: "pixels across".into(),
            value: AttributeValue::Number(SIDE as f64),
        },
        AttributeDef {
            name: "height".into(),
            description: "pixels down".into(),
            value: AttributeValue::Number(SIDE as f64),
        },
        AttributeDef {
            name: "format".into(),
            description: "pixel layout".into(),
            value: AttributeValue::Text("rgba8888".into()),
        },
    ]
}

struct Part;

impl Guest for Part {
    fn info() -> PartInfo {
        litz_part_sdk::host::log(&format!("rgb-panel: {SIDE}x{SIDE} RGBA8888"));
        PartInfo {
            name: "rgb-panel".into(),
            description: "8x8 RGBA framebuffer display".into(),
            pins: ["GND", "SCL", "SDA", "VCC"]
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

    fn i2c_connect(_addr: u16, read: bool) {
        if !read {
            CURSOR.with(|cursor| *cursor.borrow_mut() = None);
        }
    }

    fn i2c_read() -> u8 {
        0
    }

    fn i2c_write(byte: u8) -> bool {
        // The first byte of a write phase is a byte offset; the rest are
        // pixel data at that offset. Keeping the cursor here rather than in
        // the panel leaves `draw()` as the host's direct path.
        CURSOR.with(|cursor| {
            let mut cursor = cursor.borrow_mut();
            match *cursor {
                None => *cursor = Some(byte as u32),
                Some(offset) => {
                    with_panel(|panel| panel.draw(offset, &[byte]));
                    *cursor = Some(offset + 1);
                }
            }
        });
        true
    }

    fn i2c_stop() {
        CURSOR.with(|cursor| *cursor.borrow_mut() = None);
    }

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
        Some(FramebufferInfo {
            width: with_panel(|panel| panel.width()),
            height: with_panel(|panel| panel.height()),
        })
    }

    fn draw(offset: u32, pixels: Vec<u8>) {
        with_panel(|panel| panel.draw(offset, &pixels));
    }

    fn damage() -> Option<Damage> {
        with_panel(|panel| panel.take_damage()).map(|rect| Damage {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        })
    }
}

litz_part_sdk::export!(Part with_types_in litz_part_sdk::abi);
