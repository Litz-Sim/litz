//! The Universal Serial Interface of the ATtiny25/45/85.
//!
//! The USI is a shift register with a 4-bit clock counter. Firmware uses it
//! with a software protocol (three-wire SPI or two-wire I²C) by writing a
//! `USITC` strobe to `USICR` for each clock edge; after sixteen edges the
//! counter wraps and `USIOIF` rises.
//!
//! This model shifts `USIDR` on each `USITC` strobe, taking the incoming bit
//! from the low bit of `USIBR` and moving the outgoing bit there, so a host can
//! loop data through both registers. It does not model the start-condition
//! detector, the pin directions, or the clock from the `USICS` source: only the
//! software clock (`USITC`) produces edges, which is the mode a bit-banged
//! protocol uses and the only one the simulator can drive deterministically.

/// `USISR` flag bits.
pub const USISIF: u8 = 0x80;
pub const USIOIF: u8 = 0x40;
pub const USIPF: u8 = 0x20;
pub const USIDC: u8 = 0x10;

/// `USICR` bits that enable the two interrupts and the software strobe.
pub const USISIE: u8 = 0x80;
pub const USIOIE: u8 = 0x40;
pub const USITC: u8 = 0x01;

/// USI state: shift register, buffer, counter and control.
#[derive(Default)]
pub struct Usi {
    pub usidr: u8,
    pub usibr: u8,
    /// Flag bits of `USISR`; the low four bits of the read value are the
    /// clock counter.
    flags: u8,
    counter: u8,
    pub usicr: u8,
}

impl Usi {
    pub fn new() -> Self {
        Self::default()
    }

    /// `USISR`: clock counter in bits 3:0 and the flags above it.
    pub(crate) fn read_status(&self) -> u8 {
        (self.flags & 0xf0) | (self.counter & 0x0f)
    }

    /// Write `USISR`: the counter is loaded, and a one in a flag bit clears
    /// that flag.
    pub(crate) fn write_status(&mut self, value: u8) {
        self.flags &= !(value & 0xf0);
        self.counter = value & 0x0f;
    }

    pub(crate) fn write_control(&mut self, value: u8) {
        self.usicr = value;
        if value & USITC != 0 {
            self.clock_edge();
        }
    }

    fn clock_edge(&mut self) {
        let out = self.usidr >> 7;
        let input = self.usibr & 0x01;
        self.usidr = (self.usidr << 1) | input;
        self.usibr = (self.usibr >> 1) | (out << 7);
        self.counter = self.counter.wrapping_add(1) & 0x0f;
        if self.counter == 0 {
            self.flags |= USIOIF;
        }
    }

    pub(crate) fn start_pending(&self) -> bool {
        self.usicr & USISIE != 0 && self.flags & USISIF != 0
    }

    pub(crate) fn overflow_pending(&self) -> bool {
        self.usicr & USIOIE != 0 && self.flags & USIOIF != 0
    }

    pub(crate) fn any_enabled(&self) -> bool {
        self.usicr & (USISIE | USIOIE) != 0
    }

    /// Clear the flag a USI vector owns.
    pub fn acknowledge(&mut self, vector: u8, start_vector: u8, overflow_vector: u8) {
        if vector == start_vector {
            self.flags &= !USISIF;
        } else if vector == overflow_vector {
            self.flags &= !USIOIF;
        }
    }
}
