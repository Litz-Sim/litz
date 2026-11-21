//! Human-input parts: a DIP switch bank, a quadrature rotary encoder, an
//! analog joystick, a 4x4 membrane keypad and a tilt switch.
//!
//! These are pin-level models. A switch is a conductor between two contacts,
//! so the model mirrors the level it sees on one side onto the other and drives
//! it strongly; a joystick sources an analog voltage; the keypad is a matrix of
//! conductors, so a pressed key joins its row and column exactly as the real
//! membrane does.

use crate::BuildError;
use crate::flatten::FlatPart;
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};

const LOGIC_HIGH_VOLTS: f32 = 3.3;

fn index(part: &FlatPart, name: &str) -> Result<u16, BuildError> {
    part.pins
        .iter()
        .position(|candidate| candidate == name)
        .map(|i| i as u16)
        .ok_or_else(|| BuildError::Message(format!("part {:?} has no {name:?} pin", part.id)))
}

fn strong(level: Level) -> Drive {
    match level {
        Level::HighZ => Drive::HIGH_Z,
        level => Drive::strong(level),
    }
}

// ------------------------------------------------------------------ DIP switch

/// Eight independent SPST switches in one package.
///
/// Each position has two contacts (`Na`, `Nb`); when closed they are one
/// conductor, so either contact's level appears on the other.
pub struct DipSwitch {
    contacts: [(u16, u16); 8],
    levels: [(Level, Level); 8],
    closed: u8,
}

impl DipSwitch {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut contacts = [(0u16, 0u16); 8];
        for (number, pair) in contacts.iter_mut().enumerate() {
            let a = index(part, &format!("{}a", number + 1))?;
            let b = index(part, &format!("{}b", number + 1))?;
            *pair = (a, b);
        }
        Ok(Box::new(Self {
            contacts,
            levels: [(Level::HighZ, Level::HighZ); 8],
            closed: 0,
        }))
    }

    /// The switch positions as a bit mask, position 1 in bit 0.
    pub fn mask(&self) -> u8 {
        self.closed
    }

    pub fn is_closed(&self, position: usize) -> bool {
        position < 8 && self.closed & (1 << position) != 0
    }

    pub fn set_switch(&mut self, position: usize, closed: bool) {
        if position >= 8 {
            return;
        }
        if closed {
            self.closed |= 1 << position;
        } else {
            self.closed &= !(1 << position);
        }
    }

    fn position(&self, pin: u16) -> Option<(usize, bool)> {
        self.contacts
            .iter()
            .enumerate()
            .find_map(|(slot, &(a, b))| {
                if a == pin {
                    Some((slot, false))
                } else if b == pin {
                    Some((slot, true))
                } else {
                    None
                }
            })
    }
}

impl Part for DipSwitch {
    fn drive(&self, pin: PinId) -> Drive {
        let Some((slot, second)) = self.position(pin.0) else {
            return Drive::HIGH_Z;
        };
        if !self.is_closed(slot) {
            return Drive::HIGH_Z;
        }
        let (a, b) = self.levels[slot];
        strong(if second { a } else { b })
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        let Some((slot, second)) = self.position(pin.0) else {
            return;
        };
        if second {
            self.levels[slot].1 = level;
            self.levels[slot].0 = level;
        } else {
            self.levels[slot].0 = level;
            self.levels[slot].1 = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Switch {
            on: self.closed != 0,
        }
    }

    fn set_control(&mut self, value: u16) -> bool {
        // A DIP bank has no continuous travel; the low eight bits of the
        // control are the switch mask, which is what an inspector panel sends.
        self.closed = (value & 0xff) as u8;
        true
    }
}

// ------------------------------------------------------------------ encoder

/// One step of a quadrature cycle, indexed by `A << 1 | B`.
const QUADRATURE: [[i8; 4]; 4] = [[0, -1, 1, 0], [1, 0, 0, -1], [-1, 0, 0, 1], [0, 1, -1, 0]];

/// A rotary encoder with quadrature A/B outputs and a push switch.
///
/// The A/B pins are outputs of the part: a real encoder shorts them to ground
/// in Gray-code order as the shaft turns. The model decodes that same order
/// when a host moves the knob through [`Part::set_control`] and counts the
/// quarter-steps, so a hand turn and a UI turn produce one position stream.
/// The switch is active low, as the common breakout is.
pub struct RotaryEncoder {
    a: u16,
    b: u16,
    sw: u16,
    a_level: Level,
    b_level: Level,
    sw_level: Level,
    last_state: u8,
    position: i32,
    pressed: bool,
}

impl RotaryEncoder {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let a = index(part, "CLK")?;
        let b = index(part, "DT")?;
        let sw = index(part, "SW")?;
        Ok(Box::new(Self {
            a,
            b,
            sw,
            a_level: Level::Low,
            b_level: Level::Low,
            sw_level: Level::HighZ,
            last_state: 0,
            position: 0,
            pressed: false,
        }))
    }

