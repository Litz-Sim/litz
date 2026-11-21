//! The PL011 UART: TX/RX FIFOs, baud divisors, line control and the maskable
//! interrupt set.
//!
//! A byte written to `DR` reaches the host immediately when the UART and its
//! transmitter are enabled; the host drains [`Uart::tx`] every advance, so the
//! FIFO behaves as a 16-entry buffer between the guest and the console. A byte
//! the host pushes into [`Uart::rx`] is visible to the guest only while the
//! receiver is enabled, which is what the real block does: bytes that arrive
//! with `RXE` clear are lost.
//!
//! The block is byte-granular. The shift register, the baud generator and
//! framing are not timed; `IBRD`/`FBRD` are stored and
//! [`Uart::baud_hz`] exposes the divisor so the adapter can report the line
//! rate. The FIFO depth is 16 entries, as the task specifies for this model.

use std::collections::VecDeque;

use super::RegisterFile;

/// Register offsets from the UART base.
pub mod reg {
    pub const DR: u32 = 0x00;
    /// `RSR` on read, `ECR` on write.
    pub const RSR_ECR: u32 = 0x04;
    pub const FR: u32 = 0x18;
    pub const IBRD: u32 = 0x24;
    pub const FBRD: u32 = 0x28;
    pub const LCR_H: u32 = 0x2c;
    pub const CR: u32 = 0x30;
    pub const IFLS: u32 = 0x34;
    pub const IMS: u32 = 0x38;
    pub const RIS: u32 = 0x3c;
    pub const MIS: u32 = 0x40;
    pub const ICR: u32 = 0x44;
    pub const DMACR: u32 = 0x48;
}

/// Bit positions in `RIS`, `MIS`, `IMS` and `ICR`.
pub mod irq {
    pub const RXIM: u32 = 1 << 4;
    pub const TXIM: u32 = 1 << 5;
    pub const RTIM: u32 = 1 << 6;
    pub const FEIM: u32 = 1 << 7;
    pub const PEIM: u32 = 1 << 8;
    pub const BEIM: u32 = 1 << 9;
    pub const OEIM: u32 = 1 << 10;
    pub const ALL: u32 = 0x7f0;
}

/// `CR` bits.
pub mod cr {
    pub const UARTEN: u32 = 1 << 0;
    pub const TXE: u32 = 1 << 8;
    pub const RXE: u32 = 1 << 9;
}

/// The 125 MHz peripheral clock the modelled UART is fed from.
const PERI_HZ: u64 = 125_000_000;
/// Both FIFOs hold 16 bytes in this model.
const FIFO_DEPTH: usize = 16;

#[derive(Debug)]
pub struct Uart {
    regs: RegisterFile,
    /// Bytes the guest wrote, in the order the host sees them.
    pub tx: Vec<u8>,
    /// Bytes from the host waiting to be read from the data register.
    pub rx: VecDeque<u8>,
    ibrd: u32,
    fbrd: u32,
    /// Latched raw interrupt status. Receive and transmit bits are refreshed
    /// when their condition changes; error bits are not modelled and stay low.
    ris: u32,
    ims: u32,
}

impl Default for Uart {
    fn default() -> Self {
        Self::new()
    }
}

impl Uart {
    pub fn new() -> Self {
        Uart {
            regs: RegisterFile::new(0x100 / 4),
            tx: Vec::new(),
            rx: VecDeque::new(),
            ibrd: 0,
            fbrd: 0,
            ris: 0,
            ims: 0,
        }
    }

    fn enabled(&self) -> bool {
        self.regs.read(reg::CR) & cr::UARTEN != 0
    }

    fn tx_enabled(&self) -> bool {
        let cr = self.regs.read(reg::CR);
        cr & cr::UARTEN != 0 && cr & cr::TXE != 0
    }

    fn rx_enabled(&self) -> bool {
        let cr = self.regs.read(reg::CR);
        cr & cr::UARTEN != 0 && cr & cr::RXE != 0
    }

