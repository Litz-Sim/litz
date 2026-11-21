//! Display engines: a parallel HD44780 character LCD, an SPI ILI9341 colour
//! panel and an SPI e-paper panel.
//!
//! The character LCD is a pin-level part: a 4-bit HD44780 bus is just six
//! wires, so the model watches the enable strobe and assembles DDRAM and CGRAM
//! exactly as the controller does. The two SPI panels answer transactions and
//! live in [`crate::bus_parts`]'s world; their wire protocol uses one leading
//! control byte per transfer (`0x00` command, `0x40` data) because the generic
//! SPI bus does not carry the D/C line a real panel has. That keeps the models
//! reachable from any SPI adapter, and it mirrors the SSD1306 control-byte
//! convention this crate already uses.

use crate::BuildError;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};
use std::any::Any;

// ------------------------------------------------------------------ HD44780

/// The address of the first character of a display row.
///
/// The HD44780 DDRAM is not laid out as a simple rectangle: the second line
/// starts at 0x40, and on a 20x4 module lines three and four continue at 0x14
/// and 0x54. The split is the controller's, not the panel's.
fn row_address(rows: usize, row: usize) -> usize {
    match (rows, row) {
        (1, _) => 0x00,
        (2, 0) => 0x00,
        (2, _) => 0x40,
        (4, 0) => 0x00,
        (4, 1) => 0x40,
        (4, 2) => 0x14,
        (4, _) => 0x54,
        _ => 0x00,
    }
}

/// A parallel HD44780 character LCD in 4-bit mode, shared by 16x2 and 20x4.
///
/// Writes are latched on the falling edge of E: RS selects command or data,
/// RW selects write (the only direction modelled here), and D4–D7 carry one
/// nibble. Two nibbles make a byte. The controller's DDRAM and CGRAM are both
/// modelled, so `text()` and the user-character bytes are the same state a
/// firmware wrote.
pub struct Hd44780 {
    columns: usize,
    rows: usize,
    rs: u16,
    rw: u16,
    e: u16,
    /// D4 through D7, in that order.
    data: [u16; 4],
    rs_level: Level,
    rw_level: Level,
    e_level: Level,
    data_levels: [Level; 4],
    /// The first nibble of a byte, already shifted into the high half.
    pending: Option<u8>,
    /// Set once the controller has been told to enter 4-bit mode.
    four_bit: bool,
    ddram: [u8; 128],
    cgram: [u8; 64],
    ac: usize,
    cgram_mode: bool,
    entry_increment: bool,
    display_on: bool,
    cursor_on: bool,
    blink_on: bool,
    /// Which half of the address the next read phase returns.
    read_high: bool,
}

/// A 16x2 module. The type alias keeps the catalog's name available to callers
/// while both sizes share one controller model.
pub type Lcd1602 = Hd44780;
/// A 20x4 module, the same controller with a different DDRAM map.
pub type Lcd2004 = Hd44780;

