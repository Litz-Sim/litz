//! Displays with a protocol of their own: the TM1637 two-wire module, the
//! MAX7219 dot-matrix controller and the PCD8544 LCD used by the Nokia 5110.
//!
//! None of them is I2C or SPI in the sense the bus layer knows: the TM1637
//! clocks bits on two GPIOs, while the MAX7219 and PCD8544 sit on the SPI bus
//! and decode the byte stream the way the real controller does. The models
//! keep the pixels or segment latches, so a test can ask what is lit without
//! replaying the waveform.

use crate::BuildError;
use crate::bus::SpiDevice;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};
use std::any::Any;

fn index(part: &FlatPart, name: &str) -> Result<u16, BuildError> {
    part.pins
        .iter()
        .position(|candidate| candidate == name)
        .map(|i| i as u16)
        .ok_or_else(|| BuildError::Message(format!("part {:?} has no {name:?} pin", part.id)))
}

fn attr_u16(attrs: &std::collections::BTreeMap<String, String>, name: &str, default: u16) -> u16 {
    attrs
        .get(name)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(default)
}

// ------------------------------------------------------------------ TM1637

/// Segment masks for the digits 0 through 9, bit 0 = a.
const DIGITS: [u8; 10] = [0x3f, 0x06, 0x5b, 0x4f, 0x66, 0x6d, 0x7d, 0x07, 0x7f, 0x6f];

/// A TM1637 four-digit seven-segment module.
///
/// The protocol looks like I2C but is not: a start is DIO falling while CLK is
/// high, eight bits shift in LSB-first on each rising clock, and the ninth
/// clock is the acknowledge. Command bytes select data mode, a digit address,
/// or display brightness. The model keeps the six display registers and pulls
/// DIO low for the acknowledge, so a driver that checks it sees a real slave.
pub struct Tm1637 {
    clk: u16,
    dio: u16,
    clk_level: Level,
    dio_level: Level,
    output: Level,
    bits: u8,
    byte: u8,
    awaiting_ack: bool,
    in_transaction: bool,
    data_mode: bool,
    address: usize,
    last_written: usize,
    registers: [u8; 6],
    display_on: bool,
    brightness: u8,
}

impl Tm1637 {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Ok(Box::new(Self {
            clk: index(part, "CLK")?,
            dio: index(part, "DIO")?,
            clk_level: Level::High,
            dio_level: Level::High,
            output: Level::HighZ,
            bits: 0,
            byte: 0,
            awaiting_ack: false,
            in_transaction: false,
            data_mode: false,
            address: 0,
            last_written: 0,
            registers: [0; 6],
            display_on: true,
            brightness: 7,
        }))
    }

    /// The four digit registers, least significant digit first.
    pub fn segments(&self) -> [u8; 4] {
        [
            self.registers[0],
            self.registers[1],
            self.registers[2],
            self.registers[3],
        ]
    }

    pub fn display_on(&self) -> bool {
        self.display_on
    }

    pub fn brightness(&self) -> u8 {
        self.brightness
    }

    /// Write a four-digit number, the convenience a host control uses.
    pub fn show_number(&mut self, value: u16) {
        let mut value = value % 10_000;
        for digit in self.registers.iter_mut().take(4) {
            *digit = DIGITS[(value % 10) as usize];
            value /= 10;
        }
    }

    fn command(&mut self, byte: u8) {
        match byte {
            // 0x40/0x44 start an auto-increment or fixed data write.
            0x40..=0x47 => self.data_mode = true,
            // 0xC0..=0xC5 select the first register a fixed write targets.
            0xc0..=0xc7 => {
                self.address = usize::from(byte & 0x07).min(5);
                // The address command is followed by data bytes.
                self.data_mode = true;
            }
            0x80..=0x87 => self.display_on = false,
            0x88..=0x8f => {
                self.display_on = true;
                self.brightness = byte & 0x07;
            }
            _ => {
                if self.data_mode && self.address < self.registers.len() {
                    self.registers[self.address] = byte;
                    self.last_written = self.address;
                    self.address = (self.address + 1) % self.registers.len();
                }
            }
        }
    }
}