    /// Quarter-steps since reset. One detent is four quarters, one revolution
    /// is twenty detents.
    pub fn position(&self) -> i32 {
        self.position
    }

    /// Detents since reset, positive clockwise.
    pub fn detents(&self) -> i32 {
        self.position / 4
    }

    /// Shaft angle from the quarter-step count.
    pub fn angle_degrees(&self) -> f64 {
        f64::from(self.position) * 360.0 / 80.0
    }

    pub fn is_pressed(&self) -> bool {
        self.pressed
    }

    fn set_state(&mut self, state: u8) {
        let delta = QUADRATURE[usize::from(self.last_state)][usize::from(state & 3)];
        self.position = (self.position + i32::from(delta)).clamp(-1_000_000, 1_000_000);
        self.last_state = state & 3;
    }

    fn apply_position(&mut self, position: i32) {
        self.position = position;
        let quarter = position.rem_euclid(4);
        // The forward Gray order is 0 (00), 2 (10), 3 (11), 1 (01).
        let state = [0u8, 2, 3, 1][quarter as usize];
        self.a_level = if state & 2 != 0 {
            Level::High
        } else {
            Level::Low
        };
        self.b_level = if state & 1 != 0 {
            Level::High
        } else {
            Level::Low
        };
        self.last_state = state;
    }
}

impl Part for RotaryEncoder {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.a {
            strong(self.a_level)
        } else if pin.0 == self.b {
            strong(self.b_level)
        } else if pin.0 == self.sw {
            strong(self.sw_level)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.a {
            self.a_level = level;
            let state = u8::from(level == Level::High) << 1 | u8::from(self.b_level == Level::High);
            self.set_state(state);
        } else if pin.0 == self.b {
            self.b_level = level;
            let state = u8::from(self.a_level == Level::High) << 1 | u8::from(level == Level::High);
            self.set_state(state);
        } else if pin.0 == self.sw {
            self.sw_level = level;
            self.pressed = level == Level::Low;
        }
    }

    fn state(&self) -> PartState {
        // Map the shaft onto the 0..1000 control range so a UI can draw it.
        let permille = ((self.position.clamp(-80, 80) + 80) * 1000 / 160).clamp(0, 1000);
        PartState::Knob {
            permille: permille as u16,
        }
    }

    fn press(&mut self, pressed: bool) -> bool {
        self.pressed = pressed;
        self.sw_level = if pressed { Level::Low } else { Level::HighZ };
        true
    }

    fn set_control(&mut self, value: u16) -> bool {
        let target = -80 + i32::from(value.min(1000)) * 160 / 1000;
        self.apply_position(target);
        true
    }
}

// ------------------------------------------------------------------ joystick

/// A two-axis analog joystick with a push button.
///
/// The wipers idle at half the rail and swing to either end of their travel.
/// Both axes are [`PinKind::AnalogOut`](litz_diagram::PinKind), so an ADC reads
/// the voltage the model sources. The button shorts `SEL` to ground.
/// A joystick axis as Wokwi writes it: -100 through 100, with 0 at rest.
fn set_axis(axis: &mut u16, value: &str) -> bool {
    let Ok(value) = value.trim().parse::<f32>() else {
        return false;
    };
    let value = value.clamp(-100.0, 100.0);
    *axis = ((value + 100.0) * 5.0).round().clamp(0.0, 1000.0) as u16;
    true
}

pub struct AnalogJoystick {
    vert: u16,
    horz: u16,
    sel: u16,
    x: u16,
    y: u16,
    pressed: bool,
}

