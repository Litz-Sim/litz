//! A latching SPDT slide switch.
//!
//! Common pole `2` rests on `1` when off and on `3` when on. Unlike a
//! pushbutton it stays where it was thrown, so the board settles around its
//! position rather than around an edge.

use crate::BuildError;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};

/// Positions in `pins` and `levels`.
const OFF_THROW: usize = 0;
const COMMON: usize = 1;
const ON_THROW: usize = 2;

/// Common `2` to `1` when off, to `3` when on.
pub struct SlideSwitch {
    /// Pin ids of `[1, 2, 3]`, looked up from the manifest's names.
    pins: [u16; 3],
    /// Last level seen on each terminal, in the same order as `pins`.
    levels: [Level; 3],
    on: bool,
}

impl SlideSwitch {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let index = |name: &str| part.pins.iter().position(|p| p == name).map(|i| i as u16);
        let (Some(off), Some(common), Some(on)) = (index("1"), index("2"), index("3")) else {
            return Err(BuildError::Message(format!(
                "part {:?} does not have the 1/2/3 pins of a slide switch",
                part.id
            )));
        };
        Ok(Box::new(Self {
            pins: [off, common, on],
            levels: [Level::HighZ; 3],
            on: false,
        }))
    }

    fn role(&self, pin: u16) -> Option<usize> {
        self.pins.iter().position(|&p| p == pin)
    }

    /// The throw the wiper rests on. It is one conductor with the pole.
    fn selected(&self) -> usize {
        if self.on { ON_THROW } else { OFF_THROW }
    }
}

impl Part for SlideSwitch {
    fn drive(&self, pin: PinId) -> Drive {
        let source = match self.role(pin.0) {
            Some(COMMON) => self.selected(),
            Some(role) if role == self.selected() => COMMON,
            // The throw the wiper is not on is open: it floats.
            _ => return Drive::HIGH_Z,
        };
        match self.levels[source] {
            Level::HighZ => Drive::HIGH_Z,
            level => Drive::strong(level),
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        let Some(role) = self.role(pin.0) else { return };
        self.levels[role] = level;
        // The pole and its throw are one node; the open throw keeps its own.
        if role == COMMON {
            let selected = self.selected();
            self.levels[selected] = level;
        } else if role == self.selected() {
            self.levels[COMMON] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Switch { on: self.on }
    }

    fn press(&mut self, pressed: bool) -> bool {
        // A slide switch latches: the press is the throw. The UI's pointerdown
        // press and pointerup release must not cancel out, so releases are
        // ignored; both edges still report true because it does take presses.
        if pressed {
            self.on = !self.on;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn switch() -> SlideSwitch {
        SlideSwitch {
            pins: [0, 1, 2],
            levels: [Level::HighZ; 3],
            on: false,
        }
    }

    #[test]
    fn default_state_floats_every_terminal() {
        let s = switch();
        assert_eq!(s.state(), PartState::Switch { on: false });
        assert_eq!(
            [s.drive(PinId(0)), s.drive(PinId(1)), s.drive(PinId(2))],
            [Drive::HIGH_Z; 3]
        );
    }

    #[test]
    fn thrown_on_connects_the_common_to_pin_three() {
        let mut s = SlideSwitch {
            on: true,
            ..switch()
        };
        s.set_pin(PinId(2), Level::High, 0);
        assert_eq!(s.drive(PinId(1)), Drive::strong(Level::High));
        assert_eq!(s.drive(PinId(2)), Drive::strong(Level::High));
        assert_eq!(s.drive(PinId(0)), Drive::HIGH_Z);
    }

    #[test]
    fn thrown_back_connects_the_common_to_pin_one() {
        let mut s = SlideSwitch {
            on: true,
            ..switch()
        };
        s.press(true);
        s.set_pin(PinId(0), Level::Low, 0);
        assert_eq!(s.drive(PinId(1)), Drive::strong(Level::Low));
        assert_eq!(s.drive(PinId(0)), Drive::strong(Level::Low));
        assert_eq!(s.drive(PinId(2)), Drive::HIGH_Z);
    }

    #[test]
    fn press_toggles_and_release_does_nothing() {
        let mut s = switch();
        assert!(s.press(true));
        assert_eq!(s.state(), PartState::Switch { on: true });
        assert!(s.press(false));
        assert_eq!(s.state(), PartState::Switch { on: true });
        s.press(true);
        assert_eq!(s.state(), PartState::Switch { on: false });
    }
}
