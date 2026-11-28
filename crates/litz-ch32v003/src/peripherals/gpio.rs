//! GPIOA/GPIO C/GPIO D, AFIO and EXTI.
//!
//! # Pin numbering
//!
//! The board runtime addresses a chip pad by a bare number. This backend uses
//! the 18 pins the CH32V003 brings out, with the ports contiguous:
//!
//! | Number | Pad | Number | Pad | Number | Pad |
//! |--------|-----|--------|-----|--------|-----|
//! | 0..7   | PC0..PC7 | 8..15 | PD0..PD7 | 16 | PA1 |
//! |        |     |        |     | 17 | PA2 |
//!
//! A board manifest maps its header pins onto those numbers (`gpio = 5` is
//! PC5). The mapping is fixed here, [`crate::chip::SIO_PINS`] lists it and
//! [`crate::chip::analog_channel`] maps the analog-capable pads onto ADC
//! channels.
//!
//! # Registers
//!
//! Each port has the STM32F0-shaped file: `CFGLR` (four bits per pin: two mode
//! and two configuration), `INDR`, `OUTDR`, `BSHR` (bits 0–15 set, 16–31
//! reset), `BCR` and `LCKR`. A pin with mode `00` is an input; `CNF = 10` makes
//! it a pull-up/pull-down selected by `OUTDR`, and an input with no external
//! drive and no pull reads low. `CNF >= 10` on an output selects alternate
//! function, which releases the pad from the GPIO block so the timer's PWM
//! publication can own it.

use super::RegisterFile;

/// Reset `CFGLR`: every pin a floating input (CNF = 01, mode = 00).
pub const CFGLR_RESET: u32 = 0x4444_4444;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Port {
    A,
    C,
    D,
}

/// The GPIO number for a port bit, or `None` when the silicon does not bring
/// that bit out.
pub fn pin_number(port: Port, bit: u8) -> Option<u8> {
    match (port, bit) {
        (Port::C, 0..=7) => Some(bit),
        (Port::D, 0..=7) => Some(8 + bit),
        (Port::A, 1) => Some(16),
        (Port::A, 2) => Some(17),
        _ => None,
    }
}

/// The port and bit behind a GPIO number.
pub fn locate(gpio: u8) -> Option<(Port, u8)> {
    match gpio {
        0..=7 => Some((Port::C, gpio)),
        8..=15 => Some((Port::D, gpio - 8)),
        16 => Some((Port::A, 1)),
        17 => Some((Port::A, 2)),
        _ => None,
    }
}

/// The number of pads the backend brings out.
pub const PIN_COUNT: usize = 18;

#[derive(Debug)]
pub struct GpioPort {
    regs: RegisterFile,
    /// Levels the board holds on each pad, by port bit.
    external: [Option<bool>; 8],
}

impl GpioPort {
    pub fn new() -> Self {
        let mut regs = RegisterFile::new(0x100 / 4);
        regs.write(0x00, CFGLR_RESET);
        GpioPort {
            regs,
            external: [None; 8],
        }
    }

    fn config(&self, bit: u8) -> u32 {
        (self.regs.read(0x00) >> (bit * 4)) & 0xf
    }

    fn mode(&self, bit: u8) -> u32 {
        self.config(bit) & 0x3
    }

    fn cnf(&self, bit: u8) -> u32 {
        (self.config(bit) >> 2) & 0x3
    }

    pub fn is_output(&self, bit: u8) -> bool {
        self.mode(bit) != 0
    }

    /// An output in alternate-function mode is driven by a peripheral, not by
    /// `OUTDR`.
    pub fn is_alternate(&self, bit: u8) -> bool {
        self.is_output(bit) && self.cnf(bit) >= 2
    }

    /// A push-pull GPIO output: the pin the board should see `OUTDR` on.
    pub fn drives_gpio(&self, bit: u8) -> bool {
        self.is_output(bit) && self.cnf(bit) < 2
    }

    pub fn out(&self, bit: u8) -> bool {
        self.regs.read(0x0c) & (1 << bit) != 0
    }

    pub fn set_external(&mut self, bit: u8, level: Option<bool>) {
        if let Some(slot) = self.external.get_mut(usize::from(bit)) {
            *slot = level;
        }
    }

