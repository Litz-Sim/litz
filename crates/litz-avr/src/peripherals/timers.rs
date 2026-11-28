//! Timer/counter peripherals: Timer0 (8-bit), Timer1 (16-bit) and Timer2
//! (8-bit).
//!
//! Each timer counts prescaled CPU cycles, raises its overflow and compare
//! flags in `TIFR0`/`TIFR1`/`TIFR2`, and drives its compare output pins when
//! the `COMnx` bits in `TCCRnx` connect them. The pin level is derived from the
//! counter and the compare register, so a firmware `analogWrite` (which only
//! writes `OCRnA`/`OCRnB` and the `COM` bits) produces a real duty cycle on
//! `OC0A`/`OC0B`/`OC1A`/`OC1B`/`OC2A`/`OC2B`.
//!
//! Supported waveform modes:
//!
//! * Normal and CTC for all three timers.
//! * Fast PWM and phase-correct PWM, with `TOP` = `0xFF`/`0x1FF`/`0x3FF`,
//!   `OCRnA`, or `ICR1` for Timer1 (modes 0–15).
//!
//! Timer1 also captures its counter into `ICR1` on an edge of `ICP1` (PB0,
//! Arduino D8); the edge is sampled once per peripheral advance.
//!
//! Everything else is simplified: the external clock inputs (`CS=6/7`) stop
//! the timer because the simulator has no external clock source, Timer2's
//! asynchronous mode is treated as synchronous, and the force-output bits in
//! `TCCRnxC` are ignored.

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

