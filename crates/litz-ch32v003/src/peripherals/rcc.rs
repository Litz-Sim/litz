//! RCC and PWR: the clock tree and the power control registers.
//!
//! The CH32V003 starts on the 24 MHz HSI and reaches its 48 MHz maximum
//! through the PLL. The model derives the core, APB1 and APB2 rates from
//! `CFGR0` so the timers and USART divide the clock the firmware actually
//! selected; the CPU itself is always stepped at the 48 MHz the [`Chip`]
//! contract advertises, so simulated time is the same whatever the tree says.
//!
//! Every oscillator is instantly ready (`HSIRDY`, `HSERDY`, `PLLRDY` read
//! high) and the switch-complete bits (`SWS`) follow `SW` in the same write.
//! The clock-gate registers are storage: the model does not stop a peripheral
//! whose gate is closed, so hand-written firmware need not enable a peripheral
//! it uses.
//!
//! [`Chip`]: litz_board::Chip

use super::RegisterFile;

pub mod reg {
    pub const CTLR: u32 = 0x00;
    pub const CFGR0: u32 = 0x04;
    pub const INTR: u32 = 0x08;
    pub const APB2PRSTR: u32 = 0x0c;
    pub const APB1PRSTR: u32 = 0x10;
    pub const AHBPCENR: u32 = 0x14;
    pub const APB2PCENR: u32 = 0x18;
    pub const APB1PCENR: u32 = 0x1c;
    pub const BDCTLR: u32 = 0x20;
    pub const RSTSCKR: u32 = 0x24;
}

pub mod ctlr {
    pub const HSION: u32 = 1 << 0;
    pub const HSIRDY: u32 = 1 << 1;
    pub const HSEON: u32 = 1 << 16;
    pub const HSERDY: u32 = 1 << 17;
    pub const PLLON: u32 = 1 << 24;
    pub const PLLRDY: u32 = 1 << 25;
}

pub mod cfgr0 {
    pub const SW_MASK: u32 = 0b11;
    pub const SWS_SHIFT: u32 = 2;
    pub const SWS_MASK: u32 = 0b11 << SWS_SHIFT;
    pub const HPRE_SHIFT: u32 = 4;
    pub const PPRE1_SHIFT: u32 = 8;
    pub const PPRE2_SHIFT: u32 = 11;
    pub const PLLMUL_SHIFT: u32 = 18;
    pub const PLLMUL_MASK: u32 = 0xf;
}

/// The HSI frequency the part resets to.
pub const HSI_HZ: u64 = 24_000_000;

#[derive(Debug)]
pub struct Rcc {
    regs: RegisterFile,
}

impl Default for Rcc {
    fn default() -> Self {
        Self::new()
    }
}

impl Rcc {
    pub fn new() -> Self {
        let mut regs = RegisterFile::new(0x100 / 4);
        regs.write(reg::CTLR, ctlr::HSION);
        Rcc { regs }
    }

    fn config(&self) -> u32 {
        self.regs.read(reg::CFGR0)
    }

    /// The four-bit `HPRE` field, in the STM32F0 encoding where 0–7 means
    /// "divide by 1" and 8–15 divide by `2^(field - 7)`.
    fn divide(field: u32) -> u64 {
        if field < 8 { 1 } else { 1u64 << (field - 7) }
    }

    /// The three-bit `PPRE1`/`PPRE2` fields: `0xx` is undivided, then
    /// `100`=/2, `101`=/4, `110`=/8 and `111`=/16.
    fn divide_apb(field: u32) -> u64 {
        match field {
            0..=3 => 1,
            4 => 2,
            5 => 4,
            6 => 8,
            _ => 16,
        }
    }

