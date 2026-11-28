//! SPI1: the STM32F0-shaped master.
//!
//! A write to `DATAR` with `SPE` and `MSTR` set hands one frame to the board's
//! `spi0` controller and queues the answer, so a firmware polling `RXNE` or
//! `BSY` sees the transfer finish. Frames are 8 or 16 bits (`DFF`), most
//! significant bit first. `NSS` is software-controlled on this part, so the
//! model always presents chip select 0; a device wired to another select needs
//! the board's own select pin, which the SPI block cannot see.
//!
//! As with USART, `TXE` is a level but its interrupt is an event a `STATR`
//! read acknowledges, so an idle driver that leaves `TXEIE` set does not
//! interrupt on every instruction.

use std::collections::VecDeque;

use super::RegisterFile;

/// `(chip select, bytes out) -> bytes in`.
pub type SpiTransfer = Box<dyn FnMut(u8, &[u8]) -> Vec<u8>>;

pub mod reg {
    pub const CTLR1: u32 = 0x00;
    pub const CTLR2: u32 = 0x04;
    pub const STATR: u32 = 0x08;
    pub const DATAR: u32 = 0x0c;
    pub const CRCPR: u32 = 0x10;
    pub const RXCRCR: u32 = 0x14;
    pub const TXCRCR: u32 = 0x18;
    pub const I2SCFGR: u32 = 0x1c;
    pub const I2SPR: u32 = 0x20;
}

/// `CTLR1` bits.
pub mod ctlr1 {
    pub const CPHA: u32 = 1 << 0;
    pub const CPOL: u32 = 1 << 1;
    pub const MSTR: u32 = 1 << 2;
    pub const BR_SHIFT: u32 = 3;
    pub const SPE: u32 = 1 << 6;
    pub const LSBFIRST: u32 = 1 << 7;
    pub const SSI: u32 = 1 << 8;
    pub const SSM: u32 = 1 << 9;
    pub const RXONLY: u32 = 1 << 10;
    pub const DFF: u32 = 1 << 11;
}

/// `CTLR2` bits.
pub mod ctlr2 {
    pub const RXDMAEN: u32 = 1 << 0;
    pub const TXDMAEN: u32 = 1 << 1;
    pub const SSOE: u32 = 1 << 2;
    pub const ERRIE: u32 = 1 << 5;
    pub const RXNEIE: u32 = 1 << 6;
    pub const TXEIE: u32 = 1 << 7;
}

/// `STATR` bits.
pub mod statr {
    pub const RXNE: u32 = 1 << 0;
    pub const TXE: u32 = 1 << 1;
    pub const BSY: u32 = 1 << 3;
    pub const OVR: u32 = 1 << 6;
}

const FIFO_DEPTH: usize = 8;

pub struct Spi {
    regs: RegisterFile,
    transfer: Option<SpiTransfer>,
    rx: VecDeque<u16>,
    overrun: bool,
    txe_event: bool,
}

impl Default for Spi {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Spi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spi")
            .field("transfer", &self.transfer.is_some())
            .field("rx", &self.rx.len())
            .finish()
    }
}

impl Spi {
    pub fn new() -> Self {
        Spi {
            regs: RegisterFile::new(0x100 / 4),
            transfer: None,
            rx: VecDeque::new(),
            overrun: false,
            txe_event: false,
        }
    }

    pub fn set_transfer(&mut self, transfer: SpiTransfer) {
        self.transfer = Some(transfer);
    }

    pub fn take_transfer(&mut self) -> Option<SpiTransfer> {
        self.transfer.take()
    }

    fn enabled(&self) -> bool {
        self.regs.read(reg::CTLR1) & ctlr1::SPE != 0
    }

    fn master(&self) -> bool {
        self.regs.read(reg::CTLR1) & ctlr1::MSTR != 0
    }

    fn wide(&self) -> bool {
        self.regs.read(reg::CTLR1) & ctlr1::DFF != 0
    }

