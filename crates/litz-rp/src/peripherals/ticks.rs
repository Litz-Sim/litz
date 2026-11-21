//! The tick generators: two 1 µs counters and two free-running CPU-cycle
//! counters.
//!
//! The 1 µs counters divide the 125 MHz CPU clock by 125 and carry the
//! remainder, exactly like the main timer. The two processor counters add the
//! cycles each core ran; this model advances both from the one core the
//! machine executes, because nothing in the model has a second cycle source.
//!
//! # Layout
//!
//! The offsets are the compressed pair layout the machine addresses
//! (`TICK0` 0x00/0x04, `PROC0` 0x08/0x0c, `PROC1` 0x10/0x14, `TICK1`
//! 0x18/0x1c). The RP2040 datasheet interleaves a `CYCLES` register between
//! each `CTRL` and `COUNT` (so `TICK0_COUNT` is at 0x08 there); this block is
//! intentionally the smaller shape the task specifies, and no `CYCLES`
//! register is modelled because the tick period is fixed at 1 µs.

use super::RegisterFile;

/// Register offsets from the TICKS base.
pub mod reg {
    pub const TICK0_CTRL: u32 = 0x00;
    pub const TICK0_COUNT: u32 = 0x04;
    pub const PROC0_CTRL: u32 = 0x08;
    pub const PROC0_COUNT: u32 = 0x0c;
    pub const PROC1_CTRL: u32 = 0x10;
    pub const PROC1_COUNT: u32 = 0x14;
    pub const TICK1_CTRL: u32 = 0x18;
    pub const TICK1_COUNT: u32 = 0x1c;
}

/// `CTRL` bit positions shared by all four counters.
pub mod ctrl {
    pub const ENABLE: u32 = 1 << 0;
    pub const RUNNING: u32 = 1 << 1;
}

/// CPU cycles in one microsecond at 125 MHz.
const CYCLES_PER_US: u64 = 125;

#[derive(Debug)]
pub struct Ticks {
    regs: RegisterFile,
    tick_ctrl: [u32; 2],
    tick_count: [u32; 2],
    tick_carry: [u64; 2],
    proc_ctrl: [u32; 2],
    proc_count: [u32; 2],
}

impl Default for Ticks {
    fn default() -> Self {
        Self::new()
    }
}

impl Ticks {
    pub fn new() -> Self {
        Ticks {
            regs: RegisterFile::new(0x100 / 4),
            tick_ctrl: [0; 2],
            tick_count: [0; 2],
            tick_carry: [0; 2],
            proc_ctrl: [0; 2],
            proc_count: [0; 2],
        }
    }

    fn tick_index(off: u32) -> Option<usize> {
        match off {
            reg::TICK0_CTRL | reg::TICK0_COUNT => Some(0),
            reg::TICK1_CTRL | reg::TICK1_COUNT => Some(1),
            _ => None,
        }
    }

    fn proc_index(off: u32) -> Option<usize> {
        match off {
            reg::PROC0_CTRL | reg::PROC0_COUNT => Some(0),
            reg::PROC1_CTRL | reg::PROC1_COUNT => Some(1),
            _ => None,
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        if let Some(index) = Self::tick_index(off) {
            return if matches!(off, reg::TICK0_CTRL | reg::TICK1_CTRL) {
                self.tick_ctrl[index]
                    | if self.tick_ctrl[index] & ctrl::ENABLE != 0 {
                        ctrl::RUNNING
                    } else {
                        0
                    }
            } else {
                self.tick_count[index]
            };
        }
        if let Some(index) = Self::proc_index(off) {
            return if matches!(off, reg::PROC0_CTRL | reg::PROC1_CTRL) {
                self.proc_ctrl[index]
                    | if self.proc_ctrl[index] & ctrl::ENABLE != 0 {
                        ctrl::RUNNING
                    } else {
                        0
                    }
            } else {
                self.proc_count[index]
            };
        }
        self.regs.read(off)
    }

    pub fn write(&mut self, off: u32, value: u32) {
        if let Some(index) = Self::tick_index(off) {
            if matches!(off, reg::TICK0_CTRL | reg::TICK1_CTRL) {
                // RUNNING is read-only.
                self.tick_ctrl[index] = value & ctrl::ENABLE;
            }
            // COUNT is read-only.
            return;
        }
        if let Some(index) = Self::proc_index(off) {
            if matches!(off, reg::PROC0_CTRL | reg::PROC1_CTRL) {
                self.proc_ctrl[index] = value & ctrl::ENABLE;
            }
            return;
        }
        self.regs.write(off, value);
    }

    pub fn tick(&mut self, cycles: u64) {
        for index in 0..2 {
            if self.tick_ctrl[index] & ctrl::ENABLE != 0 {
                let total = self.tick_carry[index] + cycles;
                self.tick_count[index] =
                    self.tick_count[index].wrapping_add((total / CYCLES_PER_US) as u32);
                self.tick_carry[index] = total % CYCLES_PER_US;
            }
            if self.proc_ctrl[index] & ctrl::ENABLE != 0 {
                self.proc_count[index] = self.proc_count[index].wrapping_add(cycles as u32);
            }
        }
    }

    pub fn irq(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_gates_both_counters() {
        let mut ticks = Ticks::new();
        ticks.tick(1000);
        assert_eq!(ticks.read(reg::TICK0_COUNT), 0);
        assert_eq!(ticks.read(reg::PROC0_COUNT), 0);

        ticks.write(reg::TICK0_CTRL, ctrl::ENABLE);
        ticks.write(reg::PROC0_CTRL, ctrl::ENABLE);
        ticks.tick(250);
        assert_eq!(ticks.read(reg::TICK0_COUNT), 2);
        assert_eq!(ticks.read(reg::PROC0_COUNT), 250);
        assert_ne!(ticks.read(reg::TICK0_CTRL) & ctrl::RUNNING, 0);
    }

    #[test]
    fn counters_are_independent() {
        let mut ticks = Ticks::new();
        ticks.write(reg::TICK1_CTRL, ctrl::ENABLE);
        ticks.write(reg::PROC1_CTRL, ctrl::ENABLE);
        ticks.tick(125 * 3);
        assert_eq!(ticks.read(reg::TICK0_COUNT), 0);
        assert_eq!(ticks.read(reg::TICK1_COUNT), 3);
        assert_eq!(ticks.read(reg::PROC0_COUNT), 0);
        assert_eq!(ticks.read(reg::PROC1_COUNT), 125 * 3);
    }
}
