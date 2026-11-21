//! IO_BANK0 and PADS_BANK0: each pin's function select and its pad settings
//! (pull, drive, Schmitt, input enable).
//!
//! The machine needs two answers from this pair: [`IoBank::funcsel`], which
//! says whether a pin is driven by SIO (`FUNCSEL = 5`) or by a peripheral, and
//! [`PadsBank::input_enabled`], which says whether the pad presents its level
//! at all. Everything else is storage, because a firmware configures the pad
//! and then never looks at it again.

use super::RegisterFile;

/// GPIO pins the bank covers (GPIO0 through GPIO29).
pub const PINS: usize = 30;

/// The function-select value that hands a pin to SIO.
pub const FUNCSEL_SIO: u8 = 5;

/// `GPIOx_CTRL` bits.
pub mod gpio_ctrl {
    pub const FUNCSEL_MASK: u32 = 0x1f;
    pub const OUTOVER_SHIFT: u32 = 8;
    pub const OEOVER_SHIFT: u32 = 12;
    pub const INOVER_SHIFT: u32 = 14;
    pub const IRQOVER_SHIFT: u32 = 16;
}

/// PADS_BANK0 per-pin bits.
pub mod pad {
    pub const SCHMITT: u32 = 1 << 1;
    pub const PDE: u32 = 1 << 2;
    pub const PUE: u32 = 1 << 3;
    pub const DRIVE_SHIFT: u32 = 4;
    pub const DRIVE_MASK: u32 = 0x3 << DRIVE_SHIFT;
    pub const IE: u32 = 1 << 6;
    pub const OD: u32 = 1 << 7;
}

/// The reset value of a pad: input enabled, pull-down, Schmitt trigger, 4 mA
/// drive. This is the `0x56` the datasheet gives for PADS_BANK0 GPIOx.
pub const PAD_RESET: u32 = pad::IE | pad::PDE | pad::SCHMITT | (1 << pad::DRIVE_SHIFT);

#[derive(Debug)]
pub struct IoBank {
    regs: RegisterFile,
    status: [u32; PINS],
    ctrl: [u32; PINS],
}

impl Default for IoBank {
    fn default() -> Self {
        Self::new()
    }
}

impl IoBank {
    pub fn new() -> Self {
        IoBank {
            regs: RegisterFile::new(0x100 / 4),
            status: [0; PINS],
            // Reset FUNCSEL is 0x1f (NULL) until a firmware routes the pin.
            ctrl: [u32::from(gpio_ctrl::FUNCSEL_MASK as u8); PINS],
        }
    }

    fn pin_index(off: u32) -> Option<(usize, bool)> {
        // Odd offsets are STATUS, the next word is CTRL.
        let pair = off / 8;
        (pair < PINS as u32).then_some((pair as usize, off & 4 == 0))
    }

    pub fn read(&mut self, off: u32) -> u32 {
        if let Some((pin, status)) = Self::pin_index(off) {
            return if status {
                self.status[pin]
            } else {
                self.ctrl[pin]
            };
        }
        self.regs.read(off)
    }

    pub fn write(&mut self, off: u32, value: u32) {
        if let Some((pin, status)) = Self::pin_index(off) {
            if status {
                // STATUS is read-only on the hardware; keeping the write is
                // harmless and lets a test stage a value.
                self.status[pin] = value;
            } else {
                self.ctrl[pin] = value;
            }
            return;
        }
        self.regs.write(off, value);
    }

    /// The function selected for `pin`, or 0xff for a pin outside the bank.
    pub fn funcsel(&self, pin: u8) -> u8 {
        self.ctrl
            .get(usize::from(pin))
            .map(|ctrl| (ctrl & gpio_ctrl::FUNCSEL_MASK) as u8)
            .unwrap_or(0xff)
    }
}

#[derive(Debug)]
pub struct PadsBank {
    regs: RegisterFile,
    pads: [u32; PINS],
    voltage_select: u32,
}

impl Default for PadsBank {
    fn default() -> Self {
        Self::new()
    }
}

impl PadsBank {
    pub fn new() -> Self {
        PadsBank {
            regs: RegisterFile::new(0x100 / 4),
            pads: [PAD_RESET; PINS],
            voltage_select: 0,
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x00 => self.voltage_select,
            off if off >= 0x04 => {
                let pin = ((off - 0x04) / 4) as usize;
                self.pads.get(pin).copied().unwrap_or(0)
            }
            _ => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            0x00 => self.voltage_select = value & 1,
            off if off >= 0x04 => {
                let pin = ((off - 0x04) / 4) as usize;
                if let Some(slot) = self.pads.get_mut(pin) {
                    *slot = value;
                }
            }
            off => self.regs.write(off, value),
        }
    }

    /// Whether the pad presents its input to the chip (`PADS_BANK0.IE`).
    pub fn input_enabled(&self, pin: u8) -> bool {
        self.pads
            .get(usize::from(pin))
            .is_some_and(|pad| pad & pad::IE != 0)
    }

    /// Whether the pad has an internal pull-up or pull-down.
    pub fn pulls(&self, pin: u8) -> (bool, bool) {
        let pad = self.pads.get(usize::from(pin)).copied().unwrap_or(0);
        (pad & pad::PUE != 0, pad & pad::PDE != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_select_round_trips() {
        let mut io = IoBank::new();
        assert_eq!(io.funcsel(0), 0x1f);
        io.write(0x004 + 8 * 5, u32::from(FUNCSEL_SIO));
        assert_eq!(io.funcsel(5), FUNCSEL_SIO);
        assert_eq!(io.funcsel(6), 0x1f);
        assert_eq!(io.read(0x004 + 8 * 5), u32::from(FUNCSEL_SIO));
        assert_eq!(io.funcsel(29), 0x1f);
        assert_eq!(io.funcsel(30), 0xff);
    }

    #[test]
    fn pad_input_enable_defaults_on_and_can_be_cleared() {
        let mut pads = PadsBank::new();
        assert!(pads.input_enabled(0));
        pads.write(0x004 + 4 * 3, PAD_RESET & !pad::IE);
        assert!(!pads.input_enabled(3));
        assert!(pads.input_enabled(4));
        pads.write(0x004 + 4 * 3, PAD_RESET);
        assert!(pads.input_enabled(3));
    }
}
