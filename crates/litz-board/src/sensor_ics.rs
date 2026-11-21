//! I2C and 1-Wire sensor and converter ICs.
//!
//! Every I2C model here is a register machine: the address phase selects a
//! device, the first byte of a write phase selects a register or command, and a
//! read phase returns the register file at the pointer. That is the shape the
//! real parts present to a driver, so a library written for the real part
//! reads the values this board puts in the registers. The DS18B20 is the one
//! pin-level model: it answers the 1-Wire reset, write and read slots on its
//! data line, all timed from the simulated [`Tick`].

use crate::BuildError;
use crate::bus::I2cDevice;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};
use std::any::Any;
use std::collections::BTreeMap;

fn index(part: &FlatPart, name: &str) -> Result<u16, BuildError> {
    part.pins
        .iter()
        .position(|candidate| candidate == name)
        .map(|i| i as u16)
        .ok_or_else(|| BuildError::Message(format!("part {:?} has no {name:?} pin", part.id)))
}

fn attr_f32(attrs: &BTreeMap<String, String>, name: &str, default: f32) -> f32 {
    attrs
        .get(name)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(default)
}

fn attr_u16(attrs: &BTreeMap<String, String>, name: &str, default: u16) -> u16 {
    attrs
        .get(name)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(default)
}

fn be16(high: u8, low: u8) -> u16 {
    u16::from(high) << 8 | u16::from(low)
}

fn le16(low: u8, high: u8) -> u16 {
    u16::from(high) << 8 | u16::from(low)
}

/// The Sensirion CRC-8 used by SHT3x, SGP30 and SCD4x: polynomial 0x31,
/// initial value 0xFF, most significant bit first.
fn crc8_sensirion(bytes: &[u8]) -> u8 {
    let mut crc = 0xffu8;
    for &byte in bytes {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x31
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// The Dallas/Maxim CRC-8 used by the DS18B20 scratchpad: polynomial 0x31,
/// reflected, initial value 0.
fn crc8_dallas(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in bytes {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8c
            } else {
                crc >> 1
            };
        }
    }
    crc
}

// ------------------------------------------------------------------ PCF8591

/// A PCF8591 8-bit ADC with one DAC, at 0x48.
///
/// The control register picks the input channel and the auto-increment mode;
/// a read returns the previous conversion first, which is why a driver reads
/// twice. The DAC byte is latched even though this model does not drive an
/// analog pin from it.
pub struct Pcf8591 {
    channels: [f32; 4],
    reference: f32,
    control: u8,
    pointer: usize,
    dac: u8,
    latch: u8,
    first_write: bool,
    reading: bool,
}

impl Pcf8591 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let mut channels = [0.0; 4];
        for (number, channel) in channels.iter_mut().enumerate() {
            *channel = attr_f32(attrs, &format!("ain{number}"), 0.0);
        }
        Self {
            channels,
            reference: attr_f32(attrs, "vref", 3.3).max(0.0),
            control: 0,
            pointer: 0,
            dac: 0,
            latch: 0,
            first_write: true,
            reading: false,
        }
    }

    pub fn channel_volts(&self, channel: usize) -> Option<f32> {
        self.channels.get(channel).copied()
    }

    pub fn channel_raw(&self, channel: usize) -> u8 {
        let volts = self.channels.get(channel).copied().unwrap_or(0.0);
        let scale = if self.reference > 0.0 {
            255.0 / self.reference
        } else {
            0.0
        };
        (volts * scale).round().clamp(0.0, 255.0) as u8
    }

    pub fn dac(&self) -> u8 {
        self.dac
    }

    pub fn pointer(&self) -> usize {
        self.pointer
    }
}

impl I2cDevice for Pcf8591 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if !read {
            self.first_write = true;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let value = self.latch;
        self.latch = self.channel_raw(self.pointer);
        if self.control & 0x04 != 0 {
            self.pointer = (self.pointer + 1) & 3;
        }
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.control = byte;
            self.pointer = usize::from(byte & 0x03);
            self.first_write = false;
        } else {
            self.dac = byte;
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ ADS111x

/// The ADS1115 and ADS1015 4-channel delta-sigma ADCs, at 0x48.
///
/// A write is a register pointer followed by 16-bit big-endian words. Writing
/// the config register with the start bit set runs a conversion, whose result
/// lands in the conversion register. `bits` is 16 for the ADS1115 and 12 for
/// the ADS1015, whose result is left-aligned in the same 16-bit register.
pub struct Ads111x {
    bits: u8,
    inputs: [f32; 4],
    registers: [u16; 4],
    pointer: usize,
    config: u16,
    conversion: u16,
    first_write: bool,
    high: Option<u8>,
    reading: bool,
    read_low: bool,
}

impl Ads111x {
    pub fn from_part(part: &FlatPart) -> Self {
        let bits = if part.type_key == "litz-ads1015" {
            12
        } else {
            16
        };
        Self::from_attrs(&part.attrs, bits)
    }

    pub fn from_attrs(attrs: &BTreeMap<String, String>, bits: u8) -> Self {
        let mut inputs = [0.0; 4];
        for (number, input) in inputs.iter_mut().enumerate() {
            *input = attr_f32(attrs, &format!("a{number}"), 0.0);
        }
        let mut device = Self {
            bits,
            inputs,
            registers: [0; 4],
            pointer: 0,
            config: 0x8583,
            conversion: 0,
            first_write: true,
            high: None,
            reading: false,
            read_low: false,
        };
        device.registers[1] = device.config;
        device.convert();
        device
    }

    pub fn conversion_raw(&self) -> u16 {
        self.conversion
    }

    pub fn config(&self) -> u16 {
        self.config
    }

    pub fn bits(&self) -> u8 {
        self.bits
    }

    fn convert(&mut self) {
        let mux = usize::from((self.config >> 12) & 0x07);
        let gain = usize::from((self.config >> 9) & 0x07);
        let full_scale = [6.144, 4.096, 2.048, 1.024, 0.512, 0.256, 0.256, 0.256][gain];
        let volts = if mux >= 4 {
            self.inputs[mux - 4]
        } else {
            match mux {
                0 => self.inputs[0] - self.inputs[1],
                1 => self.inputs[0] - self.inputs[3],
                2 => self.inputs[1] - self.inputs[3],
                _ => self.inputs[2] - self.inputs[3],
            }
        };
        let full = if self.bits == 16 { 32_767.0 } else { 2_047.0 };
        let raw = (volts / full_scale * full).round().clamp(-full, full) as i32;
        let word = if self.bits == 16 {
            raw as i16 as u16
        } else {
            ((raw as i16) << 4) as u16
        };
        self.registers[0] = word;
        self.conversion = word;
    }

    fn write_word(&mut self, word: u16) {
        self.registers[self.pointer & 3] = word;
        if self.pointer == 1 {
            self.config = word;
            if word & 0x8000 != 0 {
                self.convert();
            }
        }
        self.pointer = (self.pointer + 1) & 3;
    }
}

impl I2cDevice for Ads111x {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if read {
            self.read_low = false;
        } else {
            self.first_write = true;
            self.high = None;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let word = self.registers[self.pointer & 3];
        if !self.read_low {
            self.read_low = true;
            (word >> 8) as u8
        } else {
            self.read_low = false;
            self.pointer = (self.pointer + 1) & 3;
            word as u8
        }
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = usize::from(byte & 0x03);
            self.first_write = false;
            self.high = None;
        } else if let Some(high) = self.high.take() {
            self.write_word(be16(high, byte));
        } else {
            self.high = Some(byte);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
        self.high = None;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ SHT31

/// A Sensirion SHT31 temperature/humidity sensor at 0x44.
///
/// A command is two bytes. Single-shot measurement commands (0x2400, 0x2406,
/// 0x2C06) fill the six-byte result register with temperature and humidity
/// plus their CRCs; a read phase returns those bytes. The values come from the
/// manifest, so there is no self-heating or conversion delay to model.
pub struct Sht31 {
    temperature: f32,
    humidity: f32,
    measurement: [u8; 6],
    command: Option<u8>,
    reading: bool,
    index: usize,
}

impl Sht31 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let temperature = attr_f32(attrs, "temperature", 25.0);
        let humidity = attr_f32(attrs, "humidity", 50.0);
        Self {
            temperature,
            humidity,
            measurement: [0; 6],
            command: None,
            reading: false,
            index: 0,
        }
    }

    pub fn temperature_c(&self) -> f32 {
        self.temperature
    }

    pub fn humidity_percent(&self) -> f32 {
        self.humidity
    }

    pub fn measurement(&self) -> [u8; 6] {
        self.measurement
    }

    fn measure(&mut self) {
        let raw_t = ((self.temperature + 45.0) / 175.0 * 65_535.0)
            .round()
            .clamp(0.0, 65_535.0) as u16;
        let raw_h = (self.humidity / 100.0 * 65_535.0)
            .round()
            .clamp(0.0, 65_535.0) as u16;
        let t = raw_t.to_be_bytes();
        let h = raw_h.to_be_bytes();
        self.measurement = [
            t[0],
            t[1],
            crc8_sensirion(&t),
            h[0],
            h[1],
            crc8_sensirion(&h),
        ];
    }

    fn apply(&mut self, word: u16) {
        match word {
            0x2400 | 0x2406 | 0x240b | 0x2416 | 0x2c06 | 0x2c10 => self.measure(),
            // Reset, heater and status commands change no measured value.
            0x30a2 | 0x306d | 0x3041 | 0x3093 => {}
            _ => {}
        }
    }
}

impl I2cDevice for Sht31 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if read {
            self.index = 0;
        } else {
            self.command = None;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let byte = self.measurement[self.index.min(5)];
        self.index = (self.index + 1).min(self.measurement.len());
        byte
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        match self.command.take() {
            Some(high) => self.apply(be16(high, byte)),
            None => self.command = Some(byte),
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.command = None;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ BH1750

/// A BH1750 ambient-light sensor at 0x23.
///
/// Commands are one byte: power on/off, reset, and one continuous or one-shot
/// measurement mode. The 16-bit result is `lux * 1.2` in high-resolution mode
/// and `lux / 4` in low-resolution mode, which is the scaling the datasheet
/// gives.
pub struct Bh1750 {
    lux: f32,
    mode: u8,
    powered: bool,
    reading: bool,
    index: u8,
}

impl Bh1750 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        Self {
            lux: attr_f32(attrs, "lux", 500.0).max(0.0),
            mode: 0,
            powered: false,
            reading: false,
            index: 0,
        }
    }

    pub fn lux(&self) -> f32 {
        if self.powered { self.lux } else { 0.0 }
    }

    pub fn powered(&self) -> bool {
        self.powered
    }

    fn raw(&self) -> u16 {
        if !self.powered {
            return 0;
        }
        let value = match self.mode {
            0x10 | 0x11 | 0x20 => self.lux * 1.2,
            0x13 | 0x23 => self.lux / 4.0,
            _ => 0.0,
        };
        value.round().clamp(0.0, 65_535.0) as u16
    }
}

impl I2cDevice for Bh1750 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if read {
            self.index = 0;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let word = self.raw();
        let byte = if self.index == 0 {
            (word >> 8) as u8
        } else {
            word as u8
        };
        self.index = (self.index + 1) & 1;
        byte
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        match byte {
            0x00 => self.powered = false,
            0x01 => self.powered = true,
            0x07 => {
                self.powered = false;
                self.mode = 0;
            }
            0x10 | 0x11 | 0x13 | 0x20 | 0x23 => {
                self.powered = true;
                self.mode = byte;
            }
            _ => {}
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ TSL2561

/// A TSL2561 light-to-digital converter at 0x39.
///
/// Each command has bit 7 set and names a register in bits 0–3; bit 5 enables
/// a block read. The data registers are little-endian 16-bit words. The model
/// seeds the two channels from the manifest's lux attribute, with channel 1 a
/// quarter of channel 0, which is a plausible indoor spectrum.
pub struct Tsl2561 {
    lux: f32,
    registers: [u16; 16],
    pointer: u8,
    auto_increment: bool,
    first_write: bool,
    reading: bool,
    read_low: bool,
}

impl Tsl2561 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let lux = attr_f32(attrs, "lux", 500.0).max(0.0);
        let mut device = Self {
            lux,
            registers: [0; 16],
            pointer: 0,
            auto_increment: false,
            first_write: true,
            reading: false,
            read_low: false,
        };
        device.registers[0x00] = 0x03;
        device.registers[0x01] = 0x02;
        device.registers[0x0a] = 0x50;
        let channel0 = (lux * 10.0).round().clamp(0.0, 65_535.0) as u16;
        let channel1 = channel0 / 4;
        for (register, value) in [(0x0c, channel0), (0x0e, channel1)] {
            device.registers[register] = value & 0xff;
            device.registers[register + 1] = value >> 8;
        }
        device
    }

    pub fn lux(&self) -> f32 {
        self.lux
    }

    pub fn channel0(&self) -> u16 {
        le16(self.registers[0x0c] as u8, self.registers[0x0d] as u8)
    }

    pub fn id(&self) -> u8 {
        self.registers[0x0a] as u8
    }
}

impl I2cDevice for Tsl2561 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if read {
            self.read_low = false;
        } else {
            self.first_write = true;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let register = usize::from(self.pointer & 0x0f);
        if !self.read_low {
            self.read_low = true;
            self.registers[register] as u8
        } else {
            self.read_low = false;
            let high = self.registers[(register + 1) & 0x0f] as u8;
            if self.auto_increment {
                self.pointer = (self.pointer + 2) & 0x0f;
            }
            high
        }
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            if byte & 0x80 != 0 {
                self.pointer = byte & 0x0f;
                self.auto_increment = byte & 0x20 != 0;
            }
            self.first_write = false;
        } else {
            let register = usize::from(self.pointer & 0x0f);
            self.registers[register] = u16::from(byte);
            if self.auto_increment {
                self.pointer = (self.pointer + 1) & 0x0f;
            }
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ VEML7700

/// A VEML7700 ambient-light sensor at 0x10.
///
/// Registers are 16-bit little-endian. Register 0x04 is the ALS count and
/// 0x05 the white channel; the model scales the manifest's lux onto both so a
/// driver's conversion returns a value in the same order of magnitude.
pub struct Veml7700 {
    lux: f32,
    registers: [u16; 8],
    pointer: u8,
    first_write: bool,
    high: Option<u8>,
    reading: bool,
    read_low: bool,
}

impl Veml7700 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let lux = attr_f32(attrs, "lux", 500.0).max(0.0);
        let mut registers = [0u16; 8];
        let raw = (lux * 2.0).round().clamp(0.0, 65_535.0) as u16;
        registers[0x04] = raw;
        registers[0x05] = raw;
        Self {
            lux,
            registers,
            pointer: 0,
            first_write: true,
            high: None,
            reading: false,
            read_low: false,
        }
    }

    pub fn lux(&self) -> f32 {
        self.lux
    }

    pub fn als_raw(&self) -> u16 {
        self.registers[0x04]
    }
}

