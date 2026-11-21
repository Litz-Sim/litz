//! The PL022 SPI master: 4- to 16-bit frames, four clock modes and an 8-entry
//! receive FIFO.
//!
//! Every frame is handed to the board as soon as the guest writes `SSPDR`, and
//! the answer is what the FIFO then holds, so a firmware that polls
//! `SSPSR.BSY` or waits for `RNE` sees the transfer complete. The model does
//! not clock the bus, so `BSY` is clear and `TFE`/`TNF` are set whenever the
//! block is idle.
//!
//! # Chip select
//!
//! On the RP2040 the PL022 is a plain master: the chip selects are GPIOs the
//! function-select mux routes to the block, not a field of a peripheral
//! register (there is no `SSPCR1` bit naming a select). The model therefore
//! always reports select 0 to the board, which is what a driver that raises
//! CS0 through SIO/IO_BANK0 does. A board with several devices on one SPI
//! controller sees every transfer on select 0; a firmware that drives two
//! selects needs the chip-select lines to reach the board separately, which
//! this peripheral model cannot see.
//!
//! # What is approximated
//!
//! The transmit FIFO is not modelled as a queue: a frame is transferred on the
//! `SSPDR` write and the received word is queued, so `BSY` never asserts and
//! `SSPRIS.TX` is set while the block is idle. The receive timeout counter and
//! the DMA request lines are storage; loopback (`SSPCR1.LBM`) is honoured by
//! returning the transmitted word instead of calling the board.

use std::collections::VecDeque;

use super::RegisterFile;

/// `(chip select, bytes out) -> bytes in`.
pub type SpiTransfer = Box<dyn FnMut(u8, &[u8]) -> Vec<u8>>;

/// Register offsets from the SPI base.
pub mod reg {
    pub const CR0: u32 = 0x00;
    pub const CR1: u32 = 0x04;
    pub const DR: u32 = 0x08;
    pub const SR: u32 = 0x0c;
    pub const CPSR: u32 = 0x10;
    pub const IMSC: u32 = 0x14;
    pub const RIS: u32 = 0x18;
    pub const MIS: u32 = 0x1c;
    pub const ICR: u32 = 0x20;
    pub const DMACR: u32 = 0x24;
}

/// `SSPCR0` bits.
pub mod cr0 {
    /// Data size select, minus one: 3 for 4 bits, 15 for 16 bits.
    pub const DSS_MASK: u32 = 0x0f;
    /// Frame format, `[5:4]`: `00` SPI, `01` TI synchronous serial,
    /// `10` Microwire, `11` reserved.
    pub const FRF_MASK: u32 = 0b11 << 4;
    pub const SPO: u32 = 1 << 6;
    pub const SPH: u32 = 1 << 7;
    /// Serial clock rate, bits `[15:8]`.
    pub const SCR_MASK: u32 = 0xff << 8;
    pub const VALID: u32 = 0xffff;
}

/// `SSPCR1` bits.
pub mod cr1 {
    pub const LBM: u32 = 1 << 0;
    pub const SSE: u32 = 1 << 1;
    pub const MS: u32 = 1 << 2;
    pub const SOD: u32 = 1 << 3;
    pub const VALID: u32 = 0x0f;
}

/// `SSPSR` bits.
pub mod sr {
    pub const TFE: u32 = 1 << 0;
    pub const TNF: u32 = 1 << 1;
    pub const RNE: u32 = 1 << 2;
    pub const RFF: u32 = 1 << 3;
    pub const BSY: u32 = 1 << 4;
}

/// Interrupt bits shared by `SSPIMSC`, `SSPRIS` and `SSPMIS`.
pub mod irq {
    pub const ROR: u32 = 1 << 0;
    pub const RT: u32 = 1 << 1;
    pub const RX: u32 = 1 << 2;
    pub const TX: u32 = 1 << 3;
    pub const VALID: u32 = 0x0f;
}

/// The PL022 in the RP2040 has 8-entry transmit and receive FIFOs.
pub const FIFO_DEPTH: usize = 8;