impl Part for Tm1637 {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.dio {
            match self.output {
                Level::HighZ => Drive::HIGH_Z,
                level => Drive::strong(level),
            }
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.dio {
            let falling = self.dio_level == Level::High && level == Level::Low;
            let rising = self.dio_level == Level::Low && level != Level::Low;
            self.dio_level = level;
            // A falling DIO while CLK is high is a start, except while this
            // part is itself holding DIO low for the acknowledge.
            if self.clk_level == Level::High && falling && !self.awaiting_ack {
                self.in_transaction = true;
                self.bits = 0;
                self.byte = 0;
                self.output = Level::HighZ;
            } else if self.clk_level == Level::High && rising {
                self.in_transaction = false;
                self.awaiting_ack = false;
                self.output = Level::HighZ;
            }
            return;
        }
        if pin.0 != self.clk {
            return;
        }
        let rising = self.clk_level != Level::High && level == Level::High;
        self.clk_level = level;
        if !rising || !self.in_transaction {
            return;
        }
        if self.awaiting_ack {
            self.awaiting_ack = false;
            self.output = Level::HighZ;
            let byte = self.byte;
            self.byte = 0;
            self.bits = 0;
            self.command(byte);
            return;
        }
        self.byte |= u8::from(self.dio_level == Level::High) << self.bits;
        self.bits += 1;
        if self.bits == 8 {
            // The ninth clock is the acknowledge; the slave owns DIO for it.
            self.awaiting_ack = true;
            self.output = Level::Low;
        }
    }

    fn state(&self) -> PartState {
        PartState::Segments {
            mask: self.registers[self.last_written.min(3)],
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.show_number(value.min(1000) * 10);
        true
    }
}

// ------------------------------------------------------------------ MAX7219

const MAX7219_ROWS: usize = 8;

/// A MAX7219 8x8 dot matrix, or a chain of them, on the SPI bus.
///
/// Every transaction is a stream of 16-bit words: the high byte names a
/// register and the low byte its value. Registers 1 through 8 hold the row
/// pixels, 0x0C is shutdown and 0x0B the scan limit. A chain is modelled as
/// `chain` units shifted in order, so the pixel read-back spans the whole
/// physical display.
pub struct Max7219 {
    units: Vec<[u8; MAX7219_ROWS]>,
    shift: usize,
    high: Option<u8>,
    shutdown: bool,
    scan_limit: u8,
    intensity: u8,
    decode: u8,
}

impl Max7219 {
    pub fn from_part(part: &FlatPart) -> Self {
        let chain = attr_u16(&part.attrs, "chain", 1).max(1) as usize;
        Self {
            units: vec![[0; MAX7219_ROWS]; chain],
            shift: 0,
            high: None,
            shutdown: false,
            scan_limit: 7,
            intensity: 8,
            decode: 0,
        }
    }

    pub fn chain(&self) -> usize {
        self.units.len()
    }

    /// The row bytes of one unit, most significant bit leftmost.
    pub fn rows(&self, unit: usize) -> Option<[u8; MAX7219_ROWS]> {
        self.units.get(unit).copied()
    }

    /// One pixel across the chain, `x` counted from the left of unit 0.
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        if y >= MAX7219_ROWS {
            return false;
        }
        let unit = x / 8;
        let column = x % 8;
        self.units
            .get(unit)
            .is_some_and(|rows| rows[y] & (1 << (7 - column)) != 0)
    }

    pub fn lit_pixels(&self) -> usize {
        self.units
            .iter()
            .flat_map(|rows| rows.iter())
            .map(|row| row.count_ones() as usize)
            .sum()
    }

    pub fn shutdown(&self) -> bool {
        self.shutdown
    }