impl AnalogJoystick {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Ok(Box::new(Self {
            vert: index(part, "VERT")?,
            horz: index(part, "HORZ")?,
            sel: index(part, "SEL")?,
            x: 500,
            y: 500,
            pressed: false,
        }))
    }

    /// `(horizontal, vertical)` in permille, 500 at rest.
    pub fn position(&self) -> (u16, u16) {
        (self.x, self.y)
    }

    /// `(horizontal, vertical)` in volts.
    pub fn volts(&self) -> (f32, f32) {
        (
            LOGIC_HIGH_VOLTS * f32::from(self.x) / 1000.0,
            LOGIC_HIGH_VOLTS * f32::from(self.y) / 1000.0,
        )
    }

    pub fn is_pressed(&self) -> bool {
        self.pressed
    }

    pub fn set_axes(&mut self, x: u16, y: u16) {
        self.x = x.min(1000);
        self.y = y.min(1000);
    }
}

impl Part for AnalogJoystick {
    fn drive(&self, pin: PinId) -> Drive {
        if pin.0 == self.sel && self.pressed {
            Drive::strong(Level::Low)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}

    fn state(&self) -> PartState {
        PartState::Knob { permille: self.x }
    }

    fn press(&mut self, pressed: bool) -> bool {
        self.pressed = pressed;
        true
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.x = value.min(1000);
        true
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        match name {
            "pressed" => {
                self.pressed = crate::parts::truthy(value);
                true
            }
            "x" | "horizontal" => set_axis(&mut self.x, value),
            "y" | "vertical" => set_axis(&mut self.y, value),
            _ => false,
        }
    }

    fn analog(&self, pin: PinId) -> Option<f32> {
        if pin.0 == self.vert {
            Some(self.volts().1)
        } else if pin.0 == self.horz {
            Some(self.volts().0)
        } else {
            None
        }
    }
}

// ------------------------------------------------------------------ keypad

const KEYPAD_ROWS: usize = 4;
const KEYPAD_COLUMNS: usize = 4;

/// A 4x4 (or 4x3) membrane keypad as a passive matrix of conductors.
///
/// A pressed key joins its row and its column. The model finds every connected
/// group and, when a group has an external level on any of its pins, drives the
/// whole group to that level. That is the electrical truth of a matrix and it
/// works whichever direction the firmware scans in.
pub struct MembraneKeypad {
    rows: [u16; KEYPAD_ROWS],
    columns: [u16; KEYPAD_COLUMNS],
    levels: [Level; KEYPAD_ROWS + KEYPAD_COLUMNS],
    pressed: Vec<(usize, usize)>,
    selected: usize,
    columns_used: usize,
}

impl MembraneKeypad {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        let mut rows = [0u16; KEYPAD_ROWS];
        for (row, pin) in rows.iter_mut().enumerate() {
            *pin = index(part, &format!("R{}", row + 1))?;
        }
        let mut columns = [0u16; KEYPAD_COLUMNS];
        for (column, pin) in columns.iter_mut().enumerate() {
            *pin = index(part, &format!("C{}", column + 1))?;
        }
        let columns_used = part
            .attrs
            .get("columns")
            .and_then(|text| text.trim().parse::<usize>().ok())
            .unwrap_or(4)
            .clamp(1, 4);
        Ok(Box::new(Self {
            rows,
            columns,
            levels: [Level::HighZ; KEYPAD_ROWS + KEYPAD_COLUMNS],
            pressed: Vec::new(),
            selected: 0,
            columns_used,
        }))
    }

    /// Keys currently held down, as `(row, column)`.
    pub fn pressed(&self) -> &[(usize, usize)] {
        &self.pressed
    }

    pub fn is_pressed(&self, row: usize, column: usize) -> bool {
        self.pressed.contains(&(row, column))
    }

    pub fn set_key(&mut self, row: usize, column: usize, pressed: bool) {
        if row >= KEYPAD_ROWS || column >= self.columns_used {
            return;
        }
        if pressed {
            if !self.pressed.contains(&(row, column)) {
                self.pressed.push((row, column));
            }
        } else {
            self.pressed.retain(|&key| key != (row, column));
        }
    }