impl I2cDevice for Veml7700 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if read {
            self.read_low = false;
        } else {
            self.first_write = true;
            self.high = None;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let word = self.registers[usize::from(self.pointer & 0x07)];
        if !self.read_low {
            self.read_low = true;
            word as u8
        } else {
            self.read_low = false;
            self.pointer = (self.pointer + 1) & 0x07;
            (word >> 8) as u8
        }
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte & 0x07;
            self.first_write = false;
            self.high = None;
        } else if let Some(low) = self.high.take() {
            self.registers[usize::from(self.pointer & 0x07)] = le16(low, byte);
            self.pointer = (self.pointer + 1) & 0x07;
        } else {
            self.high = Some(byte);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
        self.high = None;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ TCS34725

/// A TCS34725 RGBC colour sensor at 0x29.
///
/// Commands carry bit 7 and a register address in bits 0–4; bit 5 asks for
/// auto-increment. The ID register 0x12 reads 0x44, and 0x14–0x1B hold the
/// clear, red, green and blue counts as little-endian words.
pub struct Tcs34725 {
    registers: [u8; 0x20],
    pointer: usize,
    auto_increment: bool,
    reading: bool,
}

impl Tcs34725 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let mut registers = [0u8; 0x20];
        registers[0x00] = 0x01; // PON
        registers[0x01] = 0xeb; // ATIME
        registers[0x0f] = 0x01; // control: 4x gain
        registers[0x12] = 0x44; // TCS34725
        let values: [(&str, usize, u16); 4] = [
            ("clear", 0x14, 1000),
            ("red", 0x16, 2000),
            ("green", 0x18, 1000),
            ("blue", 0x1a, 500),
        ];
        for (name, register, default) in values {
            let value = attr_u16(attrs, name, default);
            registers[register] = value as u8;
            registers[register + 1] = (value >> 8) as u8;
        }
        Self {
            registers,
            pointer: 0,
            auto_increment: false,
            reading: false,
        }
    }

    pub fn id(&self) -> u8 {
        self.registers[0x12]
    }

    fn word(&self, register: usize) -> u16 {
        le16(self.registers[register], self.registers[register + 1])
    }

    /// `(clear, red, green, blue)` counts.
    pub fn rgba(&self) -> (u16, u16, u16, u16) {
        (
            self.word(0x14),
            self.word(0x16),
            self.word(0x18),
            self.word(0x1a),
        )
    }
}

impl I2cDevice for Tcs34725 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let value = self.registers[self.pointer & 0x1f];
        if self.auto_increment {
            self.pointer = (self.pointer + 1) & 0x1f;
        }
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if byte & 0x80 != 0 {
            self.pointer = usize::from(byte & 0x1f);
            self.auto_increment = byte & 0x20 != 0;
        } else {
            self.registers[self.pointer & 0x1f] = byte;
            if self.auto_increment {
                self.pointer = (self.pointer + 1) & 0x1f;
            }
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ VL53L0X

/// A VL53L0X time-of-flight distance sensor at 0x29.
///
/// The register file has the identity bytes a driver probes (model ID 0xEE at
/// 0xC0) and the 16-bit range result at 0x1E. Writing 1 to SYSRANGE_START
/// loads the manifest's distance, so a driver that starts a measurement and
/// then reads the result sees a stable millimetre value.
pub struct Vl53l0x {
    registers: [u8; 0x100],
    pointer: u8,
    reading: bool,
    first_write: bool,
    distance_mm: u16,
}

impl Vl53l0x {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let distance_mm = attr_u16(attrs, "distance", 100);
        let mut device = Self {
            registers: [0; 0x100],
            pointer: 0,
            reading: false,
            first_write: true,
            distance_mm,
        };
        device.registers[0xc0] = 0xee;
        device.registers[0xc2] = 0x10;
        device.load_range();
        device
    }

    pub fn distance_mm(&self) -> u16 {
        self.distance_mm
    }

    fn load_range(&mut self) {
        let [high, low] = self.distance_mm.to_be_bytes();
        self.registers[0x1e] = high;
        self.registers[0x1f] = low;
    }
}

impl I2cDevice for Vl53l0x {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if !read {
            self.first_write = true;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let value = self.registers[usize::from(self.pointer)];
        self.pointer = self.pointer.wrapping_add(1);
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte;
            self.first_write = false;
        } else {
            if self.pointer == 0x00 && byte != 0 {
                self.load_range();
            }
            self.registers[usize::from(self.pointer)] = byte;
            self.pointer = self.pointer.wrapping_add(1);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ PCA9685

/// A PCA9685 16-channel PWM driver at 0x40.
///
/// The model keeps the register file a driver writes: MODE1/MODE2, the
/// PRE_SCALE byte and the four LEDn registers per channel. `duty` decodes the
/// ON/OFF counters the way the chip compares them, so a servo or LED library
/// can be checked without the PWM pins being wired to anything.
pub struct Pca9685 {
    registers: [u8; 0x100],
    pointer: u8,
    reading: bool,
    first_write: bool,
    frequency: u32,
}

impl Pca9685 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let frequency = attr_u16(attrs, "frequency", 50).clamp(1, 1000) as u32;
        let prescale = (25_000_000u32 / (4096 * frequency))
            .saturating_sub(1)
            .clamp(3, 255) as u8;
        let mut registers = [0u8; 0x100];
        registers[0x00] = 0x01; // MODE1: all call
        registers[0x01] = 0x04; // MODE2: totem pole
        registers[0xfe] = prescale;
        Self {
            registers,
            pointer: 0,
            reading: false,
            first_write: true,
            frequency,
        }
    }

    pub fn frequency(&self) -> u32 {
        self.frequency
    }

    pub fn prescale(&self) -> u8 {
        self.registers[0xfe]
    }

