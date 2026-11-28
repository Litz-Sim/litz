//! The address space: 16 KiB of flash, 2 KiB of SRAM, the peripheral blocks,
//! the PFIC and SysTick.
//!
//! The map is the CH32V003's. Like the RP2040, the decoder is a table of named
//! blocks, an unimplemented offset inside a block reads back what was written,
//! and an address outside every block is a bus fault that names the address.
//! Flash is readable directly and writable only through the flash controller,
//! which sits in the bus because programming needs the array and the
//! controller state together.

use crate::cpu::{Bus, Fault};
use crate::peripherals::{FlashController, Peripherals};

/// 16 KiB of flash at address zero.
pub const FLASH_SIZE: usize = 16 * 1024;
/// 2 KiB of SRAM.
pub const SRAM_SIZE: usize = 2 * 1024;
pub const FLASH_BASE: u32 = 0x0000_0000;
pub const SRAM_BASE: u32 = 0x2000_0000;

pub const AFIO_BASE: u32 = 0x4001_0000;
pub const EXTI_BASE: u32 = 0x4001_0400;
pub const GPIOA_BASE: u32 = 0x4001_0800;
pub const GPIOC_BASE: u32 = 0x4001_1000;
pub const GPIOD_BASE: u32 = 0x4001_1400;
pub const ADC1_BASE: u32 = 0x4001_2400;
pub const TIM1_BASE: u32 = 0x4001_2c00;
pub const SPI1_BASE: u32 = 0x4001_3000;
pub const USART1_BASE: u32 = 0x4001_3800;
pub const TIM2_BASE: u32 = 0x4000_0000;
pub const WWDG_BASE: u32 = 0x4000_2c00;
pub const IWDG_BASE: u32 = 0x4000_3000;
pub const I2C1_BASE: u32 = 0x4000_5400;
pub const PWR_BASE: u32 = 0x4000_7000;
pub const RCC_BASE: u32 = 0x4002_1000;
pub const FLASH_CTRL_BASE: u32 = 0x4002_2000;
pub const PFIC_BASE: u32 = 0xe000_e000;
pub const SYSTICK_BASE: u32 = 0xe000_f000;

/// Every named block the decoder knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Afio,
    Exti,
    GpioA,
    GpioC,
    GpioD,
    Adc1,
    Tim1,
    Tim2,
    Spi1,
    Usart1,
    I2c1,
    Pwr,
    Rcc,
    Iwdg,
    Wwdg,
    Flash,
    Pfic,
    Systick,
    Unmapped,
}

impl Block {
    /// The block's base and size, or `None` for an address outside the map.
    pub fn locate(addr: u32) -> Option<(Block, u32)> {
        const TABLE: &[(u32, Block, u32)] = &[
            (TIM2_BASE, Block::Tim2, 0x400),
            (WWDG_BASE, Block::Wwdg, 0x400),
            (IWDG_BASE, Block::Iwdg, 0x400),
            (I2C1_BASE, Block::I2c1, 0x400),
            (PWR_BASE, Block::Pwr, 0x400),
            (AFIO_BASE, Block::Afio, 0x400),
            (EXTI_BASE, Block::Exti, 0x400),
            (GPIOA_BASE, Block::GpioA, 0x400),
            (GPIOC_BASE, Block::GpioC, 0x400),
            (GPIOD_BASE, Block::GpioD, 0x400),
            (ADC1_BASE, Block::Adc1, 0x400),
            (TIM1_BASE, Block::Tim1, 0x400),
            (SPI1_BASE, Block::Spi1, 0x400),
            (USART1_BASE, Block::Usart1, 0x400),
            (RCC_BASE, Block::Rcc, 0x400),
            (FLASH_CTRL_BASE, Block::Flash, 0x400),
            (PFIC_BASE, Block::Pfic, 0x600),
            (SYSTICK_BASE, Block::Systick, 0x10),
        ];
        TABLE
            .iter()
            .find(|(base, _, size)| addr >= *base && addr < *base + *size)
            .map(|(base, block, _)| (*block, *base))
    }
}

