//! The 1 KiB EEPROM.
//!
//! `EEAR` selects the byte, `EEDR` carries it and `EECR` starts the access.
//! A read (`EERE`) completes immediately and self-clears. A write follows the
//! datasheet's two-step protection: `EEMPE` must be set first (it clears
//! itself after four cycles) and `EEPE` starts the write, which holds `EEPE`
//! high for 3.4 ms of simulated clock — 54 400 CPU cycles at 16 MHz. The
//! EEPROM-ready interrupt flag rises when the write completes.

/// EEPROM size in bytes.
pub const EEPROM_SIZE: usize = 1024;

/// Cycles `EEPE` stays set for one write: 3.4 ms at 16 MHz.
pub const WRITE_CYCLES: u32 = 54_400;

/// The EEPROM array and its registers.
pub struct Eeprom {
    /// Writable control bits: `EERIE`, `EEPM1:0`.
    control: u8,
    pub eedr: u8,
    /// `EEARH:EEARL`, ten bits.
    address: u16,
    memory: [u8; EEPROM_SIZE],
    master_enable: bool,
    master_enable_remaining: u32,
    write_remaining: u32,
    busy: bool,
    ready_flag: bool,
}

impl Default for Eeprom {
    fn default() -> Self {
        Self {
            control: 0,
            eedr: 0,
            address: 0,
            memory: [0xff; EEPROM_SIZE],
            master_enable: false,
            master_enable_remaining: 0,
            write_remaining: 0,
            busy: false,
            ready_flag: false,
        }
    }
}

impl Eeprom {
    /// The byte array, for a host that wants to preload or inspect it.
    pub fn memory(&self) -> &[u8; EEPROM_SIZE] {
        &self.memory
    }

    /// Mutable access, for a host that preloads a calibration or a program
    /// that expects a previous run's data.
    pub fn memory_mut(&mut self) -> &mut [u8; EEPROM_SIZE] {
        &mut self.memory
    }

    /// Whether a write is in progress.
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Ready for a new access: no write in progress.
    pub fn ready(&self) -> bool {
        !self.busy
    }

    /// `EEAR` as a ten-bit address.
    pub fn address(&self) -> u16 {
        self.address & 0x03ff
    }

    pub(crate) fn read_low(&self) -> u8 {
        self.address as u8
    }

    pub(crate) fn read_high(&self) -> u8 {
        ((self.address >> 8) & 0x03) as u8
    }

    pub(crate) fn write_low(&mut self, value: u8) {
        self.address = (self.address & 0x0300) | u16::from(value);
    }

    pub(crate) fn write_high(&mut self, value: u8) {
        self.address = (self.address & 0x00ff) | (u16::from(value & 0x03) << 8);
    }

    /// `EECR` as firmware reads it.
    pub(crate) fn read_control(&self) -> u8 {
        (self.control & 0x38)
            | if self.master_enable { 0x04 } else { 0 }
            | if self.busy { 0x02 } else { 0 }
    }

    pub(crate) fn write_control(&mut self, value: u8) {
        self.control = value & 0x38;
        if value & 0x04 != 0 {
            self.master_enable = true;
            self.master_enable_remaining = 4;
        }
        if value & 0x02 != 0 && self.master_enable && !self.busy {
            self.busy = true;
            self.write_remaining = WRITE_CYCLES;
            self.master_enable = false;
            self.master_enable_remaining = 0;
        }
        if value & 0x01 != 0 && !self.busy {
            let address = usize::from(self.address());
            self.eedr = self.memory[address];
        }
    }

    pub(crate) fn advance(&mut self, cycles: u64) {
        if self.master_enable_remaining > 0 {
            let elapsed = cycles.min(u64::from(self.master_enable_remaining)) as u32;
            self.master_enable_remaining -= elapsed;
            if self.master_enable_remaining == 0 {
                self.master_enable = false;
            }
        }
        if !self.busy {
            return;
        }
        if cycles >= u64::from(self.write_remaining) {
            let address = usize::from(self.address());
            self.memory[address] = self.eedr;
            self.write_remaining = 0;
            self.busy = false;
            self.ready_flag = true;
        } else {
            self.write_remaining -= cycles as u32;
        }
    }

    pub(crate) fn interrupt_pending(&self) -> bool {
        self.control & 0x08 != 0 && self.ready_flag
    }

    pub(crate) fn can_wake(&self) -> bool {
        self.control & 0x08 != 0 && (self.busy || self.ready_flag)
    }

    pub(crate) fn acknowledge(&mut self) {
        self.ready_flag = false;
    }
}
