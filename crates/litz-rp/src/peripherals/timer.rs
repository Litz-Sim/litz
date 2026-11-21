//! The 64-bit microsecond timer with four alarms.
//!
//! The counter advances one microsecond per 125 CPU cycles and the fractional
//! remainder is carried, so a run that advances in arbitrary instruction-sized
//! steps still lands on the same microsecond count as one big step. The four
//! alarms compare the low 32 bits of the counter, which is what the RP2040's
//! 32-bit `ALARM` registers do.
//!
//! `TIMEHR`/`TIMELR` are the latched view of the counter: reading the high word
//! captures the low word so the pair a firmware reads is consistent even
//! though the counter keeps running between the two reads. Writing
//! `TIMEHW`/`TIMELW` sets the counter, which is how the ROM's timer test and a
//! debugger move the clock.

use super::RegisterFile;

/// Register offsets from the TIMER base.
pub mod reg {
    pub const TIMEHW: u32 = 0x00;
    pub const TIMELW: u32 = 0x04;
    pub const TIMEHR: u32 = 0x08;
    pub const TIMELR: u32 = 0x0c;
    pub const ALARM0: u32 = 0x10;
    pub const ALARM1: u32 = 0x14;
    pub const ALARM2: u32 = 0x18;
    pub const ALARM3: u32 = 0x1c;
    pub const ARMED: u32 = 0x20;
    pub const TIMERAWH: u32 = 0x24;
    pub const TIMERAWL: u32 = 0x28;
    pub const DBGPAUSE: u32 = 0x2c;
    pub const PAUSE: u32 = 0x30;
    pub const INTR: u32 = 0x34;
    pub const INTE: u32 = 0x38;
    pub const INTF: u32 = 0x3c;
    pub const INTS: u32 = 0x40;
}

/// CPU cycles in one microsecond at 125 MHz.
const CYCLES_PER_US: u64 = 125;

#[derive(Debug)]
pub struct Timer {
    regs: RegisterFile,
    /// Microseconds since the counter was set.
    time_us: u64,
    /// CPU cycles not yet worth a microsecond.
    carry: u64,
    alarms: [u32; 4],
    armed: u32,
    /// Low word captured by the last `TIMEHR` read.
    latched_low: Option<u32>,
    intr: u32,
    inte: u32,
    /// `INTF` force bits. Firmware rarely uses them, but they are writable and
    /// the combined status has to include them.
    forced: u32,
    dbgpause: u32,
    pause: bool,
}

impl Default for Timer {
    fn default() -> Self {
        Self::new()
    }
}

impl Timer {
    pub fn new() -> Self {
        Timer {
            regs: RegisterFile::new(0x100 / 4),
            time_us: 0,
            carry: 0,
            alarms: [0; 4],
            armed: 0,
            latched_low: None,
            intr: 0,
            inte: 0,
            forced: 0,
            dbgpause: 0,
            pause: false,
        }
    }

