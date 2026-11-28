//! USART1: 8N1, the flags and the interrupt set.
//!
//! The block is byte-granular: the transmit shift register completes in the
//! same cycle as the `DATAR` write, so `TXE` and `TC` are set immediately and
//! the byte reaches the console. Bytes the host pushes in wait in the receive
//! queue and set `RXNE`; reading `DATAR` pops one.
//!
//! `TXE` and `TC` are level flags, but a driver that leaves `TXEIE` enabled
//! with nothing to send must not be interrupted; the model therefore raises
//! the transmit interrupt as an event that a read of `STATR` clears, the same
//! handshake the RP2040's PL011 model uses.
//!
//! The line rate is `fCK / BRR`, with the 16-times oversampling the 8N1 mode
//! uses. `fCK` comes from the RCC's APB2 clock, which the adapter reads back.

use std::collections::VecDeque;

use super::RegisterFile;

/// Register offsets from the USART base.
pub mod reg {
    pub const STATR: u32 = 0x00;
    pub const DATAR: u32 = 0x04;
    pub const BRR: u32 = 0x08;
    pub const CTLR1: u32 = 0x0c;
    pub const CTLR2: u32 = 0x10;
    pub const CTLR3: u32 = 0x14;
    pub const GPR: u32 = 0x18;
}

/// `STATR` bits.
pub mod statr {
    pub const PE: u32 = 1 << 0;
    pub const FE: u32 = 1 << 1;
    pub const NE: u32 = 1 << 2;
    pub const ORE: u32 = 1 << 3;
    pub const IDLE: u32 = 1 << 4;
    pub const RXNE: u32 = 1 << 5;
    pub const TC: u32 = 1 << 6;
    pub const TXE: u32 = 1 << 7;
}

/// `CTLR1` bits.
pub mod ctlr1 {
    pub const SBK: u32 = 1 << 0;
    pub const RWU: u32 = 1 << 1;
    pub const RE: u32 = 1 << 2;
    pub const TE: u32 = 1 << 3;
    pub const IDLEIE: u32 = 1 << 4;
    pub const RXNEIE: u32 = 1 << 5;
    pub const TCIE: u32 = 1 << 6;
    pub const TXEIE: u32 = 1 << 7;
    pub const PEIE: u32 = 1 << 8;
    pub const PS: u32 = 1 << 9;
    pub const PCE: u32 = 1 << 10;
    pub const WAKE: u32 = 1 << 11;
    pub const M: u32 = 1 << 12;
    pub const UE: u32 = 1 << 13;
}

/// The receive queue the model keeps.
const RX_DEPTH: usize = 16;

#[derive(Debug)]
pub struct Usart {
    regs: RegisterFile,
    rx: VecDeque<u8>,
    /// Bytes the guest transmitted since the host last collected them.
    pub tx: Vec<u8>,
    /// A transmit event the driver has not acknowledged by reading `STATR`.
    txe_event: bool,
}

impl Default for Usart {
    fn default() -> Self {
        Self::new()
    }
}

impl Usart {
    pub fn new() -> Self {
        Usart {
            regs: RegisterFile::new(0x100 / 4),
            rx: VecDeque::new(),
            tx: Vec::new(),
            txe_event: false,
        }
    }

    fn control(&self) -> u32 {
        self.regs.read(reg::CTLR1)
    }

    pub fn enabled(&self) -> bool {
        self.control() & ctlr1::UE != 0
    }

    pub fn transmitter_enabled(&self) -> bool {
        self.enabled() && self.control() & ctlr1::TE != 0
    }

    pub fn receiver_enabled(&self) -> bool {
        self.enabled() && self.control() & ctlr1::RE != 0
    }

