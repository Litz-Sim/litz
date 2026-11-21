//! A seven-segment digit.
//!
//! The display is a bank of eight LEDs sharing one terminal. Which side of
//! those LEDs the common terminal sits on is a property of how the board is
//! wired, not of the part, so this engine reads the common's level and inverts
//! the segments accordingly.

use crate::BuildError;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};

/// Bit position of each segment in the state mask, least significant bit
/// first.
const SEGMENT_PINS: [&str; 8] = ["a", "b", "c", "d", "e", "f", "g", "dp"];

pub struct Segments {
    /// Pin index of each segment, in `SEGMENT_PINS` order.
    segments: [u16; 8],
    com: u16,
    levels: [Level; 8],
    /// Last level on the common terminal. `HighZ` means nobody has driven it.
    com_level: Level,
}

impl Segments {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let index = |name: &str| part.pins.iter().position(|p| p == name).map(|i| i as u16);

        let mut segments = [0; 8];
        for (slot, name) in segments.iter_mut().zip(SEGMENT_PINS) {
            *slot = index(name).ok_or_else(|| {
                BuildError::Message(format!("part {:?} has no {name:?} pin", part.id))
            })?;
        }
        let com = index("com")
            .ok_or_else(|| BuildError::Message(format!("part {:?} has no \"com\" pin", part.id)))?;

        Ok(Box::new(Self {
            segments,
            com,
            levels: [Level::HighZ; 8],
            com_level: Level::HighZ,
        }))
    }

    fn mask(&self) -> u8 {
        // A common at ground (or left floating, which is how the part is
        // normally wired) makes this common-cathode, so a high anode lights.
        // A common held at a rail makes it common-anode and inverts the drive:
        // a low anode lights. A floating anode is never lit either way.
        let lit = if self.com_level == Level::High {
            Level::Low
        } else {
            Level::High
        };
        let mut mask = 0;
        for (bit, level) in self.levels.iter().enumerate() {
            if *level == lit {
                mask |= 1 << bit;
            }
        }
        mask
    }
}

impl Part for Segments {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(i) = self.segments.iter().position(|&p| p == pin.0) {
            self.levels[i] = level;
        } else if pin.0 == self.com {
            self.com_level = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Segments { mask: self.mask() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digit() -> Segments {
        Segments {
            segments: [0, 1, 2, 3, 4, 5, 6, 7],
            com: 8,
            levels: [Level::HighZ; 8],
            com_level: Level::HighZ,
        }
    }

    #[test]
    fn common_cathode_lights_a_high_segment() {
        let mut d = digit();
        d.set_pin(PinId(8), Level::Low, 0);
        d.set_pin(PinId(0), Level::High, 0);
        assert_eq!(d.state(), PartState::Segments { mask: 0b0000_0001 });
    }

    #[test]
    fn common_anode_inverts_the_drive() {
        let mut d = digit();
        d.set_pin(PinId(8), Level::High, 0);
        d.set_pin(PinId(0), Level::High, 0);
        assert_eq!(d.state(), PartState::Segments { mask: 0 });
        d.set_pin(PinId(0), Level::Low, 0);
        assert_eq!(d.state(), PartState::Segments { mask: 0b0000_0001 });
    }

    #[test]
    fn floating_common_counts_as_ground() {
        let mut d = digit();
        d.set_pin(PinId(0), Level::High, 0);
        assert_eq!(d.state(), PartState::Segments { mask: 0b0000_0001 });
    }
}
