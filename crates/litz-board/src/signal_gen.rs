//! A free-running signal generator: the source a circuit needs when no
//! firmware is driving it.
//!
//! One output pin, an optional ground pin, and a frequency the simulation
//! honours in ticks rather than wall-clock time. The engine is the only
//! non-firmware driver in the catalog: its edges are scheduled like any other
//! part's, so a run stays deterministic and a waveform does not depend on how
//! fast the host executed it.
//!
//! Digital nets see the thresholded wave — high while the instantaneous value
//! is above the mid-point — and the analog view sees the wave itself through
//! [`Part::analog`](litz_core::Part::analog), which the board's netlist reads
//! at each solve. Square waves are scheduled edge by edge; the other shapes
//! step at a fixed resolution so a scope has points to draw.

use crate::BuildError;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PinId, TICKS_PER_SEC, Tick};

/// How many steps one period of a non-square wave is divided into. Enough for
/// a readable sine or triangle at scope resolution, few enough that a long run
/// does not turn into millions of settles.
const STEPS_PER_PERIOD: u32 = 64;

/// The fastest rate the engine accepts. Above this a period would be fewer
/// ticks than the scheduler can usefully resolve, and a fast square would only
/// flood the event queue.
const MAX_HZ: f64 = 1_000_000.0;

/// The slowest rate the engine accepts, one cycle per second.
const MIN_HZ: f64 = 1.0;

/// The waveform an instance produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wave {
    Square,
    Sine,
    Triangle,
    Sawtooth,
}

/// One generator instance.
pub struct SignalGenerator {
    out: u16,
    wave: Wave,
    /// Period in ticks.
    period: Tick,
    /// Fraction of the period spent high, in thousandths.
    duty: u16,
    /// Peak-to-peak volts.
    amplitude: f32,
    /// Mid-point volts.
    offset: f32,
    /// Normalized phase, 0 through 1, at the last scheduled event.
    phase: f64,
    /// When the next scheduled step is due.
    next_at: Option<Tick>,
    /// Last driven level, so `drive` answers without recomputing the phase.
    level: Level,
    /// The tick offset that places the starting phase on the edge grid.
    offset_ticks: Tick,
    /// Ticks between adjacent steps of a non-square wave.
    step_ticks: Tick,
}

/// Build the engine under its catalog name.
pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
    SignalGenerator::build(part)
}

impl SignalGenerator {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let out = pin_index(part, "OUT")?;
        // The reference pin is part of the contract even though the engine
        // never drives it; reject a manifest that forgot it at build time.
        pin_index(part, "GND")?;

        let wave = match part.attrs.get("wave").map(String::as_str) {
            Some(text) if text.eq_ignore_ascii_case("sine") => Wave::Sine,
            Some(text) if text.eq_ignore_ascii_case("triangle") => Wave::Triangle,
            Some(text) if text.eq_ignore_ascii_case("sawtooth") => Wave::Sawtooth,
            _ => Wave::Square,
        };
        let hz = part
            .attrs
            .get("frequency")
            .and_then(|text| parse_hz(text))
            .unwrap_or(1_000.0);
        let period = period_for(hz);
        let duty = part
            .attrs
            .get("duty")
            .and_then(|text| text.trim().parse::<f64>().ok())
            .unwrap_or(50.0)
            .clamp(1.0, 99.0);
        let duty = (duty * 10.0).round().clamp(10.0, 990.0) as u16;
        let amplitude = part
            .attrs
            .get("amplitude")
            .and_then(|text| text.trim().parse::<f32>().ok())
            .unwrap_or(3.3)
            .max(0.0);
        // A generator whose offset is not written swings 0..amplitude.
        let offset = part
            .attrs
            .get("offset")
            .and_then(|text| text.trim().parse::<f32>().ok())
            .unwrap_or(amplitude / 2.0);
        let phase = part
            .attrs
            .get("phase")
            .and_then(|text| text.trim().parse::<f64>().ok())
            .unwrap_or(0.0)
            .clamp(0.0, 360.0)
            / 360.0;
        let phase = phase.rem_euclid(1.0);