/// Define an 8-bit timer. Timer0 and Timer2 have the same register layout and
/// waveform modes and differ only in their prescaler table.
macro_rules! eight_bit_timer {
    ($(#[$doc:meta])* $name:ident, $divisor:path) => {
        $(#[$doc])*
        pub struct $name {
            pub tccr_a: u8,
            pub tccr_b: u8,
            pub count: u8,
            pub ocr_a: u8,
            pub ocr_b: u8,
            pub timsk: u8,
            /// `TIFR`: bit 0 `TOV`, bit 1 `OCFA`, bit 2 `OCFB`.
            pub tifr: u8,
            /// Overflow events since reset, the quantity a `millis()` counter
            /// is built from.
            pub overflows: u64,
            accumulator: u64,
            direction: Direction,
            output_a: bool,
            output_b: bool,
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    tccr_a: 0,
                    tccr_b: 0,
                    count: 0,
                    ocr_a: 0,
                    ocr_b: 0,
                    timsk: 0,
                    tifr: 0,
                    overflows: 0,
                    accumulator: 0,
                    direction: Direction::Up,
                    output_a: false,
                    output_b: false,
                }
            }
        }

        impl $name {
            pub(crate) fn prescaler(&self) -> u32 {
                $divisor(self.tccr_b)
            }

            pub(crate) fn running(&self) -> bool {
                self.prescaler() != 0
            }

            /// `WGM`: `TCCRxA[1:0]` plus `WGM02` in `TCCRxB` bit 3.
            fn wgm(&self) -> u8 {
                (self.tccr_a & 0x03) | ((self.tccr_b & 0x08) >> 1)
            }

            fn mode(&self) -> Mode {
                match self.wgm() {
                    0 => Mode::Normal,
                    1 | 5 => Mode::Phase,
                    2 => Mode::Ctc,
                    3 | 7 => Mode::Fast,
                    _ => Mode::Normal,
                }
            }

            fn top(&self) -> u8 {
                match self.wgm() {
                    2 | 5 | 7 => self.ocr_a,
                    _ => 0xff,
                }
            }

            fn compare_a(&mut self) {
                if self.count != self.ocr_a {
                    return;
                }
                self.tifr |= 0x02; // OCFxA
                match (self.tccr_a >> 6) & 0x03 {
                    1 => self.output_a = !self.output_a,
                    2 => self.output_a = false,
                    3 => self.output_a = true,
                    _ => {}
                }
            }

            fn compare_b(&mut self) {
                if self.count != self.ocr_b {
                    return;
                }
                self.tifr |= 0x04; // OCFxB
                match (self.tccr_a >> 4) & 0x03 {
                    1 => self.output_b = !self.output_b,
                    2 => self.output_b = false,
                    3 => self.output_b = true,
                    _ => {}
                }
            }

            pub(crate) fn advance(&mut self, cycles: u64) {
                let divisor = u64::from(self.prescaler());
                if divisor == 0 {
                    return;
                }
                self.accumulator = self.accumulator.saturating_add(cycles);
                let mut ticks = self.accumulator / divisor;
                self.accumulator %= divisor;
                // A single instruction retires a handful of cycles, so this
                // loop is short. The cap only protects a caller that advances
                // the clock by an absurd amount in one call.
                if ticks > 1_000_000 {
                    ticks = 1_000_000;
                }
                for _ in 0..ticks {
                    self.tick();
                }
            }

            fn tick(&mut self) {
                let mode = self.mode();
                let top = self.top();
                match mode {
                    Mode::Normal => {
                        self.compare_a();
                        self.compare_b();
                        if self.count == 0xff {
                            self.count = 0;
                            self.overflows = self.overflows.saturating_add(1);
                            self.tifr |= 0x01; // TOV
                        } else {
                            self.count = self.count.wrapping_add(1);
                        }
                    }
                    Mode::Ctc => {
                        self.compare_a();
                        self.compare_b();
                        if self.count == 0xff {
                            self.count = 0;
                            self.overflows = self.overflows.saturating_add(1);
                            self.tifr |= 0x01;
                        } else if self.count >= top {
                            self.count = 0;
                        } else {
                            self.count = self.count.wrapping_add(1);
                        }
                    }
                    Mode::Fast => {
                        self.compare_a();
                        self.compare_b();
                        if self.count >= top {
                            self.count = 0;
                            self.overflows = self.overflows.saturating_add(1);
                            self.tifr |= 0x01;
                        } else {
                            self.count = self.count.wrapping_add(1);
                        }
                    }
                    Mode::Phase => {
                        self.compare_a();
                        self.compare_b();
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
                                    self.tifr |= 0x01;
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

            fn oc_level(&self, com: u8, ocr: u8, output: bool) -> Option<bool> {
                if com == 0 {
                    return None;
                }
                match self.mode() {
                    Mode::Normal | Mode::Ctc => Some(output),
                    Mode::Fast => {
                        if com == 1 {
                            return Some(output); // toggle on compare match
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

            /// Level the timer wants on `OCxA`, or `None` when the compare
            /// output is disconnected (`COMA = 0`).
            pub fn oc_a_level(&self) -> Option<bool> {
                self.oc_level((self.tccr_a >> 6) & 0x03, self.ocr_a, self.output_a)
            }

            /// Level the timer wants on `OCxB`.
            pub fn oc_b_level(&self) -> Option<bool> {
                self.oc_level((self.tccr_a >> 4) & 0x03, self.ocr_b, self.output_b)
            }

            /// The high-time fraction `OCxA` is programmed to, or `None` when
            /// the compare output is disconnected or the timer is stopped. A
            /// toggle-mode output spends half its time high; an inverted one
            /// (COM = 3) spends the complement.
            pub fn oc_a_duty(&self) -> Option<f64> {
                self.oc_duty((self.tccr_a >> 6) & 0x03, self.ocr_a)
            }

            /// The high-time fraction `OCxB` is programmed to.
            pub fn oc_b_duty(&self) -> Option<f64> {
                self.oc_duty((self.tccr_a >> 4) & 0x03, self.ocr_b)
            }

            fn oc_duty(&self, com: u8, ocr: u8) -> Option<f64> {
                if com == 0 || !self.running() {
                    return None;
                }
                let duty = match self.mode() {
                    Mode::Normal | Mode::Ctc => 0.5,
                    Mode::Fast | Mode::Phase => {
                        let period = f64::from(self.top()) + 1.0;
                        (f64::from(ocr) / period).clamp(0.0, 1.0)
                    }
                };
                Some(if com == 3 { 1.0 - duty } else { duty })
            }
        }
    };
}

eight_bit_timer!(
    /// Timer/counter 0: the 8-bit timer Arduino uses for `millis()` and for
    /// `analogWrite` on pins 5 (`OC0B`) and 6 (`OC0A`).
    Timer0,
    prescale_divisor
);

eight_bit_timer!(
    /// Timer/counter 2: the second 8-bit timer, driving `analogWrite` pins 3
    /// (`OC2B`) and 11 (`OC2A`).
    Timer2,
    timer2_divisor
);

/// Timer/counter 1: the 16-bit timer, including all sixteen waveform modes
/// and input capture.
pub struct Timer1 {
    pub tccr_a: u8,
    pub tccr_b: u8,
    pub tccr_c: u8,
    pub count: u16,
    pub icr: u16,
    pub ocr_a: u16,
    pub ocr_b: u16,
    pub timsk: u8,
    /// `TIFR1`: bit 0 `TOV1`, bit 1 `OCF1A`, bit 2 `OCF1B`, bit 5 `ICF1`.
    pub tifr: u8,
    /// Overflow events since reset.
    pub overflows: u64,
    accumulator: u64,
    direction: Direction,
    output_a: bool,
    output_b: bool,
}

impl Default for Timer1 {
    fn default() -> Self {
        Self {
            tccr_a: 0,
            tccr_b: 0,
            tccr_c: 0,
            count: 0,
            icr: 0,
            ocr_a: 0,
            ocr_b: 0,
            timsk: 0,
            tifr: 0,
            overflows: 0,
            accumulator: 0,
            direction: Direction::Up,
            output_a: false,
            output_b: false,
        }
    }
}

impl Timer1 {
    pub(crate) fn prescaler(&self) -> u32 {
        prescale_divisor(self.tccr_b)
    }

    pub(crate) fn running(&self) -> bool {
        self.prescaler() != 0
    }

    fn wgm(&self) -> u8 {
        (self.tccr_a & 0x03) | ((self.tccr_b >> 1) & 0x0c)
    }

    fn mode(&self) -> Mode {
        match self.wgm() {
            0 | 13 => Mode::Normal,
            1 | 2 | 3 | 8 | 9 | 10 | 11 => Mode::Phase,
            4 | 12 => Mode::Ctc,
            _ => Mode::Fast,
        }
    }

    fn top(&self) -> u16 {
        match self.wgm() {
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
        }
    }

    fn compare_a(&mut self) {
        if self.count != self.ocr_a {
            return;
        }
        self.tifr |= 0x02;
        match (self.tccr_a >> 6) & 0x03 {
            1 => self.output_a = !self.output_a,
            2 => self.output_a = false,
            3 => self.output_a = true,
            _ => {}
        }
    }

    fn compare_b(&mut self) {
        if self.count != self.ocr_b {
            return;
        }
        self.tifr |= 0x04;
        match (self.tccr_a >> 4) & 0x03 {
            1 => self.output_b = !self.output_b,
            2 => self.output_b = false,
            3 => self.output_b = true,
            _ => {}
        }
    }

    pub(crate) fn advance(&mut self, cycles: u64) {
        let divisor = u64::from(self.prescaler());
        if divisor == 0 {
            return;
        }
        self.accumulator = self.accumulator.saturating_add(cycles);
        let mut ticks = self.accumulator / divisor;
        self.accumulator %= divisor;
        if ticks > 1_000_000 {
            ticks = 1_000_000;
        }
        for _ in 0..ticks {
            self.tick();
        }
    }

    fn tick(&mut self) {
        let mode = self.mode();
        let top = self.top();
        match mode {
            Mode::Normal => {
                self.compare_a();
                self.compare_b();
                if self.count == 0xffff {
                    self.count = 0;
                    self.overflows = self.overflows.saturating_add(1);
                    self.tifr |= 0x01;
                } else {
                    self.count = self.count.wrapping_add(1);
                }
            }
            Mode::Ctc => {
                self.compare_a();
                self.compare_b();
                if self.count == 0xffff {
                    self.count = 0;
                    self.overflows = self.overflows.saturating_add(1);
                    self.tifr |= 0x01;
                } else if self.count >= top {
                    self.count = 0;
                } else {
                    self.count = self.count.wrapping_add(1);
                }
            }
            Mode::Fast => {
                self.compare_a();
                self.compare_b();
                if self.count >= top {
                    self.count = 0;
                    self.overflows = self.overflows.saturating_add(1);
                    self.tifr |= 0x01;
                } else {
                    self.count = self.count.wrapping_add(1);
                }
            }
            Mode::Phase => {
                self.compare_a();
                self.compare_b();
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
                            self.tifr |= 0x01;
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

    /// Latch the counter into `ICR1` and raise `ICF1`.
    pub(crate) fn capture(&mut self) {
        self.icr = self.count;
        self.tifr |= 0x20;
    }

    fn oc_level(&self, com: u8, ocr: u16, output: bool) -> Option<bool> {
        if com == 0 {
            return None;
        }
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

    pub fn oc_a_level(&self) -> Option<bool> {
        self.oc_level((self.tccr_a >> 6) & 0x03, self.ocr_a, self.output_a)
    }

    pub fn oc_b_level(&self) -> Option<bool> {
        self.oc_level((self.tccr_a >> 4) & 0x03, self.ocr_b, self.output_b)
    }

    /// The high-time fraction `OC1A` is programmed to, or `None` when the
    /// compare output is disconnected or the timer is stopped.
    pub fn oc_a_duty(&self) -> Option<f64> {
        self.oc_duty((self.tccr_a >> 6) & 0x03, self.ocr_a)
    }

    /// The high-time fraction `OC1B` is programmed to.
    pub fn oc_b_duty(&self) -> Option<f64> {
        self.oc_duty((self.tccr_a >> 4) & 0x03, self.ocr_b)
    }

    fn oc_duty(&self, com: u8, ocr: u16) -> Option<f64> {
        if com == 0 || !self.running() {
            return None;
        }
        let duty = match self.mode() {
            Mode::Normal | Mode::Ctc => 0.5,
            Mode::Fast | Mode::Phase => {
                let period = f64::from(self.top()) + 1.0;
                (f64::from(ocr) / period).clamp(0.0, 1.0)
            }
        };
        Some(if com == 3 { 1.0 - duty } else { duty })
    }
}
