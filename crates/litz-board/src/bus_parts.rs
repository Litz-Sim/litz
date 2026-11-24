//! Builtin devices that answer bus transactions.
//!
//! Every model here is a register machine: an I2C write opens a phase, the
//! first byte after the address selects a register or control mode, and a read
//! phase returns the register file and advances its pointer. That is the shape
//! of the real parts and the shape the browser drivers already speak, so a
//! driver written for the real part runs unchanged.
//!
//! The models expose their own typed read-back (`pixel`, `time`, `port`,
//! `accel_g`, `channel_raw`) beside the transaction callbacks. A test, or a
//! future inspector, can then ask an SSD1306 what is on its glass without
//! inventing a transaction no controller would send.

use crate::bus::{I2cDevice, SpiDevice, UartPeer};
use crate::displays::Hd44780;
use crate::flatten::FlatPart;
use crate::storage::{attr, read_image, relative_image_path};
use litz_core::{Level, Part, PinId, Tick};
use std::any::Any;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ------------------------------------------------------------------ SSD1306

const SSD1306_WIDTH: usize = 128;
const SSD1306_HEIGHT: usize = 64;
const SSD1306_PAGES: usize = SSD1306_HEIGHT / 8;

/// Which way the GDDRAM pointer walks after a data byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddressMode {
    Horizontal,
    Vertical,
    Page,
}

/// A 128x64 monochrome OLED controller with a GDDRAM framebuffer.
///
/// The I2C write stream starts with a control byte: bit 6 clear selects the
/// command stream, bit 6 set the data stream. Commands set the page/column
/// window exactly as the SSD1306 datasheet describes, and data bytes land in
/// the framebuffer in page order (bit 0 is the top row of a page).
pub struct Ssd1306 {
    framebuffer: [u8; SSD1306_WIDTH * SSD1306_PAGES],
    column: u8,
    page: u8,
    column_start: u8,
    column_end: u8,
    page_start: u8,
    page_end: u8,
    mode: AddressMode,
    display_on: bool,
    reading: bool,
    command_stream: bool,
    first_write: bool,
    /// `(command, argument index)` while a command waits for its parameters.
    pending: Option<(u8, u8)>,
    arguments: [u8; 2],
    /// Grows on every byte written into GDDRAM, so a host can tell whether the
    /// panel changed since it last read the framebuffer.
    frame_version: u64,
}

impl Ssd1306 {
    pub fn new() -> Self {
        Self {
            framebuffer: [0; SSD1306_WIDTH * SSD1306_PAGES],
            column: 0,
            page: 0,
            column_start: 0,
            column_end: (SSD1306_WIDTH - 1) as u8,
            page_start: 0,
            page_end: (SSD1306_PAGES - 1) as u8,
            mode: AddressMode::Page,
            display_on: false,
            reading: false,
            command_stream: true,
            first_write: true,
            pending: None,
            arguments: [0; 2],
            frame_version: 0,
        }
    }

    pub fn display_on(&self) -> bool {
        self.display_on
    }

    /// The lit state of one pixel, in display coordinates.
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        if x >= SSD1306_WIDTH || y >= SSD1306_HEIGHT {
            return false;
        }
        let index = (y / 8) * SSD1306_WIDTH + x;
        self.framebuffer[index] & (1 << (y % 8)) != 0
    }

    /// Number of lit pixels, a cheap checksum for a test or a thumbnail.
    pub fn lit_pixels(&self) -> usize {
        self.framebuffer
            .iter()
            .map(|byte| byte.count_ones() as usize)
            .sum()
    }

    /// The whole GDDRAM as the panel lays it out.
    ///
    /// The tuple is `(bytes, width, height)`. The bytes are page-major: page 0
    /// columns 0..127, then page 1, and so on, so `page * 128 + column` indexes
    /// a 128x64 panel. **Within each byte the least significant bit (bit 0) is
    /// the top row of the page and bit 7 the bottom row** — least significant
    /// bit first, running top to bottom down the eight rows of the page. This
    /// is the controller's own RAM order, not display-row-major order. The
    /// slice is the live framebuffer, not a copy.
    pub fn framebuffer(&self) -> (&[u8], u32, u32) {
        (
            &self.framebuffer,
            SSD1306_WIDTH as u32,
            SSD1306_HEIGHT as u32,
        )
    }

    /// A counter that grows on every byte written into GDDRAM.
    ///
    /// A host reads it before and after a run to learn whether the screen
    /// changed, without diffing all of GDDRAM.
    pub fn frame_version(&self) -> u64 {
        self.frame_version
    }

    /// The address that is currently selected by the column/page pointers.
    pub fn framebuffer_byte(&self, page: usize, column: usize) -> Option<u8> {
        if page >= SSD1306_PAGES || column >= SSD1306_WIDTH {
            return None;
        }
        Some(self.framebuffer[page * SSD1306_WIDTH + column])
    }

    fn arguments_for(command: u8) -> Option<u8> {
        match command {
            0x20 | 0x81 | 0x8d | 0xd3 | 0xd5 | 0xd9 | 0xda | 0xdb => Some(1),
            0x21 | 0x22 => Some(2),
            _ => None,
        }
    }

    fn command(&mut self, byte: u8) {
        if let Some((command, index)) = self.pending {
            self.arguments[index as usize] = byte;
            let needed = Self::arguments_for(command).unwrap_or(0);
            if index + 1 < needed {
                self.pending = Some((command, index + 1));
            } else {
                self.apply_pending(command);
                self.pending = None;
            }
            return;
        }
        if Self::arguments_for(byte).is_some() {
            self.pending = Some((byte, 0));
            return;
        }
        match byte {
            0x00..=0x0f => self.column = (self.column & 0xf0) | (byte & 0x0f),
            0x10..=0x1f => self.column = (self.column & 0x0f) | ((byte & 0x0f) << 4),
            0x20 => {}
            0x21 | 0x22 => {}
            0x40..=0x7f => {}
            0xae => self.display_on = false,
            0xaf => self.display_on = true,
            0xb0..=0xb7 => self.page = byte & 0x07,
            _ => {}
        }
    }

    fn apply_pending(&mut self, command: u8) {
        match command {
            0x20 => {
                self.mode = match self.arguments[0] & 0x03 {
                    0 => AddressMode::Horizontal,
                    1 => AddressMode::Vertical,
                    _ => AddressMode::Page,
                }
            }
            0x21 => {
                self.column_start = self.arguments[0] & 0x7f;
                self.column_end = self.arguments[1] & 0x7f;
                self.column = self.column_start;
            }
            0x22 => {
                self.page_start = self.arguments[0] & 0x07;
                self.page_end = self.arguments[1] & 0x07;
                self.page = self.page_start;
            }
            // Contrast, charge pump, timing and orientation do not change the
            // pixels this model reports.
            _ => {}
        }
    }

    fn data(&mut self, byte: u8) {
        let index = (self.page as usize & 0x07) * SSD1306_WIDTH + (self.column as usize & 0x7f);
        if index < self.framebuffer.len() {
            self.framebuffer[index] = byte;
            self.frame_version += 1;
        }
        match self.mode {
            AddressMode::Horizontal => {
                self.column = self.column.wrapping_add(1);
                if self.column > self.column_end {
                    self.column = self.column_start;
                    self.page = self.page.wrapping_add(1);
                    if self.page > self.page_end {
                        self.page = self.page_start;
                    }
                }
            }
            AddressMode::Vertical => {
                self.page = self.page.wrapping_add(1);
                if self.page > self.page_end {
                    self.page = self.page_start;
                    self.column = self.column.wrapping_add(1);
                    if self.column > self.column_end {
                        self.column = self.column_start;
                    }
                }
            }
            AddressMode::Page => {
                self.column = self.column.wrapping_add(1) & 0x7f;
            }
        }
    }
}

