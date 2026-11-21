//! The SPI master.
//!
//! The chip side is complete enough for a polled or interrupt-driven Arduino
//! SPI transaction: `SPCR`/`SPSR`/`SPDR`, master mode, the four clock dividers
//! (`SPR1:0` plus `SPI2X`), `SPIF` on completion and `WCOL` on a write during
//! a transfer. A transfer takes eight SPI clocks, so eight times the clock
//! divider in CPU cycles; `SPIF` stays clear while it runs.
//!
//! # The far end of the bus
//!
//! The simulator has no shift register to talk to. Instead the host installs a
//! callback with [`Spi::set_callback`] (or
//! [`crate::Machine::set_spi_callback`]) shaped as
//!
//! ```text
//! spi_transfer(cs_pin: u8, tx: u8) -> u8
//! ```
//!
//! It is called when a transfer completes and returns the byte the device
//! drove on MISO. `cs_pin` is the Arduino digital pin the host declared as
//! chip select with [`Spi::set_cs_pin`] (D10 by default, the Uno's SS pin); the
//! simulator does not drive that pin itself, it only reports which device the
//! host should route the byte to. With no callback installed MISO reads as
//! `0xff`, the idle level of a bus with nothing attached.

/// SPI clock divider from `SPCR.SPR1:0` and `SPSR.SPI2X`.
fn clock_divisor(spcr: u8, spsr: u8) -> u32 {
    let double = spsr & 0x01 != 0;
    match (spcr & 0x03, double) {
        (0, false) => 4,
        (1, false) => 16,
        (2, false) => 64,
        (3, false) => 128,
        (0, true) => 2,
        (1, true) => 8,
        (2, true) => 32,
        _ => 64,
    }
}

/// Callback invoked when a byte has been shifted out and one shifted in.
pub type SpiTransfer = Box<dyn FnMut(u8, u8) -> u8>;

/// SPI master state.
#[derive(Default)]
pub struct Spi {
    pub spcr: u8,
    /// `SPIF`, `WCOL` and `SPI2X` only.
    spsr: u8,
    pub spdr: u8,
    remaining: u32,
    pending: Option<u8>,
    cs_pin: u8,
    /// Set when SPSR was read while `SPIF` was set, so the following SPDR read
    /// clears the flag as the datasheet specifies.
    spif_read: bool,
    callback: Option<SpiTransfer>,
}

impl Spi {
    /// A master with no device attached: `SPIF`/`WCOL` clear and MISO idle
    /// high.
    pub fn new() -> Self {
        Self {
            spcr: 0,
            spsr: 0,
            spdr: 0xff,
            remaining: 0,
            pending: None,
            cs_pin: 10,
            spif_read: false,
            callback: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.spcr & 0x40 != 0
    }

    pub fn master(&self) -> bool {
        self.spcr & 0x10 != 0
    }

    /// Whether a transfer is in progress.
    pub fn busy(&self) -> bool {
        self.remaining > 0
    }

    /// Install the transfer callback.
    pub fn set_callback(&mut self, callback: SpiTransfer) {
        self.callback = Some(callback);
    }

    pub(crate) fn take_callback(&mut self) -> Option<SpiTransfer> {
        self.callback.take()
    }

    /// The Arduino digital pin reported to the callback as chip select.
    pub fn set_cs_pin(&mut self, pin: u8) {
        self.cs_pin = pin;
    }

    pub fn cs_pin(&self) -> u8 {
        self.cs_pin
    }

    /// The SCK rate the firmware programmed into `SPCR.SPR1:0` and
    /// `SPSR.SPI2X`: `F_CPU / divisor`, 2 MHz at the reset setting.
    pub fn clock_hz(&self, cpu_hz: u32) -> u32 {
        cpu_hz / clock_divisor(self.spcr, self.spsr)
    }

    pub(crate) fn read_status(&mut self) -> u8 {
        if self.spsr & 0x80 != 0 {
            self.spif_read = true;
        }
        self.spsr
    }

    pub(crate) fn write_status(&mut self, value: u8) {
        // Only SPI2X is writable; SPIF and WCOL are cleared by their own
        // protocol.
        self.spsr = (self.spsr & !0x01) | (value & 0x01);
    }

    pub(crate) fn read_data(&mut self) -> u8 {
        if self.spif_read && self.spsr & 0x80 != 0 {
            self.spsr &= !0x80;
            self.spif_read = false;
        }
        self.spdr
    }

    pub(crate) fn write_data(&mut self, value: u8) {
        self.spdr = value;
        if !self.enabled() || !self.master() {
            return;
        }
        if self.busy() {
            self.spsr |= 0x40; // WCOL
            return;
        }
        self.pending = Some(value);
        self.remaining = 8 * clock_divisor(self.spcr, self.spsr);
    }

    pub(crate) fn advance(&mut self, cycles: u64) {
        let Some(tx) = self.pending else {
            return;
        };
        if cycles >= u64::from(self.remaining) {
            self.remaining = 0;
            self.pending = None;
            let rx = match self.callback.as_mut() {
                Some(callback) => callback(self.cs_pin, tx),
                None => 0xff,
            };
            self.spdr = rx;
            self.spsr |= 0x80; // SPIF
            self.spif_read = false;
        } else {
            self.remaining -= cycles as u32;
        }
    }

    pub(crate) fn interrupt_pending(&self) -> bool {
        self.spcr & 0x80 != 0 && self.spsr & 0x80 != 0
    }

    pub(crate) fn can_wake(&self) -> bool {
        self.spcr & 0x80 != 0 && (self.busy() || self.spsr & 0x80 != 0)
    }

    pub(crate) fn acknowledge(&mut self) {
        self.spsr &= !0x80;
    }
}
