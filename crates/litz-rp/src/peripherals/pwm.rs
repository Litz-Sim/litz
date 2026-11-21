//! The 8-slice PWM block with 16 channels and the wrap interrupt.
//!
//! Each slice has a counter that runs at the system clock divided by
//! `CSR.DIV` (an 8-bit integer) and `CHx_DIV` (12.4 fixed point). The counter
//! wraps at `TOP`, raising that slice's bit in `INTR`. Two output channels per
//! slice compare the counter against `CC.A`/`CC.B`; [`Pwm::level`] turns that
//! comparison into the level the machine publishes on a pin.
//!
//! Phase-correct mode is treated as the same up-counter: a firmware that
//! enables it still gets a level at the programmed duty, it just does not see
//! the symmetric count direction. The B channel's phase advance is applied by
//! offsetting its comparison, which is the observable part.

use super::RegisterFile;

/// Register offsets from the PWM base.
pub mod reg {
    /// Slice register stride.
    pub const SLICE_STRIDE: u32 = 0x14;
    pub const EN: u32 = 0xa0;
    pub const INTR: u32 = 0xa4;
    pub const INTE: u32 = 0xa8;
    pub const INTF: u32 = 0xac;
    pub const INTS: u32 = 0xb0;

    pub const CSR: u32 = 0x00;
    pub const DIV: u32 = 0x04;
    pub const CTR: u32 = 0x08;
    pub const CC: u32 = 0x0c;
    pub const TOP: u32 = 0x10;

    pub fn slice(offset: u32, index: usize) -> u32 {
        offset + SLICE_STRIDE * index as u32
    }
}

/// `CHx_CSR` bits.
pub mod csr {
    pub const EN: u32 = 1 << 0;
    pub const PH_CORRECT: u32 = 1 << 1;
    pub const A_INV: u32 = 1 << 2;
    pub const B_INV: u32 = 1 << 3;
    pub const DIV_SHIFT: u32 = 4;
    pub const DIV_MASK: u32 = 0xff << DIV_SHIFT;
    pub const B_PHASE_ADVANCE: u32 = 1 << 12;
}

pub const SLICES: usize = 8;
pub const CHANNELS: usize = 16;

#[derive(Clone, Copy, Debug)]
struct Slice {
    csr: u32,
    div: u32,
    ctr: u32,
    cc: u32,
    top: u32,
    /// Fractional PWM clocks not yet spent, in sixteenths.
    phase: u64,
}

impl Default for Slice {
    fn default() -> Self {
        Slice {
            csr: 0,
            // 12.4 fixed point, 1.0.
            div: 1 << 4,
            ctr: 0,
            cc: 0,
            top: 0xffff,
            phase: 0,
        }
    }
}

#[derive(Debug)]
pub struct Pwm {
    slices: [Slice; SLICES],
    intr: u32,
    inte: u32,
    forced: u32,
    regs: RegisterFile,
}

impl Default for Pwm {
    fn default() -> Self {
        Self::new()
    }
}

impl Pwm {
    pub fn new() -> Self {
        Pwm {
            slices: [Slice::default(); SLICES],
            intr: 0,
            inte: 0,
            forced: 0,
            regs: RegisterFile::new(0x100 / 4),
        }
    }

    /// `EN` mirrors the per-slice enable bits.
    fn enabled_mask(&self) -> u32 {
        let mut mask = 0;
        for (index, slice) in self.slices.iter().enumerate() {
            if slice.csr & csr::EN != 0 {
                mask |= 1 << index;
            }
        }
        mask
    }

    fn split(off: u32) -> Option<(usize, u32)> {
        if off >= reg::EN {
            return None;
        }
        let index = (off / reg::SLICE_STRIDE) as usize;
        let field = off % reg::SLICE_STRIDE;
        (index < SLICES && field <= reg::TOP).then_some((index, field))
    }

