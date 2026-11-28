//! The CH32V003 peripheral blocks, one module each.
//!
//! Every block follows the same shape as the RP2040's: it owns a
//! [`RegisterFile`] for the offsets it does not give meaning to, `read`/`write`
//! take offsets from the block's own base, and `tick` advances it by CPU
//! cycles. None of them knows about the core; the machine asks each block for
//! its interrupt line and lets the PFIC decide.
//!
//! The GPIO block and the flash controller straddle the rest of the chip (the
//! machine feeds pad levels in, and flash writes need the array), so the
//! machine and the bus drive those directly.

use crate::bus::Block;

pub mod adc;
pub mod flash;
pub mod gpio;
pub mod i2c;
pub mod pfic;
pub mod rcc;
pub mod spi;
pub mod timers;
pub mod usart;
pub mod watchdog;

pub use adc::Adc;
pub use flash::FlashController;
pub use gpio::{Afio, Exti, Gpio, Port};
pub use i2c::I2c;
pub use pfic::Pfic;
pub use rcc::{Pwr, Rcc};
pub use spi::Spi;
pub use timers::{SysTick, Timer};
pub use usart::Usart;
pub use watchdog::{Iwdg, Wwdg};

/// Plain storage for registers a block does not model.
#[derive(Clone, Debug, Default)]
pub struct RegisterFile {
    words: Vec<u32>,
}

impl RegisterFile {
    pub fn new(words: usize) -> Self {
        RegisterFile {
            words: vec![0; words],
        }
    }

    pub fn read(&self, offset: u32) -> u32 {
        self.words.get((offset / 4) as usize).copied().unwrap_or(0)
    }

    pub fn write(&mut self, offset: u32, value: u32) {
        if let Some(word) = self.words.get_mut((offset / 4) as usize) {
            *word = value;
        }
    }
}

/// The whole peripheral space.
#[derive(Debug)]
pub struct Peripherals {
    pub gpio: Gpio,
    pub afio: Afio,
    pub exti: Exti,
    pub pfic: Pfic,
    pub rcc: Rcc,
    pub pwr: Pwr,
    pub tim1: Timer,
    pub tim2: Timer,
    pub systick: SysTick,
    pub usart: Usart,
    pub i2c: I2c,
    pub spi: Spi,
    pub adc: Adc,
    pub iwdg: Iwdg,
    pub wwdg: Wwdg,
    /// Registers of anything the map names but the model does not.
    pub unmapped: RegisterFile,
}

impl Default for Peripherals {
    fn default() -> Self {
        Self::new()
    }
}

impl Peripherals {
    pub fn new() -> Self {
        Peripherals {
            gpio: Gpio::new(),
            afio: Afio::new(),
            exti: Exti::new(),
            pfic: Pfic::new(),
            rcc: Rcc::new(),
            pwr: Pwr::new(),
            tim1: Timer::new(),
            tim2: Timer::new(),
            systick: SysTick::new(),
            usart: Usart::new(),
            i2c: I2c::new(),
            spi: Spi::new(),
            adc: Adc::new(),
            iwdg: Iwdg::new(),
            wwdg: Wwdg::new(),
            unmapped: RegisterFile::new(0x400 / 4),
        }
    }

    pub fn block_read(&mut self, block: Block, offset: u32) -> u32 {
        match block {
            Block::Afio => self.afio.read(offset),
            Block::Exti => self.exti.read(offset),
            Block::GpioA => self.gpio.a.read(offset),
            Block::GpioC => self.gpio.c.read(offset),
            Block::GpioD => self.gpio.d.read(offset),
            Block::Adc1 => self.adc.read(offset),
            Block::Tim1 => self.tim1.read(offset),
            Block::Tim2 => self.tim2.read(offset),
            Block::Spi1 => self.spi.read(offset),
            Block::Usart1 => self.usart.read(offset),
            Block::I2c1 => self.i2c.read(offset),
            Block::Pwr => self.pwr.read(offset),
            Block::Rcc => self.rcc.read(offset),
            Block::Iwdg => self.iwdg.read(offset),
            Block::Wwdg => self.wwdg.read(offset),
            Block::Pfic => self.pfic.read(offset),
            Block::Systick => self.systick.read(offset),
            // The flash controller is read by the bus so it can see the array.
            Block::Flash => 0,
            Block::Unmapped => self.unmapped.read(offset),
        }
    }

    pub fn block_write(&mut self, block: Block, offset: u32, value: u32) {
        match block {
            Block::Afio => self.afio.write(offset, value),
            Block::Exti => self.exti.write(offset, value),
            Block::GpioA => self.gpio.a.write(offset, value),
            Block::GpioC => self.gpio.c.write(offset, value),
            Block::GpioD => self.gpio.d.write(offset, value),
            Block::Adc1 => self.adc.write(offset, value),
            Block::Tim1 => self.tim1.write(offset, value),
            Block::Tim2 => self.tim2.write(offset, value),
            Block::Spi1 => self.spi.write(offset, value),
            Block::Usart1 => self.usart.write(offset, value),
            Block::I2c1 => self.i2c.write(offset, value),
            Block::Pwr => self.pwr.write(offset, value),
            Block::Rcc => self.rcc.write(offset, value),
            Block::Iwdg => self.iwdg.write(offset, value),
            Block::Wwdg => self.wwdg.write(offset, value),
            Block::Pfic => self.pfic.write(offset, value),
            Block::Systick => self.systick.write(offset, value),
            Block::Flash => {}
            Block::Unmapped => self.unmapped.write(offset, value),
        }
    }

    /// Sample the external-interrupt lines from the pads they watch.
    pub fn sample_exti(&mut self) {
        let mut levels = [None; 8];
        for line in 0..8u8 {
            let port = self.afio.exti_port(line);
            if let Some(gpio) = gpio::pin_number(port, line) {
                levels[usize::from(line)] = Some(self.gpio.level(gpio));
            }
        }
        self.exti.sample(levels);
    }
}