    fn framebuf(&self) -> u32 {
        let mut word = 1 << 7; // TXFE: nothing queued.
        if self.rx.is_empty() {
            word |= 1 << 4; // RXFE
        }
        if self.tx.len() >= FIFO_DEPTH {
            word |= 1 << 5; // TXFF
        }
        if self.rx.len() >= FIFO_DEPTH {
            word |= 1 << 6; // RXFF
        }
        // BUSY (bit 3) is clear: a write reaches the host in the same cycle.
        word
    }

    /// Recompute the transmit half of `RIS`. The PL011 asserts it while there
    /// is room in the FIFO, so a driver that enables `TXIM` must either keep
    /// writing or mask it again.
    fn refresh_tx(&mut self) {
        if self.tx_enabled() && self.tx.len() < FIFO_DEPTH {
            self.ris |= irq::TXIM;
        } else {
            self.ris &= !irq::TXIM;
        }
    }

    /// Recompute the receive half of `RIS`.
    fn refresh_rx(&mut self) {
        if self.rx_enabled() && !self.rx.is_empty() {
            self.ris |= irq::RXIM;
        } else {
            self.ris &= !irq::RXIM;
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::DR => {
                if self.rx_enabled() {
                    let value = u32::from(self.rx.pop_front().unwrap_or(0));
                    self.refresh_rx();
                    value
                } else {
                    0
                }
            }
            reg::RSR_ECR => 0,
            reg::FR => self.framebuf(),
            reg::IBRD => self.ibrd,
            reg::FBRD => self.fbrd,
            reg::LCR_H => self.regs.read(reg::LCR_H),
            reg::CR => self.regs.read(reg::CR),
            reg::IFLS => self.regs.read(reg::IFLS),
            reg::IMS => self.ims,
            reg::RIS => self.ris,
            reg::MIS => self.ris & self.ims,
            reg::ICR => 0,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::DR => {
                if self.tx_enabled() && self.tx.len() < FIFO_DEPTH {
                    self.tx.push(value as u8);
                }
                self.refresh_tx();
            }
            // Writing `ECR` clears the error bits named by the write.
            reg::RSR_ECR => self.ris &= !(value & (irq::FEIM | irq::PEIM | irq::BEIM | irq::OEIM)),
            reg::IBRD => self.ibrd = value & 0xffff,
            reg::FBRD => self.fbrd = value & 0x3f,
            reg::CR => {
                self.regs.write(reg::CR, value);
                // Turning the receiver off discards a byte that never landed,
                // and turning it on makes an already-queued byte visible.
                self.refresh_rx();
                self.refresh_tx();
            }
            reg::IMS => {
                self.ims = value & irq::ALL;
                self.refresh_rx();
                self.refresh_tx();
            }
            reg::ICR => self.ris &= !(value & irq::ALL),
            reg::RIS | reg::MIS => {}
            off => self.regs.write(off, value),
        }
    }

    pub fn tick(&mut self, _cycles: u64) {
        // The machine drains `tx` directly after every advance, so the only
        // thing that can change here is the space-based transmit condition.
        self.refresh_tx();
    }

    pub fn irq(&self) -> bool {
        self.ris & self.ims != 0
    }

    /// Re-evaluate the receive interrupt after the host delivered bytes.
    ///
    /// Bytes that arrive while the receiver is disabled are dropped, matching
    /// the PL011: `RXE` gates the FIFO, not just the interrupt.
    pub fn update_rx_irq(&mut self) {
        if !self.rx_enabled() {
            self.rx.clear();
            self.ris &= !irq::RXIM;
            return;
        }
        while self.rx.len() > FIFO_DEPTH {
            self.rx.pop_front();
            self.ris |= irq::OEIM;
        }
        self.refresh_rx();
    }

