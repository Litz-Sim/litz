//! Catalog entries: what a `type` string in a diagram means.
//!
//! Two file kinds, one model. A **part** is a leaf: pins, attributes and a
//! simulation backend. A **board** is a composition: an MCU, the parts wired
//! to it on the PCB, and the header pins the schematic sees. Both resolve to
//! the same [`PartManifest`], so a diagram does not care which one it named.
//!
//! The catalog key (`part.id` / `board.id`) is the one identifier that must
//! never change. Everything else — names, attribute defaults, the simulation
//! model — may move under a version bump.
#![forbid(unsafe_code)]

use litz_diagram::{AttrDef, AttrType, AttrValue, Catalog, PartDef, PinDef, PinKind};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Where the behaviour of a part lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Simulation {
    /// A model compiled into the runner, selected by name.
    Builtin { engine: String },
    /// A WebAssembly module implementing the part ABI.
    Wasm { module: String, abi: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardInfo {
    /// `litz-esp` chip id, e.g. `esp32c3`. Exactly one of this and
    /// [`BoardInfo::simulator`] is set.
    pub chip: Option<String>,
    /// A microcontroller provided as a WebAssembly component instead of a
    /// backend compiled into the runner.
    pub simulator: Option<Simulator>,
    /// Flash size as written in the manifest, e.g. `4MB`. Parsed by the runner.
    pub flash_size: Option<String>,
    pub psram: bool,
    /// File name of the board's internal schematic, next to the manifest.
    pub schematic: Option<String>,
    pub header: Vec<BoardPin>,
}

/// A board whose MCU is a wasm component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Simulator {
    /// Contract version, from the manifest's `abi` (default `litz-mcu-1`).
    pub abi: String,
    /// Module path as written in the manifest, relative to the manifest's
    /// directory. Never absolute and never containing `..`.
    pub module: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardPin {
    pub name: String,
    pub target: BoardPinTarget,
    /// How many physical contacts carry this name (`GND.1`, `GND.2`).
    pub repeat: u32,
    pub label: Option<String>,
    /// Advertised alternate functions (`adc`, `pwm`, `i2c`, `uart`, `spi`).
    pub functions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardPinTarget {
    Gpio(u32),
    Fixed(FixedNet),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum FixedNet {
    #[serde(rename = "gnd")]
    Gnd,
    #[serde(rename = "3v3")]
    V33,
    #[serde(rename = "5v")]
    V5,
    #[serde(rename = "vin")]
    Vin,
}

/// The bus a part sits on, as declared by its manifest.
///
/// A bus device answers transactions rather than pin levels, so the catalog
/// records the one bus it belongs to. The physical pins are not here: they
/// come from the part's [`litz_diagram::PinKind`] bus pins and from the wires
/// that connect them to the board's chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusDecl {
    /// `[buses.i2c<n>] address = 0x..`: a target at a 7-bit address.
    I2c { controller: u8, address: u8 },
    /// `[buses.spi<n>] cs = "PIN"`: a target selected by one of its own pins.
    Spi { controller: u8, chip_select: String },
    /// `[buses.uart<n>] port = <n>`: the far end of a UART port.
    Uart { port: u8 },
}

/// One catalog entry, normalised.
#[derive(Debug, Clone)]
pub struct PartManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub license: Option<String>,
    pub pins: Vec<PinDef>,
    pub attrs: Vec<AttrDef>,
    pub simulation: Simulation,
    /// The bus this part is a device on, if any. A part is on at most one bus.
    pub buses: Vec<BusDecl>,
    pub board: Option<BoardInfo>,
    /// Where the manifest was loaded from; `None` for builtin entries. Board
    /// expansion resolves the onboard schematic relative to it.
    pub source: Option<PathBuf>,
}

impl PartDef for PartManifest {
    fn type_key(&self) -> &str {
        &self.id
    }

    fn pin(&self, name: &str) -> Option<&PinDef> {
        self.pins.iter().find(|p| p.name == name)
    }

    fn attr(&self, name: &str) -> Option<&AttrDef> {
        self.attrs.iter().find(|a| a.name == name)
    }

    fn pin_names(&self) -> Vec<&str> {
        self.pins.iter().map(|pin| pin.name.as_str()).collect()
    }
}

/// Every entry, keyed by its catalog id.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    entries: BTreeMap<String, PartManifest>,
    /// The manifest source text per id, kept so a lock file can hash exactly
    /// the bytes a run parsed rather than a checkout path.
    texts: BTreeMap<String, String>,
    /// Internal schematics the host supplied for boards whose manifest has no
    /// directory to resolve `schematic` against. A browser session uses this:
    /// the board arrives as manifest text plus the onboard JSON.
    schematics: BTreeMap<String, String>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The entries compiled into the crate: the core parts and the boards.
    pub fn builtin() -> Self {
        let mut r = Self::new();
        for (name, text) in [
            ("litz-led", include_str!("../builtin/parts/litz-led.toml")),
            (
                "litz-resistor",
                include_str!("../builtin/parts/litz-resistor.toml"),
            ),
            (
                "litz-pushbutton",
                include_str!("../builtin/parts/litz-pushbutton.toml"),
            ),
            (
                "litz-slide-switch",
                include_str!("../builtin/parts/litz-slide-switch.toml"),
            ),
            (
                "litz-7segment",
                include_str!("../builtin/parts/litz-7segment.toml"),
            ),
            (
                "litz-buzzer",
                include_str!("../builtin/parts/litz-buzzer.toml"),
            ),
            (
                "litz-servo",
                include_str!("../builtin/parts/litz-servo.toml"),
            ),
            (
                "litz-potentiometer",
                include_str!("../builtin/parts/litz-potentiometer.toml"),
            ),
            (
                "litz-bmp280",
                include_str!("../builtin/parts/litz-bmp280.toml"),
            ),
            (
                "litz-ssd1306",
                include_str!("../builtin/parts/litz-ssd1306.toml"),
            ),
            (
                "litz-xpt2046",
                include_str!("../builtin/parts/litz-xpt2046.toml"),
            ),
            (
                "litz-ds3231",
                include_str!("../builtin/parts/litz-ds3231.toml"),
            ),
            (
                "litz-pcf8574",
                include_str!("../builtin/parts/litz-pcf8574.toml"),
            ),
            (
                "litz-mpu6050",
                include_str!("../builtin/parts/litz-mpu6050.toml"),
            ),
            (
                "litz-mcp3008",
                include_str!("../builtin/parts/litz-mcp3008.toml"),
            ),
            (
                "litz-dht22",
                include_str!("../builtin/parts/litz-dht22.toml"),
            ),
            (
                "litz-hc-sr04",
                include_str!("../builtin/parts/litz-hc-sr04.toml"),
            ),
            (
                "litz-pir-motion-sensor",
                include_str!("../builtin/parts/litz-pir-motion-sensor.toml"),
            ),
            (
                "litz-photoresistor-sensor",
                include_str!("../builtin/parts/litz-photoresistor-sensor.toml"),
            ),
            (
                "litz-ntc-temperature-sensor",
                include_str!("../builtin/parts/litz-ntc-temperature-sensor.toml"),
            ),
            (
                "litz-rgb-led",
                include_str!("../builtin/parts/litz-rgb-led.toml"),
            ),
            (
                "litz-led-bar-graph",
                include_str!("../builtin/parts/litz-led-bar-graph.toml"),
            ),
            (
                "litz-relay",
                include_str!("../builtin/parts/litz-relay.toml"),
            ),
            (
                "litz-l293d",
                include_str!("../builtin/parts/litz-l293d.toml"),
            ),
            (
                "litz-74hc165",
                include_str!("../builtin/parts/litz-74hc165.toml"),
            ),
            (
                "litz-74hc138",
                include_str!("../builtin/parts/litz-74hc138.toml"),
            ),
            (
                "litz-capacitor",
                include_str!("../builtin/parts/litz-capacitor.toml"),
            ),
            (
                "litz-inductor",
                include_str!("../builtin/parts/litz-inductor.toml"),
            ),
            ("litz-bjt", include_str!("../builtin/parts/litz-bjt.toml")),
            (
                "litz-mosfet",
                include_str!("../builtin/parts/litz-mosfet.toml"),
            ),
            (
                "litz-opamp",
                include_str!("../builtin/parts/litz-opamp.toml"),
            ),
            // Batch 2: displays, input, output, sensors, logic and storage.
            (
                "litz-lcd1602",
                include_str!("../builtin/parts/litz-lcd1602.toml"),
            ),
            (
                "litz-lcd2004",
                include_str!("../builtin/parts/litz-lcd2004.toml"),
            ),
            (
                "litz-ili9341",
                include_str!("../builtin/parts/litz-ili9341.toml"),
            ),
            (
                "litz-epaper-1in54",
                include_str!("../builtin/parts/litz-epaper-1in54.toml"),
            ),
            (
                "litz-epaper-2in13",
                include_str!("../builtin/parts/litz-epaper-2in13.toml"),
            ),
            (
                "litz-dip-switch-8",
                include_str!("../builtin/parts/litz-dip-switch-8.toml"),
            ),
            (
                "litz-rotary-encoder",
                include_str!("../builtin/parts/litz-rotary-encoder.toml"),
            ),
            (
                "litz-analog-joystick",
                include_str!("../builtin/parts/litz-analog-joystick.toml"),
            ),
            (
                "litz-membrane-keypad",
                include_str!("../builtin/parts/litz-membrane-keypad.toml"),
            ),
            (
                "litz-pushbutton-6mm",
                include_str!("../builtin/parts/litz-pushbutton-6mm.toml"),
            ),
            (
                "litz-slide-potentiometer",
                include_str!("../builtin/parts/litz-slide-potentiometer.toml"),
            ),
            (
                "litz-tilt-switch",
                include_str!("../builtin/parts/litz-tilt-switch.toml"),
            ),
            (
                "litz-neopixel",
                include_str!("../builtin/parts/litz-neopixel.toml"),
            ),
            (
                "litz-neopixel-matrix",
                include_str!("../builtin/parts/litz-neopixel-matrix.toml"),
            ),
            (
                "litz-stepper-motor",
                include_str!("../builtin/parts/litz-stepper-motor.toml"),
            ),
            (
                "litz-ir-receiver",
                include_str!("../builtin/parts/litz-ir-receiver.toml"),
            ),
            (
                "litz-flame-sensor",
                include_str!("../builtin/parts/litz-flame-sensor.toml"),
            ),
            (
                "litz-gas-sensor",
                include_str!("../builtin/parts/litz-gas-sensor.toml"),
            ),
            (
                "litz-heart-beat-sensor",
                include_str!("../builtin/parts/litz-heart-beat-sensor.toml"),
            ),
            (
                "litz-hx711",
                include_str!("../builtin/parts/litz-hx711.toml"),
            ),
            (
                "litz-gps-neo6m",
                include_str!("../builtin/parts/litz-gps-neo6m.toml"),
            ),
            (
                "litz-pms5003",
                include_str!("../builtin/parts/litz-pms5003.toml"),
            ),
            (
                "litz-74hc164",
                include_str!("../builtin/parts/litz-74hc164.toml"),
            ),
            (
                "litz-74hc166",
                include_str!("../builtin/parts/litz-74hc166.toml"),
            ),
            (
                "litz-74hc573",
                include_str!("../builtin/parts/litz-74hc573.toml"),
            ),
            ("litz-4017", include_str!("../builtin/parts/litz-4017.toml")),
            ("litz-4051", include_str!("../builtin/parts/litz-4051.toml")),
            ("litz-4094", include_str!("../builtin/parts/litz-4094.toml")),
            (
                "litz-74hc00",
                include_str!("../builtin/parts/litz-74hc00.toml"),
            ),
            (
                "litz-74hc02",
                include_str!("../builtin/parts/litz-74hc02.toml"),
            ),
            (
                "litz-74hc04",
                include_str!("../builtin/parts/litz-74hc04.toml"),
            ),
            (
                "litz-74hc08",
                include_str!("../builtin/parts/litz-74hc08.toml"),
            ),
            (
                "litz-74hc14",
                include_str!("../builtin/parts/litz-74hc14.toml"),
            ),
            (
                "litz-74hc32",
                include_str!("../builtin/parts/litz-74hc32.toml"),
            ),
            (
                "litz-74hc86",
                include_str!("../builtin/parts/litz-74hc86.toml"),
            ),
            ("litz-and", include_str!("../builtin/parts/litz-and.toml")),
            ("litz-nand", include_str!("../builtin/parts/litz-nand.toml")),
            ("litz-or", include_str!("../builtin/parts/litz-or.toml")),
            ("litz-nor", include_str!("../builtin/parts/litz-nor.toml")),
            ("litz-xor", include_str!("../builtin/parts/litz-xor.toml")),
            ("litz-xnor", include_str!("../builtin/parts/litz-xnor.toml")),
            ("litz-not", include_str!("../builtin/parts/litz-not.toml")),
            (
                "litz-buffer",
                include_str!("../builtin/parts/litz-buffer.toml"),
            ),
            (
                "litz-microsd-card",
                include_str!("../builtin/parts/litz-microsd-card.toml"),
            ),
            (
                "litz-74hc595",
                include_str!("../builtin/parts/litz-74hc595.toml"),
            ),
            (
                "litz-24c02",
                include_str!("../builtin/parts/litz-24c02.toml"),
            ),
            (
                "litz-rgb-panel",
                include_str!("../builtin/parts/litz-rgb-panel.toml"),
            ),
            // Batch 3: the analog shelf and the remaining digital devices.
            (
                "litz-capacitor-electrolytic",
                include_str!("../builtin/parts/litz-capacitor-electrolytic.toml"),
            ),
            (
                "litz-diode",
                include_str!("../builtin/parts/litz-diode.toml"),
            ),
            (
                "litz-diode-1n4148",
                include_str!("../builtin/parts/litz-diode-1n4148.toml"),
            ),
            (
                "litz-diode-1n4007",
                include_str!("../builtin/parts/litz-diode-1n4007.toml"),
            ),
            (
                "litz-zener",
                include_str!("../builtin/parts/litz-zener.toml"),
            ),
            (
                "litz-voltage-regulator",
                include_str!("../builtin/parts/litz-voltage-regulator.toml"),
            ),
            (
                "litz-battery",
                include_str!("../builtin/parts/litz-battery.toml"),
            ),
            (
                "litz-power-supply",
                include_str!("../builtin/parts/litz-power-supply.toml"),
            ),
            (
                "litz-photodiode",
                include_str!("../builtin/parts/litz-photodiode.toml"),
            ),
            (
                "litz-optocoupler",
                include_str!("../builtin/parts/litz-optocoupler.toml"),
            ),
            (
                "litz-lcd1602-i2c",
                include_str!("../builtin/parts/litz-lcd1602-i2c.toml"),
            ),
            (
                "litz-lcd2004-i2c",
                include_str!("../builtin/parts/litz-lcd2004-i2c.toml"),
            ),
            (
                "litz-7segment-4digit",
                include_str!("../builtin/parts/litz-7segment-4digit.toml"),
            ),
            (
                "litz-led-ring",
                include_str!("../builtin/parts/litz-led-ring.toml"),
            ),
            (
                "litz-ds1307",
                include_str!("../builtin/parts/litz-ds1307.toml"),
            ),
            (
                "litz-rfid-rc522",
                include_str!("../builtin/parts/litz-rfid-rc522.toml"),
            ),
            (
                "litz-fingerprint",
                include_str!("../builtin/parts/litz-fingerprint.toml"),
            ),
            // Batch 4: protocol displays, analog modules, bus sensors and the
            // remaining small parts a board catalog is expected to carry.
            (
                "litz-tm1637-7segment",
                include_str!("../builtin/parts/litz-tm1637-7segment.toml"),
            ),
            (
                "litz-max7219-matrix",
                include_str!("../builtin/parts/litz-max7219-matrix.toml"),
            ),
            (
                "litz-nokia-5110-screen",
                include_str!("../builtin/parts/litz-nokia-5110-screen.toml"),
            ),
            (
                "litz-ky-040",
                include_str!("../builtin/parts/litz-ky-040.toml"),
            ),
            (
                "litz-reed-switch",
                include_str!("../builtin/parts/litz-reed-switch.toml"),
            ),
            (
                "litz-soil-moisture-sensor",
                include_str!("../builtin/parts/litz-soil-moisture-sensor.toml"),
            ),
            (
                "litz-sound-sensor",
                include_str!("../builtin/parts/litz-sound-sensor.toml"),
            ),
            (
                "litz-vibration-sensor",
                include_str!("../builtin/parts/litz-vibration-sensor.toml"),
            ),
            (
                "litz-rain-sensor",
                include_str!("../builtin/parts/litz-rain-sensor.toml"),
            ),
            (
                "litz-water-level-sensor",
                include_str!("../builtin/parts/litz-water-level-sensor.toml"),
            ),
            (
                "litz-dc-motor",
                include_str!("../builtin/parts/litz-dc-motor.toml"),
            ),
            (
                "litz-vibration-motor",
                include_str!("../builtin/parts/litz-vibration-motor.toml"),
            ),
            (
                "litz-traffic-light",
                include_str!("../builtin/parts/litz-traffic-light.toml"),
            ),
            (
                "litz-led-strip",
                include_str!("../builtin/parts/litz-led-strip.toml"),
            ),
            (
                "litz-led-matrix",
                include_str!("../builtin/parts/litz-led-matrix.toml"),
            ),
            (
                "litz-relay-module",
                include_str!("../builtin/parts/litz-relay-module.toml"),
            ),
            (
                "litz-clock-generator",
                include_str!("../builtin/parts/litz-clock-generator.toml"),
            ),
            (
                "litz-signal-generator",
                include_str!("../builtin/parts/litz-signal-generator.toml"),
            ),
            (
                "litz-a4988",
                include_str!("../builtin/parts/litz-a4988.toml"),
            ),
            (
                "litz-ks2e-m-dc5",
                include_str!("../builtin/parts/litz-ks2e-m-dc5.toml"),
            ),
            (
                "litz-pcf8591",
                include_str!("../builtin/parts/litz-pcf8591.toml"),
            ),
            (
                "litz-ads1115",
                include_str!("../builtin/parts/litz-ads1115.toml"),
            ),
            (
                "litz-ads1015",
                include_str!("../builtin/parts/litz-ads1015.toml"),
            ),
            (
                "litz-ds18b20",
                include_str!("../builtin/parts/litz-ds18b20.toml"),
            ),
            (
                "litz-sht31",
                include_str!("../builtin/parts/litz-sht31.toml"),
            ),
            (
                "litz-bh1750",
                include_str!("../builtin/parts/litz-bh1750.toml"),
            ),
            (
                "litz-tcs34725",
                include_str!("../builtin/parts/litz-tcs34725.toml"),
            ),
            (
                "litz-vl53l0x",
                include_str!("../builtin/parts/litz-vl53l0x.toml"),
            ),
            (
                "litz-pca9685",
                include_str!("../builtin/parts/litz-pca9685.toml"),
            ),
            (
                "litz-uln2003",
                include_str!("../builtin/parts/litz-uln2003.toml"),
            ),
            (
                "litz-mcp4725",
                include_str!("../builtin/parts/litz-mcp4725.toml"),
            ),
            (
                "litz-ina219",
                include_str!("../builtin/parts/litz-ina219.toml"),
            ),
            (
                "litz-bme280",
                include_str!("../builtin/parts/litz-bme280.toml"),
            ),
            (
                "litz-apds9960",
                include_str!("../builtin/parts/litz-apds9960.toml"),
            ),
            (
                "litz-74hc4067",
                include_str!("../builtin/parts/litz-74hc4067.toml"),
            ),
            (
                "litz-tsl2561",
                include_str!("../builtin/parts/litz-tsl2561.toml"),
            ),
            (
                "litz-veml7700",
                include_str!("../builtin/parts/litz-veml7700.toml"),
            ),
            (
                "litz-mlx90614",
                include_str!("../builtin/parts/litz-mlx90614.toml"),
            ),
            ("litz-fuse", include_str!("../builtin/parts/litz-fuse.toml")),
            (
                "litz-trimmer-potentiometer",
                include_str!("../builtin/parts/litz-trimmer-potentiometer.toml"),
            ),
            (
                "litz-dip-switch-4",
                include_str!("../builtin/parts/litz-dip-switch-4.toml"),
            ),
            (
                "litz-logic-level-converter",
                include_str!("../builtin/parts/litz-logic-level-converter.toml"),
            ),
            // Batch 5: packet radios. They are pin-level SPI slaves, not bus
            // devices: their engines decode SCK/MOSI and drive MISO themselves.
            (
                "litz-sx1276",
                include_str!("../builtin/parts/litz-sx1276.toml"),
            ),
            (
                "litz-nrf24l01",
                include_str!("../builtin/parts/litz-nrf24l01.toml"),
            ),
            // A router is a part with no wires: the radio backend reads its
            // configuration through `Board::network`.
            (
                "litz-wifi-router",
                include_str!("../builtin/parts/litz-wifi-router.toml"),
            ),
            // A beacon advertises into the air around it; the guests scan for
            // it through their own Bluetooth controller.
            (
                "litz-ble-beacon",
                include_str!("../builtin/parts/litz-ble-beacon.toml"),
            ),
            // An 802.15.4 transceiver puts Zigbee/Thread-shaped frames on the
            // shared 2.4 GHz medium; the MAC layer is the part's, not a chip's.
            (
                "litz-at86rf233",
                include_str!("../builtin/parts/litz-at86rf233.toml"),
            ),
            (
                "board-esp32-c3-devkitm-1",
                include_str!("../builtin/boards/board-esp32-c3-devkitm-1.toml"),
            ),
            (
                "board-litz-rv32",
                include_str!("../builtin/boards/board-litz-rv32.toml"),
            ),
            (
                "board-esp32-devkit-c-v4",
                include_str!("../builtin/boards/board-esp32-devkit-c-v4.toml"),
            ),
            (
                "board-esp32-s3-devkitc-1",
                include_str!("../builtin/boards/board-esp32-s3-devkitc-1.toml"),
            ),
            (
                "board-esp32-c6-devkitc-1",
                include_str!("../builtin/boards/board-esp32-c6-devkitc-1.toml"),
            ),
            (
                "board-esp32-p4-function-ev",
                include_str!("../builtin/boards/board-esp32-p4-function-ev.toml"),
            ),
            (
                "board-esp32-s31-coreboard",
                include_str!("../builtin/boards/board-esp32-s31-coreboard.toml"),
            ),
            (
                "board-arduino-uno",
                include_str!("../builtin/boards/board-arduino-uno.toml"),
            ),
            (
                "board-raspberry-pi-pico",
                include_str!("../builtin/boards/board-raspberry-pi-pico.toml"),
            ),
            (
                "litz-74hc139",
                include_str!("../builtin/parts/litz-74hc139.toml"),
            ),
            (
                "litz-74hc238",
                include_str!("../builtin/parts/litz-74hc238.toml"),
            ),
            (
                "litz-adxl345",
                include_str!("../builtin/parts/litz-adxl345.toml"),
            ),
            (
                "litz-bme680",
                include_str!("../builtin/parts/litz-bme680.toml"),
            ),
            (
                "litz-flex-sensor",
                include_str!("../builtin/parts/litz-flex-sensor.toml"),
            ),
            (
                "litz-force-sensor",
                include_str!("../builtin/parts/litz-force-sensor.toml"),
            ),
            (
                "litz-hall-sensor",
                include_str!("../builtin/parts/litz-hall-sensor.toml"),
            ),
            (
                "litz-ir-emitting-diode",
                include_str!("../builtin/parts/litz-ir-emitting-diode.toml"),
            ),
            (
                "litz-lm393",
                include_str!("../builtin/parts/litz-lm393.toml"),
            ),
            ("litz-lm75", include_str!("../builtin/parts/litz-lm75.toml")),
            (
                "litz-max7219-single",
                include_str!("../builtin/parts/litz-max7219-single.toml"),
            ),
            (
                "litz-mcp23017",
                include_str!("../builtin/parts/litz-mcp23017.toml"),
            ),
            (
                "litz-pushbutton-latching",
                include_str!("../builtin/parts/litz-pushbutton-latching.toml"),
            ),
            (
                "litz-pushbutton-momentary",
                include_str!("../builtin/parts/litz-pushbutton-momentary.toml"),
            ),
            (
                "litz-qmc5883l",
                include_str!("../builtin/parts/litz-qmc5883l.toml"),
            ),
            (
                "litz-sgp30",
                include_str!("../builtin/parts/litz-sgp30.toml"),
            ),
            (
                "litz-sh1106",
                include_str!("../builtin/parts/litz-sh1106.toml"),
            ),
            (
                "litz-st7735",
                include_str!("../builtin/parts/litz-st7735.toml"),
            ),
            (
                "litz-tca9548a",
                include_str!("../builtin/parts/litz-tca9548a.toml"),
            ),
            (
                "litz-tcrt5000",
                include_str!("../builtin/parts/litz-tcrt5000.toml"),
            ),
            (
                "litz-tmp102",
                include_str!("../builtin/parts/litz-tmp102.toml"),
            ),
            (
                "litz-vl53l1x",
                include_str!("../builtin/parts/litz-vl53l1x.toml"),
            ),
            (
                "litz-w25q32",
                include_str!("../builtin/parts/litz-w25q32.toml"),
            ),
            (
                "board-arduino-nano",
                include_str!("../builtin/boards/board-arduino-nano.toml"),
            ),
            (
                "board-arduino-pro-mini",
                include_str!("../builtin/boards/board-arduino-pro-mini.toml"),
            ),
            (
                "board-esp32-c6-devkitm-1",
                include_str!("../builtin/boards/board-esp32-c6-devkitm-1.toml"),
            ),
            (
                "board-esp32-s3-devkitc-1-n16r8",
                include_str!("../builtin/boards/board-esp32-s3-devkitc-1-n16r8.toml"),
            ),
        ] {
            r.insert_text(text)
                .unwrap_or_else(|e| panic!("builtin manifest {name} is invalid: {e}"));
        }
        r
    }

    /// Load every manifest in `dir`, sorted so a reload is deterministic.
    ///
    /// A part is normally one `.toml` file next to its module. A board is a
    /// directory of its own — `boards/<id>/board.toml` plus the simulator's
    /// source and WIT — so a one-level subdirectory is searched too. Nothing
    /// deeper is, which keeps a simulator crate's `Cargo.toml` from being
    /// mistaken for a manifest.
    pub fn load_dir(&mut self, dir: &Path) -> Result<(), CatalogError> {
        let entries = std::fs::read_dir(dir)
            .map_err(|source| CatalogError::Io {
                path: dir.to_path_buf(),
                source,
            })?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect::<Vec<_>>();
        let mut files = Vec::new();
        for path in entries {
            if path.is_dir() {
                if let Ok(children) = std::fs::read_dir(&path) {
                    files.extend(
                        children
                            .filter_map(|e| e.ok())
                            .map(|e| e.path())
                            .filter(|p| is_manifest(p)),
                    );
                }
            } else if is_manifest(&path) {
                files.push(path);
            }
        }
        files.sort();
        for file in files {
            let text = std::fs::read_to_string(&file).map_err(|source| CatalogError::Io {
                path: file.clone(),
                source,
            })?;
            let id = self.insert_text(&text).map_err(|e| CatalogError::Parse {
                path: file.clone(),
                message: e,
            })?;
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.source = Some(file.clone());
            }
        }
        Ok(())
    }

    pub fn insert(&mut self, manifest: PartManifest) {
        self.entries.insert(manifest.id.clone(), manifest);
    }

    /// Parse and insert one TOML file. The top-level table decides the kind:
    /// `[part]` or `[board]`.
    pub fn insert_text(&mut self, text: &str) -> Result<String, String> {
        let value: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        let manifest = if value.get("board").is_some() {
            let raw: BoardFile = value
                .try_into()
                .map_err(|e: toml::de::Error| e.to_string())?;
            raw.finish()?
        } else {
            let raw: PartFile = value
                .try_into()
                .map_err(|e: toml::de::Error| e.to_string())?;
            raw.finish()?
        };
        let id = manifest.id.clone();
        self.texts.insert(id.clone(), text.to_string());
        self.insert(manifest);
        Ok(id)
    }

    pub fn get(&self, id: &str) -> Option<&PartManifest> {
        self.entries.get(id)
    }

    /// `(type_key, version, manifest_text)` for every entry that was loaded
    /// from text, which is what a lock file records.
    pub fn manifest_texts(&self) -> Vec<(String, String, String)> {
        self.entries
            .iter()
            .filter_map(|(id, manifest)| {
                let text = self.texts.get(id)?;
                Some((id.clone(), manifest.version.clone(), text.clone()))
            })
            .collect()
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Supply the internal schematic for a board the host described itself.
    ///
    /// A manifest loaded from a directory resolves `schematic` against that
    /// directory; a manifest the host handed over has none, so the board's
    /// onboard JSON is provided alongside it.
    pub fn set_schematic(&mut self, board_id: &str, text: impl Into<String>) {
        self.schematics.insert(board_id.to_string(), text.into());
    }

    /// The internal schematic that ships with a builtin board, or the one the
    /// host supplied for that id.
    pub fn builtin_schematic(&self, board_id: &str) -> Option<&str> {
        if let Some(text) = self.schematics.get(board_id) {
            return Some(text);
        }
        match board_id {
            "board-esp32-c3-devkitm-1" => Some(include_str!(
                "../builtin/boards/board-esp32-c3-devkitm-1.onboard.json"
            )),
            "board-litz-rv32" => Some(include_str!(
                "../builtin/boards/board-litz-rv32.onboard.json"
            )),
            "board-arduino-uno" => Some(include_str!(
                "../builtin/boards/board-arduino-uno.onboard.json"
            )),
            "board-raspberry-pi-pico" => Some(include_str!(
                "../builtin/boards/board-raspberry-pi-pico.onboard.json"
            )),
            // The DevKit family shares one power-LED schematic; only the pin
            // map in the manifest differs.
            "board-esp32-devkit-c-v4"
            | "board-esp32-s3-devkitc-1"
            | "board-esp32-c6-devkitc-1"
            | "board-esp32-p4-function-ev"
            | "board-esp32-s31-coreboard" => {
                Some(include_str!("../builtin/boards/onboard-devkit.json"))
            }
            "board-arduino-nano" => Some(include_str!(
                "../builtin/boards/board-arduino-nano.onboard.json"
            )),
            "board-arduino-pro-mini" => Some(include_str!(
                "../builtin/boards/board-arduino-pro-mini.onboard.json"
            )),
            _ => None,
        }
    }

    /// A builtin board's simulator module, compiled into the binary.
    ///
    /// Builtin entries have no manifest path on disk, so a `module` name in
    /// their `[board.simulator]` resolves here instead of against a directory.
    pub fn builtin_module(&self, board_id: &str) -> Option<&'static [u8]> {
        match board_id {
            "board-litz-rv32" => Some(include_bytes!("../builtin/boards/litz-rv32.wasm")),
            _ => None,
        }
    }

    /// A builtin part's wasm module, compiled into the binary.
    ///
    /// The same rule as [`Registry::builtin_module`]: a builtin manifest has no
    /// directory, so its `module` resolves to these bytes rather than to a
    /// file. A part loaded from a directory resolves next to its manifest.
    pub fn builtin_part_module(&self, part_id: &str) -> Option<&'static [u8]> {
        match part_id {
            "litz-74hc595" => Some(include_bytes!("../builtin/parts/74hc595.wasm")),
            "litz-24c02" => Some(include_bytes!("../builtin/parts/24c02.wasm")),
            "litz-rgb-panel" => Some(include_bytes!("../builtin/parts/display.wasm")),
            _ => None,
        }
    }
}

