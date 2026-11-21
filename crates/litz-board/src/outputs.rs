//! Output parts with a little state of their own: a WS2812 addressable LED and
//! a bipolar stepper motor.
//!
//! Both watch pin edges rather than a bus. The NeoPixel decodes the 800 kHz
//! pulse-width protocol on one data line and latches a frame after the reset
//! gap; the stepper reads the four coil drives and counts half-steps from the
//! coil phase it observes. The host can also turn either one directly through
//! [`Part::set_control`], which is what a UI knob does.

use crate::BuildError;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};

fn index(part: &FlatPart, name: &str) -> Result<u16, BuildError> {
    part.pins
        .iter()
        .position(|candidate| candidate == name)
        .map(|i| i as u16)
        .ok_or_else(|| BuildError::Message(format!("part {:?} has no {name:?} pin", part.id)))
}

fn attr_f64(attrs: &std::collections::BTreeMap<String, String>, name: &str, default: f64) -> f64 {
    attrs
        .get(name)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(default)
}

// ------------------------------------------------------------------ NeoPixel

/// A WS2812 bit is a one when its high time is at least this long.
///
/// The datasheet puts T0H near 0.4 us and T1H near 0.8 us; 0.6 us sits between
/// them with margin for a firmware that does not hit either exactly.
const NEOPIXEL_ONE_NS: Tick = 600;
/// A low period longer than this latches the frame.
const NEOPIXEL_RESET_NS: Tick = 50_000;

/// A chain of WS2812 pixels, GRB order, driven by one data line.
///
/// Each part owns `pixel_count` pixels. Bits arrive most-significant first as
/// green, red, blue; after its own pixels the part forwards the remaining bits
/// on `DOUT`, so a chain of parts behaves like a chain of LEDs. A low gap of
/// 50 us latches the frame, which is what [`Part::next_event`] schedules.
pub struct NeoPixel {
    din: u16,
    dout: u16,
    pixel_count: usize,
    /// What the last latched frame shows.
    colors: Vec<(u8, u8, u8)>,
    /// The frame being shifted in.
    staging: Vec<(u8, u8, u8)>,
    pixel_index: usize,
    word: u32,
    bits: u8,
    pass_through: bool,
    din_level: Level,
    dout_level: Level,
    rise_at: Option<Tick>,
    reset_at: Option<Tick>,
}

impl NeoPixel {
    pub fn with_pixels(count: usize) -> Self {
        let count = count.max(1);
        Self {
            din: 0,
            dout: 1,
            pixel_count: count,
            colors: vec![(0, 0, 0); count],
            staging: vec![(0, 0, 0); count],
            pixel_index: 0,
            word: 0,
            bits: 0,
            pass_through: false,
            din_level: Level::Low,
            dout_level: Level::Low,
            rise_at: None,
            reset_at: None,
        }
    }

    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let din = index(part, "DIN")?;
        let dout = index(part, "DOUT")?;
        let count = if matches!(
            part.type_key.as_str(),
            "litz-neopixel-matrix" | "litz-led-matrix"
        ) {
            let rows = attr_f64(&part.attrs, "rows", 8.0).max(1.0) as usize;
            let cols = attr_f64(&part.attrs, "cols", 8.0).max(1.0) as usize;
            rows * cols
        } else {
            attr_f64(&part.attrs, "pixels", 1.0).max(1.0) as usize
        };
        Ok(Box::new(Self {
            din,
            dout,
            ..Self::with_pixels(count)
        }))
    }

    /// The latched colours as `(r, g, b)` in chain order.
    pub fn colors(&self) -> &[(u8, u8, u8)] {
        &self.colors
    }

    pub fn pixel_count(&self) -> usize {
        self.pixel_count
    }

    fn bit(&mut self, one: bool) {
        if self.pixel_index >= self.pixel_count {
            // The chain is past this part; remaining bits leave on DOUT.
            self.pass_through = true;
            return;
        }
        self.word = (self.word << 1) | u32::from(one);
        self.bits += 1;
        if self.bits == 24 {
            let green = ((self.word >> 16) & 0xff) as u8;
            let red = ((self.word >> 8) & 0xff) as u8;
            let blue = (self.word & 0xff) as u8;
            self.staging[self.pixel_index] = (red, green, blue);
            self.pixel_index += 1;
            self.word = 0;
            self.bits = 0;
            if self.pixel_index == self.pixel_count {
                self.pass_through = true;
            }
        }
    }

    fn reset(&mut self) {
        self.colors.clone_from(&self.staging);
        self.pixel_index = 0;
        self.word = 0;
        self.bits = 0;
        self.pass_through = false;
        self.dout_level = Level::Low;
        self.reset_at = None;
        self.rise_at = None;
    }
}

