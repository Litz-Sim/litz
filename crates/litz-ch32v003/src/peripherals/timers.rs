//! TIM1, TIM2 and SysTick.
//!
//! TIM1 is the advanced 16-bit timer and TIM2 the general-purpose one. Both
//! model the same core: a prescaler, an up or down counter, an auto-reload
//! register, four capture/compare channels with PWM output, and the update and
//! compare interrupts. TIM1's break, dead-time and repetition registers are
//! storage; the timer never stops on a break input because there is none.
//!
//! The counter is ticked by an accumulated ratio of CPU cycles so a timer
//! clocked from a divided APB keeps a fractional relationship without drifting.
//! PWM duty is exposed per channel for the adapter to publish, which is how a
//! board analog view sees the average of the pin.
//!
//! # Pads
//!
//! The model fixes each channel to a pad rather than following the remap bits:
//!
//! * TIM1 CH1..CH4: PD2, PD3, PD4, PD5 (GPIO 10..13)
//! * TIM2 CH1..CH4: PC5, PC2, PC6, PC7 (GPIO 5, 2, 6, 7)
//!
//! Those pads are documented rather than derived, so a board manifest can wire
//! an LED to the pad a test expects.
//!
//! # SysTick
//!
//! SysTick is the WCH block at `0xE000_F000`, clocked from the core clock. It
//! counts down, reloads from `RVR` when it reaches zero, sets `COUNTFLAG` and
//! raises its interrupt; reading `CTLR` clears `COUNTFLAG`.

use super::RegisterFile;

// -- TIM1/TIM2 -------------------------------------------------------------

/// Register offsets shared by TIM1 and TIM2.
pub mod reg {
    pub const CTLR1: u32 = 0x00;
    pub const CTLR2: u32 = 0x04;
    pub const SMCFGR: u32 = 0x08;
    pub const DIER: u32 = 0x0c;
    pub const SR: u32 = 0x10;
    pub const EGR: u32 = 0x14;
    pub const CCMR1: u32 = 0x18;
    pub const CCMR2: u32 = 0x1c;
    pub const CCER: u32 = 0x20;
    pub const CNT: u32 = 0x24;
    pub const PSC: u32 = 0x28;
    pub const ATR: u32 = 0x2c;
    pub const RCR: u32 = 0x30;
    pub const CCR1: u32 = 0x34;
    pub const CCR2: u32 = 0x38;
    pub const CCR3: u32 = 0x3c;
    pub const CCR4: u32 = 0x40;
    pub const BDTR: u32 = 0x44;
}

/// Interrupt enable bits in `DIER`.
pub mod dier {
    pub const UIE: u32 = 1 << 0;
    pub const CC1IE: u32 = 1 << 1;
    pub const CC2IE: u32 = 1 << 2;
    pub const CC3IE: u32 = 1 << 3;
    pub const CC4IE: u32 = 1 << 4;
    pub const TIE: u32 = 1 << 6;
    pub const BIE: u32 = 1 << 7;
}

/// Status bits in `SR`.
pub mod sr {
    pub const UIF: u32 = 1 << 0;
    pub const CC1IF: u32 = 1 << 1;
    pub const CC2IF: u32 = 1 << 2;
    pub const CC3IF: u32 = 1 << 3;
    pub const CC4IF: u32 = 1 << 4;
    pub const TIF: u32 = 1 << 6;
    pub const BIF: u32 = 1 << 7;
    pub const CC_MASK: u32 = CC1IF | CC2IF | CC3IF | CC4IF;
}

/// `CTLR1` bits.
pub mod ctlr1 {
    pub const CEN: u32 = 1 << 0;
    pub const UDIS: u32 = 1 << 1;
    pub const URS: u32 = 1 << 2;
    pub const OPM: u32 = 1 << 3;
    pub const DIR: u32 = 1 << 4;
    pub const ARPE: u32 = 1 << 7;
}

/// `CCER` bits, one enable at bit 0/4/8/12.
pub fn cc_enable(channel: usize) -> u32 {
    1 << (channel * 4)
}