    /// The line rate implied by `IBRD`/`FBRD` and the 125 MHz peripheral
    /// clock, rounded to the nearest integer.
    pub fn baud_hz(&self) -> u32 {
        // baud = 125e6 / (16 * (IBRD + FBRD/64)); multiply through to keep
        // the division in integers.
        let divisor = 64 * u64::from(self.ibrd) + u64::from(self.fbrd & 0x3f);
        if divisor == 0 {
            return 0;
        }
        let numerator = PERI_HZ * 4;
        ((numerator + divisor / 2) / divisor) as u32
    }

    /// Whether the UART is enabled at all, for the chip adapter.
    pub fn enabled_state(&self) -> bool {
        self.enabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_tx_rx() -> Uart {
        let mut uart = Uart::new();
        uart.write(reg::CR, cr::UARTEN | cr::TXE | cr::RXE);
        uart
    }

    #[test]
    fn transmitter_only_accepts_bytes_when_enabled() {
        let mut uart = Uart::new();
        uart.write(reg::DR, u32::from(b'x'));
        assert!(uart.tx.is_empty());

        uart.write(reg::CR, cr::UARTEN);
        uart.write(reg::DR, u32::from(b'y'));
        assert!(uart.tx.is_empty(), "TXE is clear");

        uart.write(reg::CR, cr::UARTEN | cr::TXE);
        uart.write(reg::DR, u32::from(b'z'));
        assert_eq!(uart.tx, b"z");
        assert_eq!(uart.read(reg::FR) & (1 << 7), 1 << 7, "TXFE when empty");
        uart.tx.clear();
        assert_eq!(uart.read(reg::FR) & (1 << 7), 1 << 7);
    }

    #[test]
    fn receive_fifo_and_interrupt() {
        let mut uart = enabled_tx_rx();
        assert_eq!(uart.read(reg::FR) & (1 << 4), 1 << 4, "RXFE when empty");

        uart.rx.push_back(b'a');
        uart.rx.push_back(b'b');
        uart.update_rx_irq();

        assert!(!uart.irq(), "RXIM is not enabled yet");
        uart.write(reg::IMS, irq::RXIM);
        assert!(uart.irq(), "two bytes in the FIFO raise RXIM");
        assert_ne!(uart.read(reg::MIS) & irq::RXIM, 0);
        assert_eq!(uart.read(reg::FR) & (1 << 4), 0, "RXFE clear when full");
        assert_eq!(uart.read(reg::DR), u32::from(b'a'));
        assert!(uart.irq(), "one byte remains");
        assert_eq!(uart.read(reg::DR), u32::from(b'b'));
        assert!(!uart.irq(), "empty FIFO drops the level");
        assert_eq!(uart.read(reg::FR) & (1 << 4), 1 << 4);
    }

    #[test]
    fn icr_clears_a_latched_source() {
        let mut uart = enabled_tx_rx();
        uart.write(reg::IMS, irq::RXIM);
        uart.rx.push_back(b'a');
        uart.update_rx_irq();
        assert!(uart.irq());
        uart.write(reg::ICR, irq::RXIM);
        assert!(!uart.irq());
        assert_eq!(uart.read(reg::RIS) & irq::RXIM, 0);
    }

    #[test]
    fn receiver_disabled_drops_host_bytes() {
        let mut uart = Uart::new();
        uart.write(reg::CR, cr::UARTEN | cr::TXE);
        uart.rx.push_back(b'a');
        uart.update_rx_irq();
        assert!(uart.rx.is_empty());
        assert_eq!(uart.read(reg::DR), 0);
    }

    #[test]
    fn baud_divisor() {
        let mut uart = Uart::new();
        // 115200 baud: IBRD 67, FBRD 52.
        uart.write(reg::IBRD, 67);
        uart.write(reg::FBRD, 52);
        let baud = uart.baud_hz();
        assert!((baud as i64 - 115_200).abs() < 20, "got {baud}");
        // 125 MHz / (16 * 1) = 7.8125 Mbaud.
        uart.write(reg::IBRD, 1);
        uart.write(reg::FBRD, 0);
        assert_eq!(uart.baud_hz(), 7_812_500);
    }
}