    fn status(&self) -> u32 {
        let mut value = if self.tx.is_empty() {
            statr::TXE | statr::TC
        } else {
            0
        };
        if !self.rx.is_empty() {
            value |= statr::RXNE;
        }
        value
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::STATR => {
                let value = self.status();
                // Reading the status register acknowledges the transmit
                // event, which is what stops an idle TXE interrupt.
                self.txe_event = false;
                self.regs.write(reg::STATR, 0);
                value
            }
            reg::DATAR => {
                if self.receiver_enabled() {
                    u32::from(self.rx.pop_front().unwrap_or(0))
                } else {
                    0
                }
            }
            reg::BRR => self.regs.read(reg::BRR) & 0xffff,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::DATAR => {
                if self.transmitter_enabled() {
                    self.tx.push(value as u8);
                    self.txe_event = true;
                }
            }
            reg::BRR => self.regs.write(reg::BRR, value & 0xffff),
            reg::CTLR1 => {
                let was_enabled = self.enabled();
                self.regs.write(reg::CTLR1, value);
                if !was_enabled && self.enabled() {
                    self.txe_event = true;
                }
                if !self.receiver_enabled() {
                    self.rx.clear();
                }
            }
            off => self.regs.write(off, value),
        }
    }

    pub fn irq(&self) -> bool {
        let control = self.control();
        let status = self.status();
        (control & ctlr1::RXNEIE != 0 && status & statr::RXNE != 0)
            || (control & ctlr1::TCIE != 0 && status & statr::TC != 0)
            || (control & ctlr1::TXEIE != 0 && status & statr::TXE != 0 && self.txe_event)
            || (control & ctlr1::IDLEIE != 0 && status & statr::IDLE != 0)
    }

    /// Bytes the host sent, made visible only while the receiver is on.
    pub fn push_input(&mut self, bytes: &[u8]) {
        if !self.receiver_enabled() {
            return;
        }
        for byte in bytes {
            if self.rx.len() >= RX_DEPTH {
                self.rx.pop_front();
            }
            self.rx.push_back(*byte);
        }
    }

    /// How many bytes the receiver will take right now.
    pub fn receive_room(&self) -> usize {
        if !self.receiver_enabled() {
            return 0;
        }
        RX_DEPTH.saturating_sub(self.rx.len())
    }

    /// Bytes the guest has transmitted, in order.
    pub fn take_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx)
    }

    /// The line rate for a clock `apb2_hz`, rounded to the nearest integer.
    pub fn baud_hz(&self, apb2_hz: u64) -> u32 {
        let brr = u64::from(self.regs.read(reg::BRR) & 0xffff);
        if brr == 0 {
            return 0;
        }
        (apb2_hz + brr / 2) as u32 / brr as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> Usart {
        let mut usart = Usart::new();
        usart.write(
            reg::CTLR1,
            ctlr1::UE | ctlr1::TE | ctlr1::RE | ctlr1::RXNEIE | ctlr1::TXEIE,
        );
        usart
    }

    #[test]
    fn a_transmitted_byte_reaches_the_tx_queue() {
        let mut usart = enabled();
        usart.write(reg::DATAR, u32::from(b'x'));
        assert_eq!(usart.take_tx(), b"x");
        assert_ne!(usart.read(reg::STATR) & statr::TXE, 0);
    }

    #[test]
    fn the_receiver_sets_rxne_and_the_interrupt() {
        let mut usart = enabled();
        usart.push_input(b"hi");
        assert!(usart.irq());
        assert_ne!(usart.read(reg::STATR) & statr::RXNE, 0);
        assert_eq!(usart.read(reg::DATAR), u32::from(b'h'));
        assert_eq!(usart.read(reg::DATAR), u32::from(b'i'));
        assert!(!usart.irq());
    }

    #[test]
    fn the_transmit_interrupt_is_an_event_reading_status_clears() {
        let mut usart = enabled();
        usart.write(reg::DATAR, u32::from(b'a'));
        let _ = usart.take_tx();
        assert!(
            usart.irq(),
            "RXNEIE is set but RXNE is clear; TXE event fires"
        );
        let _ = usart.read(reg::STATR);
        assert!(!usart.irq(), "the status read acknowledged it");
    }

    #[test]
    fn the_baud_follows_apb2() {
        let mut usart = Usart::new();
        // 48 MHz / 0x1A1 is 115 108 baud: the nearest BRR to 115 200.
        usart.write(reg::BRR, 0x1a1);
        assert_eq!(usart.baud_hz(48_000_000), 115_108);
    }
}