/// The output-compare mode field for a channel.
fn oc_mode(ccmr1: u32, ccmr2: u32, channel: usize) -> u32 {
    let register = if channel < 2 { ccmr1 } else { ccmr2 };
    let shift = if channel % 2 == 0 { 4 } else { 12 };
    (register >> shift) & 7
}

/// Which pad a timer channel drives, as a GPIO number.
pub fn tim1_pin(channel: usize) -> u8 {
    [10, 11, 12, 13][channel.min(3)]
}

/// Which pad a TIM2 channel drives, as a GPIO number.
pub fn tim2_pin(channel: usize) -> u8 {
    [5, 2, 6, 7][channel.min(3)]
}

#[derive(Debug)]
pub struct Timer {
    regs: RegisterFile,
    counter: u32,
    /// Fraction of a timer clock left over from the last advance, in CPU
    /// cycles scaled by the timer clock.
    accumulator: u64,
    /// Timer clocks left over after the prescaler divided them.
    prescale_accumulator: u64,
    /// Set when the counter wrapped since the last `tick`.
    update: bool,
    /// Compare flags, one bit per channel, refreshed by an edge crossing.
    compare: u32,
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
            counter: 0,
            accumulator: 0,
            prescale_accumulator: 0,
            update: false,
            compare: 0,
        }
    }

    fn enabled(&self) -> bool {
        self.regs.read(reg::CTLR1) & ctlr1::CEN != 0
    }

    fn down(&self) -> bool {
        self.regs.read(reg::CTLR1) & ctlr1::DIR != 0
    }

    fn prescaler(&self) -> u32 {
        self.regs.read(reg::PSC) & 0xffff
    }

    fn reload(&self) -> u32 {
        self.regs.read(reg::ATR) & 0xffff
    }

    fn compare_reg(&self, channel: usize) -> u32 {
        self.regs.read(reg::CCR1 + channel as u32 * 4) & 0xffff
    }

    /// Advance by `cycles` CPU cycles with the timer clocked at `timer_hz`.
    pub fn tick(&mut self, cycles: u64, timer_hz: u64, cpu_hz: u64) {
        if !self.enabled() || cycles == 0 {
            return;
        }
        let scaled = cycles.saturating_mul(timer_hz);
        let total = self.accumulator.saturating_add(scaled);
        let ticks = total / cpu_hz.max(1);
        self.accumulator = total % cpu_hz.max(1);
        if ticks == 0 {
            return;
        }
        let period = u64::from(self.prescaler()) + 1;
        self.prescale_accumulator = self.prescale_accumulator.saturating_add(ticks);
        let ticks = self.prescale_accumulator / period;
        self.prescale_accumulator %= period;
        if ticks == 0 {
            return;
        }
        let reload = u64::from(self.reload()) + 1;
        let old = self.counter as u64;
        if self.down() {
            let mut value = old;
            let mut remaining = ticks;
            let mut wrapped = false;
            while remaining > 0 {
                if value == 0 {
                    value = reload;
                    wrapped = true;
                }
                let step = remaining.min(value);
                value -= step;
                remaining -= step;
            }
            self.counter = value as u32 & 0xffff;
            if wrapped {
                self.update = true;
            }
        } else {
            let total = old + ticks;
            let wraps = total / reload;
            self.counter = (total % reload) as u32 & 0xffff;
            if wraps > 0 {
                self.update = true;
            }
        }
        self.check_compare(old);
    }

    /// Set a channel's compare flag when the counter passed its `CCR`.
    fn check_compare(&mut self, old: u64) {
        let now = u64::from(self.counter);
        for channel in 0..4usize {
            let ccr = u64::from(self.compare_reg(channel));
            let passed = if self.down() {
                old > ccr && now <= ccr
            } else {
                old < ccr && now >= ccr
            };
            if passed {
                self.compare |= 1 << channel;
            }
        }
    }

    /// The PWM duty on `channel`, when the channel is enabled and in one of the
    /// PWM output modes.
    pub fn duty(&self, channel: usize) -> Option<f64> {
        let enable = self.regs.read(reg::CCER);
        if enable & cc_enable(channel) == 0 {
            return None;
        }
        let mode = oc_mode(
            self.regs.read(reg::CCMR1),
            self.regs.read(reg::CCMR2),
            channel,
        );
        let period = f64::from(self.reload()) + 1.0;
        let ccr = f64::from(self.compare_reg(channel));
        match mode {
            6 => Some((ccr / period).clamp(0.0, 1.0)),
            7 => Some(((period - ccr) / period).clamp(0.0, 1.0)),
            _ => None,
        }
    }

    /// CPU cycles until the next counter wrap, for the idle-time skip.
    pub fn cycles_to_next(&self, cpu_hz: u64, timer_hz: u64) -> Option<u64> {
        if !self.enabled() || timer_hz == 0 {
            return None;
        }
        let counted = if self.down() {
            u64::from(self.counter)
        } else {
            u64::from(self.reload()).saturating_sub(u64::from(self.counter)) + 1
        };
        let clocks = counted
            .max(1)
            .saturating_mul(u64::from(self.prescaler()) + 1);
        Some(clocks.saturating_mul(cpu_hz) / timer_hz + 1)
    }

    pub fn irq_update(&self) -> bool {
        let dier = self.regs.read(reg::DIER);
        let status = self.status();
        dier & dier::UIE != 0 && status & sr::UIF != 0
    }

    pub fn irq_compare(&self) -> bool {
        let dier = self.regs.read(reg::DIER);
        let status = self.status();
        (dier & dier::CC1IE != 0 && status & sr::CC1IF != 0)
            || (dier & dier::CC2IE != 0 && status & sr::CC2IF != 0)
            || (dier & dier::CC3IE != 0 && status & sr::CC3IF != 0)
            || (dier & dier::CC4IE != 0 && status & sr::CC4IF != 0)
    }

    fn status(&self) -> u32 {
        self.regs.read(reg::SR) | if self.update { sr::UIF } else { 0 } | self.compare & sr::CC_MASK
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::SR => self.status(),
            reg::CTLR1 => self.regs.read(reg::CTLR1),
            reg::DIER => self.regs.read(reg::DIER),
            reg::CNT => self.counter,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::CTLR1 => self.regs.write(reg::CTLR1, value),
            reg::DIER => self.regs.write(reg::DIER, value),
            // The status register is rc_w0: a zero bit clears its flag.
            reg::SR => {
                let keep = value;
                self.update &= keep & sr::UIF != 0;
                self.compare &= keep & sr::CC_MASK;
                self.regs.write(reg::SR, keep & !(sr::UIF | sr::CC_MASK));
            }
            reg::EGR => {
                if value & 1 != 0 {
                    self.update = true;
                    self.counter = if self.down() { self.reload() } else { 0 };
                }
                if value & sr::CC_MASK != 0 {
                    self.compare |= value & sr::CC_MASK;
                }
            }
            reg::CNT => self.counter = value & 0xffff,
            reg::PSC => self.regs.write(reg::PSC, value & 0xffff),
            reg::ATR => self.regs.write(reg::ATR, value & 0xffff),
            off => self.regs.write(off, value),
        }
    }
}