impl Part for NeoPixel {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.dout {
            match self.dout_level {
                Level::HighZ => Drive::HIGH_Z,
                level => Drive::strong(level),
            }
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        if pin.0 != self.din {
            return;
        }
        self.din_level = level;
        if level == Level::High {
            self.rise_at = Some(now);
            if self.pass_through {
                self.dout_level = Level::High;
            }
        } else {
            if let Some(start) = self.rise_at.take() {
                self.bit(now.saturating_sub(start) >= NEOPIXEL_ONE_NS);
            }
            if self.pass_through {
                self.dout_level = Level::Low;
            }
            // A low gap of 50 us latches the frame; every edge pushes the
            // deadline out, so a continuous stream never latches early.
            self.reset_at = Some(now + NEOPIXEL_RESET_NS);
        }
    }

    fn next_event(&self) -> Option<Tick> {
        self.reset_at
    }

    fn advance(&mut self, now: Tick) {
        if let Some(at) = self.reset_at
            && now >= at
        {
            self.reset();
        }
    }

    fn state(&self) -> PartState {
        PartState::Led {
            lit: self.colors.iter().any(|color| *color != (0, 0, 0)),
        }
    }
}

// ------------------------------------------------------------------ stepper

/// Coil phase in half-step order: `(A, B)` where +1 is A+/B+ high, -1 is
/// A-/B- high and 0 leaves the coil unenergised.
const PHASES: [(i8, i8); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

/// A bipolar stepper motor with four coil terminals.
///
/// A firmware drives the coils through an H-bridge, so the model reads the
/// polarity of each coil (`A+` versus `A-`, `B+` versus `B-`) and walks the
/// eight half-step phases. A phase change of two (a full step) advances two
/// half-steps, which is 1.8 degrees; the model reports the same half-step
/// resolution the real part does. A host control asks for a target position, and the
/// motor then steps there on the simulated clock.
pub struct Stepper {
    plus_a: u16,
    minus_a: u16,
    plus_b: u16,
    minus_b: u16,
    levels: [Level; 4],
    steps: i64,
    last_phase: Option<usize>,
    host_driven: bool,
    target: i64,
    next_at: Option<Tick>,
    gear_ratio: f64,
}

/// Half-step period when a host asks the motor to move: 400 half-steps per
/// revolution at this rate is about 1.25 revolutions per second.
const HOST_STEP_NS: Tick = 2_500_000;

impl Stepper {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let ratio = part
            .attrs
            .get("gearRatio")
            .and_then(|text| parse_ratio(text))
            .unwrap_or(1.0);
        Ok(Box::new(Self {
            plus_a: index(part, "A+")?,
            minus_a: index(part, "A-")?,
            plus_b: index(part, "B+")?,
            minus_b: index(part, "B-")?,
            levels: [Level::HighZ; 4],
            steps: 0,
            last_phase: None,
            host_driven: false,
            target: 0,
            next_at: None,
            gear_ratio: ratio.max(0.001),
        }))
    }

    /// Half-steps turned since reset, positive in the forward phase order.
    pub fn steps(&self) -> i64 {
        self.steps
    }

    /// Shaft angle in degrees, including the gear ratio.
    pub fn angle(&self) -> f64 {
        self.steps as f64 * 0.9 / self.gear_ratio
    }

    pub fn phase(&self) -> Option<usize> {
        self.last_phase
    }

    fn coil_state(level: Level) -> i8 {
        match level {
            Level::High => 1,
            Level::Low => -1,
            Level::HighZ => 0,
        }
    }

    fn observed_phase(&self) -> Option<usize> {
        let a = Self::coil_state(self.levels[0]) - Self::coil_state(self.levels[1]);
        let b = Self::coil_state(self.levels[2]) - Self::coil_state(self.levels[3]);
        let vector = (a.signum(), b.signum());
        PHASES.iter().position(|phase| *phase == vector)
    }

    fn set_phase_outputs(&mut self, phase: usize) {
        let (a, b) = PHASES[phase % PHASES.len()];
        self.levels = [
            if a >= 0 { Level::High } else { Level::Low },
            if a <= 0 { Level::High } else { Level::Low },
            if b >= 0 { Level::High } else { Level::Low },
            if b <= 0 { Level::High } else { Level::Low },
        ];
    }
}

