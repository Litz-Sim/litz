//! USART0: the byte-oriented serial port.
//!
//! Transmission is a console buffer: a byte written to `UDR0` is appended to
//! it immediately, so `UDRE0` stays set and `TXC0` rises as soon as the byte
//! is accepted. Reception is a queue: the host pushes bytes with
//! [`crate::machine::Machine::write_console`], which sets `RXC0` and makes the
//! `USART_RX` interrupt pending.
//!
//! Timing is byte-granular. The simulator does not model the shift register,
//! the baud generator or framing; a byte appears in the transmit buffer the
//! instant it is written and a received byte is available the instant the host
//! queues it. `UBRR0`/`UCSR0C` are stored and readable but do not affect
//! timing. This is enough for `Serial.print`/`Serial.read` and for an RX
//! interrupt routine, and it keeps a console-driven run deterministic.

use std::collections::VecDeque;

/// `UCSR0A` bit positions.
pub const RXC0: u8 = 0x80;
pub const TXC0: u8 = 0x40;
pub const UDRE0: u8 = 0x20;

/// USART0 state: the control registers, the receive queue and the console
/// buffer.
#[derive(Default)]
pub struct Usart0 {
    /// Shadow of the writable bits of `UCSR0A` (framing-error bits and the
    /// like), plus the transmit-complete flag.
    shadow_a: u8,
    pub ucsr_b: u8,
    pub ucsr_c: u8,
    pub ubrr_l: u8,
    pub ubrr_h: u8,
    receive: VecDeque<u8>,
    transmit: Vec<u8>,
}

const RECEIVE_CAPACITY: usize = 4096;

impl Usart0 {
    /// `UCSR0A`: `RXC0` follows the receive queue, `UDRE0` is always set
    /// (the transmit buffer is drained immediately) and `TXC0` is the stored
    /// transmit-complete flag.
    pub(crate) fn status(&self) -> u8 {
        let rx = if self.receive.is_empty() { 0 } else { RXC0 };
        (self.shadow_a & 0x5f) | rx | UDRE0
    }

    /// Write `UCSR0A`. Bits 4:0 are storage; writing a one to `TXC0` clears
    /// it, as the datasheet specifies.
    pub(crate) fn write_status(&mut self, value: u8) {
        // TXC0 is write-one-to-clear; the low five bits are storage.
        let txc = self.shadow_a & TXC0 & !value;
        self.shadow_a = txc | (value & 0x1f);
    }

    pub(crate) fn receive_byte(&mut self, value: u8) {
        if self.receive.len() < RECEIVE_CAPACITY {
            self.receive.push_back(value);
        }
    }

    pub(crate) fn read_data(&mut self) -> u8 {
        self.receive.pop_front().unwrap_or(0)
    }

    pub(crate) fn write_data(&mut self, value: u8) {
        self.transmit.push(value);
        self.shadow_a |= TXC0;
    }

    /// Bytes the firmware has written, drained.
    pub fn take(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.transmit)
    }

    /// Bytes waiting for the firmware, without consuming them.
    pub fn pending(&self) -> usize {
        self.receive.len()
    }

    /// `RXC0`: a received byte is waiting.
    pub fn rxc(&self) -> bool {
        !self.receive.is_empty()
    }

    /// `TXC0`: the last written byte completed.
    pub fn txc(&self) -> bool {
        self.shadow_a & TXC0 != 0
    }

    /// `UDRE0`: the transmit buffer is empty.
    pub fn udre(&self) -> bool {
        true
    }

    /// Receive-complete interrupt enabled (`RXCIE0`).
    pub fn rx_interrupt_enabled(&self) -> bool {
        self.ucsr_b & 0x80 != 0
    }

    /// Data-register-empty interrupt enabled (`UDRIE0`).
    pub fn udre_interrupt_enabled(&self) -> bool {
        self.ucsr_b & 0x20 != 0
    }

    /// Transmit-complete interrupt enabled (`TXCIE0`).
    pub fn tx_interrupt_enabled(&self) -> bool {
        self.ucsr_b & 0x40 != 0
    }

    pub(crate) fn acknowledge_tx(&mut self) {
        self.shadow_a &= !TXC0;
    }
}
