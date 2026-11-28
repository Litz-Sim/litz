//! IWDG and WWDG.
//!
//! IWDG runs from the 40 kHz LSI, so the modeled count is accumulated from CPU
//! cycles; writing `0xCCCC` to `KEYR` starts it, `0xAAAA` feeds it and
//! `0x5555` unlocks the prescaler and reload registers. A counter that reaches
//! zero asks the machine for a reset.
//!
//! WWDG counts down from its 7-bit window value at a divided APB clock. When
//! the counter falls below `CFR.W` it raises its early-wakeup interrupt; when
//! it falls out of the 7-bit range it asks for a reset.
//!
//! Both watchdogs only request a reset: the machine turns that into
//! [`crate::machine::Stop::Reset`] and the adapter reports it, the same way a
//! core-requested reboot is reported on every backend.

use super::RegisterFile;

/// The low-speed internal oscillator the independent watchdog counts.
pub const LSI_HZ: u64 = 40_000;

// -- IWDG ------------------------------------------------------------------

pub mod iwdg_reg {
    pub const KEYR: u32 = 0x00;
    pub const PR: u32 = 0x04;
    pub const RLR: u32 = 0x08;
    pub const SR: u32 = 0x0c;
    pub const WINR: u32 = 0x10;
}

#[derive(Debug)]
pub struct Iwdg {
    regs: RegisterFile,
    reload: u32,
    prescaler: u32,
    counter: u32,
    unlocked: bool,
    running: bool,
    /// Fractional LSI ticks not yet applied, in CPU cycles scaled by LSI.
    accumulator: u64,
}

impl Default for Iwdg {
    fn default() -> Self {
        Self::new()
    }
}

impl Iwdg {
    pub fn new() -> Self {
        Iwdg {
            regs: RegisterFile::new(0x100 / 4),
            reload: 0xfff,
            prescaler: 4,
            counter: 0xfff,
            unlocked: false,
            running: false,
            accumulator: 0,
        }
    }

    fn divider(&self) -> u64 {
        4u64 << (self.prescaler & 7)
    }

    pub fn tick(&mut self, cycles: u64, cpu_hz: u64) -> bool {
        if !self.running || cycles == 0 {
            return false;
        }
        let scaled = cycles.saturating_mul(LSI_HZ);
        let total = self.accumulator.saturating_add(scaled);
        let ticks = total / cpu_hz.max(1);
        self.accumulator = total % cpu_hz.max(1);
        let divider = self.divider();
        let steps = ticks / divider;
        if steps == 0 {
            return false;
        }
        if u64::from(self.counter) <= steps {
            self.counter = 0;
            return true;
        }
        self.counter -= steps as u32;
        false
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            iwdg_reg::PR => self.prescaler,
            iwdg_reg::RLR => self.reload,
            iwdg_reg::SR => 0,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            iwdg_reg::KEYR => match value {
                0xcccc => {
                    self.running = true;
                    self.counter = self.reload;
                }
                0xaaaa => self.counter = self.reload,
                0x5555 => self.unlocked = true,
                _ => {}
            },
            iwdg_reg::PR if self.unlocked => self.prescaler = value & 7,
            iwdg_reg::RLR if self.unlocked => self.reload = value & 0xfff,
            iwdg_reg::WINR => self.regs.write(off, value),
            _ => {}
        }
    }

    pub fn irq(&self) -> bool {
        false
    }

    /// CPU cycles until the counter reaches zero, for the idle-time skip.
    pub fn cycles_to_next(&self, cpu_hz: u64) -> Option<u64> {
        if !self.running {
            return None;
        }
        Some(u64::from(self.counter).max(1) * self.divider() * cpu_hz / LSI_HZ + 1)
    }
}

// -- WWDG ------------------------------------------------------------------

pub mod wwdg_reg {
    pub const CTLR: u32 = 0x00;
    pub const CFR: u32 = 0x04;
    pub const SR: u32 = 0x08;
}

#[derive(Debug)]
pub struct Wwdg {
    regs: RegisterFile,
    counter: u32,
    early: bool,
    running: bool,
    accumulator: u64,
}