    /// The pin index of a row or column in the internal level table.
    fn slot(&self, pin: u16) -> Option<usize> {
        if let Some(row) = self.rows.iter().position(|&candidate| candidate == pin) {
            return Some(row);
        }
        self.columns
            .iter()
            .position(|&candidate| candidate == pin)
            .map(|column| KEYPAD_ROWS + column)
    }

    /// Union-find over the eight matrix pins, joined by pressed keys.
    fn groups(&self) -> [usize; KEYPAD_ROWS + KEYPAD_COLUMNS] {
        let mut parent: [usize; KEYPAD_ROWS + KEYPAD_COLUMNS] = std::array::from_fn(|slot| slot);
        fn find(parent: &mut [usize], mut slot: usize) -> usize {
            while parent[slot] != slot {
                parent[slot] = parent[parent[slot]];
                slot = parent[slot];
            }
            slot
        }
        for &(row, column) in &self.pressed {
            let a = find(&mut parent, row);
            let b = find(&mut parent, KEYPAD_ROWS + column);
            if a != b {
                parent[a.max(b)] = a.min(b);
            }
        }
        std::array::from_fn(|slot| find(&mut parent, slot))
    }

    fn joined(&self, groups: &[usize], slot: usize) -> bool {
        let root = groups[slot];
        groups
            .iter()
            .enumerate()
            .any(|(other, group)| *group == root && other != slot)
    }

    fn group_level(&self, groups: &[usize], slot: usize) -> Level {
        let root = groups[slot];
        groups
            .iter()
            .enumerate()
            .filter(|(_, group)| **group == root)
            .map(|(other, _)| self.levels[other])
            .find(|level| *level != Level::HighZ)
            .unwrap_or(Level::HighZ)
    }
}

impl Part for MembraneKeypad {
    fn drive(&self, pin: PinId) -> Drive {
        let Some(slot) = self.slot(pin.0) else {
            return Drive::HIGH_Z;
        };
        if self.pressed.is_empty() {
            return Drive::HIGH_Z;
        }
        let groups = self.groups();
        if !self.joined(&groups, slot) {
            return Drive::HIGH_Z;
        }
        strong(self.group_level(&groups, slot))
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(slot) = self.slot(pin.0) {
            self.levels[slot] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::None
    }

    fn press(&mut self, pressed: bool) -> bool {
        if pressed {
            let key = (
                self.selected / self.columns_used,
                self.selected % self.columns_used,
            );
            self.set_key(key.0, key.1, true);
        } else {
            self.pressed.clear();
        }
        true
    }

    fn set_control(&mut self, value: u16) -> bool {
        let keys = KEYPAD_ROWS * self.columns_used;
        self.selected = usize::from(value) * keys / 1001;
        self.selected = self.selected.min(keys.saturating_sub(1));
        true
    }
}

// ------------------------------------------------------------------ tilt

/// A ball-in-cage tilt switch: two terminals joined when the part is tilted.
pub struct TiltSwitch {
    a: u16,
    b: u16,
    a_level: Level,
    b_level: Level,
    tilted: bool,
}

impl TiltSwitch {
    pub fn build(part: &FlatPart) -> Result<Box<dyn Part>, BuildError> {
        Ok(Box::new(Self {
            a: index(part, "1")?,
            b: index(part, "2")?,
            a_level: Level::HighZ,
            b_level: Level::HighZ,
            tilted: false,
        }))
    }

