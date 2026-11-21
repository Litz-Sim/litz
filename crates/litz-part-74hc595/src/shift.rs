//! The 74HC595's shift register, independent of the component wrapper.
//!
//! Pin indices are the ones the catalog assigns. A `part.toml` collects its
//! pins in a `BTreeMap`, so the runtime's indices are alphabetical by pin
//! name, not the order a human wrote them. [`PINS`] is that order and the
//! constants below are indices into it; the integration test shifts a byte
//! through named pins and fails if the manifest and this table disagree.

/// A pin level as the contract carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Low,
    High,
    Floating,
}

/// Serial data in.
pub const DS: u16 = 0;
/// Master reset, active low.
pub const MR: u16 = 1;
/// Output enable, active low.
pub const OE: u16 = 2;
/// First output.
pub const Q0: u16 = 3;
/// Last output.
pub const Q7: u16 = 10;
/// Shift clock, rising edge shifts.
pub const SHCP: u16 = 11;
/// Storage clock, rising edge latches.
pub const STCP: u16 = 12;

/// The pin table in catalog order (alphabetical, see the module comment).
pub const PINS: [&str; 13] = [
    "DS", "MR", "OE", "Q0", "Q1", "Q2", "Q3", "Q4", "Q5", "Q6", "Q7", "SHCP", "STCP",
];

/// One 74HC595.
#[derive(Debug)]
pub struct ShiftRegister {
    /// Bits clocked in but not yet latched.
    shift: u8,
    /// The output latch, which is what `Q0`..`Q7` show.
    latch: u8,
    ds: bool,
    shcp: bool,
    stcp: bool,
    /// `OE` is active low; floating leaves the outputs enabled, which is the
    /// useful default for a schematic that does not wire the pin.
    outputs_enabled: bool,
    /// `MR` is active low and asynchronous.
    reset: bool,
}

impl Default for ShiftRegister {
    fn default() -> Self {
        Self {
            shift: 0,
            latch: 0,
            ds: false,
            shcp: false,
            stcp: false,
            outputs_enabled: true,
            reset: false,
        }
    }
}

impl ShiftRegister {
    pub fn new() -> Self {
        Self::default()
    }

    /// A net on one of this part's pins resolved a level.
    pub fn set_pin(&mut self, pin: u16, level: Level, _at_ns: u64) {
        let high = level == Level::High;
        match pin {
            DS => self.ds = high,
            SHCP => {
                let rising = !self.shcp && high;
                self.shcp = high;
                if rising {
                    self.shift_bit();
                }
            }
            STCP => {
                let rising = !self.stcp && high;
                self.stcp = high;
                if rising {
                    self.latch = self.shift;
                }
            }
            OE => self.outputs_enabled = !high,
            MR => {
                // Held low, the shift register is clear; releasing the pin
                // leaves whatever was clocked in after the reset.
                let low = level == Level::Low;
                if low {
                    self.shift = 0;
                }
                self.reset = low;
            }
            _ => {}
        }
    }

    /// Serial data enters `Q0` first, so each edge shifts toward `Q7`.
    fn shift_bit(&mut self) {
        if !self.reset {
            self.shift = (self.shift << 1) | u8::from(self.ds);
        }
    }

    /// What `Q0`..`Q7` drive, or `None` when `OE` releases the outputs.
    pub fn output(&self, pin: u16) -> Option<Level> {
        if !self.outputs_enabled || !(Q0..=Q7).contains(&pin) {
            return None;
        }
        let bit = (self.latch >> (pin - Q0)) & 1;
        Some(if bit == 1 { Level::High } else { Level::Low })
    }

    /// The output latch, for tests and diagnostics.
    pub fn latch(&self) -> u8 {
        self.latch
    }

    /// The shift register, for tests and diagnostics.
    pub fn shift(&self) -> u8 {
        self.shift
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hi(register: &mut ShiftRegister, pin: u16) {
        register.set_pin(pin, Level::High, 0);
    }

    fn lo(register: &mut ShiftRegister, pin: u16) {
        register.set_pin(pin, Level::Low, 0);
    }

    fn clock(register: &mut ShiftRegister) {
        hi(register, SHCP);
        lo(register, SHCP);
    }

    fn latch(register: &mut ShiftRegister) {
        hi(register, STCP);
        lo(register, STCP);
    }

    /// Send a byte most-significant bit first. The last bit clocked in lands
    /// on `Q0`, so this leaves bit `i` of `byte` on `Qi`; the part itself does
    /// not care which order a caller uses.
    fn shift_byte(register: &mut ShiftRegister, byte: u8) {
        for bit in (0..8).rev() {
            register.set_pin(
                DS,
                if byte & (1 << bit) != 0 {
                    Level::High
                } else {
                    Level::Low
                },
                0,
            );
            clock(register);
        }
    }

    #[test]
    fn pin_indices_are_in_catalog_order() {
        // `PINS` must stay alphabetical because that is how `part.toml`
        // stores its pins. This catches a rename that moves an index.
        let mut sorted = PINS;
        sorted.sort_unstable();
        assert_eq!(sorted, PINS);
        assert_eq!((DS, MR, OE, Q0, Q7, SHCP, STCP), (0, 1, 2, 3, 10, 11, 12));
    }

    #[test]
    fn shifting_a_byte_lands_in_the_latch_with_bit_i_on_qi() {
        let mut register = ShiftRegister::new();
        shift_byte(&mut register, 0b1010_0101);
        assert_eq!(register.shift(), 0b1010_0101);
        assert_eq!(
            register.latch(),
            0,
            "nothing reaches the outputs before STCP"
        );

        latch(&mut register);
        assert_eq!(register.latch(), 0b1010_0101);
        for bit in 0..8 {
            let expected = if 0b1010_0101 & (1 << bit) != 0 {
                Level::High
            } else {
                Level::Low
            };
            assert_eq!(register.output(Q0 + bit), Some(expected), "Q{bit}");
        }
    }

    #[test]
    fn the_latch_holds_until_the_next_storage_clock() {
        let mut register = ShiftRegister::new();
        shift_byte(&mut register, 0x0f);
        latch(&mut register);
        shift_byte(&mut register, 0xf0);
        assert_eq!(register.shift(), 0xf0);
        assert_eq!(register.latch(), 0x0f, "the old byte stays on the outputs");
    }

    #[test]
    fn output_enable_high_releases_the_outputs() {
        let mut register = ShiftRegister::new();
        shift_byte(&mut register, 0xff);
        latch(&mut register);
        assert_eq!(register.output(Q0), Some(Level::High));
        hi(&mut register, OE);
        assert_eq!(register.output(Q0), None);
        lo(&mut register, OE);
        assert_eq!(register.output(Q0), Some(Level::High));
    }

    #[test]
    fn master_reset_clears_the_shift_register_not_the_latch() {
        let mut register = ShiftRegister::new();
        shift_byte(&mut register, 0xa5);
        latch(&mut register);
        lo(&mut register, MR);
        assert_eq!(register.shift(), 0);
        assert_eq!(register.latch(), 0xa5);

        // Clocks while reset is held do nothing, and releasing it lets the
        // next bit in.
        register.set_pin(DS, Level::High, 0);
        clock(&mut register);
        assert_eq!(register.shift(), 0);
        hi(&mut register, MR);
        clock(&mut register);
        assert_eq!(register.shift(), 1);
    }
}
