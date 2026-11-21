//! The single-cycle IO block: GPIO output, output enable and input for both
//! cores, the inter-core FIFOs and the 32 spinlocks.
//!
//! Every pin on the chip ends up here: the IO mux only decides *which*
//! peripheral a pin carries, and a pin whose function select is `SIO` is
//! driven straight from `GPIO_OUT`/`GPIO_OE` with no peripheral in between.
//! That is why this block sits at `0xD000_0000` rather than in the APB window:
//! the bus grants it a single-cycle access.

use super::RegisterFile;

pub mod reg {
    pub const CPUID: u32 = 0x000;
    pub const GPIO_IN: u32 = 0x004;
    pub const GPIO_HI_IN: u32 = 0x008;
    pub const GPIO_OUT: u32 = 0x010;
    pub const GPIO_OUT_SET: u32 = 0x014;
    pub const GPIO_OUT_CLR: u32 = 0x018;
    pub const GPIO_OUT_XOR: u32 = 0x01c;
    pub const GPIO_OE: u32 = 0x020;
    pub const GPIO_OE_SET: u32 = 0x024;
    pub const GPIO_OE_CLR: u32 = 0x028;
    pub const GPIO_OE_XOR: u32 = 0x02c;
    pub const GPIO_HI_OUT: u32 = 0x030;
    pub const FIFO_ST: u32 = 0x050;
    pub const FIFO_WR: u32 = 0x054;
    pub const FIFO_RD: u32 = 0x058;
    pub const SPINLOCK_ST: u32 = 0x05c;
    pub const DIV_UDIVIDEND: u32 = 0x060;
    pub const DIV_UDIVISOR: u32 = 0x064;
    pub const DIV_SDIVIDEND: u32 = 0x068;
    pub const DIV_SDIVISOR: u32 = 0x06c;
    pub const DIV_QUOTIENT: u32 = 0x070;
    pub const DIV_REMAINDER: u32 = 0x074;
    pub const DIV_CSR: u32 = 0x078;
    /// The 32 hardware spinlocks start past the inter-core FIFO block, at
    /// `SIO_BASE + 0x100`, one word each.
    pub const SPINLOCK0: u32 = 0x100;
    pub const SPINLOCK_LAST: u32 = 0x17c;
}

#[derive(Debug)]
pub struct Sio {
    /// Driven levels, one bit per GPIO.
    pub out: u32,
    /// Output enables.
    pub oe: u32,
    /// Levels the pads present to the chip.
    pub inp: u32,
    /// The other core's FIFO, and the one it reads from here.
    pub fifo_in: std::collections::VecDeque<u32>,
    pub fifo_out: std::collections::VecDeque<u32>,
    /// Bit `n` set means spinlock `n` is taken.
    pub locks: u32,
    /// The hardware divider: both operands for each signedness, the last
    /// result, and the dirty flag. The divide completes on the operand write
    /// here, so `READY` is always set.
    div_udividend: u32,
    div_udivisor: u32,
    div_sdividend: u32,
    div_sdivisor: u32,
    div_quotient: u32,
    div_remainder: u32,
    div_dirty: bool,
    div_signed: bool,
    pub regs: RegisterFile,
}

impl Default for Sio {
    fn default() -> Self {
        Self::new()
    }
}

impl Sio {
    pub fn new() -> Self {
        Sio {
            out: 0,
            oe: 0,
            inp: 0,
            fifo_in: Default::default(),
            fifo_out: Default::default(),
            locks: 0,
            div_udividend: 0,
            div_udivisor: 0,
            div_sdividend: 0,
            div_sdivisor: 0,
            div_quotient: 0,
            div_remainder: 0,
            div_dirty: false,
            div_signed: false,
            regs: RegisterFile::new(0x100 / 4),
        }
    }