    /// The 12-bit ON and OFF counters of one channel.
    pub fn counters(&self, channel: usize) -> Option<(u16, u16)> {
        if channel >= 16 {
            return None;
        }
        let base = 0x06 + channel * 4;
        let on = le16(self.registers[base], self.registers[base + 1]) & 0x0fff;
        let off = le16(self.registers[base + 2], self.registers[base + 3]) & 0x0fff;
        Some((on, off))
    }

    /// The channel's duty in 1/4096 of the period.
    pub fn duty(&self, channel: usize) -> u16 {
        if channel >= 16 {
            return 0;
        }
        let base = 0x06 + channel * 4;
        let on = le16(self.registers[base], self.registers[base + 1]);
        let off = le16(self.registers[base + 2], self.registers[base + 3]);
        // Bit 12 of OFF is the "full on" flag; the counter bits alone would
        // wrap it away.
        if off & 0x1000 != 0 {
            return 4096;
        }
        if on == 0 && off == 0 {
            return 0;
        }
        (off.wrapping_sub(on)) & 0x0fff
    }

    pub fn duty_permille(&self, channel: usize) -> u16 {
        (self.duty(channel) as u32 * 1000 / 4096) as u16
    }
}

impl I2cDevice for Pca9685 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if !read {
            self.first_write = true;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let value = self.registers[usize::from(self.pointer)];
        self.pointer = self.pointer.wrapping_add(1);
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte;
            self.first_write = false;
        } else {
            self.registers[usize::from(self.pointer)] = byte;
            self.pointer = self.pointer.wrapping_add(1);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ MCP4725

/// A 12-bit MCP4725 DAC at 0x60.
///
/// Both the normal write (0x40 prefix) and the fast write (0x00 prefix) carry
/// the high four data bits in the first byte's low nibble and the low eight in
/// the second, so one decode handles both. A read returns the status byte and
/// the latched value.
pub struct Mcp4725 {
    value: u16,
    high: Option<u8>,
    reading: bool,
    index: u8,
}

impl Mcp4725 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        Self {
            value: attr_u16(attrs, "value", 0) & 0x0fff,
            high: None,
            reading: false,
            index: 0,
        }
    }

    pub fn value(&self) -> u16 {
        self.value
    }

    pub fn volts(&self, reference: f32) -> f32 {
        reference * f32::from(self.value) / 4095.0
    }
}

impl I2cDevice for Mcp4725 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if read {
            self.index = 0;
        } else {
            self.high = None;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let byte = match self.index {
            0 => 0,
            1 => (self.value >> 8) as u8,
            _ => self.value as u8,
        };
        self.index = (self.index + 1).min(2);
        byte
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if let Some(high) = self.high.take() {
            self.value = (u16::from(high & 0x0f) << 8 | u16::from(byte)) & 0x0fff;
        } else {
            self.high = Some(byte);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.high = None;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ INA219

/// An INA219 current/power monitor at 0x40.
///
/// The shunt, bus, power and current registers are 16-bit big-endian words.
/// Their values come from the manifest so a driver reading them with the
/// datasheet's LSB sizes reports the voltage and current the diagram intended.
pub struct Ina219 {
    registers: [u16; 6],
    pointer: usize,
    first_write: bool,
    high: Option<u8>,
    reading: bool,
    read_low: bool,
}

impl Ina219 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let bus_voltage = attr_f32(attrs, "busVoltage", 5.0);
        let shunt_mv = attr_f32(attrs, "shuntMillivolts", 10.0);
        let current_ma = attr_f32(attrs, "currentMilliamps", 100.0);
        let mut registers = [0u16; 6];
        registers[0] = 0x399f;
        registers[1] = ((shunt_mv / 0.01).round() as i16) as u16;
        registers[2] = (bus_voltage / 0.004).round().clamp(0.0, 65_535.0) as u16;
        registers[3] = (bus_voltage * current_ma / 20.0)
            .round()
            .clamp(0.0, 65_535.0) as u16;
        registers[4] = (current_ma / 0.1).round().clamp(0.0, 65_535.0) as u16;
        registers[5] = 0x1000;
        Self {
            registers,
            pointer: 0,
            first_write: true,
            high: None,
            reading: false,
            read_low: false,
        }
    }

    pub fn bus_voltage(&self) -> f32 {
        f32::from(self.registers[2]) * 0.004
    }

    pub fn current(&self) -> f32 {
        f32::from(self.registers[4]) * 0.0001
    }

    pub fn register(&self, register: usize) -> Option<u16> {
        self.registers.get(register).copied()
    }
}

impl I2cDevice for Ina219 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if read {
            self.read_low = false;
        } else {
            self.first_write = true;
            self.high = None;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let word = self.registers[self.pointer % self.registers.len()];
        if !self.read_low {
            self.read_low = true;
            (word >> 8) as u8
        } else {
            self.read_low = false;
            self.pointer = (self.pointer + 1) % self.registers.len();
            word as u8
        }
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = usize::from(byte & 0x07) % self.registers.len();
            self.first_write = false;
            self.high = None;
        } else if let Some(high) = self.high.take() {
            self.registers[self.pointer % self.registers.len()] = be16(high, byte);
            self.pointer = (self.pointer + 1) % self.registers.len();
        } else {
            self.high = Some(byte);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
        self.high = None;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ BME280

/// A BME280 temperature, pressure and humidity sensor at 0x76.
///
/// The identity register 0xD0 answers 0x60. 0xF7–0xFE carry the raw pressure,
/// temperature and humidity words the compensation formulas consume; they are
/// seeded with representative values (25 °C, 101325 Pa, 50 %RH). Calibration
/// registers are not populated, so a driver that insists on reading them gets
/// zeros and should be treated as a limitation of this model rather than a
/// hardware fault.
pub struct Bme280 {
    registers: [u8; 0x100],
    pointer: u8,
    first_write: bool,
    reading: bool,
}

impl Bme280 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let temperature = attr_f32(attrs, "temperature", 25.0);
        let pressure = attr_f32(attrs, "pressure", 101_325.0);
        let humidity = attr_f32(attrs, "humidity", 50.0);
        let mut registers = [0u8; 0x100];
        registers[0xd0] = 0x60;
        registers[0xf4] = 0x27; // ctrl_meas: oversample x1, normal mode
        registers[0xf5] = 0x80; // config: standby 500 ms
        registers[0xf2] = 0x01; // ctrl_hum: oversample x1
        let raw_t = ((temperature + 40.0) * 5120.0) as u32;
        let raw_p = (pressure * 4.0).max(0.0) as u32;
        let raw_h = (humidity / 100.0 * 32_768.0) as u16;
        registers[0xf7] = (raw_p >> 12) as u8;
        registers[0xf8] = (raw_p >> 4) as u8;
        registers[0xf9] = (raw_p << 4) as u8;
        registers[0xfa] = (raw_t >> 12) as u8;
        registers[0xfb] = (raw_t >> 4) as u8;
        registers[0xfc] = (raw_t << 4) as u8;
        registers[0xfd] = (raw_h >> 8) as u8;
        registers[0xfe] = raw_h as u8;
        Self {
            registers,
            pointer: 0,
            first_write: true,
            reading: false,
        }
    }

    pub fn id(&self) -> u8 {
        self.registers[0xd0]
    }

    pub fn temperature_raw(&self) -> u32 {
        (u32::from(self.registers[0xfa]) << 12)
            | (u32::from(self.registers[0xfb]) << 4)
            | u32::from(self.registers[0xfc] >> 4)
    }

    pub fn pressure_raw(&self) -> u32 {
        (u32::from(self.registers[0xf7]) << 12)
            | (u32::from(self.registers[0xf8]) << 4)
            | u32::from(self.registers[0xf9] >> 4)
    }

    pub fn humidity_raw(&self) -> u16 {
        u16::from(self.registers[0xfd]) << 8 | u16::from(self.registers[0xfe])
    }
}

impl I2cDevice for Bme280 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if !read {
            self.first_write = true;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let value = self.registers[usize::from(self.pointer)];
        self.pointer = self.pointer.wrapping_add(1);
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte;
            self.first_write = false;
        } else {
            self.registers[usize::from(self.pointer)] = byte;
            self.pointer = self.pointer.wrapping_add(1);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ APDS9960

/// An APDS9960 proximity, ambient-light and RGB sensor at 0x39.
///
/// The ID register 0x92 reads 0xAB, the enable register 0x80 gates the device,
/// and 0x94–0x9B hold the clear/red/green/blue counts as little-endian words.
/// Proximity is at 0x9C. The manifest seeds all of them.
pub struct Apds9960 {
    registers: [u8; 0x100],
    pointer: u8,
    first_write: bool,
    reading: bool,
}

impl Apds9960 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let mut registers = [0u8; 0x100];
        registers[0x80] = 0x01;
        registers[0x92] = 0xab;
        let values: [(&str, usize, u16); 4] = [
            ("clear", 0x94, 1000),
            ("red", 0x96, 2000),
            ("green", 0x98, 1000),
            ("blue", 0x9a, 500),
        ];
        for (name, register, default) in values {
            let value = attr_u16(attrs, name, default);
            registers[register] = value as u8;
            registers[register + 1] = (value >> 8) as u8;
        }
        let proximity = attr_u16(attrs, "proximity", 0);
        registers[0x9c] = proximity as u8;
        registers[0x9d] = (proximity >> 8) as u8;
        Self {
            registers,
            pointer: 0,
            first_write: true,
            reading: false,
        }
    }

    pub fn id(&self) -> u8 {
        self.registers[0x92]
    }

    pub fn rgba(&self) -> (u16, u16, u16, u16) {
        let word = |register: usize| le16(self.registers[register], self.registers[register + 1]);
        (word(0x94), word(0x96), word(0x98), word(0x9a))
    }

    pub fn proximity(&self) -> u16 {
        le16(self.registers[0x9c], self.registers[0x9d])
    }
}

impl I2cDevice for Apds9960 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if !read {
            self.first_write = true;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let value = self.registers[usize::from(self.pointer)];
        self.pointer = self.pointer.wrapping_add(1);
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte;
            self.first_write = false;
        } else {
            self.registers[usize::from(self.pointer)] = byte;
            self.pointer = self.pointer.wrapping_add(1);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ MLX90614

/// An MLX90614 infrared thermometer at 0x5A.
///
/// A read is the SMBus word protocol: write the RAM address (0x06 ambient,
/// 0x07 object 1), repeated start, then three bytes low/high/PEC. Each count is
/// 0.02 K, so the model encodes the manifest's Celsius values the same way.
pub struct Mlx90614 {
    object: f32,
    ambient: f32,
    command: u8,
    reading: bool,
    index: u8,
}

impl Mlx90614 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        Self {
            object: attr_f32(attrs, "object", 25.0),
            ambient: attr_f32(attrs, "ambient", 22.0),
            command: 0x07,
            reading: false,
            index: 0,
        }
    }

    pub fn object_celsius(&self) -> f32 {
        self.object
    }

    pub fn ambient_celsius(&self) -> f32 {
        self.ambient
    }

    fn counts(celsius: f32) -> [u8; 2] {
        let raw = ((celsius + 273.15) / 0.02).round().clamp(0.0, 65_535.0) as u16;
        raw.to_le_bytes()
    }
}