pub struct Spi {
    regs: RegisterFile,
    /// The board's transfer, installed by the chip adapter.
    transfer: Option<SpiTransfer>,
    /// Received words waiting for the guest, one per frame.
    rx: VecDeque<u16>,
    /// Sticky receive-overrun bit, cleared by `SSPICR.RORIC`.
    overrun: bool,
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
        }
    }

    pub fn set_transfer(&mut self, transfer: SpiTransfer) {
        self.transfer = Some(transfer);
    }

    /// Hand the installed callback back so a machine reset can reinstall it.
    pub fn take_transfer(&mut self) -> Option<SpiTransfer> {
        self.transfer.take()
    }

    fn enabled(&self) -> bool {
        self.regs.read(reg::CR1) & cr1::SSE != 0
    }

    fn loopback(&self) -> bool {
        self.regs.read(reg::CR1) & cr1::LBM != 0
    }

    /// Frame width in bits, 4 through 16.
    ///
    /// `DSS` is the width minus one; the datasheet marks 0 through 2 as
    /// unsupported, so a firmware that has not programmed a format yet still
    /// gets the minimum 4-bit frame rather than a one-bit one.
    fn data_bits(&self) -> u32 {
        ((self.regs.read(reg::CR0) & cr0::DSS_MASK) + 1).clamp(4, 16)
    }

    fn imsc(&self) -> u32 {
        self.regs.read(reg::IMSC) & irq::VALID
    }

    /// `SSPRIS`: the receive-overrun bit is sticky; the others follow the FIFO
    /// and the (instant) transmitter.
    fn ris(&self) -> u32 {
        let mut value = 0;
        if self.overrun {
            value |= irq::ROR;
        }
        if !self.rx.is_empty() {
            value |= irq::RX;
        }
        // The transmitter is always ready to take another frame.
        value |= irq::TX;
        value
    }

    /// Push one frame through the board and queue the answer.
    fn transfer_frame(&mut self, value: u16) {
        if !self.enabled() {
            // With SSE clear the block ignores data-register writes.
            return;
        }
        let bits = self.data_bits();
        let words = if bits <= 8 { 1 } else { 2 };
        let tx: Vec<u8> = if words == 1 {
            vec![value as u8]
        } else {
            // SPI shifts the most significant bit of the frame first, so a
            // 9- to 16-bit frame is two bytes, high byte first.
            vec![(value >> 8) as u8, value as u8]
        };
        let rx = if self.loopback() {
            tx.clone()
        } else if let Some(transfer) = self.transfer.as_mut() {
            transfer(0, &tx)
        } else {
            // Nothing on MISO: the line floats high.
            vec![0xff; words]
        };
        let frame = if words == 1 {
            u16::from(rx.first().copied().unwrap_or(0xff))
        } else {
            (u16::from(rx.first().copied().unwrap_or(0xff)) << 8)
                | u16::from(rx.get(1).copied().unwrap_or(0xff))
        };
        // Receive data is right-justified and only `DSS` bits are meaningful.
        let received = frame & ((1u32 << bits) - 1) as u16;
        if self.rx.len() < FIFO_DEPTH {
            self.rx.push_back(received);
        } else {
            // The overrun interrupt keeps the oldest word and reports the
            // frame that could not be stored.
            self.overrun = true;
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::DR => u32::from(self.rx.pop_front().unwrap_or(0)),
            reg::SR => {
                let mut value = sr::TFE | sr::TNF;
                if !self.rx.is_empty() {
                    value |= sr::RNE;
                }
                if self.rx.len() >= FIFO_DEPTH {
                    value |= sr::RFF;
                }
                // BSY is clear: a frame completes on the write.
                value
            }
            reg::RIS => self.ris(),
            reg::MIS => self.ris() & self.imsc(),
            // The clear registers read as zero.
            reg::ICR => 0,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::CR0 => self.regs.write(reg::CR0, value & cr0::VALID),
            reg::CR1 => self.regs.write(reg::CR1, value & cr1::VALID),
            reg::DR => self.transfer_frame(value as u16),
            reg::CPSR => self.regs.write(reg::CPSR, value & 0xff),
            reg::IMSC => self.regs.write(reg::IMSC, value & irq::VALID),
            // RORIC clears the overrun, RTIC the (unmodelled) timeout.
            reg::ICR => {
                if value & 1 != 0 {
                    self.overrun = false;
                }
            }
            reg::DMACR => self.regs.write(reg::DMACR, value & 0x03),
            // Status registers ignore writes.
            reg::SR | reg::RIS | reg::MIS => {}
            off => self.regs.write(off, value),
        }
    }

    pub fn tick(&mut self, _cycles: u64) {
        // Transfers complete on the data-register write.
    }

    pub fn irq(&self) -> bool {
        self.ris() & self.imsc() != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    type TransferLog = Rc<RefCell<Vec<(u8, Vec<u8>)>>>;

    fn enabled() -> Spi {
        let mut spi = Spi::new();
        // DSS = 7: 8-bit frames, the width most drivers use.
        spi.write(reg::CR0, 7);
        spi.write(reg::CR1, cr1::SSE);
        spi
    }

    #[test]
    fn a_data_write_transfers_and_the_answer_reads_back() {
        let mut spi = enabled();
        let seen: TransferLog = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        spi.set_transfer(Box::new(move |chip_select, tx| {
            sink.borrow_mut().push((chip_select, tx.to_vec()));
            vec![0xa5]
        }));

        spi.write(reg::DR, 0x3c);

        assert_eq!(*seen.borrow(), vec![(0, vec![0x3c])]);
        assert_eq!(spi.read(reg::DR), 0xa5);
        assert_eq!(spi.read(reg::SR) & sr::RNE, 0, "FIFO drained");
    }

    #[test]
    fn a_sixteen_bit_frame_uses_two_bytes() {
        let mut spi = enabled();
        let seen: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        spi.set_transfer(Box::new(move |_, tx| {
            sink.borrow_mut().push(tx.to_vec());
            vec![0x12, 0x34]
        }));
        spi.write(reg::CR0, 15); // DSS = 15: 16 bits

        spi.write(reg::DR, 0xabcd);
        assert_eq!(*seen.borrow(), vec![vec![0xab, 0xcd]]);
        assert_eq!(spi.read(reg::DR), 0x1234);
    }

    #[test]
    fn control_registers_read_back() {
        let mut spi = Spi::new();
        spi.write(reg::CPSR, 0x2a);
        assert_eq!(spi.read(reg::CPSR), 0x2a);

        let cr0 = (4 << 8) | cr0::SPH | cr0::SPO | 7;
        spi.write(reg::CR0, cr0);
        assert_eq!(spi.read(reg::CR0), cr0);

        spi.write(reg::CR1, cr1::SSE | cr1::MS);
        assert_eq!(spi.read(reg::CR1), cr1::SSE | cr1::MS);
    }

    #[test]
    fn the_status_register_reports_an_empty_drained_block() {
        let mut spi = enabled();
        let status = spi.read(reg::SR);
        assert_ne!(status & sr::TFE, 0);
        assert_ne!(status & sr::TNF, 0);
        assert_eq!(status & sr::RNE, 0);
        assert_eq!(status & sr::BSY, 0, "the transfer is instant");
    }

    #[test]
    fn loopback_returns_the_transmitted_word() {
        let mut spi = Spi::new();
        spi.write(reg::CR0, 7);
        spi.write(reg::CR1, cr1::SSE | cr1::LBM);
        spi.write(reg::DR, 0x5a);
        assert_eq!(spi.read(reg::DR), 0x5a);
    }

    #[test]
    fn the_interrupt_mask_gates_the_line() {
        let mut spi = enabled();
        spi.set_transfer(Box::new(|_, _| vec![0x01]));
        spi.write(reg::DR, 0);
        assert!(!spi.irq());
        spi.write(reg::IMSC, irq::RX);
        assert!(spi.irq());
        assert_ne!(spi.read(reg::MIS) & irq::RX, 0);
        spi.read(reg::DR);
        assert!(!spi.irq());
    }
}
