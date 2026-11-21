//! The reset controller: one bit per peripheral plus the watchdog-selected
//! reset source.
//!
//! Every block starts out of reset. Writing a one to `RESET` holds that block
//! in reset, and writing a zero releases it again; `RESET_DONE` reports the
//! inverse, which is the handshake a firmware uses after power-on:
//!
//! ```text
//! resets.RESET = 0;              // release everything
//! while (resets.RESET_DONE != all_mask) {}
//! ```
//!
//! The alias windows in [`super::block_write`] mean a firmware can also write
//! `RESET_CLR` with a mask to release selected blocks without disturbing the
//! rest, which is how the Pico SDK's `reset_block`/`unreset_block` pair works.

use super::RegisterFile;

/// Register offsets from the RESETS base.
pub mod reg {
    /// Write-only: a set bit holds that block in reset.
    pub const RESET: u32 = 0x00;
    /// Which blocks the watchdog resets.
    pub const WDSEL: u32 = 0x04;
    /// A set bit means the block is out of reset.
    pub const RESET_DONE: u32 = 0x08;
}

#[derive(Debug)]
pub struct Resets {
    regs: RegisterFile,
    /// Blocks currently held in reset.
    held: u32,
    wdsel: u32,
}

impl Default for Resets {
    fn default() -> Self {
        Self::new()
    }
}

impl Resets {
    pub fn new() -> Self {
        Resets {
            regs: RegisterFile::new(0x100 / 4),
            held: 0,
            wdsel: 0,
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            // RESET is write-only; reading it returns the last thing written
            // by the store file below only if a firmware relies on a shadow.
            reg::RESET => self.held,
            reg::WDSEL => self.wdsel,
            reg::RESET_DONE => !self.held,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::RESET => {
                // A one both asserts and keeps the reset; a zero releases.
                self.held = value;
            }
            reg::WDSEL => self.wdsel = value,
            reg::RESET_DONE => {}
            off => self.regs.write(off, value),
        }
    }

    pub fn tick(&mut self, _cycles: u64) {}

    pub fn irq(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_block_starts_out_of_reset() {
        let mut resets = Resets::new();
        assert_eq!(resets.read(reg::RESET_DONE), 0xffff_ffff);
    }

    #[test]
    fn reset_handshake() {
        let mut resets = Resets::new();
        // Bit 9 is the UART in the RP2040's reset map; the model does not
        // interpret the bit, it only reports it back.
        resets.write(reg::RESET, 1 << 9);
        assert_eq!(resets.read(reg::RESET_DONE) & (1 << 9), 0);
        assert_ne!(resets.read(reg::RESET_DONE) & (1 << 8), 0);
        resets.write(reg::RESET, 0);
        assert_ne!(resets.read(reg::RESET_DONE) & (1 << 9), 0);
    }

    #[test]
    fn watchdog_select_is_storage() {
        let mut resets = Resets::new();
        resets.write(reg::WDSEL, 0x123);
        assert_eq!(resets.read(reg::WDSEL), 0x123);
    }
}