impl I2cDevice for Mlx90614 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if read {
            self.index = 0;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let temperature = if self.command == 0x07 {
            self.object
        } else {
            self.ambient
        };
        let [low, high] = Self::counts(temperature);
        let byte = match self.index {
            0 => low,
            1 => high,
            _ => crc8_sensirion(&[low, high]),
        };
        self.index = (self.index + 1).min(2);
        byte
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        self.command = byte;
        true
    }

    fn stop(&mut self) {
        self.reading = false;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ DS18B20

const OW_RESET_LOW: Tick = 480_000;
const OW_PRESENCE_DELAY: Tick = 30_000;
const OW_PRESENCE_LOW: Tick = 120_000;
const OW_READ_ZERO: Tick = 25_000;
/// A write slot held low longer than this is a zero; shorter is a one.
const OW_WRITE_ZERO: Tick = 30_000;

/// What the next data byte on the wire means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// After a reset: the ROM command.
    Rom,
    /// The ROM command selected this device: a function command.
    Function,
    /// A Match ROM command is consuming its eight address bytes.
    Match(u8),
    /// A command is executing; extra bytes are ignored.
    Idle,
}

/// A DS18B20 temperature sensor on the 1-Wire bus.
///
/// The model answers a reset with the presence pulse, decodes write slots from
/// the low period the master holds, and drives read slots for the scratchpad.
/// ROM commands 0xCC (skip), 0x33 (read ROM), 0x55 (match ROM), 0x44 (convert)
/// and 0xBE (read scratchpad) are enough for the common drivers. The
/// temperature comes from the manifest; a conversion makes it visible in the
/// scratchpad, which is the observable behaviour a firmware can check.
pub struct Ds18b20 {
    dq: u16,
    output: Level,
    temperature: f32,
    device_id: [u8; 8],
    low_at: Option<Tick>,
    presence_start: Option<Tick>,
    presence_until: Option<Tick>,
    read_release: Option<Tick>,
    byte_bits: u8,
    byte: u8,
    phase: Phase,
    read_buffer: [u8; 9],
    read_index: usize,
    read_total: usize,
    scratchpad: [u8; 9],
    conversions: u32,
}

impl Ds18b20 {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let temperature = attr_f32(&part.attrs, "temperature", 25.0).clamp(-55.0, 125.0);
        let id = part
            .attrs
            .get("deviceID")
            .map(|text| parse_id(text))
            .unwrap_or([0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        Ok(Box::new(Self::new(index(part, "DQ")?, temperature, id)))
    }

    pub fn new(dq: u16, temperature: f32, device_id: [u8; 8]) -> Self {
        let mut sensor = Self {
            dq,
            output: Level::HighZ,
            temperature,
            device_id,
            low_at: None,
            presence_start: None,
            presence_until: None,
            read_release: None,
            byte_bits: 0,
            byte: 0,
            phase: Phase::Rom,
            read_buffer: [0; 9],
            read_index: 0,
            read_total: 0,
            scratchpad: [0; 9],
            conversions: 0,
        };
        sensor.load_scratchpad();
        sensor
    }

    pub fn temperature_celsius(&self) -> f32 {
        self.temperature
    }

    pub fn scratchpad(&self) -> [u8; 9] {
        self.scratchpad
    }

    pub fn conversions(&self) -> u32 {
        self.conversions
    }

    pub fn device_id(&self) -> [u8; 8] {
        self.device_id
    }

    fn load_scratchpad(&mut self) {
        let raw = (self.temperature * 16.0).round() as i16;
        let [low, high] = raw.to_le_bytes();
        self.scratchpad = [low, high, 0x4b, 0x46, 0x7f, 0xff, 0x0c, 0x10, 0];
        self.scratchpad[8] = crc8_dallas(&self.scratchpad[..8]);
    }

    fn bit_level(&self) -> Level {
        if self.read_index < self.read_total {
            let byte = self.read_buffer[self.read_index / 8];
            if byte & (1 << (self.read_index % 8)) == 0 {
                Level::Low
            } else {
                Level::HighZ
            }
        } else {
            Level::HighZ
        }
    }

    fn start_read(&mut self, bytes: &[u8]) {
        self.read_buffer = [0; 9];
        self.read_buffer[..bytes.len()].copy_from_slice(bytes);
        self.read_index = 0;
        self.read_total = bytes.len() * 8;
    }

    fn command(&mut self, byte: u8) {
        match self.phase {
            Phase::Match(remaining) => {
                self.phase = if remaining <= 1 {
                    Phase::Function
                } else {
                    Phase::Match(remaining - 1)
                };
            }
            Phase::Rom => match byte {
                0xcc => self.phase = Phase::Function,
                0x55 => self.phase = Phase::Match(8),
                0x33 => {
                    let id = self.device_id;
                    let mut bytes = [0u8; 9];
                    bytes[..8].copy_from_slice(&id);
                    bytes[8] = crc8_dallas(&id);
                    self.start_read(&bytes);
                    self.phase = Phase::Idle;
                }
                _ => self.phase = Phase::Idle,
            },
            Phase::Function => match byte {
                0x44 => {
                    self.conversions += 1;
                    self.load_scratchpad();
                    self.phase = Phase::Idle;
                }
                0xbe => {
                    let scratchpad = self.scratchpad;
                    self.start_read(&scratchpad);
                    self.phase = Phase::Idle;
                }
                _ => self.phase = Phase::Idle,
            },
            Phase::Idle => {}
        }
    }

    fn finish_byte(&mut self) {
        let byte = self.byte;
        self.byte = 0;
        self.byte_bits = 0;
        self.low_at = None;
        self.command(byte);
    }

    fn reset(&mut self, now: Tick) {
        self.byte_bits = 0;
        self.byte = 0;
        self.phase = Phase::Rom;
        self.read_index = 0;
        self.read_total = 0;
        self.read_release = None;
        self.output = Level::HighZ;
        self.presence_start = Some(now + OW_PRESENCE_DELAY);
        self.presence_until = Some(now + OW_PRESENCE_DELAY + OW_PRESENCE_LOW);
    }
}

fn parse_id(text: &str) -> [u8; 8] {
    let mut id = [0u8; 8];
    let clean: Vec<char> = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    for (index, byte) in id.iter_mut().enumerate() {
        let pair: String = clean.iter().skip(index * 2).take(2).collect();
        *byte = u8::from_str_radix(&pair, 16).unwrap_or(0);
    }
    id
}

impl Part for Ds18b20 {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 != self.dq {
            return Drive::HIGH_Z;
        }
        match self.output {
            Level::HighZ => Drive::HIGH_Z,
            level => Drive::strong(level),
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        if pin.0 != self.dq {
            return;
        }
        match level {
            Level::Low => {
                if self.low_at.is_none() {
                    self.low_at = Some(now);
                }
                // A read slot for a zero bit pulls the line low as soon as the
                // master releases it; the master samples a few microseconds in.
                if self.read_total > 0 && self.read_index < self.read_total {
                    if self.bit_level() == Level::Low {
                        self.output = Level::Low;
                        self.read_release = Some(now + OW_READ_ZERO);
                    } else {
                        self.output = Level::HighZ;
                    }
                }
            }
            Level::High | Level::HighZ => {
                let Some(start) = self.low_at.take() else {
                    return;
                };
                let low = now.saturating_sub(start);
                if low >= OW_RESET_LOW {
                    self.reset(now);
                    return;
                }
                if self.read_total > 0 && self.read_index < self.read_total {
                    self.read_index += 1;
                    self.read_release = None;
                    self.output = Level::HighZ;
                    if self.read_index >= self.read_total {
                        self.read_total = 0;
                        self.read_index = 0;
                    }
                    return;
                }
                if low >= OW_WRITE_ZERO {
                    // A long low slot is a zero.
                    self.byte_bits = (self.byte_bits + 1).min(8);
                } else {
                    self.byte |= 1 << self.byte_bits.min(7);
                    self.byte_bits = (self.byte_bits + 1).min(8);
                }
                if self.byte_bits == 8 {
                    self.finish_byte();
                }
            }
        }
    }

    fn next_event(&self) -> Option<Tick> {
        [self.presence_start, self.presence_until, self.read_release]
            .into_iter()
            .flatten()
            .min()
    }

    fn advance(&mut self, now: Tick) {
        if let Some(at) = self.presence_start
            && now >= at
        {
            self.output = Level::Low;
            self.presence_start = None;
        }
        if let Some(at) = self.presence_until
            && now >= at
        {
            self.output = Level::HighZ;
            self.presence_until = None;
        }
        if let Some(at) = self.read_release
            && now >= at
        {
            self.output = Level::HighZ;
            self.read_release = None;
        }
    }

