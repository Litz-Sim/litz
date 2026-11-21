//! A potentiometer: two ends across a supply and a wiper that taps a fraction
//! of the way along.
//!
//! The digital net model has no voltages, so this is the one part that hands
//! the backend a number instead of a level. Its wiper still participates in
//! the net as a high-impedance observer, which keeps a stray digital wire onto
//! the wiper from being silently ignored by validation.

use crate::BuildError;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};

/// Logic high is the 3.3 V rail: the ADC's full scale on this board, and the
/// only meaning "high" has without a voltage domain model.
const LOGIC_HIGH_VOLTS: f32 = 3.3;

pub struct Potentiometer {
    /// Pins `1` and `3`, the two ends of the track.
    ends: [u16; 2],
    wiper: u16,
    end_levels: [Level; 2],
    permille: u16,
}

impl Potentiometer {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Self::build_with(part, permille_of)
    }

    /// The slide potentiometer writes its value on the 0–1023 ADC scale rather
    /// than as a percentage, which is the one difference from the rotary part.
    pub fn build_slide(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Self::build_with(part, permille_of_1023)
    }

    fn build_with(part: &FlatPart, scale: fn(&str) -> u16) -> Result<Box<dyn Part>, BuildError> {
        let index = |name: &str| part.pins.iter().position(|p| p == name).map(|i| i as u16);
        let (Some(one), Some(two), Some(three)) = (index("1"), index("2"), index("3")) else {
            return Err(BuildError::Message(format!(
                "part {:?} does not have the 1/2/3 pins",
                part.id
            )));
        };
        let permille = part.attrs.get("value").map_or(500, |v| scale(v));
        Ok(Box::new(Self {
            ends: [one, three],
            wiper: two,
            end_levels: [Level::HighZ; 2],
            permille,
        }))
    }

    /// Volts held on an end of the track, from the level its net resolved to.
    fn volts(level: Level) -> Option<f32> {
        match level {
            Level::High => Some(LOGIC_HIGH_VOLTS),
            Level::Low => Some(0.0),
            Level::HighZ => None,
        }
    }

    pub fn permille(&self) -> u16 {
        self.permille
    }
}

/// The catalog writes `value` as a 0–100 percentage.
fn permille_of(text: &str) -> u16 {
    let percent: f32 = text.trim().parse().unwrap_or(50.0);
    (percent.clamp(0.0, 100.0) * 10.0).round() as u16
}

/// The slide part's value is an ADC count: 0..1023 of travel.
fn permille_of_1023(text: &str) -> u16 {
    let count: f32 = text.trim().parse().unwrap_or(0.0);
    (count.clamp(0.0, 1023.0) / 1023.0 * 1000.0).round() as u16
}

impl Part for Potentiometer {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.ends[0] {
            self.end_levels[0] = level;
        } else if pin.0 == self.ends[1] {
            self.end_levels[1] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Knob {
            permille: self.permille,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.permille = value.min(1000);
        true
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        if pin.0 != self.wiper {
            return None;
        }
        // A floating end is not a voltage. Real hardware would read noise;
        // saying "no analog source" lets the pad keep its own default.
        let from = Self::volts(self.end_levels[0])?;
        let to = Self::volts(self.end_levels[1])?;
        let travel = f32::from(self.permille) / 1000.0;
        Some(from + (to - from) * travel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pot() -> Potentiometer {
        Potentiometer {
            ends: [0, 2],
            wiper: 1,
            end_levels: [Level::HighZ; 2],
            permille: 500,
        }
    }

    #[test]
    fn the_wiper_taps_the_fraction_between_its_ends() {
        let mut p = pot();
        assert_eq!(p.analog(PinId(1)), None, "a floating track sources nothing");
        p.set_pin(PinId(2), Level::High, 0);
        p.set_pin(PinId(0), Level::Low, 0);
        assert_eq!(p.analog(PinId(1)), Some(1.65));
        p.set_control(1000);
        assert_eq!(p.analog(PinId(1)), Some(3.3));
        p.set_control(0);
        assert_eq!(p.analog(PinId(1)), Some(0.0));
        assert_eq!(p.analog(PinId(0)), None, "only the wiper carries a voltage");
    }

    #[test]
    fn one_end_floating_is_not_a_divider() {
        let mut p = pot();
        p.set_pin(PinId(0), Level::High, 0);
        assert_eq!(p.analog(PinId(1)), None);
    }

    #[test]
    fn a_value_attribute_is_a_percentage() {
        assert_eq!(permille_of("0"), 0);
        assert_eq!(permille_of("50"), 500);
        assert_eq!(permille_of("100"), 1000);
        assert_eq!(permille_of("250"), 1000, "clamped");
    }
}
