//! The two-wire (I²C) master.
//!
//! This is a master-only state machine with the status codes a polled `Wire`
//! implementation expects: START (`0x08`), repeated START (`0x10`), SLA+W
//! acknowledged (`0x18`), data acknowledged (`0x28`), SLA+R acknowledged
//! (`0x40`), data received with ACK (`0x50`) and with NACK (`0x58`), and
//! `0xF8` after a STOP. Every operation completes as soon as firmware writes
//! `TWCR` with `TWINT` set, so a polling loop sees `TWINT` immediately; the
//! bus is not timed.
//!
//! # The far end of the bus
//!
//! A host attaches devices with [`Twi::set_write_callback`] and
//! [`Twi::set_read_callback`] (see also
//! [`crate::Machine::set_i2c_callbacks`]):
//!
//! ```text
//! i2c_write(addr: u8, bytes: &[u8])
//! i2c_read(addr: u8, len: usize) -> u8
//! ```
//!
//! `i2c_write` is called once when a master-transmit transaction ends with a
//! STOP, with the 7-bit address and every data byte the master acknowledged.
//! `i2c_read` is called to obtain one byte at a time; this backend requests a
//! length of one per call, so a device that serves a register read must keep
//! its own cursor. Slave mode, arbitration and clock stretching are not
//! modelled.

/// Callback for a completed master write.
pub type I2cWrite = Box<dyn FnMut(u8, &[u8])>;
/// Callback for a master read; returns the byte to put in `TWDR`.
pub type I2cRead = Box<dyn FnMut(u8, usize) -> u8>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Started,
    Transmit,
    Receive,
}

/// The TWI master register block.
pub struct Twi {
    pub twbr: u8,
    /// Prescaler bits written to `TWSR`; the status bits read back live.
    pub twsr: u8,
    pub twar: u8,
    pub twdr: u8,
    pub twcr: u8,
    pub twamr: u8,
    status: u8,
    phase: Phase,
    address: u8,
    buffer: Vec<u8>,
    twint: bool,
    write_callback: Option<I2cWrite>,
    read_callback: Option<I2cRead>,
}

impl Default for Twi {
    fn default() -> Self {
        Self::new()
    }
}

impl Twi {
    pub fn new() -> Self {
        Self {
            twbr: 0,
            twsr: 0,
            twar: 0,
            twdr: 0xff,
            twcr: 0,
            twamr: 0,
            status: 0xf8,
            phase: Phase::Idle,
            address: 0,
            buffer: Vec::new(),
            twint: false,
            write_callback: None,
            read_callback: None,
        }
    }

    /// Install the master-write callback.
    pub fn set_write_callback(&mut self, callback: I2cWrite) {
        self.write_callback = Some(callback);
    }

    /// Install the master-read callback.
    pub fn set_read_callback(&mut self, callback: I2cRead) {
        self.read_callback = Some(callback);
    }

    pub(crate) fn take_callbacks(&mut self) -> (Option<I2cWrite>, Option<I2cRead>) {
        (self.write_callback.take(), self.read_callback.take())
    }

    /// TWI status byte, including the prescaler bits from `TWSR`.
    /// The SCL rate the firmware programmed into `TWBR` and the `TWSR`
    /// prescaler bits: `F_CPU / (16 + 2 * TWBR * 4^TWPS)`.
    pub fn scl_hz(&self, cpu_hz: u32) -> u32 {
        let prescale = 1u32 << (2 * u32::from(self.twsr & 0x03));
        let divisor = 16 + 2 * u32::from(self.twbr) * prescale;
        (cpu_hz / divisor).clamp(1_000, 5_000_000)
    }

    pub(crate) fn read_status(&self) -> u8 {
        (self.status & 0xf8) | (self.twsr & 0x03)
    }

    /// `TWCR` as firmware reads it: the control bits plus `TWINT`.
    pub(crate) fn read_control(&self) -> u8 {
        (self.twcr & 0x7f) | if self.twint { 0x80 } else { 0 }
    }

    pub(crate) fn write_status(&mut self, value: u8) {
        self.twsr = value & 0x03;
    }

    pub(crate) fn write_control(&mut self, value: u8) {
        let clear_twint = value & 0x80 != 0;
        let enabled = value & 0x04 != 0;
        let stop = value & 0x10 != 0;
        let start = value & 0x20 != 0;
        self.twcr = value & 0x7f;

        if !enabled {
            self.finish_write();
            self.phase = Phase::Idle;
            self.twint = false;
            self.status = 0xf8;
            return;
        }
        if !clear_twint {
            return;
        }
        self.twint = false;

        if stop {
            self.finish_write();
            self.phase = Phase::Idle;
            self.twint = false;
            self.status = 0xf8;
            return;
        }
        if start {
            let repeated = self.phase != Phase::Idle;
            self.buffer.clear();
            self.phase = Phase::Started;
            self.status = if repeated { 0x10 } else { 0x08 };
            self.twint = true;
            return;
        }

        match self.phase {
            Phase::Started => {
                let read = self.twdr & 0x01 != 0;
                self.address = self.twdr >> 1;
                self.buffer.clear();
                if read {
                    self.phase = Phase::Receive;
                    self.status = 0x40;
                    self.load_receive_byte();
                } else {
                    self.phase = Phase::Transmit;
                    self.status = 0x18;
                }
                self.twint = true;
            }
            Phase::Transmit => {
                self.buffer.push(self.twdr);
                self.status = 0x28;
                self.twint = true;
            }
            Phase::Receive => {
                let ack = value & 0x40 != 0; // TWEA
                if ack {
                    self.status = 0x50;
                    self.load_receive_byte();
                } else {
                    self.status = 0x58;
                }
                self.twint = true;
            }
            Phase::Idle => {
                self.status = 0xf8;
                self.twint = false;
            }
        }
    }

    fn load_receive_byte(&mut self) {
        self.twdr = match self.read_callback.as_mut() {
            Some(callback) => callback(self.address, 1),
            None => 0xff,
        };
    }

    fn finish_write(&mut self) {
        if self.phase == Phase::Transmit
            && !self.buffer.is_empty()
            && let Some(callback) = self.write_callback.as_mut()
        {
            callback(self.address, &self.buffer);
        }
        self.buffer.clear();
    }

    pub(crate) fn interrupt_pending(&self) -> bool {
        self.twint && self.twcr & 0x01 != 0
    }

    pub(crate) fn can_wake(&self) -> bool {
        self.twcr & 0x01 != 0 && self.twint
    }

    pub(crate) fn acknowledge(&mut self) {
        self.twint = false;
    }
}