    fn state(&self) -> PartState {
        PartState::Knob {
            permille: (((self.temperature + 55.0) / 180.0) * 1000.0).clamp(0.0, 1000.0) as u16,
        }
    }
}

// ------------------------------------------------------------------ BME680

/// The Bosch calibration block starts at 0x8A and is 23 bytes long.
///
/// Inferred: these are plausible coefficients, not a dump from a real part.
/// The model exposes a coherent block so a driver's calibration read succeeds;
/// it does not run Bosch's integer compensation, so a driver that does will get
/// a value in the right range only because the raw registers were seeded
/// consistently with these coefficients.
const BME680_CALIBRATION: [u8; 23] = [
    0x10, 0x70, // par_t1
    0x80, 0x63, // par_t2
    0x03, // par_t3
    0x00, 0x8d, // par_p1
    0x00, 0xd8, // par_p2
    0x58, // par_p3
    0x00, 0x19, // par_p4
    0x80, 0xf7, // par_p5
    0x1e, // par_p6
    0x14, // par_p7
    0x00, 0xfa, // par_p8
    0x04, 0xe0, // par_p9
    0x1e, // par_p10
    0x00, 0x00, // reserved
];

/// A BME680 temperature, pressure, humidity and gas sensor at 0x76 or 0x77.
///
/// The identity register 0xD0 answers 0x61. The calibration block begins at
/// 0x8A, the raw pressure/temperature/humidity words occupy 0x1F–0x26 and the
/// gas resistance reads at 0x2C–0x2D; 0x74 is the ctrl_meas register, whose low
/// two bits are the mode. The raw words are derived from the manifest's
/// attributes the way [`Bme280`] does, so a driver's compensation starts from a
/// physically sensible sample. Only the I2C interface is modelled.
pub struct Bme680 {
    registers: [u8; 0x100],
    pointer: u8,
    first_write: bool,
    reading: bool,
    temperature: f32,
    pressure: f32,
    humidity: f32,
    gas_resistance: f32,
}

impl Bme680 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let temperature = attr_f32(attrs, "temperature", 25.0);
        let pressure = attr_f32(attrs, "pressure", 101_325.0);
        let humidity = attr_f32(attrs, "humidity", 50.0);
        let gas_resistance = attr_f32(attrs, "gas", 100_000.0);
        let mut device = Self {
            registers: [0u8; 0x100],
            pointer: 0,
            first_write: true,
            reading: false,
            temperature,
            pressure,
            humidity,
            gas_resistance,
        };
        device.seed();
        device
    }

    fn seed(&mut self) {
        self.registers[0xd0] = 0x61;
        self.registers[0x74] = 0x00; // ctrl_meas: sleep
        self.registers[0x72] = 0x00; // ctrl_hum
        self.registers[0x71] = 0x00; // ctrl_gas_1
        self.registers[0x73] = 0x00; // config
        self.registers[0x8a..0x8a + BME680_CALIBRATION.len()].copy_from_slice(&BME680_CALIBRATION);
        // Inferred: plausible humidity and gas calibration bytes (0xE1–0xEE).
        for (register, value) in [
            (0xe1, 0x0f),
            (0xe2, 0x40),
            (0xe3, 0x1e),
            (0xe4, 0x00),
            (0xe5, 0x32),
            (0xe6, 0x14),
            (0xe7, 0x64),
            (0xe8, 0x32),
            (0xeb, 0x10),
            (0xec, 0x20),
            (0xed, 0x30),
            (0xee, 0x40),
        ] {
            self.registers[register] = value;
        }
        self.load_raw();
    }

    fn load_raw(&mut self) {
        // Inferred: the seeding formulas. Temperature has 0.01 °C resolution in
        // a 20-bit field, pressure is the Pascal value in a 20-bit field, and
        // humidity is a 16-bit fraction of full scale.
        let raw_t = ((self.temperature + 40.0) * 6_400.0)
            .round()
            .clamp(0.0, 1_048_575.0) as u32;
        let raw_p = self.pressure.round().clamp(0.0, 1_048_575.0) as u32;
        let raw_h = (self.humidity / 100.0 * 65_536.0)
            .round()
            .clamp(0.0, 65_535.0) as u16;
        let raw_gas = (self.gas_resistance / 1_000.0).round().clamp(0.0, 65_535.0) as u16;
        for (register, value) in [
            (0x1f, (raw_p >> 12) as u8),
            (0x20, (raw_p >> 4) as u8),
            (0x21, (raw_p << 4) as u8),
            (0x22, (raw_t >> 12) as u8),
            (0x23, (raw_t >> 4) as u8),
            (0x24, (raw_t << 4) as u8),
            (0x25, (raw_h >> 8) as u8),
            (0x26, raw_h as u8),
            (0x2c, raw_gas as u8),
            (0x2d, (raw_gas >> 8) as u8),
        ] {
            self.registers[register] = value;
        }
    }

    pub fn id(&self) -> u8 {
        self.registers[0xd0]
    }

    /// The ctrl_meas register 0x74.
    pub fn control(&self) -> u8 {
        self.registers[0x74]
    }

    pub fn temperature_raw(&self) -> u32 {
        (u32::from(self.registers[0x22]) << 12)
            | (u32::from(self.registers[0x23]) << 4)
            | u32::from(self.registers[0x24] >> 4)
    }

    pub fn pressure_raw(&self) -> u32 {
        (u32::from(self.registers[0x1f]) << 12)
            | (u32::from(self.registers[0x20]) << 4)
            | u32::from(self.registers[0x21] >> 4)
    }

    pub fn humidity_raw(&self) -> u16 {
        u16::from(self.registers[0x25]) << 8 | u16::from(self.registers[0x26])
    }

    /// Gas resistance in kΩ, from 0x2C–0x2D.
    pub fn gas_resistance_kohm(&self) -> u16 {
        le16(self.registers[0x2c], self.registers[0x2d])
    }
}

impl I2cDevice for Bme680 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if !read {
            self.first_write = true;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let value = self.registers[usize::from(self.pointer)];
        self.pointer = self.pointer.wrapping_add(1);
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte;
            self.first_write = false;
        } else {
            // The chip id at 0xD0 is read-only.
            if self.pointer != 0xd0 {
                self.registers[usize::from(self.pointer)] = byte;
            }
            self.pointer = self.pointer.wrapping_add(1);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ LM75

/// An LM75 temperature sensor at 0x48–0x4F.
///
/// Register 0x00 is the 9-bit temperature, left-justified in two bytes (bit 7
/// of the second byte is the half-degree bit), 0x01 the configuration, 0x02 the
/// Thyst set point and 0x03 the Tos set point. The pointer auto-increments
/// through those four registers, which is how a driver reads the set points as
/// one block.
pub struct Lm75 {
    temperature: [u8; 2],
    config: u8,
    thyst: [u8; 2],
    tos: [u8; 2],
    pointer: u8,
    first_write: bool,
    high: Option<u8>,
    reading: bool,
    read_index: u8,
}

impl Lm75 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        Self::with_temperature(attr_f32(attrs, "temperature", 25.0))
    }

    pub fn with_temperature(temperature: f32) -> Self {
        Self {
            temperature: Self::encode(temperature),
            config: 0x00, // comparator, normal mode
            // Inferred: the datasheet reset values for the set points.
            thyst: Self::encode(75.0),
            tos: Self::encode(80.0),
            pointer: 0,
            first_write: true,
            high: None,
            reading: false,
            read_index: 0,
        }
    }

    /// The 9-bit register image: 0.5 °C per count, left-justified.
    fn encode(celsius: f32) -> [u8; 2] {
        let counts = (celsius / 0.5).round().clamp(-256.0, 255.0) as i16;
        (counts << 7).to_be_bytes()
    }

    /// Bytes in the register the pointer names. Temperature, Thyst and Tos are
    /// 16-bit; the configuration register is one byte.
    fn width(pointer: u8) -> u8 {
        if pointer == 0x01 { 1 } else { 2 }
    }

    pub fn temperature_celsius(&self) -> f32 {
        let word = i16::from_be_bytes(self.temperature);
        f32::from(word >> 7) * 0.5
    }

    pub fn config(&self) -> u8 {
        self.config
    }

    pub fn hysteresis_celsius(&self) -> f32 {
        f32::from(i16::from_be_bytes(self.thyst) >> 7) * 0.5
    }

    pub fn overtemp_celsius(&self) -> f32 {
        f32::from(i16::from_be_bytes(self.tos) >> 7) * 0.5
    }

    fn read_register(&mut self) -> u8 {
        let index = usize::from(self.read_index.min(1));
        let byte = match self.pointer {
            0x00 => self.temperature[index],
            0x01 => self.config,
            0x02 => self.thyst[index],
            _ => self.tos[index],
        };
        self.read_index += 1;
        if self.read_index >= Self::width(self.pointer) {
            self.read_index = 0;
            self.pointer = (self.pointer + 1) & 0x03;
        }
        byte
    }

    fn write_register(&mut self, byte: u8) {
        if self.pointer == 0x01 {
            self.config = byte;
            self.pointer = 0x02;
            return;
        }
        if let Some(high) = self.high.take() {
            let word = [high, byte];
            match self.pointer {
                0x00 => self.temperature = word,
                0x02 => self.thyst = word,
                _ => self.tos = word,
            }
            self.pointer = (self.pointer + 1) & 0x03;
        } else {
            self.high = Some(byte);
        }
    }
}

impl I2cDevice for Lm75 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if read {
            self.read_index = 0;
        } else {
            self.first_write = true;
            self.high = None;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        self.read_register()
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte & 0x03;
            self.first_write = false;
            self.high = None;
        } else {
            self.write_register(byte);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
        self.high = None;
        self.read_index = 0;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ TMP102

/// A TMP102 temperature sensor at 0x48–0x4B.
///
/// The temperature register 0x00 is a 12-bit, left-justified two's-complement
/// word in 0.0625 °C counts, so 25 °C is 0x1900. Registers 0x01 (config),
/// 0x02 (T_LOW) and 0x03 (T_HIGH) are 16-bit big-endian words.
pub struct Tmp102 {
    registers: [u16; 4],
    pointer: usize,
    first_write: bool,
    high: Option<u8>,
    reading: bool,
    read_low: bool,
}

impl Tmp102 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        Self::with_temperature(attr_f32(attrs, "temperature", 25.0))
    }

    pub fn with_temperature(temperature: f32) -> Self {
        let mut device = Self {
            registers: [0; 4],
            pointer: 0,
            first_write: true,
            high: None,
            reading: false,
            read_low: false,
        };
        device.set_temperature(temperature);
        device.registers[0x01] = 0x60a0; // datasheet reset: 4 Hz, comparator
        device.registers[0x02] = 0x4b00; // T_LOW, 75 °C
        device.registers[0x03] = 0x5000; // T_HIGH, 80 °C
        device
    }

    fn set_temperature(&mut self, temperature: f32) {
        let counts = (temperature / 0.0625).round().clamp(-2048.0, 2047.0) as i16;
        self.registers[0x00] = (counts << 4) as u16;
    }

    pub fn temperature_celsius(&self) -> f32 {
        (self.registers[0x00] as i16 >> 4) as f32 * 0.0625
    }

    pub fn config(&self) -> u16 {
        self.registers[0x01]
    }
}