    pub fn is_tilted(&self) -> bool {
        self.tilted
    }
}

impl Part for TiltSwitch {
    fn drive(&self, pin: PinId) -> Drive {
        if !self.tilted {
            return Drive::HIGH_Z;
        }
        if pin.0 == self.a {
            strong(self.b_level)
        } else if pin.0 == self.b {
            strong(self.a_level)
        } else {
            Drive::HIGH_Z
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.a {
            self.a_level = level;
        } else if pin.0 == self.b {
            self.b_level = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Switch { on: self.tilted }
    }

    fn press(&mut self, pressed: bool) -> bool {
        if pressed {
            self.tilted = !self.tilted;
        }
        true
    }

    fn set_control(&mut self, value: u16) -> bool {
        self.tilted = value >= 500;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(id: u16) -> PinId {
        PinId(id)
    }

    #[test]
    fn a_closed_dip_position_joins_its_contacts() {
        let mut dip = DipSwitch {
            contacts: std::array::from_fn(|slot| (slot as u16 * 2, slot as u16 * 2 + 1)),
            levels: [(Level::HighZ, Level::HighZ); 8],
            closed: 0,
        };
        dip.set_pin(pin(1), Level::High, 0);
        assert_eq!(dip.drive(pin(0)), Drive::HIGH_Z, "open");
        dip.set_switch(0, true);
        assert_eq!(dip.drive(pin(0)), Drive::strong(Level::High));
        assert_eq!(dip.mask(), 1);
    }

    #[test]
    fn a_rotary_encoder_counts_quadrature_edges() {
        let mut encoder = RotaryEncoder {
            a: 0,
            b: 1,
            sw: 2,
            a_level: Level::Low,
            b_level: Level::Low,
            sw_level: Level::HighZ,
            last_state: 0,
            position: 0,
            pressed: false,
        };
        // 00 -> 10 -> 11 -> 01 -> 00 is one detent clockwise.
        for state in [(true, false), (true, true), (false, true), (false, false)] {
            encoder.set_pin(pin(0), if state.0 { Level::High } else { Level::Low }, 0);
            encoder.set_pin(pin(1), if state.1 { Level::High } else { Level::Low }, 0);
        }
        assert_eq!(encoder.position(), 4);
        assert_eq!(encoder.detents(), 1);
        assert!((encoder.angle_degrees() - 18.0).abs() < 0.001);
    }

    #[test]
    fn a_rotary_encoder_control_turns_the_gray_outputs() {
        let mut encoder = RotaryEncoder {
            a: 0,
            b: 1,
            sw: 2,
            a_level: Level::Low,
            b_level: Level::Low,
            sw_level: Level::HighZ,
            last_state: 0,
            position: 0,
            pressed: false,
        };
        encoder.set_control(1000);
        assert_eq!(encoder.position(), 80);
        assert_eq!(encoder.drive(pin(10)), Drive::HIGH_Z);
        assert!(matches!(encoder.state(), PartState::Knob { .. }));
    }

    #[test]
    fn a_joystick_sources_a_voltage_per_axis_and_shorts_sel() {
        let mut stick = AnalogJoystick {
            vert: 0,
            horz: 1,
            sel: 2,
            x: 500,
            y: 500,
            pressed: false,
        };
        assert_eq!(stick.analog(pin(0)), Some(1.65));
        assert_eq!(stick.analog(pin(1)), Some(1.65));
        stick.set_axes(0, 1000);
        assert_eq!(stick.analog(pin(0)), Some(3.3));
        assert_eq!(stick.analog(pin(1)), Some(0.0));
        assert_eq!(stick.drive(pin(2)), Drive::HIGH_Z);
        stick.press(true);
        assert_eq!(stick.drive(pin(2)), Drive::strong(Level::Low));
    }

    #[test]
    fn a_keypad_key_joins_its_row_and_column() {
        let mut keypad = MembraneKeypad {
            rows: [0, 1, 2, 3],
            columns: [4, 5, 6, 7],
            levels: [Level::HighZ; 8],
            pressed: Vec::new(),
            selected: 0,
            columns_used: 4,
        };
        keypad.set_pin(pin(0), Level::Low, 0); // row 1 driven low
        assert_eq!(keypad.drive(pin(4)), Drive::HIGH_Z, "no key pressed");
        keypad.set_key(0, 0, true);
        assert_eq!(keypad.drive(pin(4)), Drive::strong(Level::Low));
        keypad.set_key(0, 0, false);
        assert_eq!(keypad.drive(pin(4)), Drive::HIGH_Z);
    }

    #[test]
    fn a_tilt_switch_latches_its_throw() {
        let mut tilt = TiltSwitch {
            a: 0,
            b: 1,
            a_level: Level::HighZ,
            b_level: Level::HighZ,
            tilted: false,
        };
        tilt.set_pin(pin(0), Level::High, 0);
        assert_eq!(tilt.drive(pin(1)), Drive::HIGH_Z);
        tilt.set_control(700);
        assert_eq!(tilt.drive(pin(1)), Drive::strong(Level::High));
        assert_eq!(tilt.state(), PartState::Switch { on: true });
    }
}