fn parse_ratio(text: &str) -> Option<f64> {
    let (left, right) = text.trim().split_once(':')?;
    let left: f64 = left.trim().parse().ok()?;
    let right: f64 = right.trim().parse().ok()?;
    (right != 0.0).then_some(left / right)
}

impl Part for Stepper {
    fn drive(&self, pin: PinId) -> Drive {
        if !self.host_driven {
            return Drive::HIGH_Z;
        }
        let slot = if pin.0 == self.plus_a {
            Some(0)
        } else if pin.0 == self.minus_a {
            Some(1)
        } else if pin.0 == self.plus_b {
            Some(2)
        } else if pin.0 == self.minus_b {
            Some(3)
        } else {
            None
        };
        match slot {
            Some(slot) => match self.levels[slot] {
                Level::HighZ => Drive::HIGH_Z,
                level => Drive::strong(level),
            },
            None => Drive::HIGH_Z,
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        let slot = if pin.0 == self.plus_a {
            Some(0)
        } else if pin.0 == self.minus_a {
            Some(1)
        } else if pin.0 == self.plus_b {
            Some(2)
        } else if pin.0 == self.minus_b {
            Some(3)
        } else {
            None
        };
        let Some(slot) = slot else { return };
        self.levels[slot] = level;
        if self.host_driven {
            return;
        }
        if let Some(phase) = self.observed_phase()
            && self.last_phase != Some(phase)
        {
            if let Some(previous) = self.last_phase {
                let delta = (phase + PHASES.len() - previous) % PHASES.len();
                if delta != 0 {
                    self.steps += if delta <= PHASES.len() / 2 {
                        delta as i64
                    } else {
                        -((PHASES.len() - delta) as i64)
                    };
                }
            }
            self.last_phase = Some(phase);
        }
    }

    fn next_event(&self) -> Option<Tick> {
        if self.host_driven && self.steps != self.target {
            self.next_at
        } else {
            None
        }
    }

    fn advance(&mut self, now: Tick) {
        let mut guard = 0;
        while self.host_driven && self.steps != self.target {
            let Some(at) = self.next_at else { break };
            if at > now {
                break;
            }
            self.steps += (self.target - self.steps).signum();
            let phase = self.steps.rem_euclid(PHASES.len() as i64) as usize;
            self.last_phase = Some(phase);
            self.set_phase_outputs(phase);
            self.next_at = Some(at + HOST_STEP_NS);
            guard += 1;
            if guard > 100_000 {
                break;
            }
        }
        if self.steps == self.target {
            self.next_at = None;
        }
    }

    fn state(&self) -> PartState {
        PartState::Knob {
            permille: (self.steps.rem_euclid(400) * 1000 / 400) as u16,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.host_driven = true;
        self.target = i64::from(value.min(1000)) * 400 / 1000;
        if self.steps != self.target && self.next_at.is_none() {
            self.next_at = Some(0);
        }
        true
    }
}

impl Default for Stepper {
    fn default() -> Self {
        Self {
            plus_a: 0,
            minus_a: 1,
            plus_b: 2,
            minus_b: 3,
            levels: [Level::HighZ; 4],
            steps: 0,
            last_phase: None,
            host_driven: false,
            target: 0,
            next_at: None,
            gear_ratio: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use litz_core::TICKS_PER_SEC;

    fn pin(id: u16) -> PinId {
        PinId(id)
    }

    fn send(pixel: &mut NeoPixel, bits: &[bool], at: &mut Tick) {
        for &one in bits {
            pixel.set_pin(pin(0), Level::High, *at);
            *at += if one { 800 } else { 350 };
            pixel.set_pin(pin(0), Level::Low, *at);
            *at += 600;
        }
    }

    fn grb(r: u8, g: u8, b: u8) -> Vec<bool> {
        let word = u32::from(g) << 16 | u32::from(r) << 8 | u32::from(b);
        (0..24).map(|bit| word & (1 << (23 - bit)) != 0).collect()
    }

    #[test]
    fn a_neopixel_latches_grb_after_a_reset_gap() {
        let mut pixel = NeoPixel::with_pixels(1);
        let mut at = 100;
        send(&mut pixel, &grb(0x12, 0x34, 0x56), &mut at);
        assert_eq!(pixel.colors(), &[(0, 0, 0)], "not latched before the gap");
        let reset = pixel.next_event().expect("a reset edge is scheduled");
        pixel.advance(reset);
        assert_eq!(pixel.colors(), &[(0x12, 0x34, 0x56)]);
        assert_eq!(pixel.state(), PartState::Led { lit: true });
    }

    #[test]
    fn a_neopixel_forwards_bits_after_its_own_pixel() {
        let mut pixel = NeoPixel::with_pixels(1);
        let mut at = 0;
        let bits = grb(10, 20, 30);
        send(&mut pixel, &bits, &mut at);
        // The next bit belongs to the next part in the chain.
        pixel.set_pin(pin(0), Level::High, at);
        assert_eq!(pixel.drive(pin(1)), Drive::strong(Level::High));
    }

    #[test]
    fn a_stepper_counts_half_steps_from_coil_phase() {
        let mut motor = Stepper::default();
        // Full-step sequence: phase 0, 2, 4, 6 is two phases per step.
        let sequence = [
            (true, false, false, false),
            (false, false, true, false),
            (false, true, false, false),
            (false, false, false, true),
        ];
        for (a_plus, a_minus, b_plus, b_minus) in sequence {
            motor.set_pin(pin(0), if a_plus { Level::High } else { Level::Low }, 0);
            motor.set_pin(pin(1), if a_minus { Level::High } else { Level::Low }, 0);
            motor.set_pin(pin(2), if b_plus { Level::High } else { Level::Low }, 0);
            motor.set_pin(pin(3), if b_minus { Level::High } else { Level::Low }, 0);
        }
        assert_eq!(motor.steps(), 6);
        assert!((motor.angle() - 5.4).abs() < 0.001);
    }

    #[test]
    fn a_host_control_steps_the_motor_on_the_clock() {
        let mut motor = Stepper::default();
        assert!(motor.set_control(500));
        assert_eq!(motor.target, 200);
        assert_eq!(motor.next_event(), Some(0));
        motor.advance(0);
        assert_eq!(motor.steps(), 1);
        motor.advance(10 * TICKS_PER_SEC);
        assert_eq!(motor.steps(), 200);
        assert_eq!(motor.next_event(), None);
        assert!((motor.angle() - 180.0).abs() < 0.001);
        // The model drives the coils while the host turns the shaft.
        assert_eq!(motor.drive(pin(0)), Drive::strong(Level::High));
    }

    #[test]
    fn a_gear_ratio_scales_the_angle() {
        let mut motor = Stepper {
            gear_ratio: 2.0,
            ..Stepper::default()
        };
        motor.steps = 400;
        assert!((motor.angle() - 180.0).abs() < 0.001);
    }
}