impl I2cDevice for Tmp102 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if read {
            self.read_low = false;
        } else {
            self.first_write = true;
            self.high = None;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let word = self.registers[self.pointer & 0x03];
        if !self.read_low {
            self.read_low = true;
            (word >> 8) as u8
        } else {
            self.read_low = false;
            self.pointer = (self.pointer + 1) & 0x03;
            word as u8
        }
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = usize::from(byte & 0x03);
            self.first_write = false;
            self.high = None;
        } else if let Some(high) = self.high.take() {
            self.registers[self.pointer & 0x03] = be16(high, byte);
            self.pointer = (self.pointer + 1) & 0x03;
        } else {
            self.high = Some(byte);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
        self.high = None;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ ADXL345

/// The ADXL345's device id, read from 0x00.
pub const ADXL345_DEVICE_ID: u8 = 0xe5;

/// An ADXL345 three-axis accelerometer at 0x53.
///
/// DATA_FORMAT is 0x31 (range in bits 0–1, full-resolution in bit 3),
/// POWER_CTL 0x2D (measure in bit 3) and the data words sit at 0x32–0x37 as
/// little-endian 16-bit values. The acceleration comes from the manifest's
/// `x`/`y`/`z` attributes in g. In ten-bit mode one g is 256 counts at ±2 g,
/// halving for each wider range, which is the datasheet's scale.
pub struct Adxl345 {
    registers: [u8; 0x40],
    pointer: u8,
    first_write: bool,
    reading: bool,
    accel_g: [f32; 3],
}

impl Adxl345 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        Self::with_accel([
            attr_f32(attrs, "x", 0.0),
            attr_f32(attrs, "y", 0.0),
            attr_f32(attrs, "z", 1.0),
        ])
    }

    pub fn with_accel(accel_g: [f32; 3]) -> Self {
        let mut device = Self {
            registers: [0u8; 0x40],
            pointer: 0,
            first_write: true,
            reading: false,
            accel_g,
        };
        device.registers[0x00] = ADXL345_DEVICE_ID;
        device.load_data();
        device
    }

    fn scale(&self) -> f32 {
        let range = self.registers[0x31] & 0x03;
        if self.registers[0x31] & 0x08 != 0 {
            // Full resolution is a fixed 256 LSB/g at every range.
            256.0
        } else {
            256.0 / f32::from(1u8 << range)
        }
    }

    fn load_data(&mut self) {
        let scale = self.scale();
        for (value, base) in self.accel_g.iter().zip([0x32usize, 0x34, 0x36]) {
            let raw = (value * scale).round().clamp(-32_768.0, 32_767.0) as i16;
            self.registers[base] = raw as u8;
            self.registers[base + 1] = (raw >> 8) as u8;
        }
    }

    pub fn id(&self) -> u8 {
        self.registers[0x00]
    }

    pub fn measuring(&self) -> bool {
        self.registers[0x2d] & 0x08 != 0
    }

    pub fn range_g(&self) -> u8 {
        2 << (self.registers[0x31] & 0x03)
    }

    /// Acceleration in g, decoded from the data registers.
    pub fn accel_g(&self) -> [f32; 3] {
        let scale = self.scale();
        let word = |base: usize| {
            i16::from_le_bytes([self.registers[base], self.registers[base + 1]]) as f32 / scale
        };
        [word(0x32), word(0x34), word(0x36)]
    }
}

impl I2cDevice for Adxl345 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if !read {
            self.first_write = true;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let value = self.registers[usize::from(self.pointer)];
        self.pointer = self.pointer.wrapping_add(1);
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte;
            self.first_write = false;
        } else {
            if self.pointer != 0x00 {
                self.registers[usize::from(self.pointer)] = byte;
                if self.pointer == 0x31 {
                    // A range or resolution change re-seeds the data words.
                    self.load_data();
                }
            }
            self.pointer = self.pointer.wrapping_add(1);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ QMC5883L

/// A QMC5883L three-axis magnetometer at 0x0D.
///
/// The chip id register 0x0D answers 0xFF. The data registers 0x00–0x05 are
/// little-endian 16-bit X, Y and Z counts, 0x06 is the status register (bit 0
/// is DRDY) and 0x09 is the control register. The field comes from the
/// manifest's `x`/`y`/`z` attributes in microtesla. Inferred: 120 counts per
/// microtesla, from the part's 12 000 LSB/gauss sensitivity and 100 µT/gauss.
pub struct Qmc5883l {
    registers: [u8; 0x10],
    pointer: u8,
    first_write: bool,
    reading: bool,
    field_ut: [f32; 3],
}

impl Qmc5883l {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        Self::with_field([
            attr_f32(attrs, "x", 0.0),
            attr_f32(attrs, "y", 0.0),
            attr_f32(attrs, "z", 50.0),
        ])
    }

    pub fn with_field(field_ut: [f32; 3]) -> Self {
        let mut device = Self {
            registers: [0u8; 0x10],
            pointer: 0,
            first_write: true,
            reading: false,
            field_ut,
        };
        device.registers[0x09] = 0x1d; // OSR512, ±8 G, 200 Hz, continuous
        device.registers[0x0d] = 0xff;
        device.load_data();
        device
    }

    fn load_data(&mut self) {
        for (value, base) in self.field_ut.iter().zip([0x00usize, 0x02, 0x04]) {
            // Inferred: 120 LSB/µT from 12 000 LSB/G and 100 µT/G.
            let raw = (value * 120.0).round().clamp(-32_768.0, 32_767.0) as i16;
            self.registers[base] = raw as u8;
            self.registers[base + 1] = (raw >> 8) as u8;
        }
        self.registers[0x06] = 0x01; // DRDY
    }

    pub fn id(&self) -> u8 {
        self.registers[0x0d]
    }

    pub fn control(&self) -> u8 {
        self.registers[0x09]
    }

    pub fn data_ready(&self) -> bool {
        self.registers[0x06] & 0x01 != 0
    }

    /// The field in microtesla, decoded from the data registers.
    pub fn field_ut(&self) -> [f32; 3] {
        let word = |base: usize| {
            i16::from_le_bytes([self.registers[base], self.registers[base + 1]]) as f32 / 120.0
        };
        [word(0x00), word(0x02), word(0x04)]
    }
}