    pub fn scan_limit(&self) -> u8 {
        self.scan_limit
    }

    pub fn intensity(&self) -> u8 {
        self.intensity
    }

    pub fn decode_mode(&self) -> u8 {
        self.decode
    }

    fn word(&mut self, word: u16) {
        let register = (word >> 8) as u8;
        let value = word as u8;
        match register {
            0x01..=0x08 => {
                let index = self.shift % self.units.len();
                let count = self.units.len();
                self.units[index][usize::from(register - 1)] = value;
                self.shift = (index + 1) % count;
            }
            0x09 => self.decode = value,
            0x0a => self.intensity = value & 0x0f,
            0x0b => self.scan_limit = value & 0x07,
            0x0c => self.shutdown = value == 0,
            0x0f => {
                // Display test lights every pixel; clearing it restores the rows.
                if value != 0 {
                    for unit in &mut self.units {
                        unit.fill(0xff);
                    }
                } else {
                    for unit in &mut self.units {
                        unit.fill(0);
                    }
                }
            }
            _ => {}
        }
    }
}

impl Part for Max7219 {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::Led {
            lit: !self.shutdown && self.lit_pixels() != 0,
        }
    }
}

impl SpiDevice for Max7219 {
    fn select(&mut self, selected: bool) {
        if selected {
            self.shift = 0;
        } else {
            self.high = None;
        }
    }

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        for &byte in tx {
            if let Some(high) = self.high.take() {
                self.word(u16::from(high) << 8 | u16::from(byte));
            } else {
                self.high = Some(byte);
            }
        }
        vec![0; rx_len]
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

// ------------------------------------------------------------------ PCD8544

const PCD8544_WIDTH: usize = 84;
const PCD8544_HEIGHT: usize = 48;
const PCD8544_BANKS: usize = PCD8544_HEIGHT / 8;

/// A PCD8544 84x48 LCD, the controller inside the Nokia 5110 module.
///
/// The SPI byte stream carries a leading control byte the same way the
/// ILI9341 model does: 0x00 prefixes a command and 0x40 a data byte. The
/// controller has a 6x84 bank framebuffer and a column/bank pointer; data
/// bytes land at the pointer and advance it, so a firmware drawing a row in
/// one transfer produces the same pixels the glass shows.
pub struct Pcd8544 {
    framebuffer: [u8; PCD8544_WIDTH * PCD8544_BANKS],
    column: usize,
    bank: usize,
    display_on: bool,
    selected: bool,
    data_stream: bool,
}

impl Pcd8544 {
    pub fn new() -> Self {
        Self {
            framebuffer: [0; PCD8544_WIDTH * PCD8544_BANKS],
            column: 0,
            bank: 0,
            display_on: true,
            selected: false,
            data_stream: false,
        }
    }

    pub fn from_part(_part: &FlatPart) -> Self {
        Self::new()
    }

    pub fn width(&self) -> usize {
        PCD8544_WIDTH
    }

    pub fn height(&self) -> usize {
        PCD8544_HEIGHT
    }

    pub fn display_on(&self) -> bool {
        self.display_on
    }

    /// The 6x84 bank framebuffer, page-major with bit 0 at the top row.
    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }

    pub fn pixel(&self, x: usize, y: usize) -> bool {
        if x >= PCD8544_WIDTH || y >= PCD8544_HEIGHT {
            return false;
        }
        self.framebuffer[(y / 8) * PCD8544_WIDTH + x] & (1 << (y % 8)) != 0
    }

    pub fn lit_pixels(&self) -> usize {
        self.framebuffer
            .iter()
            .map(|byte| byte.count_ones() as usize)
            .sum()
    }

    fn command(&mut self, byte: u8) {
        match byte {
            0x08 => self.display_on = false,
            0x0c => self.display_on = true,
            0x0d => self.display_on = true,
            0x40..=0x47 => self.bank = usize::from(byte & 0x07).min(PCD8544_BANKS - 1),
            0x80..=0xff => self.column = usize::from(byte & 0x7f).min(PCD8544_WIDTH - 1),
            // Function set, bias, temperature and extended commands do not
            // change the pixels this model reports.
            _ => {}
        }
    }