    pub fn read(&mut self, off: u32) -> u32 {
        if let Some((index, field)) = Self::split(off) {
            let slice = &self.slices[index];
            return match field {
                reg::CSR => slice.csr,
                reg::DIV => slice.div,
                reg::CTR => slice.ctr,
                reg::CC => slice.cc,
                reg::TOP => slice.top,
                _ => 0,
            };
        }
        match off {
            reg::EN => self.enabled_mask(),
            reg::INTR => self.intr,
            reg::INTE => self.inte,
            reg::INTF => self.forced,
            reg::INTS => (self.intr | self.forced) & self.inte,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        if let Some((index, field)) = Self::split(off) {
            let slice = &mut self.slices[index];
            match field {
                reg::CSR => slice.csr = value,
                reg::DIV => slice.div = value & 0xffff,
                reg::CTR => slice.ctr = value & 0xffff,
                reg::CC => slice.cc = value,
                reg::TOP => slice.top = value & 0xffff,
                _ => {}
            }
            return;
        }
        match off {
            reg::EN => {
                // Inferred: writing EN is an alias for the per-slice CSR
                // enable bits; reading it reports the aggregate.
                for index in 0..SLICES {
                    if value & (1 << index) != 0 {
                        self.slices[index].csr |= csr::EN;
                    } else {
                        self.slices[index].csr &= !csr::EN;
                    }
                }
            }
            reg::INTR => self.intr &= !value,
            reg::INTE => self.inte = value & 0xff,
            reg::INTF => self.forced = value & 0xff,
            reg::INTS => {}
            off => self.regs.write(off, value),
        }
    }

    /// The effective divider in sixteenths: the CSR integer divider times the
    /// 12.4 `DIV` register.
    fn divisor16(csr: u32, div: u32) -> u64 {
        let csr_div = u64::from((csr & csr::DIV_MASK) >> csr::DIV_SHIFT);
        let csr_div = if csr_div == 0 { 1 } else { csr_div };
        let integer = u64::from(div >> 4);
        let fraction = u64::from(div & 0xf);
        let fractional = integer * 16 + fraction;
        let fractional = if fractional == 0 { 16 } else { fractional };
        (csr_div * fractional).max(1)
    }

    pub fn tick(&mut self, cycles: u64) {
        for index in 0..SLICES {
            let slice = &mut self.slices[index];
            if slice.csr & csr::EN == 0 {
                continue;
            }
            let divisor = Self::divisor16(slice.csr, slice.div);
            slice.phase += cycles * 16;
            let increments = slice.phase / divisor;
            slice.phase %= divisor;
            if increments == 0 {
                continue;
            }
            let period = u64::from(slice.top) + 1;
            let moved = u64::from(slice.ctr) + increments;
            if moved / period > 0 {
                self.intr |= 1 << index;
            }
            slice.ctr = (moved % period) as u32;
        }
    }

    pub fn irq(&self) -> bool {
        (self.intr & self.inte) != 0
    }

    /// The level channel `channel` drives: channel 0 is slice 0's A output,
    /// channel 1 slice 0's B output, and so on. `None` when the slice is
    /// stopped, which lets the machine stop driving the pin.
    pub fn level(&self, channel: usize) -> Option<bool> {
        if channel >= CHANNELS {
            return None;
        }
        let index = channel / 2;
        let slice = &self.slices[index];
        if slice.csr & csr::EN == 0 {
            return None;
        }
        let is_b = channel % 2 == 1;
        let mut position = slice.ctr;
        let mut cc = slice.cc & 0xffff;
        let mut invert = slice.csr & csr::A_INV != 0;
        if is_b {
            cc = slice.cc >> 16;
            invert = slice.csr & csr::B_INV != 0;
            if slice.csr & csr::B_PHASE_ADVANCE != 0 {
                // Inferred: the advance is the B channel seeing a counter that
                // is one step earlier.
                position = position.saturating_sub(1);
            }
        }
        let high = position < cc;
        Some(high != invert)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(slice: usize) -> u32 {
        reg::slice(reg::CSR, slice)
    }

    #[test]
    fn counter_wraps_at_top_and_raises_the_interrupt() {
        let mut pwm = Pwm::new();
        let base = run(0);
        pwm.write(base + reg::CSR, csr::EN);
        pwm.write(base + reg::TOP, 3);
        pwm.write(reg::EN, 1);
        pwm.write(reg::INTE, 1);
        pwm.tick(3);
        assert_eq!(pwm.read(base + reg::CTR), 3);
        assert!(!pwm.irq(), "no wrap yet: the period is TOP + 1");
        pwm.tick(1);
        assert_eq!(pwm.read(base + reg::CTR), 0);
        assert!(pwm.irq());
        assert_ne!(pwm.read(reg::INTS) & 1, 0);
        pwm.write(reg::INTR, 1);
        assert!(!pwm.irq());
    }

    #[test]
    fn compare_levels_drive_a_channel() {
        let mut pwm = Pwm::new();
        let base = run(0);
        pwm.write(base + reg::CSR, csr::EN);
        pwm.write(base + reg::TOP, 7);
        pwm.write(base + reg::CC, (5 << 16) | 2);
        pwm.write(reg::EN, 1);

        assert_eq!(pwm.level(0), Some(true), "counter 0 is below CC.A 2");
        assert_eq!(pwm.level(1), Some(true), "counter 0 is below CC.B 5");
        pwm.tick(2);
        assert_eq!(pwm.level(0), Some(false), "counter 2 is not below 2");
        assert_eq!(pwm.level(1), Some(true));
        pwm.tick(3);
        assert_eq!(pwm.level(1), Some(false));
        assert_eq!(pwm.level(2), None, "slice 1 is stopped");
        assert_eq!(pwm.level(16), None);
    }

    #[test]
    fn divider_slows_the_counter() {
        let mut pwm = Pwm::new();
        let base = run(1);
        pwm.write(base + reg::CSR, csr::EN | (2 << csr::DIV_SHIFT));
        pwm.write(reg::EN, 1 << 1);
        pwm.tick(8);
        assert_eq!(pwm.read(base + reg::CTR), 4);
    }
}
