//! I2C1: the STM32F1-shaped master.
//!
//! The controller is an event machine with the classic `STAR1`/`STAR2` flags:
//! `START` sets `SB`, an address write sets `ADDR` and picks the direction,
//! `DATAR` carries a byte, and `STOP` completes the transaction. The board
//! bridge installed by the chip adapter answers from the devices on the
//! board's `i2c0`, exactly as the RP2040 adapter does.
//!
//! # Transaction model
//!
//! * A transmitting address phases bytes into a buffer; `STOP` hands them to
//!   the write callback once and raises `STOPF`.
//! * A receiving address asks the callback for one byte per `DATAR` read.
//!   While `ACK` is set the next byte is prefetched, so a polling driver sees
//!   `RXNE` stay high between reads; clearing `ACK` asks for the last byte and
//!   lets the transfer end.
//! * A repeated start flushes a queued write phase to the board before the
//!   read, so a register pointer sent with `nostop` reaches the device before
//!   it answers.
//!
//! Clock stretching, arbitration and timing are not modelled; `BUSY` follows
//! the address phase and `STOP`.
//!
//! # Interrupts
//!
//! `I2C1_EV` covers the event flags (`SB`, `ADDR`, `STOPF`, `BTF`, `TXE`,
//! `RXNE`) under `ITEVTEN`/`ITBUFEN`; `I2C1_ER` covers the error flags
//! (`AF`, `BERR`, `ARLO`, `OVR`) under `ITERREN`.

use std::collections::VecDeque;

use super::RegisterFile;

/// Called when the master finishes a write phase.
pub type I2cWrite = Box<dyn FnMut(u8, &[u8])>;
/// Called when the master finishes a read phase: address and byte count.
pub type I2cRead = Box<dyn FnMut(u8, usize) -> Vec<u8>>;

pub mod reg {
    pub const CTLR1: u32 = 0x00;
    pub const CTLR2: u32 = 0x04;
    pub const OADDR1: u32 = 0x08;
    pub const OADDR2: u32 = 0x0c;
    pub const DATAR: u32 = 0x10;
    pub const STAR1: u32 = 0x14;
    pub const STAR2: u32 = 0x18;
    pub const CKCFGR: u32 = 0x1c;
}

/// `CTLR1` bits.
pub mod ctlr1 {
    pub const PE: u32 = 1 << 0;
    pub const NOSTRETCH: u32 = 1 << 7;
    pub const START: u32 = 1 << 8;
    pub const STOP: u32 = 1 << 9;
    pub const ACK: u32 = 1 << 10;
    pub const SWRST: u32 = 1 << 15;
}

/// `CTLR2` bits.
pub mod ctlr2 {
    pub const ITERREN: u32 = 1 << 8;
    pub const ITEVTEN: u32 = 1 << 9;
    pub const ITBUFEN: u32 = 1 << 10;
}

/// `STAR1` bits.
pub mod star1 {
    pub const SB: u32 = 1 << 0;
    pub const ADDR: u32 = 1 << 1;
    pub const BTF: u32 = 1 << 2;
    pub const STOPF: u32 = 1 << 4;
    pub const RXNE: u32 = 1 << 6;
    pub const TXE: u32 = 1 << 7;
    pub const BERR: u32 = 1 << 8;
    pub const ARLO: u32 = 1 << 9;
    pub const AF: u32 = 1 << 10;
    pub const OVR: u32 = 1 << 11;
    pub const ERRORS: u32 = BERR | ARLO | AF | OVR;
    pub const EVENTS: u32 = SB | ADDR | BTF | STOPF;
}

/// `STAR2` bits.
pub mod star2 {
    pub const MSL: u32 = 1 << 0;
    pub const BUSY: u32 = 1 << 1;
    pub const TRA: u32 = 1 << 2;
}

pub struct I2c {
    regs: RegisterFile,
    write_cb: Option<I2cWrite>,
    read_cb: Option<I2cRead>,
    tx: Vec<u8>,
    rx: VecDeque<u8>,
    address: u8,
    reading: bool,
    /// The address phase completed and `ADDR` has not been read yet.
    addressed: bool,
    busy: bool,
    sb: bool,
    stopf: bool,
    ack: bool,
}

