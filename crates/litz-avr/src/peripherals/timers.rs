//! Timer/counters, modelled once and configured per device.
//!
//! All four families the crate supports use the same counter core, driven by a
//! [`TimerSpec`] from [`crate::device`]: the waveform mode, `TOP`, prescaler
//! table and flag bit positions are read from the part's table, so Timer0 on an
//! ATtiny85 sets `TOV0` in bit 1 of the shared `TIFR` while Timer0 on an
//! ATmega328P sets it in bit 0 of its own register.
//!
//! Supported waveform modes are normal, CTC, fast PWM and phase-correct PWM.
//! A timer counts prescaled CPU cycles, raises its overflow and compare flags,
//! and drives its compare-output pins when the `COMnx` bits connect them, so a
//! firmware `analogWrite` produces a real duty cycle.
//!
//! Deliberately simplified:
//!
//! * External clock inputs (`CS = 6/7`, or `CS = 15` on the four-bit table)
//!   stop the timer, because the simulator has no external clock source.
//! * Timer2's asynchronous mode is treated as synchronous, and the ATmega32U4
//!   Timer4 dead-time generator and complementary outputs are not modelled.
//! * The force-output strobes in the `TCCRnC` registers are ignored.
//! * On the ATmega32U4's 10-bit Timer4 the `TC4H` high-byte latch is modelled
//!   only for the counter and compare registers; its access ordering follows
//!   the datasheet.

use crate::device::{NONE, Prescaler, TimerKind, TimerSpec};

/// Prescaler divisor selected by `TCCR0B`/`TCCR1B` `CS[2:0]`.
///
/// Values 6 and 7 select an external clock input, which the simulator has no
/// source for; they stop the timer.
pub(crate) fn prescale_divisor(cs: u8) -> u32 {
    match cs & 0x07 {
        1 => 1,
        2 => 8,
        3 => 64,
        4 => 256,
        5 => 1024,
        _ => 0,
    }
}

/// Timer2's prescaler table differs from Timer0/1: it has /32 and /128 instead
/// of /256 and /1024 in the middle of the range.
pub(crate) fn timer2_divisor(cs: u8) -> u32 {
    match cs & 0x07 {
        1 => 1,
        2 => 8,
        3 => 32,
        4 => 64,
        5 => 128,
        6 => 256,
        7 => 1024,
        _ => 0,
    }
}