impl I2cDevice for Qmc5883l {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if !read {
            self.first_write = true;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let value = self.registers[usize::from(self.pointer & 0x0f)];
        self.pointer = (self.pointer + 1) & 0x0f;
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte & 0x0f;
            self.first_write = false;
        } else {
            if self.pointer != 0x0d {
                self.registers[usize::from(self.pointer & 0x0f)] = byte;
            }
            self.pointer = (self.pointer + 1) & 0x0f;
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ SGP30

/// The Sensirion feature-set version the SGP30 reports.
pub const SGP30_FEATURE_SET: u16 = 0x0022;

/// A Sensirion SGP30 air-quality sensor at 0x58.
///
/// Commands are 16-bit big-endian words. `measure_iaq` (0x2008) answers two
/// CRC-checked words, eCO2 in ppm and TVOC in ppb; `get_feature_set_version`
/// (0x202F) answers 0x0022. `iaq_init` (0x2003) is accepted. Each word is
/// followed by the Sensirion CRC-8 (polynomial 0x31, initial value 0xFF), the
/// same checksum an SHT31 uses.
pub struct Sgp30 {
    eco2: u16,
    tvoc: u16,
    command: Option<u8>,
    response: Vec<u8>,
    index: usize,
    reading: bool,
    initialized: bool,
}

impl Sgp30 {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        Self::with_reading(attr_u16(attrs, "eco2", 400), attr_u16(attrs, "tvoc", 0))
    }

    pub fn with_reading(eco2: u16, tvoc: u16) -> Self {
        Self {
            eco2,
            tvoc,
            command: None,
            response: Vec::new(),
            index: 0,
            reading: false,
            initialized: false,
        }
    }

    pub fn eco2(&self) -> u16 {
        self.eco2
    }

    pub fn tvoc(&self) -> u16 {
        self.tvoc
    }

    pub fn initialized(&self) -> bool {
        self.initialized
    }

    fn word_crc(word: u16) -> [u8; 3] {
        let [high, low] = word.to_be_bytes();
        [high, low, crc8_sensirion(&[high, low])]
    }

    fn apply(&mut self, command: u16) {
        self.response.clear();
        match command {
            0x2003 => self.initialized = true,
            0x2008 => {
                self.response.extend_from_slice(&Self::word_crc(self.eco2));
                self.response.extend_from_slice(&Self::word_crc(self.tvoc));
            }
            0x202f => self
                .response
                .extend_from_slice(&Self::word_crc(SGP30_FEATURE_SET)),
            _ => {}
        }
    }
}

impl I2cDevice for Sgp30 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if read {
            self.index = 0;
        } else {
            self.command = None;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let byte = self.response.get(self.index).copied().unwrap_or(0);
        self.index = (self.index + 1).min(self.response.len());
        byte
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        match self.command.take() {
            Some(high) => self.apply(be16(high, byte)),
            None => self.command = Some(byte),
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.command = None;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ VL53L1X

/// The model id a VL53L1X reports at register 0x010F.
pub const VL53L1X_MODEL_ID: u8 = 0xea;

/// A VL53L1X time-of-flight distance sensor at 0x29.
///
/// Register addresses are 16-bit big-endian: a write phase sends the two
/// address bytes before the data. The model id sits at 0x010F, the range result
/// at 0x0096 (big-endian millimetres, taken from the manifest's `distance`) and
/// the range status at 0x0089, where zero means a valid measurement. `0x010F`
/// is the datasheet's IDENTIFICATION__MODEL_ID register; the value it reports
/// there is 0xEA (some datasheets print it as the 0x010F register name, which
/// is what a probe commonly logs).
pub struct Vl53l1x {
    registers: BTreeMap<u16, u8>,
    pointer: u16,
    prefix: [u8; 2],
    prefix_len: usize,
    reading: bool,
    distance_mm: u16,
}

impl Vl53l1x {
    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        Self::with_distance(attr_u16(attrs, "distance", 100))
    }

    pub fn with_distance(distance_mm: u16) -> Self {
        let mut device = Self {
            registers: BTreeMap::new(),
            pointer: 0,
            prefix: [0; 2],
            prefix_len: 0,
            reading: false,
            distance_mm,
        };
        device.seed();
        device
    }

    fn seed(&mut self) {
        self.registers.insert(0x010f, VL53L1X_MODEL_ID);
        let [high, low] = self.distance_mm.to_be_bytes();
        self.registers.insert(0x0096, high);
        self.registers.insert(0x0097, low);
        // 0 means range valid, the value a completed measurement reports.
        self.registers.insert(0x0089, 0x00);
    }

    pub fn distance_mm(&self) -> u16 {
        self.distance_mm
    }

    pub fn model_id(&self) -> u8 {
        self.registers
            .get(&0x010f)
            .copied()
            .unwrap_or(VL53L1X_MODEL_ID)
    }

    pub fn range_status(&self) -> u8 {
        self.registers.get(&0x0089).copied().unwrap_or(0)
    }

    fn register(&self, address: u16) -> u8 {
        self.registers.get(&address).copied().unwrap_or(0)
    }
}

impl I2cDevice for Vl53l1x {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if !read {
            self.prefix_len = 0;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let value = self.register(self.pointer);
        self.pointer = self.pointer.wrapping_add(1);
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.prefix_len < 2 {
            self.prefix[self.prefix_len] = byte;
            self.prefix_len += 1;
            if self.prefix_len == 2 {
                self.pointer = u16::from_be_bytes(self.prefix);
            }
        } else {
            self.registers.insert(self.pointer, byte);
            self.pointer = self.pointer.wrapping_add(1);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.prefix_len = 0;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn a_pcf8591_returns_the_previous_conversion_then_the_selected_channel() {
        let mut adc = Pcf8591::from_attrs(&attrs(&[("ain0", "1.65"), ("ain1", "0.0")]));
        assert!(adc.write(0x00));
        adc.stop();
        adc.connect(true);
        assert_eq!(adc.read(), 0, "first byte is the previous conversion");
        assert_eq!(adc.read(), 128);
        adc.stop();
        adc.connect(false);
        adc.write(0x04); // auto-increment, channel 0
        assert!(adc.write(0x80)); // DAC value
        assert_eq!(adc.dac(), 0x80);
        assert_eq!(adc.pointer(), 0);
    }

    #[test]
    fn an_ads1115_converts_a_single_ended_input() {
        let mut adc = Ads111x::from_attrs(&attrs(&[("a0", "1.024")]), 16);
        adc.connect(false);
        assert!(adc.write(0x01));
        assert!(adc.write(0xc3));
        assert!(adc.write(0x83));
        adc.stop();
        assert_eq!(adc.conversion_raw(), 0x2000);
        assert_eq!(adc.bits(), 16);
        adc.connect(false);
        adc.write(0x00);
        adc.connect(true);
        assert_eq!(be16(adc.read(), adc.read()), 0x2000);
        adc.stop();
    }

    #[test]
    fn an_ads1015_left_aligns_its_twelve_bit_result() {
        let adc = Ads111x::from_attrs(&attrs(&[("a0", "1.024")]), 12);
        assert_eq!(adc.bits(), 12);
        assert_eq!(adc.conversion_raw(), 0x4000);
    }

    #[test]
    fn a_sht31_reports_temperature_and_humidity_with_crcs() {
        let mut sensor = Sht31::from_attrs(&attrs(&[("temperature", "25"), ("humidity", "50")]));
        sensor.connect(false);
        sensor.write(0x24);
        sensor.write(0x00);
        sensor.stop();
        let measurement = sensor.measurement();
        assert_eq!(be16(measurement[0], measurement[1]), 26_214);
        assert_eq!(measurement[2], crc8_sensirion(&measurement[..2]));
        assert_eq!(be16(measurement[3], measurement[4]), 32_768);
        sensor.connect(true);
        for byte in measurement {
            assert_eq!(sensor.read(), byte);
        }
        assert_eq!(sensor.temperature_c(), 25.0);
    }

    #[test]
    fn a_bh1750_reads_its_high_resolution_lux() {
        let mut sensor = Bh1750::from_attrs(&attrs(&[("lux", "100")]));
        assert_eq!(sensor.lux(), 0.0, "unpowered");
        sensor.connect(false);
        sensor.write(0x01);
        sensor.write(0x10);
        sensor.stop();
        assert_eq!(sensor.lux(), 100.0);
        sensor.connect(true);
        assert_eq!(be16(sensor.read(), sensor.read()), 120);
        sensor.stop();
        sensor.connect(false);
        sensor.write(0x00);
        assert!(!sensor.powered());
    }

    #[test]
    fn a_tsl2561_answers_its_id_and_channel_words() {
        let sensor = Tsl2561::from_attrs(&attrs(&[("lux", "100")]));
        assert_eq!(sensor.id(), 0x50);
        assert_eq!(sensor.channel0(), 1000);
        let mut sensor = sensor;
        sensor.connect(false);
        sensor.write(0x80 | 0x0a);
        sensor.connect(true);
        assert_eq!(sensor.read(), 0x50);
        sensor.stop();
    }

    #[test]
    fn a_veml7700_returns_little_endian_als_counts() {
        let mut sensor = Veml7700::from_attrs(&attrs(&[("lux", "250")]));
        assert_eq!(sensor.als_raw(), 500);
        sensor.connect(false);
        sensor.write(0x04);
        sensor.connect(true);
        assert_eq!(le16(sensor.read(), sensor.read()), 500);
    }

    #[test]
    fn a_tcs34725_identifies_itself_and_reports_rgbc() {
        let mut sensor = Tcs34725::from_attrs(&attrs(&[("red", "1000")]));
        assert_eq!(sensor.id(), 0x44);
        assert_eq!(sensor.rgba(), (1000, 1000, 1000, 500));
        sensor.connect(false);
        sensor.write(0x80 | 0x12);
        sensor.connect(true);
        assert_eq!(sensor.read(), 0x44);
        sensor.stop();
        sensor.connect(false);
        sensor.write(0x80 | 0x20 | 0x16);
        sensor.connect(true);
        assert_eq!(le16(sensor.read(), sensor.read()), 1000);
    }

    #[test]
    fn a_vl53l0x_starts_a_range_and_reports_millimetres() {
        let mut sensor = Vl53l0x::from_attrs(&attrs(&[("distance", "321")]));
        sensor.connect(false);
        sensor.write(0x00);
        sensor.write(0x01);
        sensor.stop();
        assert_eq!(sensor.distance_mm(), 321);
        sensor.connect(false);
        sensor.write(0x1e);
        sensor.connect(true);
        assert_eq!(be16(sensor.read(), sensor.read()), 321);
        assert_eq!(sensor.distance_mm(), 321);
    }

    #[test]
    fn a_pca9685_decodes_the_led_counters_into_duty() {
        let mut driver = Pca9685::from_attrs(&attrs(&[]));
        assert_eq!(driver.frequency(), 50);
        driver.connect(false);
        driver.write(0x06);
        for byte in [0x00, 0x00, 0x00, 0x08] {
            driver.write(byte);
        }
        driver.stop();
        assert_eq!(driver.counters(0), Some((0, 2048)));
        assert_eq!(driver.duty(0), 2048);
        assert_eq!(driver.duty_permille(0), 500);
        assert_eq!(driver.duty(16), 0);
    }

    #[test]
    fn an_mcp4725_latches_a_fast_write_and_reads_it_back() {
        let mut dac = Mcp4725::from_attrs(&attrs(&[("value", "0")]));
        dac.connect(false);
        dac.write(0x0f);
        dac.write(0xff);
        dac.stop();
        assert_eq!(dac.value(), 4095);
        dac.connect(true);
        assert_eq!(dac.read(), 0, "status byte");
        assert_eq!(be16(dac.read(), dac.read()), 4095);
        assert!((dac.volts(3.3) - 3.3).abs() < 0.001);
    }

    #[test]
    fn an_ina219_reports_the_voltage_and_current_it_was_given() {
        let monitor = Ina219::from_attrs(&attrs(&[
            ("busVoltage", "5"),
            ("shuntMillivolts", "10"),
            ("currentMilliamps", "100"),
        ]));
        assert_eq!(monitor.bus_voltage(), 5.0);
        assert!((monitor.current() - 0.1).abs() < 0.0001);
        assert_eq!(monitor.register(0), Some(0x399f));
    }

    #[test]
    fn a_bme280_answers_its_chip_id_and_raw_words() {
        let sensor = Bme280::from_attrs(&attrs(&[
            ("temperature", "25"),
            ("pressure", "101325"),
            ("humidity", "50"),
        ]));
        assert_eq!(sensor.id(), 0x60);
        assert!(sensor.temperature_raw() > 300_000);
        assert!(sensor.pressure_raw() > 300_000);
        assert_eq!(sensor.humidity_raw(), 16_384);
    }

    #[test]
    fn an_apds9960_identifies_itself_and_reports_proximity() {
        let sensor = Apds9960::from_attrs(&attrs(&[("proximity", "42")]));
        assert_eq!(sensor.id(), 0xab);
        assert_eq!(sensor.proximity(), 42);
        assert_eq!(sensor.rgba(), (1000, 2000, 1000, 500));
    }

    #[test]
    fn an_mlx90614_encodes_celsius_in_tenth_millikelvin_counts() {
        let mut sensor = Mlx90614::from_attrs(&attrs(&[("object", "25"), ("ambient", "22")]));
        sensor.connect(false);
        sensor.write(0x07);
        sensor.connect(true);
        let low = sensor.read();
        let high = sensor.read();
        let raw = u16::from_le_bytes([low, high]);
        assert!((raw as f32 * 0.02 - 298.15).abs() < 0.02);
        sensor.stop();
        sensor.connect(false);
        sensor.write(0x06);
        sensor.connect(true);
        let ambient = u16::from_le_bytes([sensor.read(), sensor.read()]);
        assert!((ambient as f32 * 0.02 - 295.15).abs() < 0.02);
    }

    fn onewire_reset(sensor: &mut Ds18b20, at: &mut Tick) {
        sensor.set_pin(PinId(0), Level::Low, *at);
        *at += 500_000;
        sensor.set_pin(PinId(0), Level::High, *at);
        sensor.advance(*at + OW_PRESENCE_DELAY + OW_PRESENCE_LOW);
        *at += 1_000_000;
    }

    fn onewire_write(sensor: &mut Ds18b20, byte: u8, at: &mut Tick) {
        for bit in 0..8 {
            sensor.set_pin(PinId(0), Level::Low, *at);
            *at += if byte & (1 << bit) != 0 {
                6_000
            } else {
                60_000
            };
            sensor.set_pin(PinId(0), Level::High, *at);
            *at += 10_000;
        }
    }

    fn onewire_read(sensor: &mut Ds18b20, at: &mut Tick) -> u8 {
        let mut byte = 0;
        for bit in 0..8 {
            sensor.set_pin(PinId(0), Level::Low, *at);
            *at += 5_000;
            // A one is the released line pulled high by the master's pull-up; the
            // model has no pull-up, so "not driven low" is what was read.
            if sensor.drive(PinId(0)) != Drive::strong(Level::Low) {
                byte |= 1 << bit;
            }
            sensor.set_pin(PinId(0), Level::High, *at);
            *at += 50_000;
        }
        byte
    }

    #[test]
    fn a_ds18b20_answers_a_reset_with_a_presence_pulse() {
        let mut sensor = Ds18b20::new(0, 25.0, [0x28; 8]);
        sensor.set_pin(PinId(0), Level::Low, 0);
        sensor.set_pin(PinId(0), Level::High, 500_000);
        let start = sensor.next_event().expect("presence is scheduled");
        sensor.advance(start);
        assert_eq!(sensor.drive(PinId(0)), Drive::strong(Level::Low));
        sensor.advance(start + OW_PRESENCE_LOW);
        assert_eq!(sensor.drive(PinId(0)), Drive::HIGH_Z);
    }

    #[test]
    fn a_ds18b20_converts_and_reports_its_scratchpad_temperature() {
        let mut sensor = Ds18b20::new(0, 25.0, [0x28, 1, 2, 3, 4, 5, 6, 7]);
        let mut at = 0;
        onewire_reset(&mut sensor, &mut at);
        onewire_write(&mut sensor, 0xcc, &mut at); // skip ROM
        onewire_write(&mut sensor, 0x44, &mut at); // convert T
        assert_eq!(sensor.conversions(), 1);
        onewire_reset(&mut sensor, &mut at);
        onewire_write(&mut sensor, 0xcc, &mut at);
        onewire_write(&mut sensor, 0xbe, &mut at); // read scratchpad
        let scratchpad = sensor.scratchpad();
        for expected in scratchpad {
            assert_eq!(onewire_read(&mut sensor, &mut at), expected);
        }
        assert_eq!(i16::from_le_bytes([scratchpad[0], scratchpad[1]]), 400);
        let mut check = scratchpad;
        check[8] = 0;
        assert_eq!(scratchpad[8], crc8_dallas(&check[..8]));
    }

    #[test]
    fn a_ds18b20_read_rom_returns_its_device_id() {
        let id = [0x28, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x11];
        let mut sensor = Ds18b20::new(0, 20.0, id);
        let mut at = 0;
        onewire_reset(&mut sensor, &mut at);
        onewire_write(&mut sensor, 0x33, &mut at);
        let mut read = [0u8; 8];
        for byte in &mut read {
            *byte = onewire_read(&mut sensor, &mut at);
        }
        assert_eq!(read, id);
    }

    #[test]
    fn a_bme680_answers_its_chip_id_and_derived_raw_words() {
        let sensor = Bme680::from_attrs(&attrs(&[
            ("temperature", "25"),
            ("pressure", "101325"),
            ("humidity", "50"),
            ("gas", "100000"),
        ]));
        assert_eq!(sensor.id(), 0x61);
        assert_eq!(sensor.temperature_raw(), 416_000);
        assert_eq!(sensor.pressure_raw(), 101_325);
        assert_eq!(sensor.humidity_raw(), 32_768);
        assert_eq!(sensor.gas_resistance_kohm(), 100);

        let mut bus = crate::bus::I2c::new();
        bus.attach(0x76, Box::new(sensor)).unwrap();
        assert_eq!(bus.write_read(0x76, &[0xd0], 1), Some(vec![0x61]));
        assert_eq!(
            bus.write_read(0x76, &[0x8a], 2),
            Some(vec![0x10, 0x70]),
            "the calibration block starts at 0x8A"
        );
        assert!(bus.write(0x76, &[0x74, 0x01]), "ctrl_meas accepts a write");
        assert_eq!(bus.write_read(0x76, &[0x74], 1), Some(vec![0x01]));
    }

    #[test]
    fn an_lm75_reports_nine_bit_temperature_and_set_points() {
        let mut sensor = Lm75::from_attrs(&attrs(&[("temperature", "25")]));
        assert!((sensor.temperature_celsius() - 25.0).abs() < 0.001);
        sensor.connect(false);
        sensor.write(0x00);
        sensor.connect(true);
        assert_eq!(sensor.read(), 0x19);
        assert_eq!(sensor.read(), 0x00);
        sensor.stop();

        // The configuration register at 0x01 is one byte.
        sensor.connect(false);
        sensor.write(0x01);
        sensor.write(0x03);
        sensor.stop();
        assert_eq!(sensor.config(), 0x03);
        sensor.connect(false);
        sensor.write(0x01);
        sensor.connect(true);
        assert_eq!(sensor.read(), 0x03);
        sensor.stop();

        assert!((sensor.hysteresis_celsius() - 75.0).abs() < 0.001);
        assert!((sensor.overtemp_celsius() - 80.0).abs() < 0.001);
    }

    #[test]
    fn a_tmp102_reads_a_left_justified_twelve_bit_temperature() {
        let mut bus = crate::bus::I2c::new();
        bus.attach(
            0x48,
            Box::new(Tmp102::from_attrs(&attrs(&[("temperature", "25")]))),
        )
        .unwrap();
        assert_eq!(
            bus.write_read(0x48, &[0x00], 2),
            Some(vec![0x19, 0x00]),
            "25 °C is 400 counts shifted into the high nibble"
        );
        assert!(bus.write(0x48, &[0x01, 0x60, 0xa0]));
        assert_eq!(bus.write_read(0x48, &[0x01], 2), Some(vec![0x60, 0xa0]));
    }

    #[test]
    fn an_adxl345_reports_its_id_and_little_endian_acceleration() {
        let mut bus = crate::bus::I2c::new();
        bus.attach(
            0x53,
            Box::new(Adxl345::from_attrs(&attrs(&[
                ("x", "1.0"),
                ("y", "-1.0"),
                ("z", "0.5"),
            ]))),
        )
        .unwrap();
        assert_eq!(
            bus.write_read(0x53, &[0x00], 1),
            Some(vec![ADXL345_DEVICE_ID])
        );
        assert!(
            bus.write(0x53, &[0x2d, 0x08]),
            "the measure bit is writable"
        );
        let data = bus.write_read(0x53, &[0x32], 6).unwrap();
        assert_eq!(i16::from_le_bytes([data[0], data[1]]), 256);
        assert_eq!(i16::from_le_bytes([data[2], data[3]]), -256);
        assert_eq!(i16::from_le_bytes([data[4], data[5]]), 128);

        // A wider range changes the scale of the seeded data.
        assert!(bus.write(0x53, &[0x31, 0x03]), "±16 g");
        let data = bus.write_read(0x53, &[0x32], 2).unwrap();
        assert_eq!(i16::from_le_bytes([data[0], data[1]]), 32);
    }

    #[test]
    fn a_qmc5883l_answers_its_id_drdy_and_field_words() {
        let mut bus = crate::bus::I2c::new();
        bus.attach(
            0x0d,
            Box::new(Qmc5883l::from_attrs(&attrs(&[
                ("x", "10"),
                ("y", "-20"),
                ("z", "50"),
            ]))),
        )
        .unwrap();
        assert_eq!(bus.write_read(0x0d, &[0x0d], 1), Some(vec![0xff]));
        assert_eq!(bus.write_read(0x0d, &[0x06], 1), Some(vec![0x01]));
        let data = bus.write_read(0x0d, &[0x00], 6).unwrap();
        assert_eq!(i16::from_le_bytes([data[0], data[1]]), 1_200);
        assert_eq!(i16::from_le_bytes([data[2], data[3]]), -2_400);
        assert_eq!(i16::from_le_bytes([data[4], data[5]]), 6_000);
        assert_eq!(bus.write_read(0x0d, &[0x09], 1), Some(vec![0x1d]));

        let sensor = Qmc5883l::from_attrs(&attrs(&[("x", "10")]));
        assert!((sensor.field_ut()[0] - 10.0).abs() < 0.01);
    }

    #[test]
    fn an_sgp30_answers_feature_set_and_crc_checked_iaq_words() {
        let mut sensor = Sgp30::with_reading(812, 45);
        // get_feature_set_version: two bytes plus CRC.
        sensor.connect(false);
        sensor.write(0x20);
        sensor.write(0x2f);
        sensor.stop();
        sensor.connect(true);
        let high = sensor.read();
        let low = sensor.read();
        let crc = sensor.read();
        sensor.stop();
        assert_eq!(u16::from_be_bytes([high, low]), SGP30_FEATURE_SET);
        assert_eq!(crc, crc8_sensirion(&[high, low]));

        // iaq_init is accepted.
        sensor.connect(false);
        sensor.write(0x20);
        sensor.write(0x03);
        sensor.stop();
        assert!(sensor.initialized());

        // measure_iaq: eCO2 and TVOC, each followed by its CRC.
        sensor.connect(false);
        sensor.write(0x20);
        sensor.write(0x08);
        sensor.stop();
        sensor.connect(true);
        let bytes: Vec<u8> = (0..6).map(|_| sensor.read()).collect();
        sensor.stop();
        assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), 812);
        assert_eq!(bytes[2], crc8_sensirion(&bytes[..2]));
        assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 45);
        assert_eq!(bytes[5], crc8_sensirion(&bytes[3..5]));
    }

    #[test]
    fn a_vl53l1x_uses_sixteen_bit_addresses_for_id_and_distance() {
        let mut bus = crate::bus::I2c::new();
        bus.attach(
            0x29,
            Box::new(Vl53l1x::from_attrs(&attrs(&[("distance", "321")]))),
        )
        .unwrap();
        assert_eq!(
            bus.write_read(0x29, &[0x01, 0x0f], 1),
            Some(vec![VL53L1X_MODEL_ID])
        );
        let distance = bus.write_read(0x29, &[0x00, 0x96], 2).unwrap();
        assert_eq!(be16(distance[0], distance[1]), 321);
        assert_eq!(
            bus.write_read(0x29, &[0x00, 0x89], 1),
            Some(vec![0x00]),
            "range status 0 is a valid measurement"
        );
    }

    #[test]
    fn unknown_register_writes_do_not_panic_or_move_a_known_value() {
        let mut sensor = Bme280::from_attrs(&attrs(&[]));
        sensor.connect(false);
        for byte in [0xff, 0x00, 0x7f, 0x80, 0x40] {
            assert!(sensor.write(byte));
        }
        sensor.stop();
        assert_eq!(sensor.id(), 0x60);
        let mut dac = Mcp4725::from_attrs(&attrs(&[]));
        dac.connect(false);
        dac.write(0xf0);
        dac.write(0xab);
        assert_eq!(dac.value(), 0x0ab);
    }
}