impl Default for I2c {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for I2c {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("I2c")
            .field("write", &self.write_cb.is_some())
            .field("read", &self.read_cb.is_some())
            .field("tx", &self.tx.len())
            .field("rx", &self.rx.len())
            .finish()
    }
}

impl I2c {
    pub fn new() -> Self {
        I2c {
            regs: RegisterFile::new(0x100 / 4),
            write_cb: None,
            read_cb: None,
            tx: Vec::new(),
            rx: VecDeque::new(),
            address: 0,
            reading: false,
            addressed: false,
            busy: false,
            sb: false,
            stopf: false,
            ack: true,
        }
    }

    pub fn set_callbacks(&mut self, write: I2cWrite, read: I2cRead) {
        self.write_cb = Some(write);
        self.read_cb = Some(read);
    }

    pub fn take_callbacks(&mut self) -> (Option<I2cWrite>, Option<I2cRead>) {
        (self.write_cb.take(), self.read_cb.take())
    }

    fn enabled(&self) -> bool {
        self.regs.read(reg::CTLR1) & ctlr1::PE != 0
    }

    fn flush_write(&mut self) {
        if self.tx.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.tx);
        if let Some(callback) = self.write_cb.as_mut() {
            callback(self.address, &bytes);
        }
    }

    fn fetch_read(&mut self) {
        let bytes = match self.read_cb.as_mut() {
            Some(callback) => callback(self.address, 1),
            None => vec![0xff],
        };
        if let Some(byte) = bytes.into_iter().next() {
            self.rx.push_back(byte);
        }
    }

    fn begin_address(&mut self, byte: u32) {
        self.address = ((byte >> 1) & 0x7f) as u8;
        self.reading = byte & 1 != 0;
        self.addressed = true;
        self.busy = true;
        if self.reading {
            // A queued register pointer reaches the device before its read.
            self.flush_write();
            self.rx.clear();
            self.fetch_read();
        } else {
            self.tx.clear();
        }
    }

    fn stop(&mut self) {
        if !self.reading {
            self.flush_write();
        }
        self.reading = false;
        self.busy = false;
        self.addressed = false;
        self.stopf = true;
        self.rx.clear();
    }

    fn status1(&self) -> u32 {
        let mut value = 0;
        if self.sb {
            value |= star1::SB;
        }
        if self.addressed {
            value |= star1::ADDR;
        }
        if self.stopf {
            value |= star1::STOPF;
        }
        if self.reading && !self.rx.is_empty() {
            value |= star1::RXNE;
        }
        if !self.reading && self.addressed {
            value |= star1::TXE;
        }
        value
    }

    fn status2(&self) -> u32 {
        let mut value = 0;
        if self.busy {
            value |= star2::BUSY | star2::MSL;
        }
        if self.addressed && !self.reading {
            value |= star2::TRA;
        }
        value
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::STAR1 => {
                let value = self.status1();
                // Reading STAR1 acknowledges the start and address phases,
                // which is the sequence a blocking driver follows.
                self.sb = false;
                self.addressed = false;
                self.stopf = false;
                value
            }
            reg::STAR2 => self.status2(),
            reg::DATAR => {
                let byte = self.rx.pop_front();
                if self.reading && !self.rx.is_empty() {
                    // The driver has not finished: keep the queue ahead of it.
                } else if self.reading && self.ack_summary() {
                    self.fetch_read();
                }
                u32::from(byte.unwrap_or(0))
            }
            reg::OADDR1 => self.regs.read(reg::OADDR1),
            off => self.regs.read(off),
        }
    }

    fn ack_summary(&self) -> bool {
        self.ack
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::CTLR1 => {
                let previous = self.regs.read(reg::CTLR1);
                self.regs.write(reg::CTLR1, value);
                self.ack = value & ctlr1::ACK != 0;
                if value & ctlr1::SWRST != 0 {
                    self.tx.clear();
                    self.rx.clear();
                    self.busy = false;
                    self.addressed = false;
                    self.sb = false;
                    self.stopf = false;
                }
                if value & ctlr1::START != 0 && self.enabled() {
                    self.regs.write(reg::CTLR1, previous & !ctlr1::START);
                    self.sb = true;
                    self.busy = true;
                }
                if value & ctlr1::STOP != 0 {
                    self.regs
                        .write(reg::CTLR1, self.regs.read(reg::CTLR1) & !ctlr1::STOP);
                    self.stop();
                }
            }
            reg::OADDR1 => {
                self.regs.write(reg::OADDR1, value & 0xffff);
                if !self.sb {
                    // Without a START the byte is just a register value; a
                    // driver that relies on it must have issued one.
                    self.sb = true;
                }
                self.begin_address(value);
                self.sb = false;
            }
            reg::DATAR => {
                if !self.reading {
                    if self.tx.len() < 16 {
                        self.tx.push(value as u8);
                    }
                }
            }
            off => self.regs.write(off, value),
        }
    }

    pub fn irq_event(&self) -> bool {
        let control = self.regs.read(reg::CTLR2);
        if control & ctlr2::ITEVTEN == 0 && control & ctlr2::ITBUFEN == 0 {
            return false;
        }
        let status = self.status1();
        let events = status & star1::EVENTS;
        let buffers = status & (star1::TXE | star1::RXNE);
        (control & ctlr2::ITEVTEN != 0 && events != 0)
            || (control & ctlr2::ITBUFEN != 0 && buffers != 0)
    }

    pub fn irq_error(&self) -> bool {
        self.regs.read(reg::CTLR2) & ctlr2::ITERREN != 0 && self.status1() & star1::ERRORS != 0
    }

    pub fn tick(&mut self, _cycles: u64) {
        // Transactions complete on the register write that ends them.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    type Log = Rc<RefCell<Vec<(u8, Vec<u8>)>>>;

    fn master() -> (I2c, Log) {
        let mut i2c = I2c::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&log);
        i2c.set_callbacks(
            Box::new(move |address, bytes| sink.borrow_mut().push((address, bytes.to_vec()))),
            Box::new(|_, _| vec![0x5a]),
        );
        i2c.write(reg::CTLR1, ctlr1::PE | ctlr1::ACK);
        (i2c, log)
    }

    fn address(i2c: &mut I2c, byte: u32) {
        i2c.write(reg::CTLR1, ctlr1::PE | ctlr1::ACK | ctlr1::START);
        assert_ne!(i2c.read(reg::STAR1) & star1::SB, 0);
        i2c.write(reg::OADDR1, byte);
        assert_ne!(i2c.read(reg::STAR1) & star1::ADDR, 0);
    }

    #[test]
    fn a_write_phase_reaches_the_callback_at_stop() {
        let (mut i2c, log) = master();
        address(&mut i2c, 0x48 << 1);
        i2c.write(reg::DATAR, 0x01);
        i2c.write(reg::DATAR, 0x02);
        assert!(log.borrow().is_empty());
        i2c.write(reg::CTLR1, ctlr1::PE | ctlr1::ACK | ctlr1::STOP);
        assert_eq!(*log.borrow(), vec![(0x48, vec![0x01, 0x02])]);
        assert_ne!(i2c.read(reg::STAR1) & star1::STOPF, 0);
        assert_eq!(i2c.read(reg::STAR2) & star2::BUSY, 0);
    }

    #[test]
    fn a_read_phase_prefetches_while_ack_is_set() {
        let (mut i2c, _) = master();
        address(&mut i2c, (0x48 << 1) | 1);
        assert_ne!(i2c.read(reg::STAR1) & star1::RXNE, 0);
        assert_eq!(i2c.read(reg::DATAR), 0x5a);
        assert_ne!(i2c.read(reg::STAR1) & star1::RXNE, 0, "prefetched again");
    }

    #[test]
    fn a_pointer_write_is_flushed_before_the_repeated_start_read() {
        let (mut i2c, log) = master();
        address(&mut i2c, 0x48 << 1);
        i2c.write(reg::DATAR, 0x10);
        address(&mut i2c, (0x48 << 1) | 1);
        assert_eq!(*log.borrow(), vec![(0x48, vec![0x10])]);
    }
}