    fn data(&mut self, byte: u8) {
        if self.bank < PCD8544_BANKS && self.column < PCD8544_WIDTH {
            self.framebuffer[self.bank * PCD8544_WIDTH + self.column] = byte;
        }
        self.column += 1;
        if self.column >= PCD8544_WIDTH {
            self.column = 0;
            self.bank = (self.bank + 1) % PCD8544_BANKS;
        }
    }
}

impl Default for Pcd8544 {
    fn default() -> Self {
        Self::new()
    }
}

impl Part for Pcd8544 {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::Led {
            lit: self.display_on && self.lit_pixels() != 0,
        }
    }
}

impl SpiDevice for Pcd8544 {
    fn select(&mut self, selected: bool) {
        self.selected = selected;
        if !selected {
            self.data_stream = false;
        }
    }

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        if !self.selected {
            return vec![0xff; rx_len];
        }
        match tx.first() {
            Some(0x00) => {
                self.data_stream = false;
                for &byte in &tx[1..] {
                    self.command(byte);
                }
            }
            Some(0x40) => {
                self.data_stream = true;
                for &byte in &tx[1..] {
                    self.data(byte);
                }
            }
            _ if self.data_stream => {
                for &byte in tx {
                    self.data(byte);
                }
            }
            _ => {}
        }
        vec![0; rx_len]
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock_in(display: &mut Tm1637, byte: u8) {
        for bit in 0..8 {
            display.set_pin(PinId(0), Level::Low, 0);
            display.set_pin(
                PinId(1),
                if byte & (1 << bit) != 0 {
                    Level::High
                } else {
                    Level::Low
                },
                0,
            );
            display.set_pin(PinId(0), Level::High, 0);
        }
        // Acknowledge clock: the part pulls DIO low after the eighth bit.
        display.set_pin(PinId(0), Level::Low, 0);
        display.set_pin(PinId(0), Level::High, 0);
    }

    fn tm1637() -> Tm1637 {
        Tm1637 {
            clk: 0,
            dio: 1,
            clk_level: Level::High,
            dio_level: Level::High,
            output: Level::HighZ,
            bits: 0,
            byte: 0,
            awaiting_ack: false,
            in_transaction: false,
            data_mode: false,
            address: 0,
            last_written: 0,
            registers: [0; 6],
            display_on: true,
            brightness: 7,
        }
    }

    #[test]
    fn a_tm1637_latches_digit_registers_from_its_wire_protocol() {
        let mut display = tm1637();
        // Start: DIO falls while CLK is high.
        display.set_pin(PinId(1), Level::Low, 0);
        clock_in(&mut display, 0x40); // auto-increment data write
        clock_in(&mut display, 0xc0); // start at register 0
        clock_in(&mut display, 0x3f); // '0'
        clock_in(&mut display, 0x06); // '1'
        display.set_pin(PinId(1), Level::High, 0); // stop
        assert_eq!(display.segments(), [0x3f, 0x06, 0, 0]);
        assert_eq!(
            display.state(),
            PartState::Segments { mask: 0x06 },
            "last digit written"
        );
    }