/// Whether an address lands in flash, SRAM or a named peripheral block.
fn mapped(addr: u32) -> bool {
    (FLASH_BASE..FLASH_BASE + FLASH_SIZE as u32).contains(&addr)
        || (SRAM_BASE..SRAM_BASE + SRAM_SIZE as u32).contains(&addr)
        || Block::locate(addr).is_some()
}

/// The core's view of memory.
pub struct Memory {
    pub flash: Vec<u8>,
    pub sram: Vec<u8>,
    pub periph: Peripherals,
    pub flash_controller: FlashController,
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Memory {
    pub fn new() -> Self {
        Memory {
            flash: vec![0xff; FLASH_SIZE],
            sram: vec![0; SRAM_SIZE],
            periph: Peripherals::new(),
            flash_controller: FlashController::new(),
        }
    }

    /// Write a firmware image into flash at `offset`.
    pub fn load_flash(&mut self, offset: usize, bytes: &[u8]) -> Result<(), String> {
        let end = offset
            .checked_add(bytes.len())
            .ok_or_else(|| "image offset overflows the flash".to_string())?;
        if end > self.flash.len() {
            return Err(format!(
                "image of {} bytes at {offset:#x} does not fit in {} bytes of flash",
                bytes.len(),
                self.flash.len()
            ));
        }
        self.flash[offset..end].copy_from_slice(bytes);
        Ok(())
    }

    fn read_word(&mut self, addr: u32) -> Result<u32, Fault> {
        if (FLASH_BASE..FLASH_BASE + FLASH_SIZE as u32).contains(&addr) {
            let offset = (addr - FLASH_BASE) as usize & !3;
            return Ok(u32::from_le_bytes([
                self.flash[offset],
                self.flash[offset + 1],
                self.flash[offset + 2],
                self.flash[offset + 3],
            ]));
        }
        if (SRAM_BASE..SRAM_BASE + SRAM_SIZE as u32).contains(&addr) {
            let offset = (addr - SRAM_BASE) as usize & !3;
            return Ok(u32::from_le_bytes([
                self.sram[offset],
                self.sram[offset + 1],
                self.sram[offset + 2],
                self.sram[offset + 3],
            ]));
        }
        if let Some((block, base)) = Block::locate(addr) {
            let offset = addr - base;
            return Ok(match block {
                Block::Flash => self.flash_controller.read(offset),
                _ => self.periph.block_read(block, offset),
            });
        }
        Err(Fault::Bus {
            pc: 0,
            addr,
            write: false,
        })
    }

    fn write_word(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
        if (SRAM_BASE..SRAM_BASE + SRAM_SIZE as u32).contains(&addr) {
            let offset = (addr - SRAM_BASE) as usize & !3;
            self.sram[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            return Ok(());
        }
        if (FLASH_BASE..FLASH_BASE + FLASH_SIZE as u32).contains(&addr) {
            // The core may only write flash through the controller's unlock
            // and program sequence; a locked array ignores the store.
            self.flash_controller
                .write_word(&mut self.flash, addr, value);
            return Ok(());
        }
        if let Some((block, base)) = Block::locate(addr) {
            let offset = addr - base;
            match block {
                Block::Flash => self.flash_controller.write(offset, value, &mut self.flash),
                _ => self.periph.block_write(block, offset, value),
            }
            return Ok(());
        }
        Err(Fault::Bus {
            pc: 0,
            addr,
            write: true,
        })
    }

    /// A byte read that never disturbs a FIFO, for the debug surface.
    pub fn debug_read_byte(&self, addr: u32) -> Option<u8> {
        if (FLASH_BASE..FLASH_BASE + FLASH_SIZE as u32).contains(&addr) {
            Some(self.flash[(addr - FLASH_BASE) as usize])
        } else if (SRAM_BASE..SRAM_BASE + SRAM_SIZE as u32).contains(&addr) {
            Some(self.sram[(addr - SRAM_BASE) as usize])
        } else {
            None
        }
    }
}

impl Bus for Memory {
    fn read8(&mut self, addr: u32) -> Result<u32, Fault> {
        let word = self.read_word(addr & !3)?;
        Ok((word >> ((addr & 3) * 8)) & 0xff)
    }

    fn read16(&mut self, addr: u32) -> Result<u32, Fault> {
        if addr & 1 != 0 {
            return Err(Fault::Alignment {
                pc: 0,
                addr,
                width: 2,
            });
        }
        let word = self.read_word(addr & !3)?;
        Ok((word >> ((addr & 2) * 8)) & 0xffff)
    }

    fn read32(&mut self, addr: u32) -> Result<u32, Fault> {
        if addr & 3 != 0 {
            return Err(Fault::Alignment {
                pc: 0,
                addr,
                width: 4,
            });
        }
        self.read_word(addr)
    }

    fn write8(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
        if !mapped(addr) {
            return Err(Fault::Bus {
                pc: 0,
                addr,
                write: true,
            });
        }
        if (SRAM_BASE..SRAM_BASE + SRAM_SIZE as u32).contains(&addr) {
            self.sram[(addr - SRAM_BASE) as usize] = value as u8;
            return Ok(());
        }
        let word = self.read_word(addr & !3)?;
        let shift = (addr & 3) * 8;
        let word = (word & !(0xff << shift)) | ((value & 0xff) << shift);
        self.write_word(addr & !3, word)
    }

    fn write16(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
        if !mapped(addr) {
            return Err(Fault::Bus {
                pc: 0,
                addr,
                write: true,
            });
        }
        if addr & 1 != 0 {
            return Err(Fault::Alignment {
                pc: 0,
                addr,
                width: 2,
            });
        }
        if (SRAM_BASE..SRAM_BASE + SRAM_SIZE as u32).contains(&addr) {
            let offset = (addr - SRAM_BASE) as usize;
            self.sram[offset..offset + 2].copy_from_slice(&(value as u16).to_le_bytes());
            return Ok(());
        }
        let word = self.read_word(addr & !3)?;
        let shift = (addr & 2) * 8;
        let word = (word & !(0xffff << shift)) | ((value & 0xffff) << shift);
        self.write_word(addr & !3, word)
    }

    fn write32(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
        if addr & 3 != 0 {
            return Err(Fault::Alignment {
                pc: 0,
                addr,
                width: 4,
            });
        }
        self.write_word(addr, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::Bus;

    #[test]
    fn flash_and_sram_are_flat_arrays() {
        let mut mem = Memory::new();
        mem.load_flash(0, &[1, 2, 3, 4]).unwrap();
        assert_eq!(mem.read32(0).unwrap(), 0x0403_0201);
        mem.write32(SRAM_BASE + 0x10, 0xdead_beef).unwrap();
        assert_eq!(mem.read8(SRAM_BASE + 0x10).unwrap(), 0xef);
        assert_eq!(mem.read16(SRAM_BASE + 0x12).unwrap(), 0xdead);
    }

    #[test]
    fn an_address_outside_every_block_faults() {
        let mut mem = Memory::new();
        assert!(matches!(
            mem.read32(0x3000_0000),
            Err(Fault::Bus { write: false, .. })
        ));
        assert!(matches!(
            mem.write8(0x1000_0000, 0),
            Err(Fault::Bus { write: true, .. })
        ));
    }

    #[test]
    fn an_unimplemented_register_reads_back_what_was_written() {
        let mut mem = Memory::new();
        // A GPIO register the model does not interpret (LCKR is at 0x18).
        mem.write32(GPIOA_BASE + 0x18, 0x1234).unwrap();
        assert_eq!(mem.read32(GPIOA_BASE + 0x18).unwrap(), 0x1234);
    }

    #[test]
    fn flash_writes_go_through_the_controller() {
        use crate::peripherals::flash::{ctlr, reg};
        let mut mem = Memory::new();
        mem.write32(FLASH_CTRL_BASE + reg::KEYR, 0x4567_0123)
            .unwrap();
        mem.write32(FLASH_CTRL_BASE + reg::KEYR, 0xcdef_89ab)
            .unwrap();
        mem.write32(FLASH_CTRL_BASE + reg::CTLR, ctlr::PG).unwrap();
        mem.write32(0x100, 0x1122_3344).unwrap();
        assert_eq!(mem.read32(0x100).unwrap(), 0x1122_3344);
    }
}
