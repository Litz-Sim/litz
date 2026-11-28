//! The flash controller: unlock, page erase, word program and the option
//! bytes.
//!
//! The controller gates every write to the flash array. `KEYR` takes the two
//! unlock keys (`0x4567_0123`, then `0xCDEF_89AB`); with the array unlocked,
//! `CTLR.PG` lets a word written to a flash address program it, and
//! `CTLR.PER` erases the 64-byte page named by `ADDR`. Programming ANDs into
//! the erased byte, as NOR flash does, so a second write can only clear bits.
//!
//! `STATR.EOP` records the last operation; writing a one clears it. `LOCK`
//! relocks the array. The option bytes are storage. Fast programming
//! (`MODEKEYR`) is accepted and behaves like a word program, which is enough
//! for a firmware that keeps calibration data in a page it erases and rewrites.

pub mod reg {
    pub const ACTLR: u32 = 0x00;
    pub const KEYR: u32 = 0x04;
    pub const OBKEYR: u32 = 0x08;
    pub const STATR: u32 = 0x0c;
    pub const CTLR: u32 = 0x10;
    pub const ADDR: u32 = 0x14;
    pub const OBWR: u32 = 0x18;
    pub const WRPR: u32 = 0x1c;
    pub const MODEKEYR: u32 = 0x20;
}

pub mod statr {
    pub const BSY: u32 = 1 << 0;
    pub const WRPRTERR: u32 = 1 << 4;
    pub const EOP: u32 = 1 << 5;
}

pub mod ctlr {
    pub const PG: u32 = 1 << 0;
    pub const PER: u32 = 1 << 1;
    pub const MER: u32 = 1 << 2;
    pub const LOCK: u32 = 1 << 7;
    pub const OBL_LAUNCH: u32 = 1 << 13;
}

/// The erase granularity of the CH32V003 array.
pub const PAGE_SIZE: usize = 64;

const KEY1: u32 = 0x4567_0123;
const KEY2: u32 = 0xcdef_89ab;

#[derive(Debug)]
pub struct FlashController {
    ctlr: u32,
    addr: u32,
    eop: bool,
    obw: u32,
    wrpr: u32,
    actlr: u32,
    unlocked: bool,
    key_stage: bool,
}

impl Default for FlashController {
    fn default() -> Self {
        Self::new()
    }
}

impl FlashController {
    pub fn new() -> Self {
        FlashController {
            ctlr: ctlr::LOCK,
            addr: 0,
            eop: false,
            obw: 0xffff_ffff,
            wrpr: 0xffff_ffff,
            actlr: 0,
            unlocked: false,
            key_stage: false,
        }
    }

    pub fn reset(&mut self) {
        *self = FlashController::new();
    }

    fn erase_page(&mut self, flash: &mut [u8], addr: u32) {
        let offset = (addr as usize) & !(PAGE_SIZE - 1);
        if let Some(page) = flash.get_mut(offset..offset + PAGE_SIZE) {
            page.fill(0xff);
        }
        self.eop = true;
    }

    /// Program a word, which the bus calls for a write into the flash window.
    pub fn write_word(&mut self, flash: &mut [u8], addr: u32, value: u32) {
        if !self.unlocked || self.ctlr & ctlr::PG == 0 {
            return;
        }
        let offset = (addr as usize) & !3;
        if offset + 4 <= flash.len() {
            for (index, byte) in value.to_le_bytes().iter().enumerate() {
                flash[offset + index] &= *byte;
            }
            self.eop = true;
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::ACTLR => self.actlr,
            reg::STATR => {
                if self.eop {
                    statr::EOP
                } else {
                    0
                }
            }
            reg::CTLR => self.ctlr,
            reg::ADDR => self.addr,
            reg::OBWR => self.obw,
            reg::WRPR => self.wrpr,
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, value: u32, flash: &mut [u8]) {
        match off {
            reg::ACTLR => self.actlr = value,
            reg::KEYR => {
                if value == KEY1 {
                    self.key_stage = true;
                } else if self.key_stage && value == KEY2 {
                    self.unlocked = true;
                    self.key_stage = false;
                } else {
                    self.key_stage = false;
                }
            }
            reg::OBKEYR | reg::OBWR | reg::WRPR => {
                // Option bytes are storage in this model.
                match off {
                    reg::OBWR => self.obw = value,
                    reg::WRPR => self.wrpr = value,
                    _ => {}
                }
            }
            reg::STATR => {
                if value & statr::EOP != 0 {
                    self.eop = false;
                }
            }
            reg::CTLR => {
                self.ctlr = value;
                if value & ctlr::LOCK != 0 {
                    self.unlocked = false;
                }
            }
            reg::ADDR => {
                self.addr = value;
                if self.unlocked && self.ctlr & ctlr::PER != 0 {
                    self.erase_page(flash, value);
                }
            }
            reg::MODEKEYR => {
                // `0x4002_2024` unlocks fast programming; a word written
                // afterwards programs like `PG`.
                if value == 0x4002_2024 {
                    self.unlocked = true;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unlocked_page_erases_and_programs() {
        let mut flash = vec![0x12u8; 128];
        let mut controller = FlashController::new();
        controller.write(reg::KEYR, KEY1, &mut flash);
        controller.write(reg::KEYR, KEY2, &mut flash);
        controller.write(reg::CTLR, ctlr::PER, &mut flash);
        controller.write(reg::ADDR, 0x40, &mut flash);
        assert!(flash[64..].iter().all(|byte| *byte == 0xff));
        assert_eq!(flash[0], 0x12);

        controller.write(reg::CTLR, ctlr::PG, &mut flash);
        controller.write_word(&mut flash, 0x44, 0x0f0f_0f0f);
        assert_eq!(&flash[0x44..0x48], &[0x0f; 4]);
        // A second program can only clear bits.
        controller.write_word(&mut flash, 0x44, 0xff00_ff00);
        assert_eq!(&flash[0x44..0x48], &[0x00, 0x0f, 0x00, 0x0f]);
    }

    #[test]
    fn a_locked_controller_ignores_writes() {
        let mut flash = vec![0xffu8; 64];
        let mut controller = FlashController::new();
        controller.write(reg::CTLR, ctlr::PG, &mut flash);
        controller.write_word(&mut flash, 0, 0);
        assert_eq!(flash[0], 0xff);
    }
}