impl Default for Ssd1306 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Ssd1306 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if !read {
            self.first_write = true;
        }
    }

    fn read(&mut self) -> u8 {
        // The part has no readable GDDRAM over I2C; a real one answers status
        // bytes. Tests read pixels through `pixel` instead.
        0
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.command_stream = byte & 0x40 == 0;
            self.first_write = false;
            return true;
        }
        if self.command_stream {
            self.command(byte);
        } else {
            self.data(byte);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
        self.first_write = true;
        self.pending = None;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ DS3231

const DS3231_REGISTERS: usize = 0x13;

/// A resolved calendar time, as a DS3231 register read reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// A DS3231 real-time clock: BCD time registers behind a register pointer.
///
/// The time is fixed for the run — the part has no tick source of its own — and
/// comes from the manifest's `time` attribute. `set_time` also rewrites the
/// registers, which is what a host control does.
pub struct Ds3231 {
    registers: [u8; DS3231_REGISTERS],
    pointer: u8,
    first_write: bool,
    reading: bool,
}

fn bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

fn from_bcd(value: u8) -> u8 {
    (value >> 4) * 10 + (value & 0x0f)
}

fn parse_time(text: &str) -> Option<DateTime> {
    let (date, clock) = text.split_once('T')?;
    let mut date = date.split('-');
    let year: u16 = date.next()?.parse().ok()?;
    let month: u8 = date.next()?.parse().ok()?;
    let day: u8 = date.next()?.parse().ok()?;
    let mut clock = clock.split(':');
    let hour: u8 = clock.next()?.parse().ok()?;
    let minute: u8 = clock.next()?.parse().ok()?;
    let second: u8 = clock.next()?.parse().ok()?;
    Some(DateTime {
        year,
        month,
        day,
        weekday: 1,
        hour,
        minute,
        second,
    })
}

fn attr_f32(attrs: &BTreeMap<String, String>, name: &str, default: f32) -> f32 {
    attrs
        .get(name)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(default)
}

impl Ds3231 {
    pub fn new() -> Self {
        Self::with_time(DateTime {
            year: 2024,
            month: 1,
            day: 1,
            weekday: 1,
            hour: 0,
            minute: 0,
            second: 0,
        })
    }

    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let mut clock = attrs
            .get("time")
            .and_then(|text| parse_time(text))
            .unwrap_or(DateTime {
                year: 2024,
                month: 1,
                day: 1,
                weekday: 1,
                hour: 0,
                minute: 0,
                second: 0,
            });
        if let Some(weekday) = attrs
            .get("weekday")
            .and_then(|text| text.trim().parse().ok())
        {
            clock.weekday = weekday;
        }
        let temperature = attr_f32(attrs, "temperature", 25.0);
        Self::with_time_at(clock, temperature)
    }

    pub fn with_time(time: DateTime) -> Self {
        Self::with_time_at(time, 25.0)
    }

    pub fn with_time_at(time: DateTime, temperature: f32) -> Self {
        let mut registers = [0u8; DS3231_REGISTERS];
        registers[0x0e] = 0x1c; // RS2/RS1 set, 1 Hz square wave on INTCN
        registers[0x0f] = 0x88; // OSF clear, EN32kHz on
        let mut clock = Self {
            registers,
            pointer: 0,
            first_write: true,
            reading: false,
        };
        clock.write_time(time);
        clock.set_temperature(temperature);
        clock
    }

    pub fn time(&self) -> DateTime {
        let year = from_bcd(self.registers[0x06]);
        DateTime {
            year: 2000 + u16::from(year),
            month: from_bcd(self.registers[0x05] & 0x1f),
            day: from_bcd(self.registers[0x04]),
            weekday: self.registers[0x03] & 0x07,
            hour: from_bcd(self.registers[0x02] & 0x3f),
            minute: from_bcd(self.registers[0x01]),
            second: from_bcd(self.registers[0x00] & 0x7f),
        }
    }

    pub fn set_time(&mut self, time: DateTime) {
        self.write_time(time);
    }

    fn write_time(&mut self, time: DateTime) {
        self.registers[0x00] = bcd(time.second);
        self.registers[0x01] = bcd(time.minute);
        self.registers[0x02] = bcd(time.hour);
        self.registers[0x03] = time.weekday & 0x07;
        self.registers[0x04] = bcd(time.day);
        self.registers[0x05] = bcd(time.month) | (if time.year >= 2100 { 0x80 } else { 0 });
        self.registers[0x06] = bcd((time.year % 100) as u8);
    }

    pub fn temperature(&self) -> f32 {
        let raw = i16::from(self.registers[0x11]) << 8 | i16::from(self.registers[0x12] & 0xc0);
        f32::from(raw) / 256.0
    }

    fn set_temperature(&mut self, temperature: f32) {
        let raw = (temperature * 256.0).round() as i16;
        self.registers[0x11] = (raw >> 8) as u8;
        self.registers[0x12] = (raw & 0xc0) as u8;
    }

    /// The register byte a driver would read, without moving the pointer.
    pub fn register(&self, address: u8) -> Option<u8> {
        self.registers.get(address as usize).copied()
    }
}

impl Default for Ds3231 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Ds3231 {
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
        let value = self.registers[self.pointer as usize % DS3231_REGISTERS];
        self.pointer = (self.pointer + 1) % DS3231_REGISTERS as u8;
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte % DS3231_REGISTERS as u8;
            self.first_write = false;
        } else {
            let index = self.pointer as usize;
            self.registers[index] = byte;
            self.pointer = (self.pointer + 1) % DS3231_REGISTERS as u8;
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

// ------------------------------------------------------------------ PCF8574

/// An 8-bit I2C port expander with quasi-bidirectional pins.
///
/// A written 0 drives the pin low; a written 1 releases it so an external
/// circuit may pull it low. A read returns the latches AND the external inputs,
/// which is why `inputs` defaults to 0xFF (idle high, internal pull-ups).
pub struct Pcf8574 {
    latches: u8,
    inputs: u8,
    reading: bool,
}

impl Pcf8574 {
    pub fn new() -> Self {
        Self {
            latches: 0xff,
            inputs: 0xff,
            reading: false,
        }
    }

    pub fn with_inputs(inputs: u8) -> Self {
        Self {
            latches: 0xff,
            inputs,
            reading: false,
        }
    }

    /// What the last write latched onto the output pins.
    pub fn latched(&self) -> u8 {
        self.latches
    }

    /// The external levels the port sees, before the output latch is applied.
    pub fn set_inputs(&mut self, inputs: u8) {
        self.inputs = inputs;
    }
}

impl Default for Pcf8574 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Pcf8574 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
    }

    fn read(&mut self) -> u8 {
        if self.reading {
            self.latches & self.inputs
        } else {
            0
        }
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        self.latches = byte;
        true
    }

    fn stop(&mut self) {
        self.reading = false;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ MPU6050

const MPU6050_REGISTERS: usize = 0x80;
const ACCEL_LSB_PER_G: f32 = 16384.0;
const GYRO_LSB_PER_DPS: f32 = 131.0;

/// An MPU6050 six-axis motion sensor with a register file.
///
/// WHO_AM_I at 0x75 answers 0x68. The acceleration and rate registers are
/// seeded from the manifest so a firmware written against the real scaling
/// (16384 LSB/g, 131 LSB/(deg/s)) reads a physically sensible value.
pub struct Mpu6050 {
    registers: [u8; MPU6050_REGISTERS],
    pointer: u8,
    first_write: bool,
    reading: bool,
    accel_g: [f32; 3],
    gyro_dps: [f32; 3],
}

impl Mpu6050 {
    pub fn new() -> Self {
        Self::with_motion([0.0, 0.0, 1.0], [0.0, 0.0, 0.0])
    }

    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let accel = [
            attr_f32(attrs, "ax", 0.0),
            attr_f32(attrs, "ay", 0.0),
            attr_f32(attrs, "az", 1.0),
        ];
        let gyro = [
            attr_f32(attrs, "gx", 0.0),
            attr_f32(attrs, "gy", 0.0),
            attr_f32(attrs, "gz", 0.0),
        ];
        Self::with_motion(accel, gyro)
    }

    pub fn with_motion(accel_g: [f32; 3], gyro_dps: [f32; 3]) -> Self {
        let mut sensor = Self {
            registers: [0; MPU6050_REGISTERS],
            pointer: 0,
            first_write: true,
            reading: false,
            accel_g,
            gyro_dps,
        };
        sensor.registers[0x75] = 0x68;
        sensor.registers[0x6b] = 0x01; // PWR_MGMT_1: clock from the X gyro
        sensor.registers[0x1c] = 0x00; // ±2 g
        sensor.registers[0x1b] = 0x00; // ±250 deg/s
        sensor.load_motion();
        sensor
    }

    fn load_motion(&mut self) {
        for (value, address) in self.accel_g.iter().zip([0x3b, 0x3d, 0x3f]) {
            let raw = (value * ACCEL_LSB_PER_G).round().clamp(-32768.0, 32767.0) as i16;
            self.registers[address] = (raw >> 8) as u8;
            self.registers[address + 1] = raw as u8;
        }
        for (value, address) in self.gyro_dps.iter().zip([0x43, 0x45, 0x47]) {
            let raw = (value * GYRO_LSB_PER_DPS).round().clamp(-32768.0, 32767.0) as i16;
            self.registers[address] = (raw >> 8) as u8;
            self.registers[address + 1] = raw as u8;
        }
    }

    /// Acceleration in g, from the register file.
    pub fn accel_g(&self) -> [f32; 3] {
        let raw = |address: usize| {
            i16::from_be_bytes([self.registers[address], self.registers[address + 1]]) as f32
                / ACCEL_LSB_PER_G
        };
        [raw(0x3b), raw(0x3d), raw(0x3f)]
    }

    /// Angular rate in degrees per second, from the register file.
    pub fn gyro_dps(&self) -> [f32; 3] {
        let raw = |address: usize| {
            i16::from_be_bytes([self.registers[address], self.registers[address + 1]]) as f32
                / GYRO_LSB_PER_DPS
        };
        [raw(0x43), raw(0x45), raw(0x47)]
    }

    pub fn register(&self, address: u8) -> Option<u8> {
        self.registers.get(address as usize).copied()
    }

    /// Set one motion control from a scenario: `accel_x`, `gyro_z`, and the
    /// short spellings a part editor uses. The register file is rebuilt so the
    /// next I2C read sees the new value without a reboot.
    pub fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        let Ok(value) = value.trim().parse::<f32>() else {
            return false;
        };
        match name {
            "accel_x" | "ax" => self.accel_g[0] = value.clamp(-16.0, 16.0),
            "accel_y" | "ay" => self.accel_g[1] = value.clamp(-16.0, 16.0),
            "accel_z" | "az" => self.accel_g[2] = value.clamp(-16.0, 16.0),
            "gyro_x" | "gx" => self.gyro_dps[0] = value.clamp(-250.0, 250.0),
            "gyro_y" | "gy" => self.gyro_dps[1] = value.clamp(-250.0, 250.0),
            "gyro_z" | "gz" => self.gyro_dps[2] = value.clamp(-250.0, 250.0),
            _ => return false,
        }
        self.load_motion();
        true
    }
}

