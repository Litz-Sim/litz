//! The watchdog: a 24-bit counter, the tick generator it runs from and the
//! scratch registers that survive a reset.
//!
//! The watchdog's own tick generator is driven by the 12 MHz reference, so one
//! watchdog tick is 125 CPU cycles at the modelled 125 MHz system clock. The
//! counter wraps at `LOAD` and sets `REASON.TIME`, which is the observation a
//! firmware (or the bootrom's watchdog test) makes. The scratch registers are
//! plain storage; they are the only registers whose values the chip keeps
//! across a watchdog reset, which is why a firmware writes its reset cause
//! there.

use super::RegisterFile;

/// Register offsets from the WATCHDOG base.
pub mod reg {
    pub const CTRL: u32 = 0x00;
    pub const LOAD: u32 = 0x04;
    pub const REASON: u32 = 0x08;
    pub const SCRATCH0: u32 = 0x0c;
    pub const SCRATCH7: u32 = 0x28;
    /// The tick generator's cycle register. Stored but not used for the
    /// period: this model fixes the watchdog tick at 1 µs.
    pub const TICK: u32 = 0x2c;
}

/// `CTRL` bits.
pub mod ctrl {
    pub const ENABLE: u32 = 1 << 30;
}

/// `REASON` bits.
pub mod reason {
    pub const FORCE: u32 = 1 << 0;
    pub const TIME: u32 = 1 << 1;
    pub const RUNNING: u32 = 1 << 2;
}

/// CPU cycles in one microsecond at 125 MHz, which is one 12 MHz tick.
const CYCLES_PER_US: u64 = 125;

#[derive(Debug)]
pub struct Watchdog {
    regs: RegisterFile,
    ctrl: u32,
    load: u32,
    reason: u32,
    scratch: [u32; 8],
    tick_cycles: u32,
    /// Microseconds since the counter last reloaded.
    count: u32,
    carry: u64,
}

impl Default for Watchdog {
    fn default() -> Self {
        Self::new()
    }
}

impl Watchdog {
    pub fn new() -> Self {
        Watchdog {
            regs: RegisterFile::new(0x100 / 4),
            ctrl: 0,
            load: 0,
            reason: 0,
            scratch: [0; 8],
            tick_cycles: 0,
            count: 0,
            carry: 0,
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::CTRL => self.ctrl,
            reg::LOAD => self.load,
            reg::REASON => {
                self.reason
                    | if self.ctrl & ctrl::ENABLE != 0 {
                        reason::RUNNING
                    } else {
                        0
                    }
            }
            reg::SCRATCH0..=reg::SCRATCH7 => self.scratch[((off - reg::SCRATCH0) / 4) as usize],
            reg::TICK => self.tick_cycles,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::CTRL => {
                let was_enabled = self.ctrl & ctrl::ENABLE != 0;
                self.ctrl = value;
                let now_enabled = self.ctrl & ctrl::ENABLE != 0;
                if now_enabled && !was_enabled {
                    self.count = 0;
                    self.carry = 0;
                }
            }
            reg::LOAD => self.load = value & 0x00ff_ffff,
            // Inferred: FORCE forces an expiry, TIME clears the sticky bit.
            // The datasheet marks TIME as write-one-to-clear.
            reg::REASON => {
                if value & reason::FORCE != 0 {
                    self.reason |= reason::TIME;
                }
                if value & reason::TIME != 0 {
                    self.reason &= !reason::TIME;
                }
            }
            reg::SCRATCH0..=reg::SCRATCH7 => {
                let index = ((off - reg::SCRATCH0) / 4) as usize;
                self.scratch[index] = value;
            }
            reg::TICK => self.tick_cycles = value & 0x1ff,
            off => self.regs.write(off, value),
        }
    }

    pub fn tick(&mut self, cycles: u64) {
        if self.ctrl & ctrl::ENABLE == 0 {
            return;
        }
        let total = self.carry + cycles;
        let micros = total / CYCLES_PER_US;
        self.carry = total % CYCLES_PER_US;
        if micros == 0 {
            return;
        }
        // LOAD is the terminal count, so the period is LOAD + 1 ticks.
        let period = u64::from(self.load) + 1;
        let elapsed = u64::from(self.count) + micros;
        if elapsed >= period {
            self.reason |= reason::TIME;
            self.count = (elapsed % period) as u32;
        } else {
            self.count = elapsed as u32;
        }
    }

    /// The RTC interrupt line the machine latches. The watchdog alarm itself
    /// resets the chip rather than raising an interrupt, so this stays low.
    pub fn irq(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_sets_the_time_reason() {
        let mut watchdog = Watchdog::new();
        watchdog.write(reg::LOAD, 1);
        watchdog.write(reg::CTRL, ctrl::ENABLE);
        assert_eq!(watchdog.read(reg::REASON) & reason::TIME, 0);
        watchdog.tick(125);
        assert_eq!(watchdog.read(reg::REASON) & reason::TIME, 0);
        watchdog.tick(125);
        assert_ne!(watchdog.read(reg::REASON) & reason::TIME, 0);
        assert_ne!(watchdog.read(reg::REASON) & reason::RUNNING, 0);
    }

    #[test]
    fn scratch_survives_and_is_latched() {
        let mut watchdog = Watchdog::new();
        for index in 0..8 {
            watchdog.write(reg::SCRATCH0 + 4 * index, index + 1);
        }
        for index in 0..8 {
            assert_eq!(watchdog.read(reg::SCRATCH0 + 4 * index), index + 1);
        }
    }

    #[test]
    fn disabled_counter_does_not_expire() {
        let mut watchdog = Watchdog::new();
        watchdog.write(reg::LOAD, 0);
        watchdog.tick(125 * 10);
        assert_eq!(watchdog.read(reg::REASON) & reason::TIME, 0);
    }
}
