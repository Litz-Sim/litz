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

/// Where the four interrupt register groups sit, in words of eight pins.
pub mod irq {
    pub const INTR0: u32 = 0x0f0;
    pub const PROC0_INTE0: u32 = 0x100;
    pub const PROC0_INTF0: u32 = 0x110;
    pub const PROC0_INTS0: u32 = 0x120;
    pub const PROC1_INTE0: u32 = 0x130;
    pub const PROC1_INTF0: u32 = 0x140;
    pub const PROC1_INTS0: u32 = 0x150;
}

/// The four bits each pin owns in an `INTRn`/`INTEn` word.
pub mod event {
    pub const LEVEL_LOW: u32 = 1 << 0;
    pub const LEVEL_HIGH: u32 = 1 << 1;
    pub const EDGE_LOW: u32 = 1 << 2;
    pub const EDGE_HIGH: u32 = 1 << 3;
    /// Every bit one pin's group can carry.
    pub const PER_PIN: u32 = 0xf;
}

#[derive(Debug)]
pub struct IoBank {
    regs: RegisterFile,
    status: [u32; PINS],
    ctrl: [u32; PINS],
    /// Latched edges, four bits per pin across `INTR0..3`. Level bits are
    /// computed from the pad's last level when the register is read, the way
    /// the die presents them.
    intr: [u32; 4],
    /// Per-core enable and force masks, four words each.
    inte: [[u32; 4]; 2],
    intf: [[u32; 4]; 2],
    /// The IRQ signal each pin presented last, after `IRQOVER`.
    irq_level: u32,
}

impl Default for IoBank {
    fn default() -> Self {
        Self::new()
    }
}

impl IoBank {
    pub fn new() -> Self {
        IoBank {
            regs: RegisterFile::new(0x1a0 / 4),
            status: [0; PINS],
            // Reset FUNCSEL is 0x1f (NULL) until a firmware routes the pin.
            ctrl: [u32::from(gpio_ctrl::FUNCSEL_MASK as u8); PINS],
            intr: [0; 4],
            inte: [[0; 4]; 2],
            intf: [[0; 4]; 2],
            irq_level: 0,
        }
    }

    /// The level a pin presents to the interrupt detector after `IRQOVER`:
    /// direct, inverted, forced low or forced high.
    fn irq_signal(&self, pin: usize, level: bool) -> bool {
        match (self.ctrl[pin] >> gpio_ctrl::IRQOVER_SHIFT) & 3 {
            0 => level,
            1 => !level,
            2 => false,
            _ => true,
        }
    }

    /// Feed the pad levels so the block can latch edges and report levels.
    ///
    /// A level low-to-high transition sets `EDGE_HIGH`, high-to-low sets
    /// `EDGE_LOW`; the level bits are read straight from the level a pin
    /// presents, so they clear themselves when the pin moves.
    pub fn set_pads(&mut self, levels: u32) {
        let mut signal = 0u32;
        for pin in 0..PINS {
            let level = levels & (1 << pin) != 0;
            if self.irq_signal(pin, level) {
                signal |= 1 << pin;
            }
        }
        for pin in 0..PINS {
            let before = self.irq_level & (1 << pin) != 0;
            let now = signal & (1 << pin) != 0;
            if before == now {
                continue;
            }
            let word = pin / 8;
            let shift = (pin % 8) * 4;
            self.intr[word] |= if now {
                event::EDGE_HIGH
            } else {
                event::EDGE_LOW
            } << shift;
        }
        self.irq_level = signal;
    }

    /// The live level bits of one word, as the die computes them on a read.
    fn level_bits(&self, word: usize) -> u32 {
        let mut bits = 0;
        for bit in 0..8 {
            let pin = word * 8 + bit;
            if pin >= PINS {
                break;
            }
            let shift = bit * 4;
            bits |= if self.irq_level & (1 << pin) != 0 {
                event::LEVEL_HIGH
            } else {
                event::LEVEL_LOW
            } << shift;
        }
        bits
    }

    /// `INTRn`: latched edges plus the live level bits.
    fn raw_intr(&self, word: usize) -> u32 {
        self.intr[word] | self.level_bits(word)
    }

    /// `INTSn` for one core: raw status and forced bits, gated by `INTEn`.
    fn ints(&self, core: usize, word: usize) -> u32 {
        (self.raw_intr(word) | self.intf[core][word]) & self.inte[core][word]
    }

    /// Whether core `core` has any GPIO interrupt to take.
    pub fn irq(&self, core: usize) -> bool {
        (0..4).any(|word| self.ints(core.min(1), word) != 0)
    }

    /// The latched interrupt bits for a pin, for a test that wants to see the
    /// edge detector without going through the enable mask.
    pub fn latched(&self, pin: u8) -> u32 {
        let word = usize::from(pin) / 8;
        let shift = (usize::from(pin) % 8) * 4;
        (self.intr[word] >> shift) & event::PER_PIN
    }

    fn irq_window(off: u32) -> Option<(u8, usize, usize)> {
        // (kind: 0 raw, 1 enable, 2 force, 3 status, core, word)
        let table = [
            (irq::INTR0, 0u8, 0usize),
            (irq::PROC0_INTE0, 1, 0),
            (irq::PROC0_INTF0, 2, 0),
            (irq::PROC0_INTS0, 3, 0),
            (irq::PROC1_INTE0, 1, 1),
            (irq::PROC1_INTF0, 2, 1),
            (irq::PROC1_INTS0, 3, 1),
        ];
        for (base, kind, core) in table {
            if (base..base + 16).contains(&off) {
                return Some((kind, core, ((off - base) / 4) as usize));
            }
        }
        None
    }

    fn pin_index(off: u32) -> Option<(usize, bool)> {
        // Odd offsets are STATUS, the next word is CTRL.
        let pair = off / 8;
        (pair < PINS as u32).then_some((pair as usize, off & 4 == 0))
    }

    pub fn read(&mut self, off: u32) -> u32 {
        if let Some((kind, core, word)) = Self::irq_window(off) {
            return match kind {
                0 => self.raw_intr(word),
                1 => self.inte[core][word],
                2 => self.intf[core][word],
                _ => self.ints(core, word),
            };
        }
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
        if let Some((kind, core, word)) = Self::irq_window(off) {
            match kind {
                // Edge bits are write-one-to-clear; the level bits are the
                // pin's own state and cannot be written.
                0 => self.intr[word] &= !(value & 0xcccc_cccc),
                1 => self.inte[core][word] = value,
                2 => self.intf[core][word] = value,
                _ => {}
            }
            return;
        }
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

    /// The whole `GPIOx_CTRL` word, which carries the function select and the
    /// four `*OVER` fields.
    pub fn control(&self, pin: u8) -> u32 {
        self.ctrl.get(usize::from(pin)).copied().unwrap_or(0)
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

    /// Whether the pad's outputs are cut off (`PADS_BANK0.GPIOx.OD`), which
    /// holds the pad high-impedance whatever the function select drives.
    pub fn output_disabled(&self, pin: u8) -> bool {
        self.pads
            .get(usize::from(pin))
            .is_some_and(|pad| pad & pad::OD != 0)
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