impl Default for Mpu6050 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Mpu6050 {
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
        let value = self.registers[self.pointer as usize % MPU6050_REGISTERS];
        self.pointer = (self.pointer + 1) % MPU6050_REGISTERS as u8;
        if self.pointer == 0x76 {
            // WHO_AM_I and the other identity bytes never change.
            self.registers[0x75] = 0x68;
        }
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte % MPU6050_REGISTERS as u8;
            self.first_write = false;
        } else {
            let index = self.pointer as usize;
            if index != 0x75 {
                self.registers[index] = byte;
            }
            self.pointer = (self.pointer + 1) % MPU6050_REGISTERS as u8;
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

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ MCP3008

/// An 8-channel 10-bit SPI ADC.
///
/// A conversion is the classic three-byte exchange: `0x01`, `0x80 |
/// channel << 4`, then a null byte. The result arrives as two bits in the
/// second reply byte and eight in the third. Channel voltages come from the
/// manifest and decide the conversion, so a firmware reads the same values a
/// real divider would give.
pub struct Mcp3008 {
    channels: [f32; 8],
    reference: f32,
    selected: u8,
    raw: u16,
    conversions: u32,
}

impl Mcp3008 {
    pub fn new() -> Self {
        Self::with_channels([0.0; 8], 3.3)
    }

    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let mut channels = [0.0; 8];
        for (index, channel) in channels.iter_mut().enumerate() {
            *channel = attr_f32(attrs, &format!("v{index}"), 0.0);
        }
        let reference = attr_f32(attrs, "vref", 3.3);
        Self::with_channels(channels, reference)
    }

    pub fn with_channels(channels: [f32; 8], reference: f32) -> Self {
        Self {
            channels,
            reference: reference.max(0.0),
            selected: 0,
            raw: 0,
            conversions: 0,
        }
    }

    pub fn reference(&self) -> f32 {
        self.reference
    }

    pub fn channel_volts(&self, channel: usize) -> Option<f32> {
        self.channels.get(channel).copied()
    }

    /// The 10-bit conversion result for one channel.
    pub fn channel_raw(&self, channel: usize) -> u16 {
        let volts = self.channels.get(channel).copied().unwrap_or(0.0);
        let scale = if self.reference > 0.0 {
            1023.0 / self.reference
        } else {
            0.0
        };
        (volts * scale).round().clamp(0.0, 1023.0) as u16
    }

    /// The channel the last conversion addressed.
    pub fn selected_channel(&self) -> u8 {
        self.selected
    }

    /// The last 10-bit result the part shifted out.
    pub fn last_raw(&self) -> u16 {
        self.raw
    }

    pub fn conversions(&self) -> u32 {
        self.conversions
    }
}

impl Default for Mcp3008 {
    fn default() -> Self {
        Self::new()
    }
}

impl SpiDevice for Mcp3008 {
    fn select(&mut self, _selected: bool) {}

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        // Byte 1 carries SGL/D2/D1/D0 in bits 4..=6.
        let channel = if tx.len() >= 2 {
            (tx[1] >> 4) & 0x07
        } else {
            0
        };
        self.selected = channel;
        self.raw = self.channel_raw(channel as usize);
        self.conversions += 1;
        let mut out = vec![0u8; rx_len];
        match rx_len {
            0 => {}
            1 => out[0] = (self.raw >> 8) as u8 & 0x03,
            2 => out[1] = self.raw as u8,
            _ => {
                out[1] = ((self.raw >> 8) as u8) & 0x03;
                out[2] = self.raw as u8;
            }
        }
        out
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ LCD over I2C

/// A 16x2 or 20x4 character LCD behind a PCF8574 I2C backpack.
///
/// The backpack is a port expander wired to the HD44780's 4-bit bus in the
/// usual way: `P0` is RS, `P1` RW, `P2` E, `P3` the backlight, and `P4..P7`
/// the four data lines. Every byte written to the expander's address is one
/// port latch; the model translates it into pin changes on the controller it
/// owns, so the display's DDRAM and text are the same state a parallel module
/// would hold. The backlight bit is accepted and ignored.
pub struct LcdI2c {
    lcd: Hd44780,
    columns: usize,
    rows: usize,
    reading: bool,
}

impl LcdI2c {
    pub fn new(columns: usize, rows: usize) -> Self {
        Self {
            lcd: Hd44780::new(columns, rows),
            columns,
            rows,
            reading: false,
        }
    }

    pub fn from_part(part: &FlatPart) -> Self {
        let (columns, rows) =
            if part.type_key == "litz-lcd2004-i2c" || part.type_key == "litz-lcd2004" {
                (20, 4)
            } else {
                (16, 2)
            };
        Self::new(columns, rows)
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn text(&self) -> String {
        self.lcd.text()
    }

    pub fn line(&self, row: usize) -> String {
        self.lcd.line(row)
    }

    pub fn display_on(&self) -> bool {
        self.lcd.display_on()
    }

    /// Apply one expander latch to the controller's pins.
    fn apply(&mut self, byte: u8) {
        let level = |high: bool| if high { Level::High } else { Level::Low };
        self.lcd.set_pin(PinId(0), level(byte & 0x01 != 0), 0); // RS
        self.lcd.set_pin(PinId(1), level(byte & 0x02 != 0), 0); // RW
        for slot in 0..4 {
            self.lcd
                .set_pin(PinId(3 + slot), level(byte & (1 << (4 + slot)) != 0), 0);
        }
        // The controller latches on E's falling edge, which is the usual
        // "write the byte with E high, then write it with E low" sequence.
        self.lcd.set_pin(PinId(2), level(byte & 0x04 != 0), 0);
    }
}

impl I2cDevice for LcdI2c {
    fn connect(&mut self, read: bool) {
        self.reading = read;
    }

    fn read(&mut self) -> u8 {
        // The backpack is write-only; a real one returns the port latch.
        0
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        self.apply(byte);
        true
    }

