//! Turning a diagram into something that can be run.
//!
//! Two passes:
//!
//! * [`flatten`] expands development boards and rewrites names, producing one
//!   flat part list and one set of nets. No hierarchy survives.
//! * [`runtime::Board`] owns the part instances, resolves each net's level
//!   from the drives on it, and reports what the chip should see on its input
//!   pads.
//!
//! The crate knows nothing about ESP32 or any other backend. An adapter feeds
//! it `set_chip_output` and forwards `take_chip_inputs`.
//!
//! ```
//! use litz_catalog::Registry;
//! use litz_diagram::Circuit;
//! use litz_board::{Board, FlatBoard};
//!
//! let registry = Registry::builtin();
//! let circuit = Circuit::from_json(r#"{
//!   "parts": [
//!     {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
//!     {"id": "r1", "type": "litz-resistor", "attrs": {"value": "220"}},
//!     {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
//!   ],
//!   "connections": [
//!     ["esp:GPIO8", "r1:1", "", []],
//!     ["r1:2", "led1:A", "", []],
//!     ["led1:C", "esp:GND.1", "", []]
//!   ]
//! }"#).unwrap();
//! let mut board = Board::new(FlatBoard::build(&circuit, &registry).unwrap()).unwrap();
//! board.set_chip_output(8, litz_core::Drive::strong(litz_core::Level::High), 0);
//! assert_eq!(board.state("led1"), Some(litz_core::PartState::Led { lit: true }));
//! ```
// The host bridge to JavaScript components declares `extern "C"` imports on
// wasm32, which edition 2024 counts as unsafe code. It is the one module that
// needs it; everything else stays safe.
#![deny(unsafe_code)]

pub mod analog;
pub mod ble_parts;
pub mod bus;
pub mod bus_parts;
pub mod chip;
pub mod devices;
pub mod displays;
pub mod flatten;
/// The JavaScript host bridge for custom parts and chips (wasm32 builds).
#[cfg(target_arch = "wasm32")]
pub mod host;
pub mod ieee802154_parts;
pub mod inputs;
pub mod logic;
pub mod module_parts;
pub mod network_parts;
pub mod outputs;
pub mod parts;
pub mod pin_parts;
pub mod pixels;
pub mod potentiometer;
pub mod probe;
pub mod protocol_displays;
pub mod radio_parts;
pub mod runtime;
pub mod segments;
pub mod sensor_ics;
pub mod sensors;
pub mod signal_gen;
pub mod storage;
pub mod switch;
pub mod timing;
pub mod touch;
pub mod waveform;

pub use analog::{AnalogConfig, AnalogSolution};
pub use bus::{Buses, I2c, I2cDevice, Spi, SpiDevice, Uart, UartPeer};
pub use bus_parts::{DateTime, Ds3231, Mcp3008, Mpu6050, Pcf8574, Ssd1306};
pub use chip::{Chip, ChipDebug, ChipInfo, FlashLayout, ImageKind, Input, RunResult, Stop};
pub use devices::Bmp280;
pub use displays::{Epd, EpdSize, Hd44780, Ili9341, Lcd1602, Lcd2004};
pub use flatten::{
    Device, Endpoint, Engine, FlatBoard, FlatBuses, FlatPart, I2cBus, Rail, SpiBus, UartBus,
    WasmModule,
};
pub use inputs::{AnalogJoystick, DipSwitch, MembraneKeypad, RotaryEncoder, TiltSwitch};
pub use litz_ble::{Beacon, BleConfig};
pub use litz_radio::{Packet, RadioConfig, RadioHub, RadioHubHandle, RadioId, RadioStats};
pub use logic::{Cd4017, Cd4051, Cd4094, Gate, GateFn, Hc164, Hc573};
pub use outputs::{NeoPixel, Stepper};
pub use pin_parts::{
    Bjt, Capacitor, Dht22, DhtReading, Hc138, Hc165, HcSr04, Inductor, L293d, LedBarGraph, Mosfet,
    NtcSensor, Opamp, Photoresistor, PirMotionSensor, Relay, RgbLed,
};
pub use pixels::{Diff, Framebuffer, PixelFormat, compare_framebuffers};
pub use probe::{Channel, Measurements, Probe, Series};
pub use radio_parts::{Nrf24l01, Sx1276};
pub use runtime::{
    ANALOG_DEFAULT_DT_NS, Board, CONNECTION_DELAY_NS, CONNECTION_EVENT_NS, CONNECTION_INTERVAL_NS,
    Conflict,
};
pub use sensors::{
    FlameSensor, GasSensor, GpsNeo6m, HeartBeatSensor, Hx711, IrReceiver, NecFrame, Pms5003,
};
pub use signal_gen::{SignalGenerator, Wave};
pub use storage::MicroSd;
pub use touch::Xpt2046;

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("the diagram has errors:\n{}", flatten::render(.0))]
    Diagnostics(Vec<litz_diagram::Diagnostic>),
    #[error("{0}")]
    Message(String),
    #[error("{0}")]
    Diagram(#[from] litz_diagram::DiagramError),
}