    /// The pin's level as the input register reports it.
    pub fn level(&self, bit: u8) -> bool {
        if self.is_output(bit) {
            return self.out(bit);
        }
        if let Some(level) = self.external.get(usize::from(bit)).copied().flatten() {
            return level;
        }
        // Input pull-up/pull-down: CNF = 10, with OUTDR choosing the direction.
        if self.cnf(bit) == 2 && self.mode(bit) == 0 {
            return self.out(bit);
        }
        false
    }

    fn idr(&self) -> u32 {
        let mut value = 0;
        for bit in 0..8u8 {
            if self.level(bit) {
                value |= 1 << bit;
            }
        }
        value
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x00 => self.regs.read(0x00),
            0x08 => self.idr(),
            0x0c => self.regs.read(0x0c),
            0x18 => self.regs.read(0x18),
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            0x00 => self.regs.write(0x00, value),
            0x08 => {}
            0x0c => self.regs.write(0x0c, value & 0xffff),
            0x10 => {
                // BSHR: the low half sets, the high half resets.
                let out = self.regs.read(0x0c);
                let out = (out | (value & 0xffff)) & !((value >> 16) & 0xffff);
                self.regs.write(0x0c, out & 0xffff);
            }
            0x14 => {
                // BCR: the low bits reset.
                let out = self.regs.read(0x0c) & !(value & 0xffff);
                self.regs.write(0x0c, out & 0xffff);
            }
            0x18 => self.regs.write(0x18, value),
            off => self.regs.write(off, value),
        }
    }
}

/// The three ports, owned together so the machine can walk every pad.
#[derive(Debug)]
pub struct Gpio {
    pub a: GpioPort,
    pub c: GpioPort,
    pub d: GpioPort,
}

impl Default for Gpio {
    fn default() -> Self {
        Self::new()
    }
}

impl Gpio {
    pub fn new() -> Self {
        Gpio {
            a: GpioPort::new(),
            c: GpioPort::new(),
            d: GpioPort::new(),
        }
    }

    pub fn port(&self, port: Port) -> &GpioPort {
        match port {
            Port::A => &self.a,
            Port::C => &self.c,
            Port::D => &self.d,
        }
    }

    pub fn port_mut(&mut self, port: Port) -> &mut GpioPort {
        match port {
            Port::A => &mut self.a,
            Port::C => &mut self.c,
            Port::D => &mut self.d,
        }
    }

    pub fn set_external(&mut self, gpio: u8, level: Option<bool>) {
        if let Some((port, bit)) = locate(gpio) {
            self.port_mut(port).set_external(bit, level);
        }
    }

    pub fn level(&self, gpio: u8) -> bool {
        match locate(gpio) {
            Some((port, bit)) => self.port(port).level(bit),
            None => false,
        }
    }

    /// What the GPIO block drives on a pad, or `None` when the pad is an input
    /// or belongs to an alternate function.
    pub fn drive(&self, gpio: u8) -> Option<bool> {
        let (port, bit) = locate(gpio)?;
        let port = self.port(port);
        port.drives_gpio(bit).then(|| port.out(bit))
    }
}

/// The alternate-function I/O block. Only the EXTI line-to-port routing is
/// given meaning; the remap register is storage because the model fixes each
/// peripheral's pads.
#[derive(Debug)]
pub struct Afio {
    regs: RegisterFile,
    /// Two bits per EXTI line: 0 = A, 1 = C, 2 = D.
    exti_port: [u8; 8],
}

impl Default for Afio {
    fn default() -> Self {
        Self::new()
    }
}