        let mut generator = Self {
            out,
            wave,
            period,
            duty,
            amplitude,
            offset,
            phase,
            next_at: None,
            level: Level::High,
            offset_ticks: (phase * period as f64).round() as Tick % period.max(1),
            step_ticks: (period / Tick::from(STEPS_PER_PERIOD)).max(1),
        };
        generator.start();
        Ok(Box::new(generator))
    }

    /// Put the instance on its starting phase and schedule the first event.
    fn start(&mut self) {
        match self.wave {
            Wave::Square => self.advance_square(0),
            _ => {
                self.level = self.level_for_phase();
                self.next_at = Some(self.step_ticks);
            }
        }
    }

    /// The number of ticks the square is high in one period.
    fn high_ticks(&self) -> Tick {
        let high = (u128::from(self.period) * u128::from(self.duty) / 1000) as Tick;
        high.clamp(1, self.period.saturating_sub(1).max(1))
    }

    fn duty_fraction(&self) -> f64 {
        f64::from(self.duty) / 1000.0
    }

    /// The waveform value at `phase`, normalized to -1 through 1.
    fn shape(&self, phase: f64) -> f64 {
        match self.wave {
            Wave::Square => {
                if phase < self.duty_fraction() {
                    1.0
                } else {
                    -1.0
                }
            }
            // The sine peaks at phase 0 so it starts at the top of its swing,
            // like the square starts high.
            Wave::Sine => (core::f64::consts::TAU * phase).cos(),
            Wave::Triangle => 4.0 * (phase - 0.5).abs() - 1.0,
            Wave::Sawtooth => 2.0 * phase - 1.0,
        }
    }

    /// The digital level a stepped wave presents at its current phase.
    fn level_for_phase(&self) -> Level {
        if self.shape(self.phase) >= 0.0 {
            Level::High
        } else {
            Level::Low
        }
    }

    /// Recompute the square's level and next edge straight from `now`.
    ///
    /// Anchoring to the tick grid instead of walking edges makes a late
    /// wake-up cost the same as an on-time one, and keeps every rise exactly
    /// one period after the previous one.
    fn advance_square(&mut self, now: Tick) {
        let period = self.period.max(1);
        let high = self.high_ticks();
        let pos = (now + self.offset_ticks) % period;
        self.level = if pos < high { Level::High } else { Level::Low };
        self.phase = pos as f64 / period as f64;
        let delta = if pos < high { high - pos } else { period - pos };
        self.next_at = Some(now + delta.max(1));
    }

    /// Advance a stepped wave to `now`, collapsing any gap into one jump.
    fn advance_steps(&mut self, now: Tick) {
        let Some(at) = self.next_at else { return };
        if now < at {
            return;
        }
        let step = self.step_ticks.max(1);
        // The number of step boundaries reached, including the one at `at`.
        let steps = (now - at) / step + 1;
        // Only the phase within the period matters, so a pause of any length
        // costs one modular increment rather than a loop over every step.
        let within_period = steps % u64::from(STEPS_PER_PERIOD);
        self.phase =
            (self.phase + within_period as f64 / f64::from(STEPS_PER_PERIOD)).rem_euclid(1.0);
        self.level = self.level_for_phase();
        let last = at + (steps - 1) * step;
        self.next_at = Some(last.saturating_add(step));
    }
}

impl Part for SignalGenerator {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.out {
            Drive::strong(self.level)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next_at
    }

    fn advance(&mut self, now: Tick) {
        if self.next_at.is_some_and(|at| now < at) {
            return;
        }
        match self.wave {
            Wave::Square => self.advance_square(now),
            _ => self.advance_steps(now),
        }
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        if pin.0 != self.out {
            return None;
        }
        let half = self.amplitude / 2.0;
        Some(self.offset + half * self.shape(self.phase) as f32)
    }
}

/// Find a pin's index by name, as the engine's `PinId` addresses it.
fn pin_index(part: &FlatPart, name: &str) -> Result<u16, BuildError> {
    part.pins
        .iter()
        .position(|candidate| candidate == name)
        .map(|index| index as u16)
        .ok_or_else(|| BuildError::Message(format!("part {:?} has no {name:?} pin", part.id)))
}

/// Parse a frequency as a diagram writes it: a decimal with an optional
/// `k`/`K`/`m`/`M` multiplier and an optional `hz` suffix. As in the clock
/// generator, `m` means megahertz.
fn parse_hz(text: &str) -> Option<f64> {
    let cleaned = text.trim().replace([' ', '_'], "").to_ascii_lowercase();
    let text = cleaned.strip_suffix("hz").unwrap_or(&cleaned);
    let (number, scale) = match text.chars().last() {
        Some('k') => (&text[..text.len() - 1], 1e3),
        Some('m') => (&text[..text.len() - 1], 1e6),
        _ => (text, 1.0),
    };
    let value = number.trim().parse::<f64>().ok()? * scale;
    (value > 0.0).then_some(value)
}