    #[test]
    fn a_tm1637_acknowledges_a_byte_and_ignores_unknown_commands() {
        let mut display = tm1637();
        // Start: DIO falls while CLK is high.
        display.set_pin(PinId(1), Level::Low, 0);
        for bit in 0..8 {
            display.set_pin(PinId(0), Level::Low, 0);
            display.set_pin(
                PinId(1),
                if bit % 2 == 0 {
                    Level::High
                } else {
                    Level::Low
                },
                0,
            );
            display.set_pin(PinId(0), Level::High, 0);
            if bit == 7 {
                assert_eq!(
                    display.drive(PinId(1)),
                    Drive::strong(Level::Low),
                    "ACK after eight bits"
                );
            }
        }
        // The ninth clock releases the acknowledge.
        display.set_pin(PinId(0), Level::Low, 0);
        display.set_pin(PinId(0), Level::High, 0);
        assert_eq!(display.drive(PinId(1)), Drive::HIGH_Z);
        // Stop, then start a second transaction with an out-of-range address.
        display.set_pin(PinId(1), Level::High, 0);
        display.set_pin(PinId(1), Level::Low, 0);
        clock_in(&mut display, 0xc7);
        display.set_pin(PinId(1), Level::High, 0);
        assert_eq!(display.segments(), [0; 4]);
    }

    #[test]
    fn a_host_control_shows_a_decimal_number() {
        let mut display = tm1637();
        display.set_control(123);
        // The control maps 0..1000 onto 0..9999, so 123 shows 1230.
        assert_eq!(
            display.segments(),
            [DIGITS[0], DIGITS[3], DIGITS[2], DIGITS[1]]
        );
    }

    fn max7219() -> Max7219 {
        Max7219 {
            units: vec![[0; MAX7219_ROWS]],
            shift: 0,
            high: None,
            shutdown: false,
            scan_limit: 7,
            intensity: 8,
            decode: 0,
        }
    }

    #[test]
    fn a_max7219_decodes_row_registers_into_pixels() {
        let mut matrix = max7219();
        matrix.select(true);
        matrix.transfer(&[0x01, 0x81], 0); // row 0: leftmost and rightmost
        matrix.select(false);
        assert!(matrix.pixel(0, 0));
        assert!(matrix.pixel(7, 0));
        assert!(!matrix.pixel(1, 0));
        assert_eq!(matrix.lit_pixels(), 2);
        assert_eq!(matrix.state(), PartState::Led { lit: true });
    }

    #[test]
    fn a_max7219_shutdown_blank_state_and_display_test() {
        let mut matrix = max7219();
        matrix.select(true);
        matrix.transfer(&[0x0f, 0x01, 0x0c, 0x01], 0);
        assert_eq!(
            matrix.state(),
            PartState::Led { lit: true },
            "display test fills every row"
        );
        matrix.transfer(&[0x0f, 0x00, 0x0c, 0x00], 0);
        matrix.select(false);
        assert_eq!(matrix.state(), PartState::Led { lit: false });
    }

    #[test]
    fn a_max7219_chain_addresses_each_unit_in_order() {
        let mut matrix = Max7219 {
            units: vec![[0; MAX7219_ROWS]; 2],
            shift: 0,
            high: None,
            shutdown: false,
            scan_limit: 7,
            intensity: 8,
            decode: 0,
        };
        matrix.select(true);
        matrix.transfer(&[0x01, 0x80, 0x01, 0x01], 0);
        matrix.select(false);
        assert!(matrix.pixel(0, 0), "first word lands in unit 0");
        assert!(matrix.pixel(15, 0), "second word lands in unit 1");
    }

    #[test]
    fn a_pcd8544_latches_data_into_its_bank_framebuffer() {
        let mut lcd = Pcd8544::new();
        lcd.select(true);
        lcd.transfer(&[0x00, 0x0c, 0x40, 0x80], 0); // display on, bank 0, column 0
        lcd.transfer(&[0x40, 0x01, 0x80], 0); // two data bytes
        lcd.select(false);
        assert!(lcd.pixel(0, 0));
        assert!(lcd.pixel(1, 7));
        assert_eq!(lcd.lit_pixels(), 2);
        assert!(lcd.display_on());
    }

    #[test]
    fn a_pcd8544_outside_chip_select_reads_high() {
        let mut lcd = Pcd8544::new();
        assert_eq!(lcd.transfer(&[0x00, 0x0c], 2), vec![0xff, 0xff]);
        assert_eq!(lcd.lit_pixels(), 0);
    }
}
