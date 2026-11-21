//! The XIP cache and the QSPI SSI controller that reads the flash part.
//!
//! Firmware does not read flash through the SSI; the XIP cache answers those
//! loads and [`crate::bus::Memory`] serves them directly from the flash image.
//! The SSI matters for the other job it has: a stock `boot2` stage opens it,
//! programs `CTRLR0`, `BAUDR` and `SER`, enables it and pushes flash commands
//! through `DR`. This model accepts that sequence, keeps the configuration
//! registers readable, and makes every `DR` write complete with a byte in the
//! receive FIFO so the status handshake (`TFNF` before a write, `RFNE` before
//! a read) terminates.
//!
//! `SR.TFNF` is bit 2 and `SR.RFNE` is bit 3, as the machine needs. The
//! DW_apb SSI datasheet names bit 1 `TFNF` and bit 2 `TFE`; this block follows
//! the bit numbers the task gave besides keeping `TFE` (bit 1) set too.

use std::collections::VecDeque;

use super::RegisterFile;

/// XIP_CTRL register offsets.
pub mod xip_ctrl {
    pub const CTRL: u32 = 0x00;
    pub const FLUSH: u32 = 0x04;
    pub const STAT: u32 = 0x08;
}

/// QSPI SSI register offsets.
pub mod ssi {
    pub const CTRLR0: u32 = 0x00;
    pub const CTRLR1: u32 = 0x04;
    pub const SSIENR: u32 = 0x08;
    pub const SER: u32 = 0x10;
    pub const BAUDR: u32 = 0x14;
    pub const TXFTLR: u32 = 0x18;
    pub const RXFTLR: u32 = 0x1c;
    pub const TXFLR: u32 = 0x20;
    pub const RXFLR: u32 = 0x24;
    pub const SR: u32 = 0x28;
    pub const IMR: u32 = 0x2c;
    pub const ISR: u32 = 0x30;
    pub const RIS: u32 = 0x34;
    pub const ICR: u32 = 0x48;
    pub const DR: u32 = 0x60;
}

/// `SR` bits.
pub mod sr {
    pub const BUSY: u32 = 1 << 0;
    pub const TFE: u32 = 1 << 1;
    pub const TFNF: u32 = 1 << 2;
    pub const RFNE: u32 = 1 << 3;
    pub const RFF: u32 = 1 << 4;
}

/// The SSI FIFOs hold eight entries, as the DW_apb default does.
const FIFO_DEPTH: usize = 8;

#[derive(Debug)]
pub struct XipCtrl {
    regs: RegisterFile,
}

impl Default for XipCtrl {
    fn default() -> Self {
        Self::new()
    }
}

impl XipCtrl {
    pub fn new() -> Self {
        XipCtrl {
            regs: RegisterFile::new(0x100 / 4),
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            // A flush completes immediately; the cache model has no tags to
            // report, so STAT stays zero.
            xip_ctrl::FLUSH | xip_ctrl::STAT => 0,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            xip_ctrl::FLUSH => {}
            xip_ctrl::STAT => {}
            off => self.regs.write(off, value),
        }
    }
}

#[derive(Debug)]
pub struct XipSsi {
    regs: RegisterFile,
    tx_fifo: VecDeque<u32>,
    rx_fifo: VecDeque<u32>,
    /// The last command a firmware pushed, useful when debugging a boot2
    /// stage.
    last_command: u32,
}

impl Default for XipSsi {
    fn default() -> Self {
        Self::new()
    }
}

impl XipSsi {
    pub fn new() -> Self {
        XipSsi {
            regs: RegisterFile::new(0x100 / 4),
            tx_fifo: VecDeque::new(),
            rx_fifo: VecDeque::new(),
            last_command: 0,
        }
    }

    fn enabled(&self) -> bool {
        self.regs.read(ssi::SSIENR) & 1 != 0
    }

    fn status(&self) -> u32 {
        let mut word = 0;
        if self.tx_fifo.is_empty() {
            word |= sr::TFE;
        }
        if self.tx_fifo.len() < FIFO_DEPTH {
            word |= sr::TFNF;
        }
        if !self.rx_fifo.is_empty() {
            word |= sr::RFNE;
        }
        if self.rx_fifo.len() >= FIFO_DEPTH {
            word |= sr::RFF;
        }
        // BUSY is clear: a command completes in the cycle it is written.
        word
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            ssi::TXFLR => self.tx_fifo.len() as u32,
            ssi::RXFLR => self.rx_fifo.len() as u32,
            ssi::SR => self.status(),
            ssi::ISR | ssi::RIS => 0,
            ssi::DR => self.rx_fifo.pop_front().unwrap_or(0),
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            ssi::DR => {
                if !self.enabled() {
                    return;
                }
                self.last_command = value;
                if self.tx_fifo.len() >= FIFO_DEPTH {
                    self.tx_fifo.pop_front();
                }
                self.tx_fifo.push_back(value);
                // The transfer completes at once: drain the command and make
                // a response byte available. The value is not interpreted;
                // flash contents are read through the XIP window.
                self.tx_fifo.clear();
                if self.rx_fifo.len() >= FIFO_DEPTH {
                    self.rx_fifo.pop_front();
                }
                self.rx_fifo.push_back(0x00);
            }
            ssi::TXFLR | ssi::RXFLR | ssi::SR | ssi::ISR | ssi::RIS => {}
            ssi::ICR => {}
            off => self.regs.write(off, value),
        }
    }

    /// The last command written to `DR`, for a test or a probe.
    pub fn last_command(&self) -> u32 {
        self.last_command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot2_style_handshake() {
        let mut ssi = XipSsi::new();
        ssi.write(ssi::CTRLR0, 0x0100_0000);
        ssi.write(ssi::BAUDR, 4);
        ssi.write(ssi::SER, 1);
        ssi.write(ssi::SSIENR, 1);

        // TFNF is set, so the boot2's first poll passes.
        assert_ne!(ssi.read(ssi::SR) & sr::TFNF, 0);

        ssi.write(ssi::DR, 0xab);
        assert_eq!(ssi.last_command(), 0xab);
        assert_ne!(ssi.read(ssi::SR) & sr::RFNE, 0);
        assert_eq!(ssi.read(ssi::SR) & sr::BUSY, 0, "a write is never busy");
        assert_eq!(ssi.read(ssi::DR), 0x00);
        assert_eq!(ssi.read(ssi::SR) & sr::RFNE, 0);
    }

    #[test]
    fn disabled_transfers_are_dropped() {
        let mut ssi = XipSsi::new();
        ssi.write(ssi::DR, 0x55);
        assert_eq!(ssi.last_command(), 0);
        assert_eq!(ssi.read(ssi::DR), 0);
    }

    #[test]
    fn xip_ctrl_stores_control_and_flushes() {
        let mut ctrl = XipCtrl::new();
        ctrl.write(xip_ctrl::CTRL, 0x5);
        assert_eq!(ctrl.read(xip_ctrl::CTRL), 0x5);
        ctrl.write(xip_ctrl::FLUSH, 1);
        assert_eq!(ctrl.read(xip_ctrl::STAT), 0);
    }
}