    fn stop(&mut self) {
        self.reading = false;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ DS1307

const DS1307_REGISTERS: usize = 0x40;

/// A DS1307 real-time clock: BCD time registers behind a register pointer.
///
/// It is the simpler sibling of the DS3231 — no temperature, no aging offset —
/// and lives at the same 0x68 address. Registers 0x08..0x3F are battery-backed
/// SRAM and read back whatever was written.
pub struct Ds1307 {
    registers: [u8; DS1307_REGISTERS],
    pointer: u8,
    first_write: bool,
    reading: bool,
}

impl Ds1307 {
    pub fn new() -> Self {
        Self::with_time(DateTime {
            year: 2024,
            month: 1,
            day: 1,
            weekday: 1,
            hour: 0,
            minute: 0,
            second: 0,
        })
    }

    pub fn from_attrs(attrs: &BTreeMap<String, String>) -> Self {
        let mut clock = attrs
            .get("time")
            .and_then(|text| parse_time(text))
            .unwrap_or(DateTime {
                year: 2024,
                month: 1,
                day: 1,
                weekday: 1,
                hour: 0,
                minute: 0,
                second: 0,
            });
        if let Some(weekday) = attrs
            .get("weekday")
            .and_then(|text| text.trim().parse().ok())
        {
            clock.weekday = weekday;
        }
        Self::with_time(clock)
    }

    pub fn with_time(time: DateTime) -> Self {
        let mut clock = Self {
            registers: [0u8; DS1307_REGISTERS],
            pointer: 0,
            first_write: true,
            reading: false,
        };
        clock.write_time(time);
        clock
    }

    pub fn time(&self) -> DateTime {
        let year = from_bcd(self.registers[0x06]);
        DateTime {
            year: 2000 + u16::from(year),
            month: from_bcd(self.registers[0x05] & 0x1f),
            day: from_bcd(self.registers[0x04]),
            weekday: self.registers[0x03] & 0x07,
            hour: from_bcd(self.registers[0x02] & 0x3f),
            minute: from_bcd(self.registers[0x01]),
            second: from_bcd(self.registers[0x00] & 0x7f),
        }
    }

    pub fn set_time(&mut self, time: DateTime) {
        self.write_time(time);
    }

    fn write_time(&mut self, time: DateTime) {
        // CH (bit 7 of seconds) stays clear: the oscillator is running.
        self.registers[0x00] = bcd(time.second) & 0x7f;
        self.registers[0x01] = bcd(time.minute);
        self.registers[0x02] = bcd(time.hour) & 0x3f;
        self.registers[0x03] = time.weekday & 0x07;
        self.registers[0x04] = bcd(time.day);
        self.registers[0x05] = bcd(time.month) & 0x1f;
        self.registers[0x06] = bcd((time.year % 100) as u8);
    }

    /// The register byte a driver would read, without moving the pointer.
    pub fn register(&self, address: u8) -> Option<u8> {
        self.registers.get(address as usize).copied()
    }
}

impl Default for Ds1307 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Ds1307 {
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
        let value = self.registers[self.pointer as usize % DS1307_REGISTERS];
        self.pointer = (self.pointer + 1) % DS1307_REGISTERS as u8;
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte % DS1307_REGISTERS as u8;
            self.first_write = false;
        } else {
            let index = self.pointer as usize;
            self.registers[index] = byte;
            self.pointer = (self.pointer + 1) % DS1307_REGISTERS as u8;
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

// ------------------------------------------------------------------ RC522

const RC522_REGISTERS: usize = 0x40;
/// The MFRC522 VersionReg value the reference silicon reports.
const RC522_VERSION: u8 = 0x92;

/// An MFRC522 RFID front end, modelled on its SPI register interface.
///
/// The first SPI byte is the address phase: bit 7 selects a read, bits 6..1
/// carry the register number. A read shifts the register value out on the
/// bytes after the address; a write takes the following bytes as the value.
/// Only the identity register matters to a probe — `VersionReg` (0x37) answers
/// 0x92 — but the register file is writable so a driver's setup sequence does
/// not fault.
pub struct Rc522 {
    registers: [u8; RC522_REGISTERS],
    selected: bool,
}

impl Rc522 {
    pub fn new() -> Self {
        let mut registers = [0u8; RC522_REGISTERS];
        registers[0x37] = RC522_VERSION;
        registers[0x01] = 0x20; // CommandReg idle
        registers[0x02] = 0x80; // ComIEn
        Self {
            registers,
            selected: false,
        }
    }

    /// The value VersionReg reports, which identifies the silicon.
    pub fn version(&self) -> u8 {
        self.registers[0x37]
    }

    pub fn register(&self, address: u8) -> Option<u8> {
        self.registers.get(address as usize).copied()
    }

    fn address(control: u8) -> (usize, bool) {
        (((control >> 1) & 0x3f) as usize, control & 0x80 != 0)
    }
}

impl Default for Rc522 {
    fn default() -> Self {
        Self::new()
    }
}

impl SpiDevice for Rc522 {
    fn select(&mut self, selected: bool) {
        self.selected = selected;
    }

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        if !self.selected || tx.is_empty() {
            return vec![0xff; rx_len];
        }
        let (mut address, read) = Self::address(tx[0]);
        let mut out = vec![0u8; rx_len];
        if read {
            // The address byte's MISO is undefined; report zero and then the
            // register file, auto-incrementing as a burst read does.
            for slot in out.iter_mut().skip(1) {
                *slot = self.registers[address];
                address = (address + 1) % RC522_REGISTERS;
            }
        } else {
            for &byte in tx.iter().skip(1) {
                self.registers[address] = byte;
                address = (address + 1) % RC522_REGISTERS;
            }
        }
        out
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ fingerprint

/// The acknowledgement a fingerprint sensor returned for one packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FingerprintAck {
    pub package_id: u8,
    pub instruction: u8,
    pub confirmation: u8,
}

/// A fingerprint sensor on UART, modelled on the R307/GT-521F packet protocol.
///
/// A command is `EF 01`, a four-byte address, a package id, a big-endian length
/// and `length` bytes of instruction, parameters and a 16-bit checksum. The
/// model consumes complete, checksummed packets and answers each with the
/// generic acknowledgement packet (`instruction 0x00`, confirmation `0x00`),
/// which is enough for a driver's probe and handshake to succeed.
pub struct Fingerprint {
    address: u32,
    buffer: Vec<u8>,
    pending: Vec<u8>,
    acknowledgements: u32,
    last_ack: Option<FingerprintAck>,
}

impl Fingerprint {
    pub fn new() -> Self {
        Self {
            address: 0xffff_ffff,
            buffer: Vec::new(),
            pending: Vec::new(),
            acknowledgements: 0,
            last_ack: None,
        }
    }

    pub fn acknowledgements(&self) -> u32 {
        self.acknowledgements
    }

    pub fn last_ack(&self) -> Option<FingerprintAck> {
        self.last_ack
    }

    /// The packet id a response would carry.
    pub const ACK_PACKAGE_ID: u8 = 0x07;

    fn checksum(bytes: &[u8]) -> u16 {
        bytes
            .iter()
            .fold(0u16, |sum, byte| sum.wrapping_add(u16::from(*byte)))
    }

    fn respond(&mut self, package_id: u8, instruction: u8) {
        // Content is the instruction, one parameter (the confirmation code)
        // and the two-byte checksum; `length` counts all four.
        let mut content = vec![0x00, 0x00];
        let length = (content.len() + 2) as u16;
        let mut packet = vec![0xef, 0x01];
        packet.extend_from_slice(&self.address.to_be_bytes());
        packet.push(Self::ACK_PACKAGE_ID);
        packet.extend_from_slice(&length.to_be_bytes());
        packet.append(&mut content);
        let checksum = Self::checksum(&packet[6..]);
        packet.extend_from_slice(&checksum.to_be_bytes());
        self.pending.extend_from_slice(&packet);
        self.acknowledgements += 1;
        self.last_ack = Some(FingerprintAck {
            package_id,
            instruction,
            confirmation: 0x00,
        });
    }

    fn drain_packets(&mut self) {
        loop {
            let Some(start) = self.buffer.windows(2).position(|pair| pair == [0xef, 0x01]) else {
                // Keep the last byte: it may be the first half of a header.
                if self.buffer.len() > 1 {
                    self.buffer.drain(..self.buffer.len() - 1);
                }
                return;
            };
            if start > 0 {
                self.buffer.drain(..start);
            }
            if self.buffer.len() < 9 {
                return;
            }
            let address = u32::from_be_bytes([
                self.buffer[2],
                self.buffer[3],
                self.buffer[4],
                self.buffer[5],
            ]);
            let package_id = self.buffer[6];
            let length = usize::from(u16::from_be_bytes([self.buffer[7], self.buffer[8]]));
            let total = 9 + length;
            if self.buffer.len() < total || length < 3 {
                return;
            }
            let checksum = u16::from_be_bytes([self.buffer[total - 2], self.buffer[total - 1]]);
            if Self::checksum(&self.buffer[6..total - 2]) == checksum {
                self.address = address;
                let instruction = self.buffer[9];
                self.respond(package_id, instruction);
            }
            self.buffer.drain(..total);
        }
    }
}

impl Default for Fingerprint {
    fn default() -> Self {
        Self::new()
    }
}

impl UartPeer for Fingerprint {
    fn receive(&mut self, _at: Tick, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
        self.drain_packets();
    }

    fn poll(&mut self, _at: Tick) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

// ------------------------------------------------------------------ SH1106

/// A 128x64 monochrome OLED controller at 0x3C.
///
/// The SH1106 uses the same I2C control-byte split as the SSD1306 (`0x00`
/// command, `0x40` data) and the same page-addressing commands, so this model
/// delegates to [`Ssd1306`]'s page-addressing core rather than duplicating it.
/// Inferred: the real part has 132 columns of GDDRAM with the visible 128
/// offset by two columns; this model presents the visible window as a plain
/// 128-column panel.
pub struct Sh1106 {
    core: Ssd1306,
}

impl Sh1106 {
    pub fn new() -> Self {
        Self {
            core: Ssd1306::new(),
        }
    }

    pub fn display_on(&self) -> bool {
        self.core.display_on()
    }

    pub fn pixel(&self, x: usize, y: usize) -> bool {
        self.core.pixel(x, y)
    }

    pub fn lit_pixels(&self) -> usize {
        self.core.lit_pixels()
    }

    pub fn framebuffer(&self) -> (&[u8], u32, u32) {
        self.core.framebuffer()
    }

    pub fn frame_version(&self) -> u64 {
        self.core.frame_version()
    }

    pub fn framebuffer_byte(&self, page: usize, column: usize) -> Option<u8> {
        self.core.framebuffer_byte(page, column)
    }
}

impl Default for Sh1106 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Sh1106 {
    fn connect(&mut self, read: bool) {
        self.core.connect(read);
    }

    fn read(&mut self) -> u8 {
        self.core.read()
    }

    fn write(&mut self, byte: u8) -> bool {
        self.core.write(byte)
    }

    fn stop(&mut self) {
        self.core.stop();
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ W25Q32

/// A Winbond-style SPI NOR flash, 4 MiB by default.
///
/// The model is a plain byte array, so a firmware can program a page and read
/// it back through the part exactly as it would on real silicon. The command
/// set is the one an SPI flash driver probes with: `0x90`/`0xAB` read the
/// manufacturer and device ids, `0x03` reads, `0x06` sets the write-enable
/// latch, `0x02` programs a page, `0x20` erases a 4 KiB sector and `0x05`
/// reads status register 1.
///
/// Programming and erasing are instantaneous, so WIP (status bit 0) is always
/// clear; a driver's poll therefore succeeds on the first attempt. The
/// write-enable latch (bit 1) is cleared when a program or erase completes,
/// which is the datasheet's behaviour.
///
/// A diagram can change the capacity (`size`, a power of two from 64 KiB to
/// 16 MiB; the device id follows the Winbond code for that size), load an
/// image, put files on a FAT volume, and name a `save` path the board writes
/// back when the guest programmed or erased the array.
pub struct W25Q32 {
    memory: Vec<u8>,
    status: u8,
    selected: bool,
    /// The Winbond capacity code: `0x16` for 4 MiB, one step per doubling.
    device_id: u8,
    save: Option<PathBuf>,
    dirty: bool,
}

impl W25Q32 {
    /// 4 MiB, the W25Q32's array size.
    pub const CAPACITY: usize = 4 * 1024 * 1024;
    /// The Winbond manufacturer id `0x90` and `0x9F` report.
    pub const MANUFACTURER_ID: u8 = 0xef;
    /// The device id of the 4 MiB part; smaller and larger arrays step the
    /// code down and up.
    pub const DEVICE_ID: u8 = 0x16;
    /// The smallest array the model accepts, a W25Q512's: 64 KiB.
    pub const MIN_CAPACITY: usize = 64 * 1024;
    /// The largest array 24-bit addressing reaches: 16 MiB.
    pub const MAX_CAPACITY: usize = 16 * 1024 * 1024;

    pub fn new() -> Self {
        Self::with_capacity(Self::CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            memory: vec![0xff; capacity],
            status: 0,
            selected: false,
            device_id: Self::capacity_code(capacity),
            save: None,
            dirty: false,
        }
    }

    /// Build a part from a manifest: `size`, `image`, `files` and `save`.
    pub fn from_attrs(part: &FlatPart) -> Result<Self, String> {
        let image_text = attr(part, "image");
        let files_text = attr(part, "files");
        let size_text = attr(part, "size");
        if !image_text.is_empty() && !files_text.is_empty() {
            return Err("`image` and `files` are exclusive: load an image or build one".into());
        }
        let capacity = if size_text.is_empty() {
            Self::CAPACITY
        } else {
            crate::block::parse_size(size_text)
                .map_err(|error| format!("size {size_text:?}: {error}"))?
        };
        if !(Self::MIN_CAPACITY..=Self::MAX_CAPACITY).contains(&capacity) {
            return Err(format!(
                "size {capacity} is outside the {}..={} byte range of a 24-bit addressed part",
                Self::MIN_CAPACITY,
                Self::MAX_CAPACITY
            ));
        }
        if !capacity.is_power_of_two() {
            return Err(format!(
                "size {capacity} is not a power of two, so no Winbond part matches it"
            ));
        }
        let mut flash = Self::with_capacity(capacity);
        if !image_text.is_empty() {
            let bytes = read_image(&relative_image_path(image_text)?)
                .map_err(|error| format!("image {image_text:?}: {error}"))?;
            if bytes.len() > capacity {
                return Err(format!(
                    "image {image_text:?} is {} bytes, larger than the {capacity}-byte part",
                    bytes.len()
                ));
            }
            flash.memory[..bytes.len()].copy_from_slice(&bytes);
        } else if !files_text.is_empty() {
            // An erased flash reads 0xFF; the formatter writes the volume's
            // own sectors and leaves the rest of the array erased.
            let mut image = crate::block::Image::blank(capacity, 0xff);
            let tree = crate::fat::read_tree(Path::new(files_text))
                .map_err(|error| format!("files {files_text:?}: {error}"))?;
            let files: Vec<crate::fat::File<'_>> = tree
                .iter()
                .map(|(path, data)| crate::fat::File {
                    path,
                    data: data.as_slice(),
                })
                .collect();
            crate::fat::format(&mut image, &files)
                .map_err(|error| format!("files {files_text:?}: {error}"))?;
            flash.memory = image.to_vec();
        }
        if let Some(text) = part.attrs.get("save").map(String::as_str).map(str::trim)
            && !text.is_empty()
        {
            flash.save = Some(relative_image_path(text)?);
        }
        Ok(flash)
    }

    /// The JEDEC capacity code: the base-2 logarithm of the byte count, which
    /// is how Winbond names its arrays (`0x16` is 2^22 bytes, the W25Q32).
    fn capacity_code(capacity: usize) -> u8 {
        (usize::BITS - capacity.leading_zeros() - 1) as u8
    }

    pub fn capacity(&self) -> usize {
        self.memory.len()
    }

    pub fn memory(&self) -> &[u8] {
        &self.memory
    }

    /// Whether the guest programmed or erased anything since construction.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// The file the diagram asked the board to write at the end of a run.
    pub fn save_target(&self) -> Option<&Path> {
        self.save.as_deref()
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, &self.memory)
    }

    /// Save only when the array changed, so a runner can call it
    /// unconditionally.
    pub fn save_changed(&mut self, path: &Path) -> std::io::Result<bool> {
        if !self.dirty {
            return Ok(false);
        }
        self.save_to(path)?;
        self.dirty = false;
        Ok(true)
    }

    pub fn write_in_progress(&self) -> bool {
        self.status & 0x01 != 0
    }

    pub fn write_enabled(&self) -> bool {
        self.status & 0x02 != 0
    }

    fn address(tx: &[u8]) -> Option<usize> {
        (tx.len() >= 4)
            .then(|| (usize::from(tx[1]) << 16) | (usize::from(tx[2]) << 8) | usize::from(tx[3]))
    }
}

impl Default for W25Q32 {
    fn default() -> Self {
        Self::new()
    }
}

impl SpiDevice for W25Q32 {
    fn select(&mut self, selected: bool) {
        self.selected = selected;
    }

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        if !self.selected || tx.is_empty() {
            return vec![0xff; rx_len];
        }
        let mut out = vec![0xff; rx_len];
        match tx[0] {
            // Read data: three address bytes, then the array streams out.
            0x03 => {
                if let Some(mut address) = Self::address(tx) {
                    let capacity = self.memory.len();
                    for slot in out.iter_mut().skip(4) {
                        *slot = self.memory[address % capacity];
                        address = address.wrapping_add(1);
                    }
                }
            }
            // Read status register 1.
            0x05 => {
                for slot in out.iter_mut().skip(1) {
                    *slot = self.status;
                }
            }
            // Write enable.
            0x06 => self.status |= 0x02,
            // Page program: bytes wrap within the addressed 256-byte page.
            0x02 => {
                if self.status & 0x02 != 0 {
                    if let Some(address) = Self::address(tx) {
                        let capacity = self.memory.len();
                        let page = address & !0xff;
                        for (offset, &byte) in tx.iter().skip(4).enumerate() {
                            let target = page | ((address + offset) & 0xff);
                            self.memory[target % capacity] = byte;
                        }
                        self.dirty = true;
                    }
                    self.status &= !0x02;
                }
            }
            // Sector erase: 4 KiB, back to the erased 0xFF state.
            0x20 => {
                if self.status & 0x02 != 0 {
                    if let Some(address) = Self::address(tx) {
                        let capacity = self.memory.len();
                        let start = address & !0x0fff;
                        let end = (start + 0x1000).min(capacity);
                        self.memory[start..end].fill(0xff);
                        self.dirty = true;
                    }
                    self.status &= !0x02;
                }
            }
            // Manufacturer/device id: three address bytes then 0xEF <id>.
            0x90 => {
                if out.len() > 4 {
                    out[4] = Self::MANUFACTURER_ID;
                }
                if out.len() > 5 {
                    out[5] = self.device_id;
                }
            }
            // Release power-down / device id: three dummy bytes then the id.
            0xab => {
                if out.len() > 4 {
                    out[4] = self.device_id;
                }
            }
            // JEDEC id: manufacturer, memory type, capacity.
            0x9f => {
                for (slot, byte) in
                    out.iter_mut()
                        .skip(1)
                        .zip([Self::MANUFACTURER_ID, 0x40, self.device_id])
                {
                    *slot = byte;
                }
            }
            _ => {}
        }
        out
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ MCP23017

const MCP23017_REGISTERS: usize = 0x16;

/// A 16-bit I2C I/O expander at 0x20.
///
/// Registers follow the MCP23017's bank-0 map: IODIRA/B at 0x00/0x01, IPOLA/B
/// 0x02/0x03, GPPUA/B 0x0C/0x0D, GPIOA/B 0x12/0x13 and OLATA/B 0x14/0x15. An
/// IODIR bit set to one makes that pin an input; a zero makes it an output.
/// Reading GPIO returns the output latch for output pins and the external level
/// for input pins, with IPOL inverting the input level. `set_inputs` is the
/// host hook a test or scenario uses to drive the input side.
pub struct Mcp23017 {
    registers: [u8; MCP23017_REGISTERS],
    inputs: [u8; 2],
    pointer: u8,
    first_write: bool,
    reading: bool,
}

impl Mcp23017 {
    pub fn new() -> Self {
        let mut registers = [0u8; MCP23017_REGISTERS];
        registers[0x00] = 0xff; // IODIRA: all inputs
        registers[0x01] = 0xff; // IODIRB: all inputs
        Self {
            registers,
            inputs: [0xff; 2],
            pointer: 0,
            first_write: true,
            reading: false,
        }
    }

    /// Drive the external level of one port's input pins.
    pub fn set_inputs(&mut self, port: usize, value: u8) {
        if let Some(slot) = self.inputs.get_mut(port) {
            *slot = value;
        }
    }

    /// The direction register of one port; a one bit is an input.
    pub fn iodir(&self, port: usize) -> u8 {
        self.registers[usize::from(port != 0)]
    }

    /// The output latch of one port.
    pub fn output_latch(&self, port: usize) -> u8 {
        self.registers[0x14 + usize::from(port != 0)]
    }

    /// What a GPIO read reports for one port.
    pub fn gpio(&self, port: usize) -> u8 {
        let index = usize::from(port != 0);
        let iodir = self.registers[index];
        let ipol = self.registers[0x02 + index];
        let latch = self.registers[0x14 + index];
        let inputs = self.inputs[index] ^ ipol;
        (latch & !iodir) | (inputs & iodir)
    }

    fn read_register(&self, address: u8) -> u8 {
        match address {
            0x12 => self.gpio(0),
            0x13 => self.gpio(1),
            _ => self.registers[usize::from(address) % MCP23017_REGISTERS],
        }
    }

    fn write_register(&mut self, address: u8, byte: u8) {
        match address {
            // Writing a GPIO register updates that port's output latch.
            0x12 => self.registers[0x14] = byte,
            0x13 => self.registers[0x15] = byte,
            _ => self.registers[usize::from(address) % MCP23017_REGISTERS] = byte,
        }
    }
}

impl Default for Mcp23017 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Mcp23017 {
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
        let value = self.read_register(self.pointer);
        self.pointer = (self.pointer + 1) % MCP23017_REGISTERS as u8;
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte % MCP23017_REGISTERS as u8;
            self.first_write = false;
        } else {
            let address = self.pointer;
            self.write_register(address, byte);
            self.pointer = (self.pointer + 1) % MCP23017_REGISTERS as u8;
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

// ------------------------------------------------------------------ TCA9548A

/// A TCA9548A 8-channel I2C multiplexer at 0x70–0x77.
///
/// The part is a single read/write control register: a written byte latches
/// the channel mask and a read returns it. Inferred: the downstream buses are
/// not modelled—selecting a channel records the mask but does not route
/// transactions to devices behind it, because the board's [`I2c`](crate::bus::I2c)
/// fabric has no path through a multiplexer.
pub struct Tca9548a {
    mask: u8,
    reading: bool,
}

impl Tca9548a {
    pub fn new() -> Self {
        Self {
            mask: 0,
            reading: false,
        }
    }

    pub fn channel_mask(&self) -> u8 {
        self.mask
    }
}

impl Default for Tca9548a {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Tca9548a {
    fn connect(&mut self, read: bool) {
        self.reading = read;
    }

    fn read(&mut self) -> u8 {
        if self.reading { self.mask } else { 0 }
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        self.mask = byte;
        true
    }

    fn stop(&mut self) {
        self.reading = false;
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_command(part: &mut impl I2cDevice, bytes: &[u8]) {
        part.connect(false);
        // Every transaction starts with a control byte; 0x00 selects the
        // command stream.
        assert!(part.write(0x00), "the command control byte was nacked");
        for &byte in bytes {
            assert!(part.write(byte), "a command byte was nacked");
        }
        part.stop();
    }

    fn write_data(part: &mut impl I2cDevice, bytes: &[u8]) {
        part.connect(false);
        assert!(part.write(0x40), "the data control byte was nacked");
        for &byte in bytes {
            assert!(part.write(byte), "a data byte was nacked");
        }
        part.stop();
    }

    #[test]
    fn an_ssd1306_accepts_a_page_write_and_reports_pixels() {
        let mut oled = Ssd1306::new();
        // Page 0, column 0: 0xB0 is page start, 0x00/0x10 the low/high column.
        write_command(&mut oled, &[0xb0, 0x00, 0x10]);
        write_data(&mut oled, &[0xaa]);

        assert_eq!(oled.framebuffer_byte(0, 0), Some(0xaa));
        assert!(!oled.pixel(0, 0), "bit 0 is clear");
        assert!(oled.pixel(0, 1), "bit 1 is set");
        assert!(!oled.pixel(0, 2));
        assert!(oled.pixel(0, 3));
        assert_eq!(oled.lit_pixels(), 4);
    }

    #[test]
    fn an_ssd1306_walks_to_the_next_page_in_horizontal_mode() {
        let mut oled = Ssd1306::new();
        // Horizontal addressing, columns 0..127, pages 0..7.
        write_command(&mut oled, &[0x20, 0x00, 0x21, 0x00, 0x7f, 0x22, 0x00, 0x07]);
        // A full page advances the pointer to page 1, column 0.
        let mut bytes = vec![0xff; 128];
        bytes.push(0x0f);
        write_data(&mut oled, &bytes);
        assert_eq!(oled.framebuffer_byte(0, 0), Some(0xff));
        assert_eq!(oled.framebuffer_byte(1, 0), Some(0x0f));
        assert!(oled.pixel(0, 8));
    }

    #[test]
    fn a_ds3231_reports_the_configured_time_in_bcd() {
        let mut clock = Ds3231::with_time(DateTime {
            year: 2026,
            month: 11,
            day: 20,
            weekday: 5,
            hour: 13,
            minute: 45,
            second: 30,
        });
        // Seconds is BCD: 30 -> 0x30.
        clock.connect(false);
        clock.write(0x00);
        clock.connect(true);
        let seconds = clock.read();
        clock.stop();
        assert_eq!(seconds, 0x30);

        // The typed read-back decodes the same BCD registers.
        let time = clock.time();
        assert_eq!((time.hour, time.minute, time.second), (13, 45, 30));
        assert_eq!(time.year, 2026);
        assert_eq!(clock.register(0x05), Some(0x11));
        assert_eq!(clock.register(0x06), Some(0x26));
    }

    #[test]
    fn a_ds3231_register_pointer_reads_a_block() {
        let clock = Ds3231::new();
        let mut bus = crate::bus::I2c::new();
        bus.attach(0x68, Box::new(clock)).unwrap();
        let bytes = bus
            .write_read(0x68, &[0x00], 3)
            .expect("the clock answered");
        assert_eq!(bytes, vec![0x00, 0x00, 0x00], "midnight is all zero BCD");
    }

    #[test]
    fn a_pcf8574_latches_and_reads_its_port() {
        let mut port = Pcf8574::new();
        port.connect(false);
        assert!(port.write(0x5a));
        port.stop();
        assert_eq!(port.latched(), 0x5a);

        port.connect(true);
        assert_eq!(port.read(), 0x5a);
        port.stop();

        // An external circuit pulling a released pin low shows through.
        port.set_inputs(0x0f);
        port.connect(true);
        assert_eq!(port.read(), 0x0a);
        port.stop();
    }

    #[test]
    fn a_pcf8574_zero_latch_reads_low_even_with_pull_ups() {
        let mut port = Pcf8574::new();
        port.connect(false);
        port.write(0xf0);
        port.stop();
        port.set_inputs(0xff);
        port.connect(true);
        assert_eq!(port.read(), 0xf0);
    }

    #[test]
    fn an_mpu6050_reports_who_am_i_and_the_configured_motion() {
        let mut sensor = Mpu6050::with_motion([0.0, 0.0, 1.0], [0.0, 0.0, 90.0]);
        sensor.connect(false);
        sensor.write(0x75);
        sensor.connect(true);
        assert_eq!(sensor.read(), 0x68, "WHO_AM_I");
        sensor.stop();

        let accel = sensor.accel_g();
        assert!((accel[2] - 1.0).abs() < 0.001, "{accel:?}");
        let gyro = sensor.gyro_dps();
        assert!((gyro[2] - 90.0).abs() < 0.5, "{gyro:?}");
    }

    #[test]
    fn an_mpu6050_acceleration_reads_as_two_bytes_per_axis() {
        let sensor = Mpu6050::with_motion([0.5, -0.5, 1.0], [0.0; 3]);
        // 0.5 g is 8192 = 0x2000.
        assert_eq!(sensor.register(0x3b), Some(0x20));
        assert_eq!(sensor.register(0x3c), Some(0x00));
        assert_eq!(sensor.register(0x3d), Some(0xe0), "-8192 = 0xE000");
        assert_eq!(sensor.register(0x3e), Some(0x00));
    }

    #[test]
    fn an_mcp3008_reads_the_selected_channel() {
        let mut adc = Mcp3008::with_channels([0.0, 1.65, 3.3, 0.0, 0.0, 0.0, 0.0, 0.0], 3.3);
        // Channel 1 at half scale: 0x01, 0x90, 0x00.
        let rx = adc.transfer(&[0x01, 0x90, 0x00], 3);
        assert_eq!(adc.selected_channel(), 1);
        let raw = (u16::from(rx[1] & 0x03) << 8) | u16::from(rx[2]);
        assert_eq!(raw, 512);
        assert_eq!(adc.last_raw(), 512);

        // Channel 2 at full scale.
        let rx = adc.transfer(&[0x01, 0xa0, 0x00], 3);
        assert_eq!((rx[1] & 0x03, rx[2]), (0x03, 0xff));
        assert_eq!(adc.selected_channel(), 2);
        assert_eq!(adc.conversions(), 2);
    }

    #[test]
    fn an_mcp3008_clamps_a_channel_above_the_reference() {
        let adc = Mcp3008::with_channels([5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 3.3);
        assert_eq!(adc.channel_raw(0), 1023);
    }

    /// The PCF8574 backpack mapping: P0 RS, P1 RW, P2 E, P3 backlight,
    /// P4..P7 data.
    fn backpack(rs: bool, nibble: u8) -> u8 {
        let mut byte = (nibble & 0x0f) << 4;
        if rs {
            byte |= 0x01;
        }
        byte | 0x08
    }

    fn pulse(lcd: &mut LcdI2c, rs: bool, nibble: u8) {
        // Write the latch with E high, then again with E low.
        lcd.write(backpack(rs, nibble) | 0x04);
        lcd.write(backpack(rs, nibble));
    }

    fn send_byte(lcd: &mut LcdI2c, rs: bool, value: u8) {
        pulse(lcd, rs, value >> 4);
        pulse(lcd, rs, value & 0x0f);
    }

    fn init_lcd(lcd: &mut LcdI2c) {
        for nibble in [0x3, 0x3, 0x3, 0x2] {
            pulse(lcd, false, nibble);
        }
        send_byte(lcd, false, 0x28); // 4-bit, two lines
        send_byte(lcd, false, 0x0c); // display on
        send_byte(lcd, false, 0x06); // entry increment
        send_byte(lcd, false, 0x01); // clear
    }

    #[test]
    fn an_i2c_lcd_translates_the_backpack_latch_into_text() {
        let mut lcd = LcdI2c::new(16, 2);
        init_lcd(&mut lcd);
        send_byte(&mut lcd, false, 0x80); // DDRAM address 0
        for ch in b"Hi" {
            send_byte(&mut lcd, true, *ch);
        }
        assert_eq!(lcd.text(), "Hi");
        assert_eq!(lcd.line(0).len(), 16);
        assert!(lcd.display_on());
        assert_eq!((lcd.columns(), lcd.rows()), (16, 2));
    }

    #[test]
    fn an_i2c_lcd_board_reports_its_backpack_as_a_device() {
        let lcd = LcdI2c::from_part(&crate::flatten::FlatPart {
            id: "lcd".into(),
            type_key: "litz-lcd2004-i2c".into(),
            engine: crate::flatten::Engine::Builtin("hd44780-i2c".into()),
            pins: Vec::new(),
            pin_kinds: Vec::new(),
            attrs: Default::default(),
            buses: Vec::new(),
            device: None,
        });
        assert_eq!((lcd.columns(), lcd.rows()), (20, 4));
    }

    #[test]
    fn a_ds1307_reports_its_bcd_time_and_keeps_sram() {
        let mut clock = Ds1307::with_time(DateTime {
            year: 2026,
            month: 11,
            day: 20,
            weekday: 5,
            hour: 13,
            minute: 45,
            second: 30,
        });
        // Seconds is BCD 0x30.
        clock.connect(false);
        clock.write(0x00);
        clock.connect(true);
        assert_eq!(clock.read(), 0x30);
        clock.stop();

        let time = clock.time();
        assert_eq!((time.year, time.month, time.day), (2026, 11, 20));
        assert_eq!((time.hour, time.minute, time.second), (13, 45, 30));

        // A byte in the SRAM window reads back: write it, then address it again
        // in a fresh transaction.
        clock.connect(false);
        clock.write(0x10);
        clock.write(0x5a);
        clock.stop();
        assert_eq!(clock.register(0x10), Some(0x5a));
        clock.connect(false);
        clock.write(0x10);
        clock.connect(true);
        assert_eq!(clock.read(), 0x5a);
    }

    #[test]
    fn an_rc522_answers_version_register_over_spi() {
        let mut reader = Rc522::new();
        reader.select(true);
        // 0x37 << 1 | 0x80 = 0xEE reads VersionReg.
        let rx = reader.transfer(&[0xee, 0x00], 2);
        reader.select(false);
        assert_eq!(rx[1], 0x92);
        assert_eq!(reader.version(), 0x92);

        // A write lands in the register file.
        reader.select(true);
        reader.transfer(&[0x0a << 1, 0x55], 0);
        reader.select(false);
        assert_eq!(reader.register(0x0a), Some(0x55));
    }

    fn fingerprint_command(address: u32, package_id: u8, instruction: u8) -> Vec<u8> {
        let mut content = vec![instruction];
        let length = (content.len() + 2) as u16;
        let mut packet = vec![0xef, 0x01];
        packet.extend_from_slice(&address.to_be_bytes());
        packet.push(package_id);
        packet.extend_from_slice(&length.to_be_bytes());
        packet.append(&mut content);
        let checksum = Fingerprint::checksum(&packet[6..]);
        packet.extend_from_slice(&checksum.to_be_bytes());
        packet
    }

    #[test]
    fn a_fingerprint_acknowledges_a_command_packet() {
        let mut sensor = Fingerprint::new();
        let command = fingerprint_command(0xffff_ffff, 0x01, 0x01); // GenImg
        // Deliver it in two chunks to exercise the reassembly.
        sensor.receive(0, &command[..5]);
        assert_eq!(sensor.acknowledgements(), 0);
        sensor.receive(0, &command[5..]);
        assert_eq!(sensor.acknowledgements(), 1);
        assert_eq!(
            sensor.last_ack(),
            Some(FingerprintAck {
                package_id: 0x01,
                instruction: 0x01,
                confirmation: 0x00
            })
        );

        let reply = sensor.poll(0);
        assert_eq!(&reply[..2], &[0xef, 0x01]);
        assert_eq!(reply[6], Fingerprint::ACK_PACKAGE_ID);
        assert_eq!(reply[9], 0x00, "the ack instruction");
        let length = usize::from(u16::from_be_bytes([reply[7], reply[8]]));
        assert_eq!(reply.len(), 9 + length);
        let checksum = u16::from_be_bytes([reply[reply.len() - 2], reply[reply.len() - 1]]);
        assert_eq!(Fingerprint::checksum(&reply[6..reply.len() - 2]), checksum);
    }

    #[test]
    fn a_fingerprint_ignores_a_packet_with_a_bad_checksum() {
        let mut sensor = Fingerprint::new();
        let mut command = fingerprint_command(0xffff_ffff, 0x01, 0x01);
        let last = command.len() - 1;
        command[last] ^= 0xff;
        sensor.receive(0, &command);
        assert_eq!(sensor.acknowledgements(), 0);
        assert!(sensor.poll(0).is_empty());
    }

    #[test]
    fn an_sh1106_draws_through_the_ssd1306_page_core() {
        let mut oled = Sh1106::new();
        write_command(&mut oled, &[0xb0, 0x00, 0x10]);
        write_data(&mut oled, &[0xaa]);
        assert_eq!(oled.framebuffer_byte(0, 0), Some(0xaa));
        assert!(!oled.pixel(0, 0));
        assert!(oled.pixel(0, 1));
        assert_eq!(oled.lit_pixels(), 4);
        assert_eq!(oled.framebuffer().1, 128);
        assert_eq!(oled.framebuffer().2, 64);
    }

    fn flash_part(attrs: &[(&str, &str)]) -> FlatPart {
        FlatPart {
            id: "flash".into(),
            type_key: "litz-w25q32".into(),
            engine: crate::flatten::Engine::Builtin("w25q32".into()),
            pins: Vec::new(),
            pin_kinds: Vec::new(),
            attrs: attrs
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            buses: Vec::new(),
            device: None,
        }
    }

    #[test]
    fn a_flash_part_takes_its_size_and_names_the_matching_winbond() {
        let flash = W25Q32::from_attrs(&flash_part(&[("size", "8MB")])).unwrap();
        assert_eq!(flash.capacity(), 8 << 20);
        assert_eq!(flash.device_id, 0x17, "8 MiB is a W25Q64");
        let mut flash = flash;
        flash.select(true);
        let id = flash.transfer(&[0x9f, 0x00, 0x00, 0x00], 4);
        assert_eq!(&id[1..4], &[0xef, 0x40, 0x17]);
    }

    #[test]
    fn a_flash_size_that_no_part_has_is_refused() {
        for text in ["48MB", "32KB", "128MB"] {
            let error = W25Q32::from_attrs(&flash_part(&[("size", text)]))
                .err()
                .unwrap_or_else(|| panic!("{text:?} should be refused"));
            assert!(!error.is_empty());
        }
    }

    #[test]
    fn files_land_on_a_flash_volume() {
        let dir = Path::new("build").join(format!("flash-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), b"remember\n").unwrap();
        let flash = W25Q32::from_attrs(&flash_part(&[
            ("files", dir.to_str().unwrap()),
            ("size", "1MB"),
        ]))
        .unwrap();
        // Sector zero is the FAT boot sector; the erased rest still reads 0xFF.
        assert_eq!(&flash.memory()[..3], &[0xeb, 0x3c, 0x90]);
        assert_eq!(&flash.memory()[510..512], &[0x55, 0xaa]);
        let image = crate::block::Image::from_bytes(flash.memory().to_vec());
        let files = crate::fat::read_files(&image).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "notes.txt");
        assert_eq!(files[0].1, b"remember\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_program_marks_the_flash_dirty_until_it_is_saved() {
        let mut flash = W25Q32::new();
        assert!(!flash.dirty());
        flash.select(true);
        flash.transfer(&[0x06], 0);
        flash.transfer(&[0x02, 0x00, 0x00, 0x10, b'x'], 0);
        assert!(flash.dirty());
        let dir = Path::new("build").join(format!("flash-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("flash.bin");
        assert!(flash.save_changed(&path).unwrap());
        assert_eq!(std::fs::read(&path).unwrap()[0x10], b'x');
        assert!(!flash.dirty(), "a save clears the flag");
        assert!(
            !flash.save_changed(&path).unwrap(),
            "a clean array is not rewritten"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_w25q32_programs_a_page_and_reads_it_back() {
        let mut flash = W25Q32::new();
        assert_eq!(flash.memory().len(), W25Q32::CAPACITY);
        assert_eq!(flash.memory()[0], 0xff, "an erased NOR array is all ones");

        flash.select(true);
        flash.transfer(&[0x06], 0);
        flash.transfer(&[0x02, 0x00, 0x00, 0x10, b'H', b'i'], 0);
        // The program completed, so WIP is clear and WEL was consumed.
        let status = flash.transfer(&[0x05, 0x00], 2);
        assert_eq!(status[1] & 0x01, 0, "WIP");
        assert_eq!(status[1] & 0x02, 0, "WEL is cleared by a program");

        let read = flash.transfer(&[0x03, 0x00, 0x00, 0x10, 0x00, 0x00], 6);
        assert_eq!(&read[4..6], b"Hi");

        // 0x16 is 2^22 bytes, the W25Q32's 4 MiB array.
        let id = flash.transfer(&[0x90, 0x00, 0x00, 0x00, 0x00, 0x00], 6);
        assert_eq!((id[4], id[5]), (0xef, 0x16));
        let id = flash.transfer(&[0xab, 0x00, 0x00, 0x00, 0x00], 5);
        assert_eq!(id[4], 0x16);

        flash.transfer(&[0x06], 0);
        flash.transfer(&[0x20, 0x00, 0x00, 0x00], 0);
        let erased = flash.transfer(&[0x03, 0x00, 0x00, 0x10, 0x00], 5);
        assert_eq!(erased[4], 0xff);
        assert_eq!(flash.memory()[0x10], 0xff);
        flash.select(false);
    }

    #[test]
    fn an_mcp23017_drives_outputs_and_reads_inputs() {
        let mut expander = Mcp23017::new();
        // Both ports are inputs after reset.
        assert_eq!(expander.iodir(0), 0xff);

        // Port A as outputs, then latch a value through OLATA.
        expander.connect(false);
        expander.write(0x00);
        expander.write(0x00);
        expander.stop();
        expander.connect(false);
        expander.write(0x14);
        expander.write(0x5a);
        expander.stop();
        assert_eq!(expander.output_latch(0), 0x5a);
        assert_eq!(expander.gpio(0), 0x5a);

        expander.connect(false);
        expander.write(0x12);
        expander.connect(true);
        assert_eq!(expander.read(), 0x5a);
        expander.stop();

        // Back to inputs: the external level shows through.
        expander.connect(false);
        expander.write(0x00);
        expander.write(0xff);
        expander.stop();
        expander.set_inputs(0, 0xa5);
        expander.connect(false);
        expander.write(0x12);
        expander.connect(true);
        assert_eq!(expander.read(), 0xa5);
        expander.stop();

        // IPOLA inverts the input pins.
        expander.connect(false);
        expander.write(0x02);
        expander.write(0xff);
        expander.stop();
        assert_eq!(expander.gpio(0), !0xa5);

        // GPPUA is a plain read/write register.
        expander.connect(false);
        expander.write(0x0c);
        expander.write(0x0f);
        expander.stop();
        expander.connect(false);
        expander.write(0x0c);
        expander.connect(true);
        assert_eq!(expander.read(), 0x0f);
        expander.stop();
    }

    #[test]
    fn a_tca9548a_stores_and_reports_its_channel_mask() {
        let mut bus = crate::bus::I2c::new();
        bus.attach(0x70, Box::new(Tca9548a::new())).unwrap();
        assert!(bus.write(0x70, &[0x05]));
        assert_eq!(bus.read(0x70, 1), Some(vec![0x05]));
        assert!(bus.write(0x70, &[0x80]));
        assert_eq!(bus.read(0x70, 1), Some(vec![0x80]));
    }
}