    /// The clock rate as a divisor of the APB2 clock (`BR` field).
    pub fn clock_divisor(&self) -> u32 {
        2u32 << ((self.regs.read(reg::CTLR1) >> ctlr1::BR_SHIFT) & 7)
    }

    fn transfer_word(&mut self, value: u16) {
        if !self.enabled() || !self.master() {
            return;
        }
        let words = if self.wide() { 2 } else { 1 };
        let tx: Vec<u8> = if words == 2 {
            vec![(value >> 8) as u8, value as u8]
        } else {
            vec![value as u8]
        };
        let answer = match self.transfer.as_mut() {
            Some(callback) => callback(0, &tx),
            None => vec![0xff; words],
        };
        let frame = if words == 2 {
            (u16::from(answer.first().copied().unwrap_or(0xff)) << 8)
                | u16::from(answer.get(1).copied().unwrap_or(0xff))
        } else {
            u16::from(answer.first().copied().unwrap_or(0xff))
        };
        if self.rx.len() >= FIFO_DEPTH {
            self.overrun = true;
        } else {
            self.rx.push_back(frame);
        }
        self.txe_event = true;
    }

    fn status(&self) -> u32 {
        let mut value = statr::TXE;
        if !self.rx.is_empty() {
            value |= statr::RXNE;
        }
        if self.overrun {
            value |= statr::OVR;
        }
        // BSY is clear: a frame completes on the data-register write.
        value
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::DATAR => {
                let word = self.rx.pop_front().unwrap_or(0);
                self.regs.write(reg::DATAR, u32::from(word));
                u32::from(word)
            }
            reg::STATR => {
                let value = self.status();
                self.txe_event = false;
                value
            }
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::CTLR1 => {
                let was = self.enabled();
                self.regs.write(reg::CTLR1, value);
                if !was && self.enabled() {
                    self.txe_event = true;
                }
            }
            reg::CTLR2 => self.regs.write(reg::CTLR2, value),
            reg::DATAR => self.transfer_word(value as u16),
            reg::CRCPR | reg::I2SCFGR | reg::I2SPR => self.regs.write(off, value),
            // Status and receive-CRC registers ignore writes.
            _ => {}
        }
    }

    pub fn irq(&self) -> bool {
        let control = self.regs.read(reg::CTLR2);
        let status = self.status();
        (control & ctlr2::RXNEIE != 0 && status & statr::RXNE != 0)
            || (control & ctlr2::TXEIE != 0 && self.txe_event)
            || (control & ctlr2::ERRIE != 0 && status & statr::OVR != 0)
    }

    pub fn tick(&mut self, _cycles: u64) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn enabled() -> Spi {
        let mut spi = Spi::new();
        spi.write(reg::CTLR1, ctlr1::SPE | ctlr1::MSTR);
        spi
    }

    #[test]
    fn a_frame_round_trips_through_the_transfer() {
        let mut spi = enabled();
        let seen: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        spi.set_transfer(Box::new(move |_, tx| {
            sink.borrow_mut().push(tx.to_vec());
            vec![0xa5]
        }));
        spi.write(reg::DATAR, 0x3c);
        assert_eq!(*seen.borrow(), vec![vec![0x3c]]);
        assert_eq!(spi.read(reg::DATAR), 0xa5);
        assert_eq!(spi.read(reg::STATR) & statr::RXNE, 0);
    }

    #[test]
    fn a_sixteen_bit_frame_uses_two_bytes() {
        let mut spi = Spi::new();
        spi.write(reg::CTLR1, ctlr1::SPE | ctlr1::MSTR | ctlr1::DFF);
        spi.set_transfer(Box::new(|_, _| vec![0x12, 0x34]));
        spi.write(reg::DATAR, 0xabcd);
        assert_eq!(spi.read(reg::DATAR), 0x1234);
    }

    #[test]
    fn the_transmit_interrupt_is_an_event() {
        let mut spi = enabled();
        spi.write(reg::CTLR2, ctlr2::TXEIE);
        spi.write(reg::DATAR, 0);
        assert!(spi.irq());
        let _ = spi.read(reg::STATR);
        assert!(!spi.irq());
    }
}