// -- SysTick ---------------------------------------------------------------

/// Register offsets from the SysTick base (`0xE000_F000`).
pub mod systick_reg {
    pub const CTLR: u32 = 0x00;
    pub const SR: u32 = 0x04;
    pub const RVR: u32 = 0x08;
    pub const CNT: u32 = 0x0c;
}

/// `CTLR` bits.
pub mod systick_ctlr {
    pub const ENABLE: u32 = 1 << 0;
    pub const TICKINT: u32 = 1 << 1;
    pub const CLKSOURCE: u32 = 1 << 2;
    pub const COUNTFLAG: u32 = 1 << 16;
}

#[derive(Debug)]
pub struct SysTick {
    ctlr: u32,
    reload: u32,
    counter: u32,
    countflag: bool,
}

impl Default for SysTick {
    fn default() -> Self {
        Self::new()
    }
}

impl SysTick {
    pub fn new() -> Self {
        SysTick {
            ctlr: 0,
            reload: 0,
            counter: 0,
            countflag: false,
        }
    }

    pub fn tick(&mut self, cycles: u64) {
        if self.ctlr & systick_ctlr::ENABLE == 0 || cycles == 0 {
            return;
        }
        let mut remaining = cycles;
        while remaining > 0 {
            if self.counter == 0 {
                // A reload of zero gives a period of one; the arm's SysTick
                // treats zero as "no reload" but the WCH block counts anyway.
                self.reload();
            }
            let step = remaining.min(u64::from(self.counter));
            if step == 0 {
                continue;
            }
            self.counter -= step as u32;
            remaining -= step;
        }
    }