/// The four-bit prescaler table: `1 << (cs - 1)` up to `/16384`; 15 is an
/// external input.
fn power2_divisor(cs: u8) -> u32 {
    match cs & 0x0f {
        0 | 15 => 0,
        other => 1u32 << (other - 1),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Up,
    Down,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    Ctc,
    Fast,
    Phase,
}

/// One timer/counter. The same type backs every device's timers; `spec`
/// selects the behaviour and `present` is false for a timer the part lacks.
pub struct Timer {
    spec: Option<&'static TimerSpec>,
    pub tccr_a: u8,
    pub tccr_b: u8,
    pub tccr_c: u8,
    pub tccr_d: u8,
    pub tccr_e: u8,
    /// `GTCCR`, which holds Timer1's B-side control on the ATtiny85.
    pub gtccr: u8,
    pub count: u16,
    pub icr: u16,
    pub ocr_a: u16,
    pub ocr_b: u16,
    pub ocr_c: u16,
    pub ocr_d: u16,
    /// The ATmega32U4's `TC4H` high-byte latch.
    pub temp_high: u8,
    pub timsk: u8,
    /// The timer's `TIFR`, using the part's flag positions.
    pub tifr: u8,
    /// Overflow events since reset, the quantity a `millis()` counter is built
    /// from.
    pub overflows: u64,
    accumulator: u64,
    direction: Direction,
    outputs: [bool; 3],
}

impl Default for Timer {
    fn default() -> Self {
        Self::empty()
    }
}

impl Timer {
    /// A timer the device does not have, so no register matches it.
    pub fn empty() -> Self {
        Self {
            spec: None,
            tccr_a: 0,
            tccr_b: 0,
            tccr_c: 0,
            tccr_d: 0,
            tccr_e: 0,
            gtccr: 0,
            count: 0,
            icr: 0,
            ocr_a: 0,
            ocr_b: 0,
            ocr_c: 0,
            ocr_d: 0,
            temp_high: 0,
            timsk: 0,
            tifr: 0,
            overflows: 0,
            accumulator: 0,
            direction: Direction::Up,
            outputs: [false; 3],
        }
    }

    /// A timer configured by one device table entry.
    pub fn new(spec: &'static TimerSpec) -> Self {
        Self {
            spec: Some(spec),
            ..Self::empty()
        }
    }

    pub fn spec(&self) -> Option<&'static TimerSpec> {
        self.spec
    }

    fn kind(&self) -> Option<TimerKind> {
        self.spec.map(|spec| spec.kind)
    }

    fn is_sixteen(&self) -> bool {
        self.kind() == Some(TimerKind::Long16)
    }

    fn masks(&self) -> crate::device::FlagMasks {
        self.spec
            .map_or(crate::device::FlagMasks::default(), |spec| spec.masks)
    }

    /// The prescaler divisor the control bits select, or zero when stopped.
    pub(crate) fn prescaler(&self) -> u32 {
        let Some(spec) = self.spec else {
            return 0;
        };
        match spec.kind {
            TimerKind::Standard8 | TimerKind::Long16 => match spec.prescaler {
                Prescaler::Timer2 => timer2_divisor(self.tccr_b),
                _ => prescale_divisor(self.tccr_b),
            },
            TimerKind::Tiny8 => power2_divisor(self.tccr_a),
            TimerKind::HighSpeed4 => power2_divisor(self.tccr_b),
        }
    }

    pub(crate) fn running(&self) -> bool {
        self.prescaler() != 0
    }

    // -- waveform mode ------------------------------------------------------

    /// `WGM` for the timers that split it across `TCCRnA`/`TCCRnB`.
    fn wgm8(&self) -> u8 {
        (self.tccr_a & 0x03) | ((self.tccr_b & 0x08) >> 1)
    }

    /// `WGM` for the 16-bit timers: `TCCRnA[1:0]` plus `TCCRnB[4:3]`.
    fn wgm16(&self) -> u8 {
        (self.tccr_a & 0x03) | ((self.tccr_b >> 1) & 0x0c)
    }

    fn mode(&self) -> Mode {
        match self.kind() {
            Some(TimerKind::Standard8) => match self.wgm8() {
                0 => Mode::Normal,
                1 | 5 => Mode::Phase,
                2 => Mode::Ctc,
                3 | 7 => Mode::Fast,
                _ => Mode::Normal,
            },
            Some(TimerKind::Tiny8) => {
                if self.tccr_a & 0x40 != 0 {
                    Mode::Fast // PWM1A
                } else if self.tccr_a & 0x80 != 0 {
                    Mode::Ctc // CTC1
                } else {
                    Mode::Normal
                }
            }
            Some(TimerKind::Long16) => match self.wgm16() {
                0 | 13 => Mode::Normal,
                1 | 2 | 3 | 8 | 9 | 10 | 11 => Mode::Phase,
                4 | 12 => Mode::Ctc,
                _ => Mode::Fast,
            },
            Some(TimerKind::HighSpeed4) => Mode::Fast,
            None => Mode::Normal,
        }
    }

    fn top(&self) -> u16 {
        match self.kind() {
            Some(TimerKind::Standard8) => match self.wgm8() {
                2 | 5 | 7 => self.ocr_a,
                _ => 0xff,
            },
            Some(TimerKind::Tiny8) => match self.mode() {
                Mode::Normal => 0xff,
                // OCR1C is TOP in both CTC and PWM modes.
                _ => self.ocr_c,
            },
            Some(TimerKind::Long16) => match self.wgm16() {
                1 => 0x00ff,
                2 => 0x01ff,
                3 => 0x03ff,
                4 => self.ocr_a,
                5 => 0x00ff,
                6 => 0x01ff,
                7 => 0x03ff,
                8 | 10 => self.icr,
                9 | 11 => self.ocr_a,
                12 => self.icr,
                14 => self.icr,
                15 => self.ocr_a,
                _ => 0xffff,
            },
            // The 10-bit high-speed timer uses OCR4C as TOP in every mode.
            Some(TimerKind::HighSpeed4) => self.ocr_c & 0x03ff,
            None => 0xff,
        }
    }

    fn compare_register(&self, channel: usize) -> u16 {
        match channel {
            0 => self.ocr_a,
            1 => self.ocr_b,
            2 => self.ocr_c,
            3 => self.ocr_d,
            _ => 0,
        }
    }

    /// The compare register behind an output channel. The ATmega32U4's fourth
    /// compare flag and output are called D, but the model has three output
    /// channels, so channel C there reads `OCR4D`.
    fn output_register(&self, channel: usize) -> u16 {
        if channel == 2 && self.kind() == Some(TimerKind::HighSpeed4) {
            self.ocr_d
        } else {
            self.compare_register(channel)
        }
    }

    fn com(&self, channel: usize) -> u8 {
        match self.kind() {
            Some(TimerKind::Standard8) => match channel {
                0 => (self.tccr_a >> 6) & 0x03,
                1 => (self.tccr_a >> 4) & 0x03,
                _ => 0,
            },
            Some(TimerKind::Tiny8) => match channel {
                0 => (self.tccr_a >> 4) & 0x03,
                1 => (self.gtccr >> 4) & 0x03,
                _ => 0,
            },
            Some(TimerKind::Long16) => match channel {
                0 => (self.tccr_a >> 6) & 0x03,
                1 => (self.tccr_a >> 4) & 0x03,
                2 => (self.tccr_a >> 2) & 0x03,
                _ => 0,
            },
            Some(TimerKind::HighSpeed4) => match channel {
                0 => (self.tccr_a >> 6) & 0x03,
                1 => (self.tccr_a >> 4) & 0x03,
                2 => (self.tccr_c >> 2) & 0x03, // COM4D
                _ => 0,
            },
            None => 0,
        }
    }

    fn compare_flag(&self, channel: usize) -> u8 {
        let masks = self.masks();
        match channel {
            0 => masks.compare_a,
            1 => masks.compare_b,
            2 => masks.compare_c,
            _ => 0,
        }
    }

    fn compare(&mut self, channel: usize) {
        if self.output_register(channel) != self.count {
            return;
        }
        let flag = self.compare_flag(channel);
        if flag != 0 {
            self.tifr |= flag;
        }
        if channel < 3 {
            match self.com(channel) {
                1 => self.outputs[channel] = !self.outputs[channel],
                2 => self.outputs[channel] = false,
                3 => self.outputs[channel] = true,
                _ => {}
            }
        }
    }

    // -- counting -----------------------------------------------------------

    pub(crate) fn advance(&mut self, cycles: u64) {
        let divisor = u64::from(self.prescaler());
        if divisor == 0 {
            return;
        }
        self.accumulator = self.accumulator.saturating_add(cycles);
        let mut ticks = self.accumulator / divisor;
        self.accumulator %= divisor;
        // A single instruction retires a handful of cycles, so this loop is
        // short. The cap only protects a caller that advances the clock by an
        // absurd amount in one call.
        if ticks > 1_000_000 {
            ticks = 1_000_000;
        }
        for _ in 0..ticks {
            self.tick();
        }
    }

    fn overflow(&mut self) {
        self.overflows = self.overflows.saturating_add(1);
        let flag = self.masks().overflow;
        if flag != 0 {
            self.tifr |= flag;
        }
    }

    fn tick(&mut self) {
        let mode = self.mode();
        let top = self.top();
        let maximum = match self.kind() {
            Some(TimerKind::Long16) => 0xffff,
            Some(TimerKind::HighSpeed4) => 0x03ff,
            _ => 0xff,
        };
        match mode {
            Mode::Normal => {
                for channel in 0..3 {
                    self.compare(channel);
                }
                if self.count >= maximum {
                    self.count = 0;
                    self.overflow();
                } else {
                    self.count = self.count.wrapping_add(1);
                }
            }
            Mode::Ctc => {
                for channel in 0..3 {
                    self.compare(channel);
                }
                if self.kind() == Some(TimerKind::Tiny8) {
                    // The ATtiny85 reaches TOP in CTC mode and raises TOV1
                    // there.
                    if self.count >= top {
                        self.count = 0;
                        self.overflow();
                    } else {
                        self.count = self.count.wrapping_add(1);
                    }
                } else if self.count >= maximum {
                    self.count = 0;
                    self.overflow();
                } else if self.count >= top {
                    self.count = 0;
                } else {
                    self.count = self.count.wrapping_add(1);
                }
            }
            Mode::Fast => {
                for channel in 0..3 {
                    self.compare(channel);
                }
                if self.count >= top {
                    self.count = 0;
                    self.overflow();
                } else {
                    self.count = self.count.wrapping_add(1);
                }
            }
            Mode::Phase => {
                for channel in 0..3 {
                    self.compare(channel);
                }
                match self.direction {
                    Direction::Up => {
                        if self.count >= top {
                            self.direction = Direction::Down;
                            self.count = self.count.saturating_sub(1);
                        } else {
                            self.count = self.count.wrapping_add(1);
                        }
                    }
                    Direction::Down => {
                        if self.count == 0 {
                            self.direction = Direction::Up;
                            self.overflows = self.overflows.saturating_add(1);
                            let flag = self.masks().overflow;
                            if flag != 0 {
                                self.tifr |= flag;
                            }
                            if top > 0 {
                                self.count = 1;
                            }
                        } else {
                            self.count = self.count.wrapping_sub(1);
                        }
                    }
                }
            }
        }
    }

    /// Latch the counter into `ICR` and raise the capture flag.
    pub(crate) fn capture(&mut self) {
        self.icr = self.count;
        let flag = self.masks().capture;
        if flag != 0 {
            self.tifr |= flag;
        }
    }

    // -- compare output -----------------------------------------------------

    fn oc_level(&self, channel: usize) -> Option<bool> {
        let com = self.com(channel);
        if com == 0 {
            return None;
        }
        let ocr = self.output_register(channel);
        let output = self.outputs[channel];
        match self.mode() {
            Mode::Normal | Mode::Ctc => Some(output),
            Mode::Fast => {
                if com == 1 {
                    return Some(output);
                }
                let high = self.count < ocr;
                Some(if com == 2 { high } else { !high })
            }
            Mode::Phase => {
                if com == 1 {
                    return Some(output);
                }
                let high = match self.direction {
                    Direction::Up => self.count < ocr,
                    Direction::Down => self.count <= ocr,
                };
                Some(if com == 2 { high } else { !high })
            }
        }
    }

    /// Level the timer wants on channel `channel` (0 = A, 1 = B, 2 = C/D),
    /// or `None` when the compare output is disconnected.
    pub fn oc_level_of(&self, channel: usize) -> Option<bool> {
        if channel > 2 || self.spec.is_none() {
            return None;
        }
        self.oc_level(channel)
    }

    /// Level the timer wants on `OCxA`.
    pub fn oc_a_level(&self) -> Option<bool> {
        self.oc_level_of(0)
    }

    /// Level the timer wants on `OCxB`.
    pub fn oc_b_level(&self) -> Option<bool> {
        self.oc_level_of(1)
    }

    fn oc_duty(&self, channel: usize) -> Option<f64> {
        let com = self.com(channel);
        if com == 0 || !self.running() {
            return None;
        }
        let ocr = self.output_register(channel);
        let duty = match self.mode() {
            Mode::Normal | Mode::Ctc => 0.5,
            Mode::Fast | Mode::Phase => {
                let period = f64::from(self.top()) + 1.0;
                (f64::from(ocr) / period).clamp(0.0, 1.0)
            }
        };
        Some(if com == 3 { 1.0 - duty } else { duty })
    }

    /// The high-time fraction channel `channel` is programmed to, or `None`
    /// when it is disconnected or the timer is stopped.
    pub fn duty_of(&self, channel: usize) -> Option<f64> {
        if channel > 2 || self.spec.is_none() {
            return None;
        }
        self.oc_duty(channel)
    }

    /// The high-time fraction `OCxA` is programmed to.
    pub fn oc_a_duty(&self) -> Option<f64> {
        self.duty_of(0)
    }

    /// The high-time fraction `OCxB` is programmed to.
    pub fn oc_b_duty(&self) -> Option<f64> {
        self.duty_of(1)
    }

    // -- registers ----------------------------------------------------------

    fn control(&self, index: usize) -> u8 {
        match index {
            0 => self.tccr_a,
            1 => self.tccr_b,
            2 => self.tccr_c,
            3 => self.tccr_d,
            _ => self.tccr_e,
        }
    }

    fn set_control(&mut self, index: usize, value: u8) {
        match index {
            0 => self.tccr_a = value,
            1 => self.tccr_b = value,
            2 => self.tccr_c = value,
            3 => self.tccr_d = value,
            _ => self.tccr_e = value,
        }
    }

    /// Read a register this timer owns, or `None` when `addr` is another
    /// peripheral's.
    pub(crate) fn read_register(&mut self, addr: u16) -> Option<u8> {
        let spec = self.spec?;
        let regs = &spec.regs;
        for (index, &control) in regs.control.iter().enumerate() {
            if control == addr {
                return Some(self.control(index));
            }
        }
        if regs.gtccr == addr {
            return Some(self.gtccr);
        }
        if regs.count == addr {
            let low = self.count as u8;
            if spec.kind == TimerKind::HighSpeed4 {
                self.temp_high = (self.count >> 8) as u8;
            }
            return Some(low);
        }
        if regs.count_high != NONE && regs.count_high == addr {
            return Some((self.count >> 8) as u8);
        }
        if regs.capture != NONE && regs.capture == addr {
            let low = self.icr as u8;
            if spec.kind == TimerKind::HighSpeed4 {
                self.temp_high = (self.icr >> 8) as u8;
            }
            return Some(low);
        }
        if regs.capture_high != NONE && regs.capture_high == addr {
            return Some((self.icr >> 8) as u8);
        }
        for channel in 0..4 {
            if regs.compare[channel] != NONE && regs.compare[channel] == addr {
                let value = self.compare_register(channel);
                if spec.kind == TimerKind::HighSpeed4 {
                    self.temp_high = (value >> 8) as u8;
                }
                return Some(value as u8);
            }
            if regs.compare_high[channel] != NONE && regs.compare_high[channel] == addr {
                return Some((self.compare_register(channel) >> 8) as u8);
            }
        }
        if regs.temp_high != NONE && regs.temp_high == addr {
            return Some(self.temp_high);
        }
        if regs.timsk == addr {
            return Some(self.timsk);
        }
        if regs.tifr == addr {
            return Some(self.tifr);
        }
        None
    }

    /// Write a register this timer owns. Returns false when `addr` is another
    /// peripheral's.
    pub(crate) fn write_register(&mut self, addr: u16, value: u8) -> bool {
        let Some(spec) = self.spec else {
            return false;
        };
        let regs = &spec.regs;
        for index in 0..regs.control.len() {
            if regs.control[index] == addr {
                self.set_control(index, value);
                return true;
            }
        }
        if regs.gtccr == addr {
            self.gtccr = value;
            return true;
        }
        if regs.count == addr {
            if self.is_sixteen() || spec.kind == TimerKind::HighSpeed4 {
                let high = if spec.kind == TimerKind::HighSpeed4 {
                    u16::from(self.temp_high & 0x03)
                } else {
                    self.count >> 8
                };
                self.count = (high << 8) | u16::from(value);
            } else {
                self.count = u16::from(value);
            }
            return true;
        }
        if regs.count_high != NONE && regs.count_high == addr {
            self.count = (u16::from(value) << 8) | (self.count & 0x00ff);
            return true;
        }
        if regs.capture != NONE && regs.capture == addr {
            self.icr = (self.icr & 0xff00) | u16::from(value);
            return true;
        }
        if regs.capture_high != NONE && regs.capture_high == addr {
            self.icr = (self.icr & 0x00ff) | (u16::from(value) << 8);
            return true;
        }
        for channel in 0..4 {
            if regs.compare[channel] != NONE && regs.compare[channel] == addr {
                let high = if self.is_sixteen() {
                    self.compare_register(channel) >> 8
                } else if spec.kind == TimerKind::HighSpeed4 {
                    u16::from(self.temp_high & 0x03)
                } else {
                    0
                };
                self.set_compare(channel, (high << 8) | u16::from(value));
                return true;
            }
            if regs.compare_high[channel] != NONE && regs.compare_high[channel] == addr {
                let low = self.compare_register(channel) & 0x00ff;
                self.set_compare(channel, (u16::from(value) << 8) | low);
                return true;
            }
        }
        if regs.temp_high != NONE && regs.temp_high == addr {
            self.temp_high = value & 0x03;
            return true;
        }
        if regs.timsk == addr {
            self.timsk = value;
            return true;
        }
        if regs.tifr == addr {
            // Write-one-to-clear.
            self.tifr &= !value;
            return true;
        }
        false
    }

    fn set_compare(&mut self, channel: usize, value: u16) {
        match channel {
            0 => self.ocr_a = value,
            1 => self.ocr_b = value,
            2 => self.ocr_c = value,
            _ => self.ocr_d = value,
        }
    }

    /// The vector and flag mask of each interrupt source, in channel order.
    pub(crate) fn interrupt_sources(&self) -> impl Iterator<Item = (u8, u8)> + '_ {
        let spec = self.spec;
        let masks = self.masks();
        let mut sources: [Option<(u8, u8)>; 5] = [None; 5];
        if let Some(spec) = spec {
            let entries = [
                (spec.vectors.overflow, masks.overflow),
                (spec.vectors.compare_a, masks.compare_a),
                (spec.vectors.compare_b, masks.compare_b),
                (spec.vectors.compare_c, masks.compare_c),
                (spec.vectors.capture, masks.capture),
            ];
            for (slot, entry) in sources.iter_mut().zip(entries) {
                match entry {
                    (Some(vector), mask) if mask != 0 => *slot = Some((vector, mask)),
                    _ => {}
                }
            }
        }
        sources.into_iter().flatten()
    }
}

/// The ATmega328P's Timer/Counter0, kept as a name for callers that spell the
/// type out.
pub type Timer0 = Timer;
/// The ATmega328P's Timer/Counter1.
pub type Timer1 = Timer;
/// The ATmega328P's Timer/Counter2.
pub type Timer2 = Timer;