    /// Complete the divide the operand write just started. The silicon takes
    /// eight cycles; here the result is ready before the write returns, which
    /// is indistinguishable to software that polls `READY` or reads after the
    /// documented delay.
    fn divide(&mut self) {
        self.div_dirty = true;
        if self.div_signed {
            let dividend = self.div_sdividend as i32;
            let divisor = self.div_sdivisor as i32;
            if divisor == 0 {
                self.div_quotient = if dividend < 0 { 1 } else { u32::MAX };
                self.div_remainder = dividend as u32;
            } else {
                self.div_quotient = dividend.wrapping_div(divisor) as u32;
                self.div_remainder = dividend.wrapping_rem(divisor) as u32;
            }
        } else if let Some(quotient) = self.div_udividend.checked_div(self.div_udivisor) {
            self.div_quotient = quotient;
            self.div_remainder = self.div_udividend % self.div_udivisor;
        } else {
            self.div_quotient = u32::MAX;
            self.div_remainder = self.div_udividend;
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::CPUID => 0,
            reg::GPIO_IN => self.inp,
            reg::GPIO_HI_IN => 0,
            reg::GPIO_OUT | reg::GPIO_OUT_SET | reg::GPIO_OUT_CLR | reg::GPIO_OUT_XOR => self.out,
            reg::GPIO_OE | reg::GPIO_OE_SET | reg::GPIO_OE_CLR | reg::GPIO_OE_XOR => self.oe,
            reg::FIFO_ST => {
                let mut word = 0;
                if !self.fifo_out.is_empty() {
                    word |= 1;
                }
                if self.fifo_in.is_empty() {
                    word |= 1 << 1;
                }
                word
            }
            reg::FIFO_RD => self.fifo_in.pop_front().unwrap_or(0),
            reg::SPINLOCK_ST => self.locks,
            reg::DIV_UDIVIDEND => self.div_udividend,
            reg::DIV_UDIVISOR => self.div_udivisor,
            reg::DIV_SDIVIDEND => self.div_sdividend,
            reg::DIV_SDIVISOR => self.div_sdivisor,
            reg::DIV_QUOTIENT => {
                self.div_dirty = false;
                self.div_quotient
            }
            reg::DIV_REMAINDER => self.div_remainder,
            reg::DIV_CSR => 1 | u32::from(self.div_dirty) << 1,
            off if (reg::SPINLOCK0..=reg::SPINLOCK_LAST).contains(&off) => {
                let index = (off - reg::SPINLOCK0) / 4;
                if self.locks & (1 << index) != 0 {
                    0
                } else {
                    self.locks |= 1 << index;
                    self.locks
                }
            }
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::GPIO_OUT => self.out = value,
            reg::GPIO_OUT_SET => self.out |= value,
            reg::GPIO_OUT_CLR => self.out &= !value,
            reg::GPIO_OUT_XOR => self.out ^= value,
            reg::GPIO_OE => self.oe = value,
            reg::GPIO_OE_SET => self.oe |= value,
            reg::GPIO_OE_CLR => self.oe &= !value,
            reg::GPIO_OE_XOR => self.oe ^= value,
            reg::FIFO_WR => {
                if self.fifo_out.len() < 8 {
                    self.fifo_out.push_back(value);
                }
            }
            reg::DIV_UDIVIDEND => {
                self.div_udividend = value;
                self.div_signed = false;
                self.divide();
            }
            reg::DIV_UDIVISOR => {
                self.div_udivisor = value;
                self.div_signed = false;
                self.divide();
            }
            reg::DIV_SDIVIDEND => {
                self.div_sdividend = value;
                self.div_signed = true;
                self.divide();
            }
            reg::DIV_SDIVISOR => {
                self.div_sdivisor = value;
                self.div_signed = true;
                self.divide();
            }
            // Writing a result register stops any calculation and latches the
            // value, which is how the SDK saves and restores the divider.
            reg::DIV_QUOTIENT => {
                self.div_quotient = value;
                self.div_dirty = true;
            }
            reg::DIV_REMAINDER => {
                self.div_remainder = value;
                self.div_dirty = true;
            }
            reg::DIV_CSR | reg::SPINLOCK_ST => {}
            off if (reg::SPINLOCK0..=reg::SPINLOCK_LAST).contains(&off) => {
                let index = (off - reg::SPINLOCK0) / 4;
                self.locks &= !(1 << index);
            }
            off => self.regs.write(off, value),
        }
    }
}
