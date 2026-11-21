//! Parts whose behaviour depends on when their edges arrive.
//!
//! Both are observers: they resolve nothing and never drive a net. A buzzer
//! reports the square-wave rate it sees; a servo the angle its pulse train asks
//! for.
//!
//! Pin edges time a square wave only; a PWM duty cycle is invisible here, so a
//! passive buzzer is heard only when the firmware toggles it.

use crate::{BuildError, flatten::FlatPart};
use litz_core::{Drive, Level, Part, PartState, PinId, TICKS_PER_SEC, Tick};

/// Above what a small buzzer can follow; a larger reading is edge noise.
const MAX_TONE_HZ: u64 = 20_000;

pub fn buzzer(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
    let index = |name: &str| part.pins.iter().position(|p| p == name).map(|i| i as u16);
    let (Some(positive), Some(_negative)) = (index("1"), index("2")) else {
        return Err(BuildError::Message(format!(
            "part {:?} has no 1/2 pins",
            part.id
        )));
    };
    Ok(Box::new(Buzzer::new(positive)))
}

/// A buzzer driven by one pin. Frequency is the interval between rising edges,
/// which is what a firmware tone produces and a passive buzzer needs.
pub struct Buzzer {
    positive: u16,
    last_rising: Option<Tick>,
    /// The pin is high right now.
    high: bool,
    /// At least one edge has been seen, so the reading is a measurement.
    seen: bool,
    hz: u32,
}

impl Buzzer {
    pub fn new(positive: u16) -> Self {
        Self {
            positive,
            last_rising: None,
            high: false,
            seen: false,
            hz: 0,
        }
    }
}

impl Part for Buzzer {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        if pin.0 != self.positive {
            return;
        }
        match level {
            Level::High => {
                if let Some(last) = self.last_rising {
                    // A zero-width period says nothing about a rate, which is
                    // what `checked_div` answers with.
                    let period = now.saturating_sub(last);
                    self.hz = TICKS_PER_SEC
                        .checked_div(period)
                        .unwrap_or(0)
                        .min(MAX_TONE_HZ) as u32;
                }
                self.last_rising = Some(now);
                self.high = true;
                self.seen = true;
            }
            // Falling or released: silent until the next rising edge.
            _ => self.high = false,
        }
    }

    fn state(&self) -> PartState {
        PartState::Tone {
            sounding: self.seen && self.high,
            hz: self.hz,
        }
    }
}

/// Shorter than the narrowest legal 0.5 ms pulse, with glitch margin.
const MIN_PULSE_US: u64 = 100;
/// Longer than the widest legal 2.5 ms pulse, so a stuck edge cannot steer.
const MAX_PULSE_US: u64 = 5_000;

pub fn servo(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
    let index = |name: &str| part.pins.iter().position(|p| p == name).map(|i| i as u16);
    let (Some(_gnd), Some(_vplus), Some(pwm)) = (index("GND"), index("V+"), index("PWM")) else {
        return Err(BuildError::Message(format!(
            "part {:?} has no GND/V+/PWM pins",
            part.id
        )));
    };
    Ok(Box::new(Servo::new(pwm)))
}

/// A hobby servo: a 50 Hz train where high time picks the angle. Supplies are
/// validated so a miswired part fails at build time.
pub struct Servo {
    pwm: u16,
    rising: Option<Tick>,
    degrees: u16,
}

impl Servo {
    pub fn new(pwm: u16) -> Self {
        Self {
            pwm,
            rising: None,
            degrees: 0,
        }
    }
}

impl Part for Servo {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, now: Tick) {
        if pin.0 != self.pwm {
            return;
        }
        if level == Level::High {
            self.rising = Some(now);
        } else if let Some(start) = self.rising.take()
            && let Some(degrees) = degrees_for(now.saturating_sub(start))
        {
            self.degrees = degrees;
        }
    }

    fn state(&self) -> PartState {
        PartState::Servo {
            degrees: self.degrees,
        }
    }
}

/// Pulse width in ticks to an angle, or `None` when the edge was not a command.
///
/// 0.5 ms is 0 degrees and 2.5 ms is 180, so 1.5 ms lands at 90. Out-of-range
/// widths are rejected rather than clamped, or a stray spike would slam the
/// horn to an end stop.
fn degrees_for(width: Tick) -> Option<u16> {
    let pulse_us = width / 1_000;
    if !(MIN_PULSE_US..=MAX_PULSE_US).contains(&pulse_us) {
        return None;
    }
    Some((pulse_us.saturating_sub(500) * 180 / 2_000).min(180) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pulse(servo: &mut Servo, at: Tick, width: Tick) {
        servo.set_pin(PinId(0), Level::High, at);
        servo.set_pin(PinId(0), Level::Low, at + width);
    }

    #[test]
    fn buzzer_reads_a_kilohertz_square_wave() {
        let mut buzzer = Buzzer::new(0);
        buzzer.set_pin(PinId(0), Level::High, 0);
        buzzer.set_pin(PinId(0), Level::Low, 500_000);
        buzzer.set_pin(PinId(0), Level::High, 1_000_000);
        assert_eq!(
            buzzer.state(),
            PartState::Tone {
                sounding: true,
                hz: 1_000
            }
        );
    }

    #[test]
    fn servo_maps_pulse_width_to_angle() {
        let mut servo = Servo::new(0);
        pulse(&mut servo, 0, 1_500_000);
        assert_eq!(servo.state(), PartState::Servo { degrees: 90 });
        pulse(&mut servo, 20_000_000, 500_000);
        assert_eq!(servo.state(), PartState::Servo { degrees: 0 });
        pulse(&mut servo, 40_000_000, 2_500_000);
        assert_eq!(servo.state(), PartState::Servo { degrees: 180 });
    }

    #[test]
    fn servo_ignores_a_glitch() {
        let mut servo = Servo::new(0);
        pulse(&mut servo, 0, 1_500_000);
        pulse(&mut servo, 20_000_000, 20_000);
        assert_eq!(servo.state(), PartState::Servo { degrees: 90 });
    }

    #[test]
    fn observers_never_drive() {
        let buzzer = Buzzer::new(0);
        let servo = Servo::new(0);
        for pin in 0..4 {
            assert_eq!(buzzer.drive(PinId(pin)), Drive::HIGH_Z);
            assert_eq!(servo.drive(PinId(pin)), Drive::HIGH_Z);
        }
    }
}
