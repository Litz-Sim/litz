//! The PL011 UART: TX/RX FIFOs, baud divisors, line control and the maskable
//! interrupt set.
//!
//! A byte written to `DR` waits in the 16-entry transmit FIFO until the shifter
//! takes it, then leaves one bit at a time at the baud rate `IBRD`/`FBRD`
//! divide out of `clk_peri`. [`Uart::tx`] collects a byte once its stop bit has
//! been on the line, which is when the host hears it, and [`Uart::line_level`]
//! is the level the transmitter drives onto its pin — start bit, eight data
//! bits low bit first, stop bit — so a scope on a UART pin sees the frame the
//! console received. A byte the host pushes into [`Uart::rx`] is visible to the
//! guest only while the receiver is enabled, which is what the real block
//! does: bytes that arrive with `RXE` clear are lost; reception itself is not
//! timed.

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

/// The 125 MHz peripheral clock the unit tests feed the divider.
#[cfg(test)]
const PERI_HZ: u32 = 125_000_000;
/// Both FIFOs hold 16 bytes in this model.
const FIFO_DEPTH: usize = 16;

#[derive(Debug)]
pub struct Uart {
    regs: RegisterFile,
    /// Bytes the shifter has finished, in the order the host sees them.
    pub tx: Vec<u8>,
    /// Bytes from the host waiting to be read from the data register.
    pub rx: VecDeque<u8>,
    /// The transmit FIFO: bytes the guest wrote, waiting for the shifter.
    wire: VecDeque<u8>,
    /// The frame on the line, shifted out low bit first: a zero start bit,
    /// eight data bits and a one stop bit.
    frame: u16,
    /// Bits of `frame` still to come, including the one on the line.
    frame_bits: u8,
    /// The byte `frame` carries, handed to the host when the stop bit ends it.
    frame_byte: u8,
    /// Cycles spent in the current bit of the frame.
    bit_cycles: u64,
    ibrd: u32,
    fbrd: u32,
    /// Latched raw interrupt status. Receive and transmit bits are refreshed
    /// when their condition changes; error bits are not modelled and stay low.
    ris: u32,
    ims: u32,
    /// A byte left the transmit FIFO since the interrupt was last
    /// acknowledged. The transmit interrupt is an event, not a level: a
    /// driver that leaves `TXIM` enabled with an empty FIFO (MicroPython
    /// does) must not be interrupted on every cycle.
    tx_ready: bool,
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
            wire: VecDeque::new(),
            frame: 0,
            frame_bits: 0,
            frame_byte: 0,
            bit_cycles: 0,
            ibrd: 0,
            fbrd: 0,
            ris: 0,
            ims: 0,
            tx_ready: false,
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
        let mut word = 0;
        if self.tx.is_empty() && self.wire.is_empty() {
            word |= 1 << 7; // TXFE: nothing queued behind the shifter.
        }
        if self.rx.is_empty() {
            word |= 1 << 4; // RXFE
        }
        if self.wire.len() >= FIFO_DEPTH {
            word |= 1 << 5; // TXFF
        }
        if self.rx.len() >= FIFO_DEPTH {
            word |= 1 << 6; // RXFF
        }
        if self.frame_bits != 0 || !self.wire.is_empty() {
            word |= 1 << 3; // BUSY
        }
        word
    }

    /// Recompute the transmit half of `RIS`.
    ///
    /// The condition is "a byte has shifted out since the last
    /// acknowledgement", not "the FIFO has room". The difference matters for
    /// a driver that leaves the transmit interrupt enabled with nothing to
    /// send: a level would re-interrupt on every instruction, while the PL011
    /// reports one event per byte and expects the driver to clear it.
    fn refresh_tx(&mut self) {
        if self.tx_enabled() && self.tx_ready {
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
                if self.tx_enabled() && self.wire.len() < FIFO_DEPTH {
                    self.wire.push_back(value as u8);
                }
                if self.wire.len() >= FIFO_DEPTH {
                    // Nothing more fits, so there is no room event to report.
                    self.tx_ready = false;
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
            reg::ICR => {
                // Acknowledging the transmit event also consumes it, so the
                // next `tick` does not raise it again with an empty FIFO.
                if value & irq::TXIM != 0 {
                    self.tx_ready = false;
                }
                self.ris &= !(value & irq::ALL);
            }
            reg::RIS | reg::MIS => {}
            off => self.regs.write(off, value),
        }
    }

    /// Advance the transmitter by `cycles` of the system clock.
    ///
    /// `sys_hz` converts those cycles into the peripheral clock the baud
    /// divisor counts, so a line rate stays put when `clk_peri` is slowed
    /// independently of `clk_sys` — which is what MicroPython does.
    pub fn tick(&mut self, cycles: u64, sys_hz: u32, peri_hz: u32) {
        self.shift(cycles, sys_hz, peri_hz);
        self.refresh_tx();
    }

    /// The level the transmitter drives onto its pin: high between frames,
    /// which is the stop bit and the idle line.
    pub fn line_level(&self) -> bool {
        self.frame_bits == 0 || self.frame & 1 != 0
    }

    /// Clock the current frame out over `cycles`, starting the next queued
    /// byte as soon as a stop bit ends.
    fn shift(&mut self, cycles: u64, sys_hz: u32, peri_hz: u32) {
        let per_bit = self.cycles_per_bit(sys_hz, peri_hz);
        let mut left = cycles;
        loop {
            if self.frame_bits == 0 {
                // Draining the FIFO needs the transmitter on; a frame already
                // in the shifter always finishes.
                let Some(byte) = self.wire.front().copied().filter(|_| self.tx_enabled()) else {
                    self.bit_cycles = 0;
                    return;
                };
                self.wire.pop_front();
                // Start bit, the byte low bit first, stop bit.
                self.frame_byte = byte;
                self.frame = 0x200 | (u16::from(byte) << 1);
                self.frame_bits = 10;
                self.bit_cycles = 0;
            }
            let until_next = per_bit - self.bit_cycles;
            if left < until_next {
                self.bit_cycles += left;
                return;
            }
            left -= until_next;
            self.bit_cycles = 0;
            self.frame >>= 1;
            self.frame_bits -= 1;
            if self.frame_bits == 0 {
                // The stop bit has been on the line for its full period, so
                // the host hears the byte now.
                self.tx.push(self.frame_byte);
            }
        }
    }

    /// System-clock cycles one bit occupies at the programmed baud rate.
    fn cycles_per_bit(&self, sys_hz: u32, peri_hz: u32) -> u64 {
        let divisor = 64 * u64::from(self.ibrd) + u64::from(self.fbrd & 0x3f);
        if divisor == 0 || peri_hz == 0 {
            // No divider to divide: the shifter runs as fast as the core.
            return 1;
        }
        // 16 × (IBRD + FBRD/64) peripheral cycles per bit.
        let peri_cycles = (divisor / 4).max(1);
        u64::from(sys_hz.max(1)) * peri_cycles / u64::from(peri_hz)
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

    /// The transmitter finished a byte and the host has taken it. The machine
    /// calls this once per drain, which is what turns the transmit interrupt
    /// into one event per byte rather than a level.
    pub fn tx_shifted(&mut self) {
        self.tx_ready = true;
        self.refresh_tx();
    }

    /// The line rate implied by `IBRD`/`FBRD` at `peri_hz`, rounded to the
    /// nearest integer.
    ///
    /// The divisor is a ratio of the peripheral clock, not of 125 MHz:
    /// MicroPython parks `clk_peri` at 48 MHz so the line rate does not move
    /// with the CPU clock, and a model that assumes the system clock would
    /// report a waveform 2.6 times too fast.
    pub fn baud_hz(&self, peri_hz: u64) -> u32 {
        // baud = peri / (16 * (IBRD + FBRD/64)); multiply through to keep the
        // division in integers.
        let divisor = 64 * u64::from(self.ibrd) + u64::from(self.fbrd & 0x3f);
        if divisor == 0 {
            return 0;
        }
        let numerator = peri_hz * 4;
        ((numerator + divisor / 2) / divisor) as u32
    }

    /// Whether the UART is enabled at all, for the chip adapter.
    pub fn enabled_state(&self) -> bool {
        self.enabled()
    }

    /// Whether the receiver is on, so a delivered byte is kept rather than
    /// dropped.
    pub fn receiver_enabled(&self) -> bool {
        self.rx_enabled()
    }

    /// How many bytes the receive FIFO takes before it is full.
    pub fn receive_room(&self) -> usize {
        FIFO_DEPTH.saturating_sub(self.rx.len())
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
        assert!(uart.wire.is_empty());

        uart.write(reg::CR, cr::UARTEN);
        uart.write(reg::DR, u32::from(b'y'));
        assert!(uart.wire.is_empty(), "TXE is clear");

        uart.write(reg::CR, cr::UARTEN | cr::TXE);
        uart.write(reg::DR, u32::from(b'z'));
        assert_eq!(
            uart.wire.iter().copied().collect::<Vec<_>>(),
            b"z",
            "the byte waits for the shifter"
        );
        assert_eq!(
            uart.read(reg::FR) & (1 << 7),
            0,
            "TXFE is clear while queued"
        );
        uart.tick(10 * 16, PERI_HZ, PERI_HZ);
        uart.tx.clear();
        assert_eq!(uart.read(reg::FR) & (1 << 7), 1 << 7, "TXFE when empty");
    }

    #[test]
    fn the_pin_carries_the_frame_the_host_hears_after_it() {
        let mut uart = enabled_tx_rx();
        // IBRD 1 puts one bit every 16 peripheral cycles.
        uart.write(reg::IBRD, 1);
        uart.write(reg::FBRD, 0);
        let bit = 16;
        assert!(uart.line_level(), "the idle line rests at the stop level");

        uart.write(reg::DR, u32::from(b'A')); // 0x41, low bit first
        assert_eq!(uart.read(reg::FR) & (1 << 3), 1 << 3, "BUSY");
        uart.tick(1, PERI_HZ, PERI_HZ);
        assert!(!uart.line_level(), "the start bit begins as it loads");

        for (index, expected) in [true, false, false, false, false, false, true, false]
            .into_iter()
            .enumerate()
        {
            uart.tick(bit, PERI_HZ, PERI_HZ);
            assert_eq!(uart.line_level(), expected, "data bit {index}");
        }
        uart.tick(bit, PERI_HZ, PERI_HZ);
        assert!(uart.line_level(), "the stop bit follows the last data bit");
        assert!(uart.tx.is_empty(), "the byte is not the host's yet");
        uart.tick(bit, PERI_HZ, PERI_HZ);
        assert_eq!(uart.tx, b"A", "the host hears the byte after the stop bit");
        assert!(uart.line_level());
        assert_eq!(uart.read(reg::FR) & (1 << 3), 0, "BUSY drops when idle");
    }

    #[test]
    fn a_slow_line_takes_longer_than_a_fast_one() {
        let mut uart = enabled_tx_rx();
        uart.write(reg::IBRD, 1);
        uart.write(reg::DR, u32::from(b'x'));
        uart.tick(10 * 16 - 1, PERI_HZ, PERI_HZ);
        assert!(
            uart.tx.is_empty(),
            "nine bits and a bit less is not a frame"
        );
        uart.tick(1, PERI_HZ, PERI_HZ);
        assert_eq!(uart.tx, b"x");

        // The same byte at a quarter of the line rate needs four times the
        // cycles; clk_peri slower than clk_sys stretches each bit further.
        let mut slow = enabled_tx_rx();
        slow.write(reg::IBRD, 4);
        slow.write(reg::DR, u32::from(b'x'));
        slow.tick(10 * 64 - 1, PERI_HZ, PERI_HZ);
        assert!(slow.tx.is_empty());
        slow.tick(1, PERI_HZ, PERI_HZ);
        assert_eq!(slow.tx, b"x");

        let mut divided = enabled_tx_rx();
        divided.write(reg::IBRD, 1);
        divided.write(reg::DR, u32::from(b'x'));
        // clk_peri at half of clk_sys: 32 system cycles per bit.
        divided.tick(10 * 32 - 1, PERI_HZ, PERI_HZ / 2);
        assert!(divided.tx.is_empty());
        divided.tick(1, PERI_HZ, PERI_HZ / 2);
        assert_eq!(divided.tx, b"x");
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
        let baud = uart.baud_hz(u64::from(PERI_HZ));
        assert!((baud as i64 - 115_200).abs() < 20, "got {baud}");
        // 125 MHz / (16 * 1) = 7.8125 Mbaud.
        uart.write(reg::IBRD, 1);
        uart.write(reg::FBRD, 0);
        assert_eq!(uart.baud_hz(u64::from(PERI_HZ)), 7_812_500);
    }

    #[test]
    fn the_divisor_follows_the_peripheral_clock() {
        // MicroPython asks for 115 200 with clk_peri at 48 MHz, which is IBRD
        // 26 and FBRD 3. Reading it against the system clock instead would
        // report 299 940 baud and draw every edge 2.6 times too close.
        let mut uart = Uart::new();
        uart.write(reg::IBRD, 26);
        uart.write(reg::FBRD, 3);
        let baud = uart.baud_hz(48_000_000);
        assert!((baud as i64 - 115_200).abs() < 60, "got {baud}");
    }
}