impl Afio {
    pub fn new() -> Self {
        Afio {
            regs: RegisterFile::new(0x100 / 4),
            exti_port: [1; 8],
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        self.regs.read(off)
    }

    pub fn write(&mut self, off: u32, value: u32) {
        self.regs.write(off, value);
        // EXTICR1..4 start at 0x08, four lines to a word, four bits each.
        if (0x08..0x18).contains(&off) {
            let word = (off - 0x08) / 4;
            for line in 0..4u8 {
                let shift = u32::from(line) * 4;
                let port = ((value >> shift) & 0x3) as u8;
                let index = usize::from(word as u8 * 4 + line);
                if let Some(slot) = self.exti_port.get_mut(index) {
                    *slot = port.min(2);
                }
            }
        }
    }

    /// The port an EXTI line watches.
    pub fn exti_port(&self, line: u8) -> Port {
        match self.exti_port.get(usize::from(line)).copied().unwrap_or(1) {
            0 => Port::A,
            2 => Port::D,
            _ => Port::C,
        }
    }
}

/// The eight external-interrupt lines. On the CH32V003 lines 0–7 share one
/// interrupt source, `EXTI7_0`.
#[derive(Debug)]
pub struct Exti {
    regs: RegisterFile,
    /// Sticky pending flags, one bit per line.
    pending: u32,
    /// The level each line saw last cycle, for edge detection.
    last: [Option<bool>; 8],
}

impl Default for Exti {
    fn default() -> Self {
        Self::new()
    }
}

impl Exti {
    pub fn new() -> Self {
        Exti {
            regs: RegisterFile::new(0x100 / 4),
            pending: 0,
            last: [None; 8],
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x14 => self.pending,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            0x10 => {
                // A software interrupt event sets the line pending.
                self.pending |= value & 0xff;
                self.regs.write(off, value & 0xff);
            }
            0x14 => {
                // Write one to clear.
                self.pending &= !(value & 0xff);
            }
            off => self.regs.write(off, value),
        }
    }

    /// Sample the eight line levels and latch the enabled edges.
    pub fn sample(&mut self, levels: [Option<bool>; 8]) {
        let rising = self.regs.read(0x08) & 0xff;
        let falling = self.regs.read(0x0c) & 0xff;
        let mut edges = 0u32;
        for line in 0..8u8 {
            let now = levels[usize::from(line)];
            if let (Some(previous), Some(now)) = (self.last[usize::from(line)], now)
                && previous != now
            {
                let bit = 1u32 << line;
                if (now && rising & bit != 0) || (!now && falling & bit != 0) {
                    edges |= bit;
                }
            }
            self.last[usize::from(line)] = now;
        }
        self.pending |= edges;
    }

    pub fn irq(&self) -> bool {
        self.pending & self.regs.read(0x00) & 0xff != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pin_numbers_cover_exactly_the_18_pads() {
        let mut seen = 0u32;
        for gpio in 0..PIN_COUNT as u8 {
            let (port, bit) = locate(gpio).expect("a pad");
            assert_eq!(pin_number(port, bit), Some(gpio));
            seen += 1;
        }
        assert_eq!(seen, 18);
        assert_eq!(locate(5), Some((Port::C, 5)));
        assert_eq!(locate(8), Some((Port::D, 0)));
        assert_eq!(locate(16), Some((Port::A, 1)));
        assert_eq!(locate(18), None);
    }

    #[test]
    fn bshr_sets_and_clears_output_bits() {
        let mut port = GpioPort::new();
        port.write(0x00, 0x2222_2222); // every pin a push-pull output
        port.write(0x10, (1 << 3) | (1 << 19));
        assert!(!port.out(3));
        assert!(port.drives_gpio(3));
        port.write(0x10, 1 << 3);
        assert!(port.out(3));
        port.write(0x14, 1 << 3);
        assert!(!port.out(3));
    }

    #[test]
    fn an_input_with_a_pull_reads_the_pull_direction() {
        let mut port = GpioPort::new();
        // PC0: input with pull-up/pull-down (CNF = 10, mode = 00).
        port.write(0x00, 0x4444_4448);
        port.write(0x0c, 1);
        assert!(port.level(0), "OUTDR high selects a pull-up");
        port.write(0x0c, 0);
        assert!(!port.level(0), "OUTDR low selects a pull-down");

        port.set_external(0, Some(false));
        port.write(0x0c, 1);
        assert!(!port.level(0), "the board's drive wins over the pull");
    }

    #[test]
    fn exti_latches_a_rising_edge_once() {
        let mut exti = Exti::new();
        exti.write(0x00, 0xff); // interrupt mask
        exti.write(0x08, 0xff); // rising edges
        exti.sample([Some(false); 8]);
        exti.sample([Some(true), Some(false), None, None, None, None, None, None]);
        assert!(exti.irq());
        assert_eq!(exti.read(0x14) & 1, 1);
        exti.write(0x14, 1);
        assert!(!exti.irq());
        exti.sample([Some(true), None, None, None, None, None, None, None]);
        assert!(!exti.irq(), "no second edge without a falling one");
    }
}