    fn reload(&mut self) {
        // A zero reload would make the counter never move; the model takes it
        // as a period of one rather than looping.
        self.counter = self.reload.max(1);
        self.countflag = true;
    }

    pub fn irq(&self) -> bool {
        self.ctlr & systick_ctlr::TICKINT != 0 && self.countflag
    }

    /// Cycles until the next reload, for the idle-time skip.
    pub fn cycles_to_next(&self) -> Option<u64> {
        if self.ctlr & systick_ctlr::ENABLE == 0 {
            return None;
        }
        Some(u64::from(self.counter).max(1))
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            systick_reg::CTLR => {
                let value = self.ctlr
                    | if self.countflag {
                        systick_ctlr::COUNTFLAG
                    } else {
                        0
                    };
                // COUNTFLAG is clear-on-read.
                self.countflag = false;
                value
            }
            systick_reg::SR => 0,
            systick_reg::RVR => self.reload,
            systick_reg::CNT => self.counter,
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            systick_reg::CTLR => {
                let was_enabled = self.ctlr & systick_ctlr::ENABLE != 0;
                self.ctlr = value
                    & (systick_ctlr::ENABLE | systick_ctlr::TICKINT | systick_ctlr::CLKSOURCE);
                if self.ctlr & systick_ctlr::ENABLE == 0 {
                    self.counter = 0;
                    self.countflag = false;
                } else if !was_enabled {
                    // Enabling loads the period, so the first event is one
                    // full reload away rather than a spurious immediate one.
                    self.counter = self.reload.max(1);
                    self.countflag = false;
                }
            }
            systick_reg::RVR => self.reload = value & 0x00ff_ffff,
            systick_reg::CNT => self.counter = value & 0x00ff_ffff,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counter_wraps_at_the_reload_and_raises_the_update_flag() {
        let mut timer = Timer::new();
        timer.write(reg::ATR, 9);
        timer.write(reg::DIER, dier::UIE);
        timer.write(reg::CTLR1, ctlr1::CEN);
        timer.tick(8, 48_000_000, 48_000_000);
        assert_eq!(timer.read(reg::CNT), 8);
        assert!(!timer.irq_update());
        timer.tick(2, 48_000_000, 48_000_000);
        assert_eq!(timer.read(reg::CNT), 0);
        assert!(timer.irq_update());
        // Writing zero to UIF clears it.
        timer.write(reg::SR, 0);
        assert!(!timer.irq_update());
    }

    #[test]
    fn the_prescaler_divides_the_timer_clock() {
        let mut timer = Timer::new();
        timer.write(reg::ATR, 0xffff);
        timer.write(reg::PSC, 9);
        timer.write(reg::CTLR1, ctlr1::CEN);
        timer.tick(100, 48_000_000, 48_000_000);
        assert_eq!(timer.read(reg::CNT), 10);
    }

    #[test]
    fn pwm_mode_reports_a_duty() {
        let mut timer = Timer::new();
        timer.write(reg::ATR, 99);
        timer.write(reg::CCR1, 25);
        // OC1M = 110 (PWM mode 1) in CCMR1 bits 6:4.
        timer.write(reg::CCMR1, 6 << 4);
        timer.write(reg::CCER, cc_enable(0));
        assert_eq!(timer.duty(0), Some(0.25));
        timer.write(reg::CCR1, 0);
        assert_eq!(timer.duty(0), Some(0.0));
    }

    #[test]
    fn systick_counts_down_and_latches() {
        let mut systick = SysTick::new();
        systick.write(systick_reg::RVR, 10);
        systick.write(
            systick_reg::CTLR,
            systick_ctlr::ENABLE | systick_ctlr::TICKINT,
        );
        systick.tick(4);
        assert!(!systick.irq());
        systick.tick(10);
        assert!(systick.irq());
        assert_ne!(systick.read(systick_reg::CTLR) & systick_ctlr::COUNTFLAG, 0);
    }
}