impl Default for Wwdg {
    fn default() -> Self {
        Self::new()
    }
}

impl Wwdg {
    pub fn new() -> Self {
        Wwdg {
            regs: RegisterFile::new(0x100 / 4),
            counter: 0x7f,
            early: false,
            running: false,
            accumulator: 0,
        }
    }

    fn window(&self) -> u32 {
        self.regs.read(wwdg_reg::CFR) & 0x7f
    }

    /// Advance by `cycles`; returns `true` when the watchdog asks for a reset.
    pub fn tick(&mut self, cycles: u64) -> bool {
        if !self.running || cycles == 0 {
            return false;
        }
        // The window counter runs from PCLK1 divided by 4096 and the prescaler.
        let prescaler = 1u64 << ((self.regs.read(wwdg_reg::CFR) >> 7) & 0x3);
        let period = 4096 * prescaler;
        self.accumulator = self.accumulator.saturating_add(cycles);
        let steps = self.accumulator / period;
        self.accumulator %= period;
        if steps == 0 {
            return false;
        }
        if u64::from(self.counter) <= steps {
            self.counter = 0;
            return true;
        }
        self.counter -= steps as u32;
        if self.counter <= self.window() && self.regs.read(wwdg_reg::CFR) & (1 << 9) != 0 {
            self.early = true;
        }
        false
    }

    pub fn irq(&self) -> bool {
        self.early && self.regs.read(wwdg_reg::CFR) & (1 << 9) != 0
    }

    /// CPU cycles until the counter reaches zero, for the idle-time skip.
    pub fn cycles_to_next(&self) -> Option<u64> {
        if !self.running {
            return None;
        }
        let prescaler = 1u64 << ((self.regs.read(wwdg_reg::CFR) >> 7) & 0x3);
        Some(u64::from(self.counter).max(1) * 4096 * prescaler)
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            wwdg_reg::CTLR => (self.counter & 0x7f) | self.regs.read(wwdg_reg::CTLR) & 0x80,
            wwdg_reg::CFR => self.regs.read(wwdg_reg::CFR),
            wwdg_reg::SR => u32::from(self.early),
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            wwdg_reg::CTLR => {
                self.regs.write(wwdg_reg::CTLR, value & 0x80);
                self.counter = value & 0x7f;
                self.running = value & 0x80 != 0;
                self.early = false;
            }
            wwdg_reg::CFR => self.regs.write(wwdg_reg::CFR, value & 0x3ff),
            wwdg_reg::SR => {
                if value & 1 != 0 {
                    self.early = false;
                }
            }
            off => self.regs.write(off, value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_iwdg_resets_when_it_is_not_fed() {
        let mut iwdg = Iwdg::new();
        iwdg.write(iwdg_reg::KEYR, 0x5555);
        iwdg.write(iwdg_reg::RLR, 1);
        iwdg.write(iwdg_reg::PR, 0);
        iwdg.write(iwdg_reg::KEYR, 0xcccc);
        assert!(!iwdg.tick(10, 48_000_000));
        // 16 LSI ticks at 40 kHz is 400 us, about 19 200 CPU cycles.
        assert!(iwdg.tick(48_000_000, 48_000_000));
    }

    #[test]
    fn feeding_the_iwdg_keeps_it_alive() {
        let mut iwdg = Iwdg::new();
        iwdg.write(iwdg_reg::KEYR, 0x5555);
        iwdg.write(iwdg_reg::RLR, 100);
        iwdg.write(iwdg_reg::PR, 0);
        iwdg.write(iwdg_reg::KEYR, 0xcccc);
        for _ in 0..5 {
            assert!(!iwdg.tick(48_000, 48_000_000));
            iwdg.write(iwdg_reg::KEYR, 0xaaaa);
        }
    }

    #[test]
    fn the_wwdg_counts_down_from_its_window() {
        let mut wwdg = Wwdg::new();
        wwdg.write(wwdg_reg::CTLR, 0x80 | 0x40);
        assert!(wwdg.tick(4096 * 10_000));
    }
}