impl Catalog for Registry {
    fn part(&self, type_key: &str) -> Option<&dyn PartDef> {
        self.entries.get(type_key).map(|p| p as &dyn PartDef)
    }

    fn ids(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }
}

// ---------------------------------------------------------------- raw forms

#[derive(Debug, Deserialize)]
struct PartFile {
    part: PartMeta,
    #[serde(default)]
    pins: BTreeMap<String, PinSpec>,
    #[serde(default)]
    attrs: BTreeMap<String, AttrSpec>,
    /// `[buses.i2c0]`, `[buses.spi0]` or `[buses.uart0]`. At most one entry:
    /// a device sits on one bus.
    #[serde(default)]
    buses: BTreeMap<String, BusSpec>,
    simulation: SimulationSpec,
}

/// The one field a bus section carries, checked against its kind below.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BusSpec {
    #[serde(default)]
    address: Option<u32>,
    #[serde(default)]
    cs: Option<String>,
    #[serde(default)]
    port: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct PartMeta {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    license: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PinSpec {
    kind: PinKind,
    #[serde(default)]
    number: Option<u32>,
    #[serde(default = "one")]
    repeat: u32,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AttrSpec {
    #[serde(rename = "type")]
    kind: AttrType,
    #[serde(default)]
    values: Vec<String>,
    #[serde(default)]
    default: Option<AttrValue>,
}

#[derive(Debug, Deserialize)]
struct SimulationSpec {
    backend: Backend,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    module: Option<String>,
    #[serde(default)]
    abi: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Backend {
    Builtin,
    Wasm,
}

#[derive(Debug, Deserialize)]
struct BoardFile {
    board: BoardMeta,
    #[serde(default)]
    pins: Vec<BoardPinSpec>,
}

#[derive(Debug, Deserialize)]
struct BoardMeta {
    id: String,
    name: String,
    /// A chip compiled into the runner, by backend name.
    #[serde(default)]
    chip: Option<String>,
    /// The same thing, provided as a WebAssembly component.
    #[serde(default)]
    simulator: Option<SimulatorSpec>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    flash_size: Option<String>,
    #[serde(default)]
    psram: bool,
    #[serde(default)]
    schematic: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SimulatorSpec {
    kind: SimulatorKindSpec,
    module: String,
    #[serde(default)]
    abi: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum SimulatorKindSpec {
    Wasm,
}

#[derive(Debug, Deserialize)]
struct BoardPinSpec {
    name: String,
    #[serde(default)]
    gpio: Option<u32>,
    #[serde(default)]
    fixed: Option<FixedNet>,
    #[serde(default = "one")]
    repeat: u32,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    functions: Vec<String>,
}

/// Whether a path is a catalog manifest rather than some other TOML file that
/// happens to sit beside one (a simulator crate's `Cargo.toml`).
fn is_manifest(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "toml")
        && path.file_name().is_none_or(|name| name != "Cargo.toml")
}

fn one() -> u32 {
    1
}

impl PartFile {
    fn finish(self) -> Result<PartManifest, String> {
        if self.part.id.trim().is_empty() {
            return Err("part.id is empty".into());
        }
        if self.pins.is_empty() {
            return Err(format!("part {:?} declares no pins", self.part.id));
        }

        let mut pins = Vec::with_capacity(self.pins.len());
        for (name, spec) in self.pins {
            if name.is_empty() || name.contains(':') {
                return Err(format!(
                    "part {:?} has an invalid pin name {name:?}",
                    self.part.id
                ));
            }
            if spec.repeat == 0 {
                return Err(format!(
                    "part {:?} pin {name:?} has repeat = 0",
                    self.part.id
                ));
            }
            pins.push(PinDef {
                name,
                kind: spec.kind,
                repeat: spec.repeat,
                number: spec.number,
                label: spec.label,
            });
        }

        let mut attrs = Vec::with_capacity(self.attrs.len());
        for (name, spec) in self.attrs {
            let attr = AttrDef {
                name: name.clone(),
                kind: spec.kind,
                values: spec.values,
                default: spec.default,
            };
            check_attr(&self.part.id, &attr)?;
            attrs.push(attr);
        }

        let simulation = match self.simulation.backend {
            Backend::Builtin => Simulation::Builtin {
                engine: self.simulation.engine.ok_or_else(|| {
                    format!("part {:?}: builtin backend needs `engine`", self.part.id)
                })?,
            },
            Backend::Wasm => {
                let module = self.simulation.module.ok_or_else(|| {
                    format!("part {:?}: wasm backend needs `module`", self.part.id)
                })?;
                let abi = self.simulation.abi.unwrap_or_else(|| WASM_PART_ABI.into());
                check_wasm_module(&self.part.id, &module, &abi)?;
                Simulation::Wasm { module, abi }
            }
        };

        let buses = parse_buses(&self.part.id, self.buses)?;

        Ok(PartManifest {
            id: self.part.id,
            name: self.part.name,
            version: self.part.version,
            description: self.part.description,
            license: self.part.license,
            pins,
            attrs,
            simulation,
            buses,
            board: None,
            source: None,
        })
    }
}

impl BoardFile {
    fn finish(self) -> Result<PartManifest, String> {
        if self.board.id.trim().is_empty() {
            return Err("board.id is empty".into());
        }
        if self.pins.is_empty() {
            return Err(format!("board {:?} declares no pins", self.board.id));
        }

        let mut seen = BTreeSet::new();
        let mut pins = Vec::with_capacity(self.pins.len());
        let mut header = Vec::with_capacity(self.pins.len());
        for spec in self.pins {
            if !seen.insert(spec.name.clone()) {
                return Err(format!(
                    "board {:?} declares pin {:?} twice",
                    self.board.id, spec.name
                ));
            }
            if spec.repeat == 0 {
                return Err(format!(
                    "board {:?} pin {:?} has repeat = 0",
                    self.board.id, spec.name
                ));
            }
            let (target, kind) = match (spec.gpio, spec.fixed) {
                (Some(gpio), None) => (BoardPinTarget::Gpio(gpio), PinKind::DigitalIo),
                (None, Some(fixed)) => (
                    BoardPinTarget::Fixed(fixed),
                    match fixed {
                        FixedNet::Gnd => PinKind::Ground,
                        _ => PinKind::Power,
                    },
                ),
                (Some(_), Some(_)) => {
                    return Err(format!(
                        "board {:?} pin {:?} sets both `gpio` and `fixed`",
                        self.board.id, spec.name
                    ));
                }
                (None, None) => {
                    return Err(format!(
                        "board {:?} pin {:?} sets neither `gpio` nor `fixed`",
                        self.board.id, spec.name
                    ));
                }
            };
            pins.push(PinDef {
                name: spec.name.clone(),
                kind,
                repeat: spec.repeat,
                number: None,
                label: spec.label.clone(),
            });
            header.push(BoardPin {
                name: spec.name,
                target,
                repeat: spec.repeat,
                label: spec.label,
                functions: spec.functions,
            });
        }

        let simulator = check_simulator(
            &self.board.id,
            self.board.chip.as_deref(),
            self.board.simulator,
        )?;

        Ok(PartManifest {
            id: self.board.id,
            name: self.board.name,
            version: self.board.version.unwrap_or_else(|| "0.0.0".into()),
            description: self.board.description,
            license: None,
            pins,
            attrs: Vec::new(),
            simulation: Simulation::Builtin {
                engine: "board".into(),
            },
            buses: Vec::new(),
            source: None,
            board: Some(BoardInfo {
                chip: self.board.chip,
                simulator,
                flash_size: self.board.flash_size,
                psram: self.board.psram,
                schematic: self.board.schematic,
                header,
            }),
        })
    }
}

/// Exactly one way to provide the MCU: a builtin chip or a wasm module.
///
/// The module path is checked here rather than at load time so a manifest that
/// escapes its directory fails when the catalog is read, on every machine,
/// instead of only when that one board is used.
fn check_simulator(
    board: &str,
    chip: Option<&str>,
    simulator: Option<SimulatorSpec>,
) -> Result<Option<Simulator>, String> {
    match (chip, simulator) {
        (Some(_), Some(_)) => Err(format!(
            "board-simulator: board {board:?} sets both `chip` and `[board.simulator]`; \
             a board has one MCU"
        )),
        (None, None) => Err(format!(
            "board-simulator: board {board:?} sets neither `chip` nor `[board.simulator]`; \
             a board needs an MCU"
        )),
        (Some(_), None) => Ok(None),
        (None, Some(spec)) => {
            let path = std::path::Path::new(&spec.module);
            if spec.module.trim().is_empty() {
                return Err(format!(
                    "board-simulator: board {board:?}: [board.simulator] module is empty"
                ));
            }
            if path.is_absolute() || spec.module.split('/').any(|part| part == "..") {
                return Err(format!(
                    "board-simulator: board {board:?}: [board.simulator] module {:?} must be \
                     relative to the manifest and stay inside its directory",
                    spec.module
                ));
            }
            let SimulatorKindSpec::Wasm = spec.kind;
            let abi = spec.abi.unwrap_or_else(|| "litz-mcu-1".into());
            if !WASM_MCU_ABIS.contains(&abi.as_str()) {
                return Err(format!(
                    "board-simulator: board {board:?} names abi {abi:?}; this build \
                     implements {}",
                    WASM_MCU_ABIS.join(", ")
                ));
            }
            Ok(Some(Simulator {
                abi,
                module: spec.module,
            }))
        }
    }
}

/// The MCU ABIs the runner can load. `litz-mcu-1` is the pin-and-console
/// contract; `litz-mcu-2` adds analog, buses and the simulated wireless
/// services. A manifest's `abi` defaults to the first.
const WASM_MCU_ABIS: &[&str] = &["litz-mcu-1", "litz-mcu-2"];

/// The part ABIs this build implements, from `litz-part-wit`.
///
/// `litz-part-2` is `litz:part@0.2.0`, the current contract. `litz-part-1` is
/// `litz:part@0.1.0`; the host still loads those modules, so a manifest written
/// before the bus/display additions keeps working. The current contract is the
/// default when a manifest omits `abi`.
///
/// It is duplicated as a literal rather than depending on the WIT crate: the
/// catalog is data-only and stays free of a wasm toolchain. A bump there moves
/// these strings with it.
const WASM_PART_ABIS: &[&str] = &["litz-part-1", "litz-part-2"];
const WASM_PART_ABI: &str = "litz-part-2";

/// Validate a wasm part's module reference when the catalog is read.
///
/// A module path is resolved next to the manifest, never against the current
/// directory, and one that is absolute or contains `..` is refused here so a
/// manifest cannot reach outside its catalog. The ABI is checked at the same
/// time: a module written against a different contract would otherwise fail
/// only when a diagram tried to run it.
fn check_wasm_module(part: &str, module: &str, abi: &str) -> Result<(), String> {
    if !WASM_PART_ABIS.contains(&abi) {
        return Err(format!(
            "part-abi: part {part:?} names abi {abi:?}; this build implements {}",
            WASM_PART_ABIS.join(", ")
        ));
    }
    if module.trim().is_empty() {
        return Err(format!(
            "part-module: part {part:?}: wasm backend module is empty"
        ));
    }
    let path = std::path::Path::new(module);
    if path.is_absolute() || module.split('/').any(|segment| segment == "..") {
        return Err(format!(
            "part-module: part {part:?}: module {module:?} must be relative to the \
             manifest and stay inside its directory"
        ));
    }
    Ok(())
}

/// Parse a part's single `[buses.<kind><n>]` section.
///
/// The section name carries the kind and controller index (`i2c0`, `spi1`,
/// `uart0`), and the body carries that kind's one field. Everything else is
/// refused here, when the catalog loads, rather than when a diagram is
/// flattened: a device that names no address or two buses is a manifest
/// mistake on every machine.
fn parse_buses(part: &str, specs: BTreeMap<String, BusSpec>) -> Result<Vec<BusDecl>, String> {
    let mut specs = specs.into_iter();
    let Some((name, spec)) = specs.next() else {
        return Ok(Vec::new());
    };
    if specs.next().is_some() {
        return Err(format!(
            "buses: part {part:?} declares more than one bus; a device sits on one bus"
        ));
    }
    let (kind, index) = split_bus_name(&name).ok_or_else(|| {
        format!("buses: part {part:?} bus {name:?} must be named i2c<n>, spi<n> or uart<n>")
    })?;
    let wrong_field =
        |field: &str| format!("buses: part {part:?} bus {name:?} does not take `{field}`");
    match kind {
        "i2c" => {
            if spec.cs.is_some() {
                return Err(wrong_field("cs"));
            }
            if spec.port.is_some() {
                return Err(wrong_field("port"));
            }
            let address = spec
                .address
                .ok_or_else(|| format!("buses: part {part:?} bus {name:?} needs `address`"))?;
            if !(0x08..=0x7f).contains(&address) {
                return Err(format!(
                    "buses: part {part:?} bus {name:?} address 0x{address:02x} is not a usable 7-bit address"
                ));
            }
            Ok(vec![BusDecl::I2c {
                controller: index,
                address: address as u8,
            }])
        }
        "spi" => {
            if spec.address.is_some() {
                return Err(wrong_field("address"));
            }
            if spec.port.is_some() {
                return Err(wrong_field("port"));
            }
            let chip_select = spec
                .cs
                .filter(|pin| !pin.trim().is_empty())
                .ok_or_else(|| {
                    format!(
                        "buses: part {part:?} bus {name:?} needs `cs`, the pin it is selected on"
                    )
                })?;
            Ok(vec![BusDecl::Spi {
                controller: index,
                chip_select,
            }])
        }
        "uart" => {
            if spec.address.is_some() {
                return Err(wrong_field("address"));
            }
            if spec.cs.is_some() {
                return Err(wrong_field("cs"));
            }
            let port = spec
                .port
                .ok_or_else(|| format!("buses: part {part:?} bus {name:?} needs `port`"))?;
            if port != u32::from(index) {
                return Err(format!(
                    "buses: part {part:?} bus {name:?} declares port {port}; the name says {index}"
                ));
            }
            Ok(vec![BusDecl::Uart { port: index }])
        }
        _ => unreachable!("split_bus_name accepts only i2c, spi and uart"),
    }
}

/// Split `i2c0` into `("i2c", 0)`, requiring a canonical name.
fn split_bus_name(name: &str) -> Option<(&str, u8)> {
    let kind = ["i2c", "spi", "uart"]
        .into_iter()
        .find(|kind| name.starts_with(kind))?;
    let digits = &name[kind.len()..];
    if digits.is_empty() {
        return None;
    }
    let index: u8 = digits.parse().ok()?;
    (format!("{kind}{index}") == name).then_some((kind, index))
}

fn check_attr(part: &str, attr: &AttrDef) -> Result<(), String> {
    if attr.name.is_empty() {
        return Err(format!("part {part:?} has an empty attribute name"));
    }
    match attr.kind {
        AttrType::Enum => {
            if attr.values.is_empty() {
                return Err(format!(
                    "part {part:?} attribute {:?} is an enum with no values",
                    attr.name
                ));
            }
            if let Some(AttrValue::String(default)) = &attr.default
                && !attr.values.contains(default)
            {
                return Err(format!(
                    "part {part:?} attribute {:?} defaults to {default:?}, which is not one of {:?}",
                    attr.name, attr.values
                ));
            }
        }
        _ if !attr.values.is_empty() => {
            return Err(format!(
                "part {part:?} attribute {:?} lists values but is not an enum",
                attr.name
            ));
        }
        _ => {}
    }
    if let Some(v) = &attr.default {
        let ok = matches!(
            (attr.kind, v),
            (AttrType::Bool, AttrValue::Bool(_))
                | (AttrType::Int, AttrValue::Int(_))
                | (AttrType::Float, AttrValue::Float(_))
                | (AttrType::Float, AttrValue::Int(_))
                | (AttrType::String, AttrValue::String(_))
                | (AttrType::Enum, AttrValue::String(_))
        );
        if !ok {
            return Err(format!(
                "part {part:?} attribute {:?} default does not match its type {:?}",
                attr.name, attr.kind
            ));
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_loads() {
        let r = Registry::builtin();
        assert!(r.get("litz-led").is_some());
        assert!(r.get("board-esp32-c3-devkitm-1").is_some());
        let board = r.get("board-esp32-c3-devkitm-1").unwrap();
        assert_eq!(
            board.board.as_ref().unwrap().chip.as_deref(),
            Some("esp32c3")
        );
    }

    #[test]
    fn rejects_builtin_backend_without_engine() {
        let mut r = Registry::new();
        let err = r
            .insert_text(
                r#"
                [part]
                id = "x"
                name = "X"
                version = "1.0.0"
                [pins.P]
                kind = "passive"
                [simulation]
                backend = "builtin"
                "#,
            )
            .unwrap_err();
        assert!(err.contains("needs `engine`"), "{err}");
    }

    #[test]
    fn rejects_board_pin_with_two_targets() {
        let mut r = Registry::new();
        let err = r
            .insert_text(
                r#"
                [board]
                id = "b"
                name = "B"
                chip = "esp32c3"
                [[pins]]
                name = "P"
                gpio = 1
                fixed = "gnd"
                "#,
            )
            .unwrap_err();
        assert!(err.contains("both `gpio` and `fixed`"), "{err}");
    }

    fn board_error(board: &str) -> String {
        Registry::new().insert_text(board).unwrap_err()
    }

    #[test]
    fn a_board_must_name_exactly_one_mcu() {
        let both = r#"
[board]
id = "b"
name = "B"
chip = "esp32c3"
[board.simulator]
kind = "wasm"
module = "chip.wasm"
[[pins]]
name = "GND"
fixed = "gnd"
"#;
        let error = board_error(both);
        assert!(error.contains("board-simulator"), "{error}");
        assert!(error.contains("both"), "{error}");

        let neither = r#"
[board]
id = "b"
name = "B"
[[pins]]
name = "GND"
fixed = "gnd"
"#;
        let error = board_error(neither);
        assert!(error.contains("board-simulator"), "{error}");
        assert!(error.contains("neither"), "{error}");
    }

    #[test]
    fn a_simulator_module_may_not_escape_the_manifest_directory() {
        for module in ["../chip.wasm", "/abs/chip.wasm", "a/../../b.wasm"] {
            let board = format!(
                r#"
[board]
id = "b"
name = "B"
[board.simulator]
kind = "wasm"
module = "{module}"
[[pins]]
name = "GND"
fixed = "gnd"
"#
            );
            let error = board_error(&board);
            assert!(error.contains("board-simulator"), "{module}: {error}");
        }
    }

    #[test]
    fn a_wasm_board_parses_with_its_default_abi() {
        let board = r#"
[board]
id = "b"
name = "B"
[board.simulator]
kind = "wasm"
module = "chip.wasm"
[[pins]]
name = "GPIO0"
gpio = 0
"#;
        let mut registry = Registry::new();
        let id = registry.insert_text(board).unwrap();
        let info = registry.get(&id).and_then(|m| m.board.as_ref()).unwrap();
        assert!(info.chip.is_none());
        let simulator = info.simulator.as_ref().unwrap();
        assert_eq!(simulator.module, "chip.wasm");
        assert_eq!(simulator.abi, "litz-mcu-1");
    }

    fn wasm_part(module: &str, abi: &str) -> String {
        format!(
            r#"
[part]
id = "x"
name = "X"
version = "1.0.0"
[pins.P]
kind = "digital-in"
[simulation]
backend = "wasm"
module = "{module}"
abi = "{abi}"
"#
        )
    }

    #[test]
    fn a_wasm_part_with_a_foreign_abi_is_rejected() {
        let error = Registry::new()
            .insert_text(&wasm_part("x.wasm", "litz-part-9"))
            .unwrap_err();
        assert!(error.contains("part-abi"), "{error}");
        assert!(error.contains("litz-part-1"), "{error}");
        assert!(error.contains("litz-part-2"), "{error}");
    }

    #[test]
    fn a_wasm_part_defaults_to_the_current_abi() {
        let text = r#"
[part]
id = "x2"
name = "X2"
version = "1.0.0"
[pins.P]
kind = "digital-in"
[simulation]
backend = "wasm"
module = "x.wasm"
"#;
        let registry = {
            let mut registry = Registry::new();
            registry.insert_text(text).unwrap();
            registry
        };
        match &registry.get("x2").unwrap().simulation {
            Simulation::Wasm { abi, .. } => assert_eq!(abi, "litz-part-2"),
            other => panic!("expected a wasm backend, got {other:?}"),
        }
    }

    #[test]
    fn a_wasm_part_module_may_not_escape_the_manifest_directory() {
        for module in ["../x.wasm", "/abs/x.wasm", "a/../../b.wasm"] {
            let error = Registry::new()
                .insert_text(&wasm_part(module, "litz-part-1"))
                .unwrap_err();
            assert!(error.contains("part-module"), "{module}: {error}");
        }
    }

    #[test]
    fn a_bmp280_declares_one_i2c_bus_at_0x76() {
        let registry = Registry::builtin();
        let manifest = registry
            .get("litz-bmp280")
            .expect("the part is in the builtin catalog");
        assert_eq!(
            manifest.buses,
            vec![BusDecl::I2c {
                controller: 0,
                address: 0x76
            }]
        );
        assert!(manifest.pins.iter().any(|pin| pin.kind == PinKind::I2cSda));
        assert!(manifest.pins.iter().any(|pin| pin.kind == PinKind::I2cScl));
    }

    #[test]
    fn a_part_with_two_bus_sections_is_rejected() {
        let text = r#"
[part]
id = "x"
name = "X"
version = "1.0.0"
[pins.P]
kind = "digital-in"
[buses.i2c0]
address = 0x76
[buses.spi0]
cs = "P"
[simulation]
backend = "builtin"
engine = "x"
"#;
        let error = Registry::new().insert_text(text).unwrap_err();
        assert!(error.contains("one bus"), "{error}");
    }

    #[test]
    fn a_bus_section_needs_the_field_its_kind_uses() {
        let no_address = r#"
[part]
id = "x"
name = "X"
version = "1.0.0"
[pins.P]
kind = "i2c-sda"
[buses.i2c0]
[simulation]
backend = "builtin"
engine = "x"
"#;
        let error = Registry::new().insert_text(no_address).unwrap_err();
        assert!(error.contains("needs `address`"), "{error}");

        let wrong_field = r#"
[part]
id = "x"
name = "X"
version = "1.0.0"
[pins.P]
kind = "spi-cs"
[buses.spi0]
cs = "P"
port = 0
[simulation]
backend = "builtin"
engine = "x"
"#;
        let error = Registry::new().insert_text(wrong_field).unwrap_err();
        assert!(error.contains("does not take `port`"), "{error}");
    }

    #[test]
    fn a_uart_bus_name_and_port_must_agree() {
        let text = r#"
[part]
id = "x"
name = "X"
version = "1.0.0"
[pins.TX]
kind = "uart-tx"
[buses.uart0]
port = 1
[simulation]
backend = "builtin"
engine = "x"
"#;
        let error = Registry::new().insert_text(text).unwrap_err();
        assert!(error.contains("the name says 0"), "{error}");
    }

    #[test]
    fn the_reference_i2c_parts_are_builtin_wasm_devices() {
        let registry = Registry::builtin();
        for (id, address) in [("litz-24c02", 0x50), ("litz-rgb-panel", 0x3c)] {
            let manifest = registry
                .get(id)
                .unwrap_or_else(|| panic!("{id} is missing"));
            assert_eq!(
                manifest.buses,
                vec![BusDecl::I2c {
                    controller: 0,
                    address
                }],
                "{id}"
            );
            match &manifest.simulation {
                Simulation::Wasm { abi, .. } => assert_eq!(abi, "litz-part-2", "{id}"),
                other => panic!("{id}: expected a wasm backend, got {other:?}"),
            }
            assert!(
                registry.builtin_part_module(id).is_some(),
                "{id} has no builtin module"
            );
        }
    }

    #[test]
    fn the_reference_shift_register_is_a_builtin_wasm_part() {
        let registry = Registry::builtin();
        let manifest = registry
            .get("litz-74hc595")
            .expect("the part is in the builtin catalog");
        match &manifest.simulation {
            Simulation::Wasm { module, abi } => {
                assert_eq!(module, "74hc595.wasm");
                assert_eq!(abi, "litz-part-1");
            }
            other => panic!("expected a wasm backend, got {other:?}"),
        }
        assert!(registry.builtin_part_module("litz-74hc595").is_some());
    }
}