/// A frequency in hertz as a period in ticks, clamped to the engine's range.
fn period_for(hz: f64) -> Tick {
    let hz = hz.clamp(MIN_HZ, MAX_HZ);
    (TICKS_PER_SEC as f64 / hz).round().max(1.0) as Tick
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flatten::Engine;
    use std::collections::BTreeMap;

    /// Build an instance directly, with `duty` already in thousandths.
    fn generator(wave: Wave, hz: f64, duty: u16, amplitude: f32, offset: f32) -> SignalGenerator {
        let period = period_for(hz);
        let phase = 0.0;
        let mut generator = SignalGenerator {
            out: 0,
            wave,
            period,
            duty,
            amplitude,
            offset,
            phase,
            next_at: None,
            level: Level::High,
            offset_ticks: 0,
            step_ticks: (period / Tick::from(STEPS_PER_PERIOD)).max(1),
        };
        generator.start();
        generator
    }

    fn level(generator: &SignalGenerator) -> Level {
        generator.drive(PinId(generator.out)).level
    }

    #[test]
    fn a_kilohertz_square_has_a_millisecond_period() {
        let mut square = generator(Wave::Square, 1_000.0, 500, 3.3, 1.65);
        assert_eq!(square.period, 1_000_000);
        assert_eq!(level(&square), Level::High);
        assert_eq!(square.next_event(), Some(500_000));

        square.advance(500_000);
        assert_eq!(level(&square), Level::Low);
        assert_eq!(square.next_event(), Some(1_000_000));

        square.advance(1_000_000);
        assert_eq!(level(&square), Level::High);
    }

    #[test]
    fn a_quarter_duty_square_is_high_for_a_quarter_period() {
        let mut square = generator(Wave::Square, 1_000.0, 250, 3.3, 1.65);
        assert_eq!(square.next_event(), Some(250_000));
        square.advance(249_999);
        assert_eq!(level(&square), Level::High);
        square.advance(250_000);
        assert_eq!(level(&square), Level::Low);
        assert_eq!(square.next_event(), Some(1_000_000));
        square.advance(1_000_000);
        assert_eq!(level(&square), Level::High);
    }

    #[test]
    fn a_sine_swings_from_top_through_offset_to_bottom() {
        let mut sine = generator(Wave::Sine, 1_000.0, 500, 3.3, 1.65);
        let top = sine.analog(PinId(0)).unwrap();
        assert!((top - 3.3).abs() < 1e-4, "top of swing is {top}");

        sine.advance(250_000);
        let middle = sine.analog(PinId(0)).unwrap();
        assert!((middle - 1.65).abs() < 1e-3, "quarter period is {middle}");

        sine.advance(500_000);
        let bottom = sine.analog(PinId(0)).unwrap();
        assert!((bottom - 0.0).abs() < 1e-4, "bottom of swing is {bottom}");
    }

    #[test]
    fn triangle_and_sawtooth_reach_their_extremes_within_one_period() {
        let mut triangle = generator(Wave::Triangle, 1_000.0, 500, 2.0, 1.0);
        let peak = triangle.analog(PinId(0)).unwrap();
        assert!((peak - 2.0).abs() < 1e-4, "triangle starts at {peak}");
        triangle.advance(500_000);
        let trough = triangle.analog(PinId(0)).unwrap();
        assert!((trough - 0.0).abs() < 1e-4, "triangle half is {trough}");

        let mut saw = generator(Wave::Sawtooth, 1_000.0, 500, 2.0, 1.0);
        let trough = saw.analog(PinId(0)).unwrap();
        assert!((trough - 0.0).abs() < 1e-4, "sawtooth starts at {trough}");
        saw.advance(63 * saw.step_ticks);
        let peak = saw.analog(PinId(0)).unwrap();
        assert!(peak > 1.9, "sawtooth peak reached only {peak}");
    }

    #[test]
    fn frequency_text_parses_to_tick_periods() {
        assert_eq!(period_for(parse_hz("2.5Hz").unwrap()), 400_000_000);
        assert_eq!(period_for(parse_hz("10kHz").unwrap()), 100_000);
        assert_eq!(period_for(parse_hz("1k").unwrap()), 1_000_000);
        // Clamped at both ends of the accepted range.
        assert_eq!(period_for(parse_hz("10MHz").unwrap()), 1_000);
        assert_eq!(period_for(parse_hz("0.1Hz").unwrap()), 1_000_000_000);
        assert_eq!(parse_hz("nonsense"), None);
    }

    #[test]
    fn build_reads_the_attributes_off_a_flat_part() {
        let attrs = BTreeMap::from([
            ("wave".to_string(), "square".to_string()),
            ("frequency".to_string(), "10kHz".to_string()),
            ("duty".to_string(), "50".to_string()),
            ("amplitude".to_string(), "3.3".to_string()),
        ]);
        let part = FlatPart {
            id: "sig".into(),
            type_key: "litz-signal-generator".into(),
            engine: Engine::Builtin("signal-generator".into()),
            pins: vec!["OUT".into(), "GND".into()],
            pin_kinds: vec![],
            attrs,
            buses: vec![],
            device: None,
        };
        let built = SignalGenerator::build(&part).unwrap();
        assert_eq!(built.next_event(), Some(50_000));
        assert_eq!(built.drive(PinId(0)), Drive::strong(Level::High));
    }

    #[test]
    fn only_the_output_pin_is_driven() {
        let mut square = generator(Wave::Square, 1_000.0, 500, 3.3, 1.65);
        assert_eq!(square.drive(PinId(0)), Drive::strong(Level::High));
        assert_eq!(square.drive(PinId(1)), Drive::HIGH_Z);
        assert_eq!(square.analog(PinId(0)), Some(3.3));
        assert_eq!(square.analog(PinId(1)), None);

        square.advance(500_000);
        assert_eq!(square.drive(PinId(0)), Drive::strong(Level::Low));
        assert_eq!(square.drive(PinId(1)), Drive::HIGH_Z);
    }
}