impl Hd44780 {
    pub fn new(columns: usize, rows: usize) -> Self {
        Self {
            columns,
            rows,
            rs: 0,
            rw: 1,
            e: 2,
            data: [3, 4, 5, 6],
            rs_level: Level::HighZ,
            rw_level: Level::HighZ,
            e_level: Level::HighZ,
            data_levels: [Level::HighZ; 4],
            pending: None,
            four_bit: false,
            ddram: [0x20; 128],
            cgram: [0; 64],
            ac: 0,
            cgram_mode: false,
            entry_increment: true,
            display_on: true,
            cursor_on: false,
            blink_on: false,
            read_high: true,
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        if part
            .attrs
            .get("pins")
            .is_some_and(|pins| pins.trim().eq_ignore_ascii_case("i2c"))
        {
            return Err(BuildError::Message(format!(
                "part {:?}: the I2C LCD configuration needs the PCF8574 backpack, which this build does not model; wire the full parallel pins",
                part.id
            )));
        }
        let index = |name: &str| {
            part.pins
                .iter()
                .position(|candidate| candidate == name)
                .map(|i| i as u16)
        };
        let (Some(rs), Some(rw), Some(e)) = (index("RS"), index("RW"), index("E")) else {
            return Err(BuildError::Message(format!(
                "part {:?} does not have the RS/RW/E pins of an HD44780",
                part.id
            )));
        };
        let mut data = [0u16; 4];
        for (offset, pin) in data.iter_mut().enumerate() {
            let name = format!("D{}", offset + 4);
            *pin = index(&name).ok_or_else(|| {
                BuildError::Message(format!(
                    "part {:?} has no {name} pin; only 4-bit mode is modelled",
                    part.id
                ))
            })?;
        }
        let (columns, rows) = if part.type_key == "litz-lcd2004" {
            (20, 4)
        } else {
            (16, 2)
        };
        Ok(Box::new(Self {
            columns,
            rows,
            rs,
            rw,
            e,
            data,
            ..Self::new(columns, rows)
        }))
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn display_on(&self) -> bool {
        self.display_on
    }

    pub fn cursor_on(&self) -> bool {
        self.cursor_on
    }

    pub fn blink_on(&self) -> bool {
        self.blink_on
    }

    /// The display text, one trimmed line per row, joined with `\n`.
    pub fn text(&self) -> String {
        let mut text = (0..self.rows)
            .map(|row| self.line(row).trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        // An unwritten bottom row is not a line of text; keep internal blanks
        // (a blank row above content) but drop the empty tail.
        while text.ends_with('\n') {
            text.pop();
        }
        text
    }

    /// One row exactly as it sits in DDRAM: every column, trailing blanks kept.
    pub fn line(&self, row: usize) -> String {
        if row >= self.rows {
            return String::new();
        }
        let start = row_address(self.rows, row);
        (0..self.columns)
            .map(|column| {
                let byte = self.ddram[(start + column) % self.ddram.len()];
                match byte {
                    0x20..=0x7e => char::from(byte),
                    _ => ' ',
                }
            })
            .collect()
    }

    /// One DDRAM byte, by controller address.
    pub fn ddram_byte(&self, address: usize) -> Option<u8> {
        self.ddram.get(address).copied()
    }

    /// One CGRAM byte, by controller address.
    pub fn cgram_byte(&self, address: usize) -> Option<u8> {
        self.cgram.get(address).copied()
    }

    /// The address counter. While it points into CGRAM the display text is
    /// unchanged; `cgram_mode()` says which space it is walking.
    pub fn address(&self) -> usize {
        self.ac
    }

    pub fn cgram_mode(&self) -> bool {
        self.cgram_mode
    }

    fn rising(&self, level: Level) -> bool {
        level == Level::High
    }

    fn input(&mut self, pin: PinId, level: Level, now: Tick) {
        let _ = now;
        if pin.0 == self.rs {
            self.rs_level = level;
        } else if pin.0 == self.rw {
            self.rw_level = level;
        } else if pin.0 == self.e {
            let was = self.e_level;
            self.e_level = level;
            if self.rising(was) && !self.rising(level) {
                self.latch();
            }
        } else if let Some(slot) = self.data.iter().position(|&candidate| candidate == pin.0) {
            self.data_levels[slot] = level;
        }
    }

    fn nibble(&self) -> u8 {
        self.data_levels
            .iter()
            .enumerate()
            .fold(0u8, |nibble, (index, level)| {
                nibble | ((*level == Level::High) as u8) << index
            })
    }

    fn latch(&mut self) {
        if self.rising(self.rw_level) {
            // A read phase returns AC in two nibbles, high first. The busy bit
            // is always clear: this model finishes a command immediately.
            self.read_high = !self.read_high;
            return;
        }
        let nibble = self.nibble();
        if !self.four_bit && nibble & 0x0e == 0x02 {
            // The 4-bit wake-up sequence sends 0x3, 0x3, 0x3, 0x2 as single
            // nibbles. Each is a function-set attempt; the 0x2 selects 4-bit
            // mode and everything after that is paired.
            self.four_bit = nibble == 0x02;
            self.function_set(nibble << 4);
            return;
        }
        match self.pending.take() {
            Some(high) => {
                let byte = high | nibble;
                if self.rising(self.rs_level) {
                    self.write_data(byte);
                } else {
                    self.command(byte);
                }
            }
            None => self.pending = Some(nibble << 4),
        }
    }

    fn command(&mut self, byte: u8) {
        match byte {
            0x01 => {
                self.ddram.fill(0x20);
                self.ac = 0;
                self.cgram_mode = false;
                self.entry_increment = true;
            }
            0x02 => {
                self.ac = 0;
                self.cgram_mode = false;
            }
            0x04..=0x07 => self.entry_increment = byte & 0x02 != 0,
            0x08..=0x0f => {
                self.display_on = byte & 0x04 != 0;
                self.cursor_on = byte & 0x02 != 0;
                self.blink_on = byte & 0x01 != 0;
            }
            0x10..=0x1f => {}
            0x20..=0x3f => self.function_set(byte),
            0x40..=0x7f => {
                self.ac = usize::from(byte & 0x3f);
                self.cgram_mode = true;
            }
            0x80..=0xff => {
                self.ac = usize::from(byte & 0x7f);
                self.cgram_mode = false;
            }
            _ => {}
        }
    }

    fn function_set(&mut self, byte: u8) {
        // Bits 4 (8/4-bit), 3 (two lines) and 2 (font). The panel size is a
        // property of the part, so only the interface width matters here.
        self.four_bit = byte & 0x10 == 0;
    }

    fn write_data(&mut self, byte: u8) {
        if self.cgram_mode {
            self.cgram[self.ac % self.cgram.len()] = byte;
            self.advance_address(self.cgram.len());
        } else {
            self.ddram[self.ac % self.ddram.len()] = byte;
            self.advance_address(self.ddram.len());
        }
    }

    fn advance_address(&mut self, limit: usize) {
        if self.entry_increment {
            self.ac = (self.ac + 1) % limit;
        } else {
            self.ac = (self.ac + limit - 1) % limit;
        }
    }
}

impl Part for Hd44780 {
    fn drive(&self, pin: PinId) -> Drive {
        if self.rising(self.rw_level)
            && self.rising(self.e_level)
            && let Some(slot) = self.data.iter().position(|&candidate| candidate == pin.0)
        {
            let nibble = if self.read_high {
                ((self.ac >> 4) & 0x0f) as u8
            } else {
                (self.ac & 0x0f) as u8
            };
            return Drive::strong(if nibble & (1 << slot) != 0 {
                Level::High
            } else {
                Level::Low
            });
        }
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        self.input(pin, level, now);
    }

    fn state(&self) -> PartState {
        PartState::None
    }
}

impl Default for Hd44780 {
    fn default() -> Self {
        Self::new(16, 2)
    }
}

// ------------------------------------------------------------------ ILI9341

/// The width of the ILI9341 panel this model presents.
pub const ILI9341_WIDTH: usize = 240;
/// The height of the ILI9341 panel this model presents.
pub const ILI9341_HEIGHT: usize = 320;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelMode {
    Command,
    Data,
    Read,
}

/// An ILI9341 240x320 TFT controller with an RGB565 framebuffer.
///
/// The generic SPI bus has no D/C line, so every transfer starts with one
/// control byte: `0x00` selects the command stream and `0x40` the data stream,
/// the same split the SSD1306 uses over I2C. A command and its arguments arrive
/// in a command transfer; pixel bytes arrive in a data transfer. `RAMRD` puts
/// the controller in read mode: subsequent transfers shift pixels out on MISO.
///
/// Only the commands a display driver needs to bring the panel up and address a
/// window are implemented: SWRESET, SLPOUT, DISPON/OFF, MADCTL, COLMOD, CASET,
/// PASET, RAMWR and RAMRD. Everything else is accepted and ignored, which is how
/// a real controller treats an unknown command on its command stream.
pub struct Ili9341 {
    framebuffer: Vec<u16>,
    width: usize,
    height: usize,
    mode: PanelMode,
    command: u8,
    arguments: Vec<u8>,
    collect: usize,
    caset: (u16, u16),
    paset: (u16, u16),
    x: u16,
    y: u16,
    high_byte: Option<u8>,
    display_on: bool,
    madctl: u8,
    colmod: u8,
    selected: bool,
    /// Grows on every pixel written, so a host can tell whether the panel
    /// changed since it last read the framebuffer.
    frame_version: u64,
}

impl Ili9341 {
    pub fn new() -> Self {
        Self {
            framebuffer: vec![0; ILI9341_WIDTH * ILI9341_HEIGHT],
            width: ILI9341_WIDTH,
            height: ILI9341_HEIGHT,
            mode: PanelMode::Command,
            command: 0,
            arguments: Vec::new(),
            collect: 0,
            caset: (0, (ILI9341_WIDTH - 1) as u16),
            paset: (0, (ILI9341_HEIGHT - 1) as u16),
            x: 0,
            y: 0,
            high_byte: None,
            display_on: false,
            madctl: 0,
            colmod: 0x55,
            selected: false,
            frame_version: 0,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn display_on(&self) -> bool {
        self.display_on
    }

    /// The RGB565 word at `(x, y)`, or `None` outside the panel.
    pub fn pixel(&self, x: usize, y: usize) -> Option<u16> {
        if x >= self.width || y >= self.height {
            return None;
        }
        Some(self.framebuffer[y * self.width + x])
    }

    /// The whole panel as RGB565 pixels in row-major order.
    ///
    /// The tuple is `(pixels, width, height)`: one `u16` per pixel, indexed
    /// `y * width + x`, so the slice reads like the image a screenshot would
    /// show. It is the live framebuffer, not a copy.
    pub fn framebuffer(&self) -> (&[u16], u32, u32) {
        (&self.framebuffer, self.width as u32, self.height as u32)
    }

    /// A counter that grows on every pixel written to the panel.
    ///
    /// A host reads it before and after a run to learn whether the screen
    /// changed, without diffing the whole framebuffer. Every write path —
    /// RAMWR data, the convenience `fill`, and the SWRESET clear — bumps it.
    pub fn frame_version(&self) -> u64 {
        self.frame_version
    }

    /// The addressed window `((x0, x1), (y0, y1))`, in panel coordinates.
    pub fn window(&self) -> ((u16, u16), (u16, u16)) {
        (self.caset, self.paset)
    }

    /// Fill the whole panel with one colour. A host convenience, not a command
    /// a driver sends.
    pub fn fill(&mut self, color: u16) {
        self.framebuffer.fill(color);
        self.frame_version += 1;
    }

    fn parameter_count(command: u8) -> usize {
        match command {
            0x2a | 0x2b => 4,
            0x36 | 0x3a | 0x51 | 0x53 => 1,
            _ => 0,
        }
    }

    fn command_byte(&mut self, byte: u8) {
        if self.collect > 0 {
            self.arguments.push(byte);
            if self.arguments.len() == self.collect {
                let command = self.command;
                self.apply(command);
                self.arguments.clear();
                self.collect = 0;
            }
            return;
        }
        self.command = byte;
        self.arguments.clear();
        self.collect = Self::parameter_count(byte);
        if self.collect == 0 {
            self.apply(byte);
        }
    }

    fn apply(&mut self, command: u8) {
        match command {
            0x01 => {
                self.framebuffer.fill(0);
                self.frame_version += 1;
                self.caset = (0, (self.width - 1) as u16);
                self.paset = (0, (self.height - 1) as u16);
                self.x = 0;
                self.y = 0;
                self.mode = PanelMode::Command;
                self.high_byte = None;
            }
            0x11 => {}
            0x28 => self.display_on = false,
            0x29 => self.display_on = true,
            0x2a if self.arguments.len() == 4 => {
                self.caset = (
                    (u16::from(self.arguments[0]) << 8 | u16::from(self.arguments[1]))
                        .min((self.width - 1) as u16),
                    (u16::from(self.arguments[2]) << 8 | u16::from(self.arguments[3]))
                        .min((self.width - 1) as u16),
                );
                self.x = self.caset.0;
            }
            0x2b if self.arguments.len() == 4 => {
                self.paset = (
                    (u16::from(self.arguments[0]) << 8 | u16::from(self.arguments[1]))
                        .min((self.height - 1) as u16),
                    (u16::from(self.arguments[2]) << 8 | u16::from(self.arguments[3]))
                        .min((self.height - 1) as u16),
                );
                self.y = self.paset.0;
            }
            0x2c => {
                self.mode = PanelMode::Data;
                self.x = self.caset.0;
                self.y = self.paset.0;
            }
            0x2e => {
                self.mode = PanelMode::Read;
                self.x = self.caset.0;
                self.y = self.paset.0;
            }
            0x36 if !self.arguments.is_empty() => self.madctl = self.arguments[0],
            0x3a if !self.arguments.is_empty() => self.colmod = self.arguments[0],
            _ => {}
        }
    }

    fn data_byte(&mut self, byte: u8) {
        let Some(high) = self.high_byte.take() else {
            self.high_byte = Some(byte);
            return;
        };
        let color = u16::from(high) << 8 | u16::from(byte);
        if (self.x as usize) < self.width && (self.y as usize) < self.height {
            self.framebuffer[self.y as usize * self.width + self.x as usize] = color;
            self.frame_version += 1;
        }
        self.advance();
    }

    fn advance(&mut self) {
        self.x = self.x.saturating_add(1);
        if self.x > self.caset.1 {
            self.x = self.caset.0;
            self.y = self.y.saturating_add(1);
            if self.y > self.paset.1 {
                self.y = self.paset.0;
            }
        }
    }

    fn read_bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let color = if (self.x as usize) < self.width && (self.y as usize) < self.height {
                self.framebuffer[self.y as usize * self.width + self.x as usize]
            } else {
                0
            };
            out.push((color >> 8) as u8);
            if out.len() < len {
                out.push(color as u8);
            }
            self.advance();
        }
        out
    }
}

impl Default for Ili9341 {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::bus::SpiDevice for Ili9341 {
    fn select(&mut self, selected: bool) {
        self.selected = selected;
        if !selected {
            self.arguments.clear();
            self.collect = 0;
            self.high_byte = None;
        }
    }

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        if !self.selected {
            return vec![0xff; rx_len];
        }
        if self.mode == PanelMode::Read && tx.first().is_none_or(|byte| *byte != 0x00) {
            return self.read_bytes(rx_len);
        }
        match tx.first() {
            Some(0x00) => {
                self.mode = PanelMode::Command;
                self.high_byte = None;
                for &byte in &tx[1..] {
                    self.command_byte(byte);
                }
            }
            Some(0x40) => {
                self.mode = PanelMode::Data;
                self.high_byte = None;
                for &byte in &tx[1..] {
                    self.data_byte(byte);
                }
            }
            _ if self.mode == PanelMode::Data => {
                for &byte in tx {
                    self.data_byte(byte);
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

// ------------------------------------------------------------------ e-paper

/// The size of one e-paper panel, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EpdSize {
    /// Waveshare 1.54", 200x200.
    In54,
    /// Waveshare 2.13", 122 wide by 250 tall.
    In213,
}

impl EpdSize {
    pub fn dimensions(self) -> (usize, usize) {
        match self {
            Self::In54 => (200, 200),
            Self::In213 => (122, 250),
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "1.54" | "1in54" => Some(Self::In54),
            "2.13" | "2in13" => Some(Self::In213),
            _ => None,
        }
    }
}

/// A one-bit-per-pixel e-paper panel on SPI.
///
/// The transfer protocol matches the other SPI panels: `0x00` starts a command
/// stream, `0x40` a data stream. `0x10` starts a frame, data bytes fill the
/// pending frame in row-major order with the most significant bit leftmost,
/// `0x11` ends the transfer, and `0x12` copies the pending frame to the glass.
/// Only a refresh changes what `pixel` reports, which is what makes e-paper
/// e-paper.
pub struct Epd {
    width: usize,
    height: usize,
    pending: Vec<u8>,
    displayed: Vec<u8>,
    mode: PanelMode,
    command: u8,
    arguments: Vec<u8>,
    collect: usize,
    cursor: usize,
    power_on: bool,
    selected: bool,
    /// Grows on every refresh that copies the pending frame to the glass.
    frame_version: u64,
}

impl Epd {
    pub fn new(size: EpdSize) -> Self {
        let (width, height) = size.dimensions();
        let bytes = width.div_ceil(8) * height;
        Self {
            width,
            height,
            pending: vec![0; bytes],
            displayed: vec![0xff; bytes],
            mode: PanelMode::Command,
            command: 0,
            arguments: Vec::new(),
            collect: 0,
            cursor: 0,
            power_on: false,
            selected: false,
            frame_version: 0,
        }
    }

    pub fn from_attrs(part: &FlatPart) -> Self {
        let size = part
            .attrs
            .get("size")
            .and_then(|text| EpdSize::parse(text))
            .unwrap_or(if part.type_key == "litz-epaper-2in13" {
                EpdSize::In213
            } else {
                EpdSize::In54
            });
        Self::new(size)
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn power_on(&self) -> bool {
        self.power_on
    }

    /// A pixel on the refreshed glass, from the displayed 1-bit frame.
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let byte = self.displayed[y * self.width.div_ceil(8) + x / 8];
        byte & (0x80 >> (x % 8)) != 0
    }

    /// The pending frame, before a refresh.
    pub fn pending(&self) -> &[u8] {
        &self.pending
    }

    /// The frame on the glass, after the last refresh.
    pub fn displayed(&self) -> &[u8] {
        &self.displayed
    }

    /// A counter that grows on every refresh that changes the glass.
    ///
    /// Data written with `0x10` lands in the pending frame and does not bump
    /// it; only `0x12`, which copies the pending frame to the displayed one,
    /// does. That is the moment a host's screenshot would change.
    pub fn frame_version(&self) -> u64 {
        self.frame_version
    }

    fn command_byte(&mut self, byte: u8) {
        if self.collect > 0 {
            self.arguments.push(byte);
            if self.arguments.len() == self.collect {
                let command = self.command;
                self.apply(command);
                self.arguments.clear();
                self.collect = 0;
            }
            return;
        }
        self.command = byte;
        self.arguments.clear();
        self.collect = match byte {
            0x01 => 4,
            _ => 0,
        };
        if self.collect == 0 {
            self.apply(byte);
        }
    }

    fn apply(&mut self, command: u8) {
        match command {
            0x01 => {
                self.pending.fill(0);
                self.cursor = 0;
                self.mode = PanelMode::Command;
            }
            0x04 => self.power_on = true,
            0x06 => self.power_on = false,
            0x10 => {
                self.mode = PanelMode::Data;
                self.cursor = 0;
            }
            0x11 => self.mode = PanelMode::Command,
            0x12 => {
                self.displayed.clone_from(&self.pending);
                self.frame_version += 1;
            }
            _ => {}
        }
    }

    fn data_byte(&mut self, byte: u8) {
        if let Some(slot) = self.pending.get_mut(self.cursor) {
            *slot = byte;
            self.cursor = (self.cursor + 1) % self.pending.len();
        }
    }
}

impl Default for Epd {
    fn default() -> Self {
        Self::new(EpdSize::In54)
    }
}

impl crate::bus::SpiDevice for Epd {
    fn select(&mut self, selected: bool) {
        self.selected = selected;
        if !selected {
            self.arguments.clear();
            self.collect = 0;
        }
    }

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        if !self.selected {
            return vec![0xff; rx_len];
        }
        match tx.first() {
            Some(0x00) => {
                self.mode = PanelMode::Command;
                for &byte in &tx[1..] {
                    self.command_byte(byte);
                }
            }
            Some(0x40) => {
                self.mode = PanelMode::Data;
                for &byte in &tx[1..] {
                    self.data_byte(byte);
                }
            }
            _ if self.mode == PanelMode::Data => {
                for &byte in tx {
                    self.data_byte(byte);
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

// ------------------------------------------------------------------ ST7735

/// The width of the ST7735 panel this model presents.
pub const ST7735_WIDTH: usize = 160;
/// The height of the ST7735 panel this model presents.
pub const ST7735_HEIGHT: usize = 128;

/// An ST7735 160x128 TFT controller with an RGB565 framebuffer.
///
/// The generic SPI bus carries no D/C line, so—following [`Ili9341`]—every
/// transfer starts with one control byte: `0x00` selects the command stream and
/// `0x40` the data stream. CASET 0x2A and RASET 0x2B set the address window,
/// RAMWR 0x2C starts a pixel stream, and 0x2E enters read mode. The usual
/// ST7735 init commands are accepted so a panel driver's startup sequence does
/// not leak their arguments into the command stream. Unknown commands on the
/// command stream are ignored, which is what the real controller does.
pub struct St7735 {
    framebuffer: Vec<u16>,
    width: usize,
    height: usize,
    mode: PanelMode,
    command: u8,
    arguments: Vec<u8>,
    collect: usize,
    caset: (u16, u16),
    raset: (u16, u16),
    x: u16,
    y: u16,
    high_byte: Option<u8>,
    display_on: bool,
    madctl: u8,
    colmod: u8,
    selected: bool,
    /// Grows on every pixel written, so a host can tell whether the panel
    /// changed since it last read the framebuffer.
    frame_version: u64,
}

impl St7735 {
    pub fn new() -> Self {
        Self {
            framebuffer: vec![0; ST7735_WIDTH * ST7735_HEIGHT],
            width: ST7735_WIDTH,
            height: ST7735_HEIGHT,
            mode: PanelMode::Command,
            command: 0,
            arguments: Vec::new(),
            collect: 0,
            caset: (0, (ST7735_WIDTH - 1) as u16),
            raset: (0, (ST7735_HEIGHT - 1) as u16),
            x: 0,
            y: 0,
            high_byte: None,
            display_on: false,
            madctl: 0,
            colmod: 0x55,
            selected: false,
            frame_version: 0,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn display_on(&self) -> bool {
        self.display_on
    }

    /// The RGB565 word at `(x, y)`, or `None` outside the panel.
    pub fn pixel(&self, x: usize, y: usize) -> Option<u16> {
        if x >= self.width || y >= self.height {
            return None;
        }
        Some(self.framebuffer[y * self.width + x])
    }

    /// The whole panel as RGB565 pixels in row-major order.
    ///
    /// The tuple is `(pixels, width, height)`, the same layout
    /// [`Ili9341::framebuffer`] uses.
    pub fn framebuffer(&self) -> (&[u16], u32, u32) {
        (&self.framebuffer, self.width as u32, self.height as u32)
    }

    pub fn frame_version(&self) -> u64 {
        self.frame_version
    }

    /// The addressed window `((x0, x1), (y0, y1))`, in panel coordinates.
    pub fn window(&self) -> ((u16, u16), (u16, u16)) {
        (self.caset, self.raset)
    }

    /// Fill the whole panel with one colour. A host convenience.
    pub fn fill(&mut self, color: u16) {
        self.framebuffer.fill(color);
        self.frame_version += 1;
    }

    fn parameter_count(command: u8) -> usize {
        match command {
            0x2a | 0x2b => 4,
            0x36 | 0x3a | 0x51 | 0x53 | 0xb0 | 0xb4 | 0xbb | 0xc1 | 0xc5 | 0xc6 | 0xc7 => 1,
            0xb6 | 0xc0 | 0xc2 | 0xc3 | 0xc4 | 0xd0 => 2,
            0xb1 | 0xb2 => 3,
            0xb3 => 6,
            0xe0 | 0xe1 => 16,
            _ => 0,
        }
    }

    fn command_byte(&mut self, byte: u8) {
        if self.collect > 0 {
            self.arguments.push(byte);
            if self.arguments.len() == self.collect {
                let command = self.command;
                self.apply(command);
                self.arguments.clear();
                self.collect = 0;
            }
            return;
        }
        self.command = byte;
        self.arguments.clear();
        self.collect = Self::parameter_count(byte);
        if self.collect == 0 {
            self.apply(byte);
        }
    }

    fn apply(&mut self, command: u8) {
        match command {
            0x01 => {
                self.framebuffer.fill(0);
                self.frame_version += 1;
                self.caset = (0, (self.width - 1) as u16);
                self.raset = (0, (self.height - 1) as u16);
                self.x = 0;
                self.y = 0;
                self.mode = PanelMode::Command;
                self.high_byte = None;
            }
            0x11 => {}
            0x28 => self.display_on = false,
            0x29 => self.display_on = true,
            0x2a if self.arguments.len() == 4 => {
                self.caset = (
                    (u16::from(self.arguments[0]) << 8 | u16::from(self.arguments[1]))
                        .min((self.width - 1) as u16),
                    (u16::from(self.arguments[2]) << 8 | u16::from(self.arguments[3]))
                        .min((self.width - 1) as u16),
                );
                self.x = self.caset.0;
            }
            0x2b if self.arguments.len() == 4 => {
                self.raset = (
                    (u16::from(self.arguments[0]) << 8 | u16::from(self.arguments[1]))
                        .min((self.height - 1) as u16),
                    (u16::from(self.arguments[2]) << 8 | u16::from(self.arguments[3]))
                        .min((self.height - 1) as u16),
                );
                self.y = self.raset.0;
            }
            0x2c => {
                self.mode = PanelMode::Data;
                self.x = self.caset.0;
                self.y = self.raset.0;
            }
            0x2e => {
                self.mode = PanelMode::Read;
                self.x = self.caset.0;
                self.y = self.raset.0;
            }
            0x36 if !self.arguments.is_empty() => self.madctl = self.arguments[0],
            0x3a if !self.arguments.is_empty() => self.colmod = self.arguments[0],
            _ => {}
        }
    }

    fn data_byte(&mut self, byte: u8) {
        let Some(high) = self.high_byte.take() else {
            self.high_byte = Some(byte);
            return;
        };
        let color = u16::from(high) << 8 | u16::from(byte);
        if (self.x as usize) < self.width && (self.y as usize) < self.height {
            self.framebuffer[self.y as usize * self.width + self.x as usize] = color;
            self.frame_version += 1;
        }
        self.advance();
    }

    fn advance(&mut self) {
        self.x = self.x.saturating_add(1);
        if self.x > self.caset.1 {
            self.x = self.caset.0;
            self.y = self.y.saturating_add(1);
            if self.y > self.raset.1 {
                self.y = self.raset.0;
            }
        }
    }

    fn read_bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let color = if (self.x as usize) < self.width && (self.y as usize) < self.height {
                self.framebuffer[self.y as usize * self.width + self.x as usize]
            } else {
                0
            };
            out.push((color >> 8) as u8);
            if out.len() < len {
                out.push(color as u8);
            }
            self.advance();
        }
        out
    }
}

impl Default for St7735 {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::bus::SpiDevice for St7735 {
    fn select(&mut self, selected: bool) {
        self.selected = selected;
        if !selected {
            self.arguments.clear();
            self.collect = 0;
            self.high_byte = None;
        }
    }

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        if !self.selected {
            return vec![0xff; rx_len];
        }
        if self.mode == PanelMode::Read && tx.first().is_none_or(|byte| *byte != 0x00) {
            return self.read_bytes(rx_len);
        }
        match tx.first() {
            Some(0x00) => {
                self.mode = PanelMode::Command;
                self.high_byte = None;
                for &byte in &tx[1..] {
                    self.command_byte(byte);
                }
            }
            Some(0x40) => {
                self.mode = PanelMode::Data;
                self.high_byte = None;
                for &byte in &tx[1..] {
                    self.data_byte(byte);
                }
            }
            _ if self.mode == PanelMode::Data => {
                for &byte in tx {
                    self.data_byte(byte);
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
    use crate::bus::SpiDevice;

    fn lcd() -> Hd44780 {
        Hd44780::new(16, 2)
    }

    fn pulse(lcd: &mut Hd44780, rs: Level, nibble: u8, at: Tick) {
        lcd.set_pin(PinId(0), rs, at);
        for (slot, pin) in [3u16, 4, 5, 6].into_iter().enumerate() {
            lcd.set_pin(
                PinId(pin),
                if nibble & (1 << slot) != 0 {
                    Level::High
                } else {
                    Level::Low
                },
                at,
            );
        }
        lcd.set_pin(PinId(2), Level::High, at);
        lcd.set_pin(PinId(2), Level::Low, at + 1);
    }

    fn byte(lcd: &mut Hd44780, rs: Level, value: u8, at: Tick) {
        pulse(lcd, rs, value >> 4, at);
        pulse(lcd, rs, value & 0x0f, at + 2);
    }

    fn init(lcd: &mut Hd44780) {
        lcd.set_pin(PinId(1), Level::Low, 0); // RW low: write
        for (i, nibble) in [0x3, 0x3, 0x3, 0x2].into_iter().enumerate() {
            pulse(lcd, Level::Low, nibble, i as Tick * 10);
        }
        byte(lcd, Level::Low, 0x28, 100); // 4-bit, two lines
        byte(lcd, Level::Low, 0x0c, 110); // display on
        byte(lcd, Level::Low, 0x06, 120); // entry increment
        byte(lcd, Level::Low, 0x01, 130); // clear
    }

    #[test]
    fn an_hd44780_assembles_nibbles_into_text() {
        let mut lcd = lcd();
        init(&mut lcd);
        byte(&mut lcd, Level::Low, 0x80, 200); // DDRAM address 0
        for (i, ch) in b"Hi".iter().enumerate() {
            byte(&mut lcd, Level::High, *ch, 210 + i as Tick * 5);
        }
        assert_eq!(lcd.text(), "Hi");
        assert_eq!(lcd.line(0).len(), 16);
        assert_eq!(lcd.line(1), " ".repeat(16));
    }

    #[test]
    fn an_hd44780_writes_cgram_and_second_line() {
        let mut lcd = lcd();
        init(&mut lcd);
        byte(&mut lcd, Level::Low, 0x40, 200); // CGRAM address 0
        byte(&mut lcd, Level::High, 0b00001, 210);
        assert_eq!(lcd.cgram_byte(0), Some(0b00001));
        assert!(lcd.cgram_mode());

        byte(&mut lcd, Level::Low, 0xc0, 220); // line 2 starts at 0x40
        byte(&mut lcd, Level::High, b'B', 230);
        assert!(!lcd.cgram_mode());
        assert_eq!(lcd.text(), "\nB");
    }

    #[test]
    fn a_20x4_uses_the_split_ddram_map() {
        let mut lcd = Hd44780::new(20, 4);
        init(&mut lcd);
        for (address, ch) in [(0x00u8, b'A'), (0x40, b'B'), (0x14, b'C'), (0x54, b'D')] {
            byte(&mut lcd, Level::Low, 0x80 | address, 300);
            byte(&mut lcd, Level::High, ch, 310);
        }
        assert_eq!(lcd.text(), "A\nB\nC\nD");
        assert_eq!(lcd.line(2).len(), 20);
    }

    #[test]
    fn a_clear_command_blanks_the_display() {
        let mut lcd = lcd();
        init(&mut lcd);
        byte(&mut lcd, Level::Low, 0x80, 200);
        byte(&mut lcd, Level::High, b'X', 210);
        byte(&mut lcd, Level::Low, 0x01, 220);
        assert_eq!(lcd.text(), "");
    }

    fn ili() -> Ili9341 {
        Ili9341::new()
    }

    fn spi(panel: &mut Ili9341, tx: &[u8], rx: usize) -> Vec<u8> {
        use crate::bus::SpiDevice;
        panel.select(true);
        let out = panel.transfer(tx, rx);
        panel.select(false);
        out
    }

    #[test]
    fn an_ili9341_writes_a_window_and_reads_it_back() {
        let mut panel = ili();
        spi(&mut panel, &[0x00, 0x2a, 0x00, 0x0a, 0x00, 0x0b], 0); // x 10..11
        spi(&mut panel, &[0x00, 0x2b, 0x00, 0x14, 0x00, 0x14], 0); // y 20
        spi(&mut panel, &[0x00, 0x2c], 0);
        spi(&mut panel, &[0x40, 0xf8, 0x00, 0x07, 0xe0], 0); // red, green
        assert_eq!(panel.window(), ((10, 11), (20, 20)));
        assert_eq!(panel.pixel(10, 20), Some(0xf800));
        assert_eq!(panel.pixel(11, 20), Some(0x07e0));

        spi(&mut panel, &[0x00, 0x2a, 0x00, 0x0a, 0x00, 0x0b], 0);
        spi(&mut panel, &[0x00, 0x2b, 0x00, 0x14, 0x00, 0x14], 0);
        spi(&mut panel, &[0x00, 0x2e], 0);
        let bytes = spi(&mut panel, &[0xff, 0xff, 0xff, 0xff], 4);
        assert_eq!(bytes, vec![0xf8, 0x00, 0x07, 0xe0]);
    }

    #[test]
    fn an_ili9341_reset_clears_the_framebuffer() {
        let mut panel = ili();
        panel.fill(0x1234);
        spi(&mut panel, &[0x00, 0x01], 0);
        assert_eq!(panel.pixel(0, 0), Some(0));
        assert!(!panel.display_on());
        spi(&mut panel, &[0x00, 0x29], 0);
        assert!(panel.display_on());
    }

    fn epd() -> Epd {
        Epd::new(EpdSize::In54)
    }

    #[test]
    fn an_epaper_refresh_reveals_the_pending_frame() {
        use crate::bus::SpiDevice;
        let mut panel = epd();
        panel.select(true);
        panel.transfer(&[0x00, 0x10], 0); // start a frame
        panel.transfer(&[0x40, 0x80], 0); // one lit pixel in the first data byte
        panel.transfer(&[0x00, 0x11, 0x12], 0); // stop, refresh
        panel.select(false);
        assert!(panel.pixel(0, 0));
        assert!(!panel.pixel(1, 0));
        assert_eq!(panel.width(), 200);
        assert_eq!(panel.height(), 200);
    }

    #[test]
    fn an_epaper_213_is_122_by_250() {
        let panel = Epd::new(EpdSize::In213);
        assert_eq!((panel.width(), panel.height()), (122, 250));
    }

    #[test]
    fn an_st7735_writes_a_window_and_reads_it_back() {
        let mut panel = St7735::new();
        assert_eq!((panel.width(), panel.height()), (160, 128));
        panel.select(true);
        panel.transfer(&[0x00, 0x2a, 0x00, 0x0a, 0x00, 0x0b], 0); // x 10..11
        panel.transfer(&[0x00, 0x2b, 0x00, 0x14, 0x00, 0x14], 0); // y 20
        panel.transfer(&[0x00, 0x2c], 0);
        panel.transfer(&[0x40, 0xf8, 0x00, 0x07, 0xe0], 0); // red, green
        panel.select(false);
        assert_eq!(panel.window(), ((10, 11), (20, 20)));
        assert_eq!(panel.pixel(10, 20), Some(0xf800));
        assert_eq!(panel.pixel(11, 20), Some(0x07e0));
        assert_eq!(panel.pixel(160, 0), None);

        panel.select(true);
        panel.transfer(&[0x00, 0x29], 0);
        panel.select(false);
        assert!(panel.display_on());

        // Read the window back out on MISO.
        panel.select(true);
        panel.transfer(&[0x00, 0x2a, 0x00, 0x0a, 0x00, 0x0b], 0);
        panel.transfer(&[0x00, 0x2b, 0x00, 0x14, 0x00, 0x14], 0);
        panel.transfer(&[0x00, 0x2e], 0);
        let bytes = panel.transfer(&[0xff, 0xff, 0xff, 0xff], 4);
        panel.select(false);
        assert_eq!(bytes, vec![0xf8, 0x00, 0x07, 0xe0]);
    }

    #[test]
    fn an_st7735_reset_clears_the_framebuffer() {
        let mut panel = St7735::new();
        panel.fill(0x1234);
        assert_eq!(panel.pixel(0, 0), Some(0x1234));
        panel.select(true);
        panel.transfer(&[0x00, 0x01], 0);
        panel.select(false);
        assert_eq!(panel.pixel(0, 0), Some(0));
        assert!(!panel.display_on());
    }
}