    /// The current counter, for bring-up tests and the adapter.
    pub fn time_us(&self) -> u64 {
        self.time_us
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::TIMEHR => {
                self.latched_low = Some(self.time_us as u32);
                (self.time_us >> 32) as u32
            }
            reg::TIMELR => self.latched_low.take().unwrap_or(self.time_us as u32),
            reg::ALARM0..=reg::ALARM3 => self.alarms[((off - reg::ALARM0) / 4) as usize],
            reg::ARMED => self.armed,
            reg::TIMERAWH => (self.time_us >> 32) as u32,
            reg::TIMERAWL => self.time_us as u32,
            reg::DBGPAUSE => self.dbgpause,
            reg::PAUSE => u32::from(self.pause),
            reg::INTR => self.intr,
            reg::INTE => self.inte,
            reg::INTF => self.forced,
            reg::INTS => (self.intr | self.forced) & self.inte,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::TIMEHW => {
                // The pair is latched on the high write in hardware; writing
                // both halves here is indistinguishable to a firmware that
                // follows the documented order (low, then high).
                self.time_us = (self.time_us & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
                self.latched_low = None;
            }
            reg::TIMELW => {
                self.time_us = (self.time_us & 0xffff_ffff_0000_0000) | u64::from(value);
                self.latched_low = None;
            }
            reg::ALARM0..=reg::ALARM3 => {
                let index = ((off - reg::ALARM0) / 4) as usize;
                self.alarms[index] = value;
                self.armed |= 1 << index;
            }
            // ARMED is write-one-to-disarm.
            reg::ARMED => self.armed &= !(value & 0xf),
            reg::DBGPAUSE => self.dbgpause = value,
            reg::PAUSE => self.pause = value & 1 != 0,
            // INTR is write-one-to-clear.
            reg::INTR => self.intr &= !(value & 0xf),
            reg::INTE => self.inte = value & 0xf,
            reg::INTF => self.forced = value & 0xf,
            reg::INTS => {}
            reg::TIMEHR | reg::TIMELR | reg::TIMERAWH | reg::TIMERAWL => {}
            off => self.regs.write(off, value),
        }
    }

    pub fn tick(&mut self, cycles: u64) {
        if self.pause {
            return;
        }
        let total = self.carry + cycles;
        let micros = total / CYCLES_PER_US;
        self.carry = total % CYCLES_PER_US;
        if micros == 0 {
            return;
        }

        let start = self.time_us as u32;
        self.time_us = self.time_us.wrapping_add(micros);
        for index in 0..4 {
            if self.armed & (1 << index) == 0 {
                continue;
            }
            // Distance from the counter at the start of the tick to the alarm,
            // modulo 2^32. `micros` is the width of the interval just crossed.
            let distance = u64::from(self.alarms[index].wrapping_sub(start));
            // The counter visits `start + 1 ..= start + micros`, so an alarm
            // is reached when its forward distance is at most the width of
            // this interval.
            if distance <= micros || micros >= 1 << 32 {
                self.intr |= 1 << index;
            }
        }
    }

    /// Whether alarm `index` has fired and not been acknowledged.
    pub fn irq(&self, index: usize) -> bool {
        index < 4 && self.intr & self.inte & (1 << index) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_carries_the_fractional_microsecond() {
        let mut timer = Timer::new();
        timer.tick(124);
        assert_eq!(timer.time_us(), 0);
        timer.tick(1);
        assert_eq!(timer.time_us(), 1);
        timer.tick(250);
        assert_eq!(timer.time_us(), 3);
    }

    #[test]
    fn alarm_fires_when_the_counter_reaches_it() {
        let mut timer = Timer::new();
        timer.write(reg::INTE, 1);
        timer.write(reg::ALARM0, 10);
        timer.tick(125 * 9);
        assert!(!timer.irq(0), "nine microseconds");
        timer.tick(125);
        assert!(timer.irq(0), "ten microseconds");
        assert_eq!(timer.read(reg::INTR) & 1, 1);
        assert_eq!(timer.read(reg::INTS) & 1, 1);
        timer.write(reg::INTR, 1);
        assert!(!timer.irq(0));
    }

    #[test]
    fn high_word_latches_the_low_word() {
        let mut timer = Timer::new();
        timer.write(reg::TIMELW, 0xffff_fffe);
        timer.write(reg::TIMEHW, 0);
        let high = timer.read(reg::TIMEHR);
        timer.tick(125 * 4);
        let low = timer.read(reg::TIMELR);
        assert_eq!(high, 0);
        assert_eq!(low, 0xffff_fffe, "the latched low word does not advance");
        assert_eq!(timer.read(reg::TIMERAWL), 2);
    }

    #[test]
    fn pause_holds_the_counter() {
        let mut timer = Timer::new();
        timer.write(reg::PAUSE, 1);
        timer.tick(125 * 5);
        assert_eq!(timer.time_us(), 0);
        timer.write(reg::PAUSE, 0);
        timer.tick(125);
        assert_eq!(timer.time_us(), 1);
    }
}