    /// The system clock in hertz.
    pub fn sysclk_hz(&self) -> u64 {
        let source = self.config() & cfgr0::SW_MASK;
        match source {
            // HSE is not fitted on the bare part; a firmware that selects it
            // still gets a clock, but at the crystal the model knows.
            1 => 24_000_000,
            2 => {
                let field = (self.config() >> cfgr0::PLLMUL_SHIFT) & cfgr0::PLLMUL_MASK;
                // PLLMUL is the multiplier minus two: 0 -> x2, 2 -> x4.
                let multiplier = u64::from(field) + 2;
                (HSI_HZ * multiplier).min(48_000_000)
            }
            _ => HSI_HZ,
        }
    }

    pub fn hclk_hz(&self) -> u64 {
        self.sysclk_hz() / Self::divide((self.config() >> cfgr0::HPRE_SHIFT) & 0xf)
    }

    pub fn apb1_hz(&self) -> u64 {
        self.sysclk_hz() / Self::divide_apb((self.config() >> cfgr0::PPRE1_SHIFT) & 0x7)
    }

    pub fn apb2_hz(&self) -> u64 {
        self.sysclk_hz() / Self::divide_apb((self.config() >> cfgr0::PPRE2_SHIFT) & 0x7)
    }

    /// A timer on APB1 or APB2 runs at the bus clock, doubled when the bus is
    /// divided, which is the STM32 rule.
    pub fn timer_hz(&self, apb_hz: u64) -> u64 {
        if apb_hz == self.sysclk_hz() {
            apb_hz
        } else {
            apb_hz * 2
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::CTLR => {
                let value = self.regs.read(reg::CTLR);
                // Every oscillator is instantly ready.
                value
                    | ctlr::HSIRDY
                    | if value & ctlr::HSEON != 0 {
                        ctlr::HSERDY
                    } else {
                        0
                    }
                    | if value & ctlr::PLLON != 0 {
                        ctlr::PLLRDY
                    } else {
                        0
                    }
            }
            reg::CFGR0 => {
                let config = self.config();
                // SWS reports the source actually selected.
                (config & !cfgr0::SWS_MASK) | ((config & cfgr0::SW_MASK) << cfgr0::SWS_SHIFT)
            }
            reg::INTR => 0,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::CTLR => self.regs.write(reg::CTLR, value),
            reg::CFGR0 => self.regs.write(reg::CFGR0, value),
            reg::INTR => {}
            off => self.regs.write(off, value),
        }
    }
}

/// PWR: the power control register. `CSR1.PVDO` is set (the supply is above
/// the programmable threshold) and everything else is storage.
#[derive(Debug)]
pub struct Pwr {
    regs: RegisterFile,
}

impl Default for Pwr {
    fn default() -> Self {
        Self::new()
    }
}

impl Pwr {
    pub fn new() -> Self {
        Pwr {
            regs: RegisterFile::new(0x100 / 4),
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x04 => self.regs.read(off) | (1 << 1),
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        self.regs.write(off, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reset_clock_is_the_24_mhz_hsi() {
        let rcc = Rcc::new();
        assert_eq!(rcc.sysclk_hz(), 24_000_000);
        assert_eq!(rcc.apb2_hz(), 24_000_000);
    }

    #[test]
    fn the_pll_doubles_the_hsi_to_the_48_mhz_maximum() {
        let mut rcc = Rcc::new();
        rcc.write(reg::CTLR, ctlr::HSION | ctlr::PLLON);
        // SW = PLL, PLLMUL field = 0 (x2).
        rcc.write(reg::CFGR0, 2);
        assert_eq!(rcc.sysclk_hz(), 48_000_000);
        assert_eq!(
            rcc.read(reg::CFGR0) & cfgr0::SWS_MASK,
            2 << cfgr0::SWS_SHIFT
        );
    }

    #[test]
    fn a_divided_apb_doubles_the_timer_clock() {
        let mut rcc = Rcc::new();
        rcc.write(reg::CFGR0, 4 << cfgr0::PPRE2_SHIFT);
        assert_eq!(rcc.apb2_hz(), 12_000_000);
        assert_eq!(rcc.timer_hz(rcc.apb2_hz()), 24_000_000);
    }
}
