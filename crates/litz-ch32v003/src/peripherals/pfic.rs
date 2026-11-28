//! The PFIC: the QingKe programmable fast interrupt controller.
//!
//! It carries 32 interrupt sources (the CH32V003 uses the first 24, and this
//! model adds SysTick on line 24), each with a pending flag, an enable and a
//! two-bit priority. The register layout follows the reference manual's idea
//! of a status word per four sources: in every `ISR` word a source's field is
//! eight bits wide, bit 0 is its pending flag and bit 1 its enable; the `IPR`
//! word beside it carries the priority in bits 2–3.
//!
//! Firmware written against WCH's `core_riscv.h` uses the bit-per-source
//! aliases (`IENR`/`IRER`/`IPSR`/`IPRR`) instead, so those are implemented as
//! whole-register views over the same state. `IPRIOR` gives one byte per
//! source with the priority in bits 7–6, which is where `NVIC_SetPriority`
//! puts it.
//!
//! # Nested delivery
//!
//! A handler runs with `mstatus.MIE` clear, so nesting only happens when it
//! sets `MIE` itself. When it does, [`Pfic::best_pending`] refuses a source
//! whose priority is not strictly better than the one executing; priority 0 is
//! the most urgent. The threshold register (`ITHRESDR`) masks everything at or
//! below the level written, with zero meaning "mask nothing", the same
//! convention as ARM's base priority.

use super::RegisterFile;

/// Interrupt source numbers, in the CH32V003's own order.
pub mod irq {
    pub const WWDG: u16 = 0;
    pub const PVD: u16 = 1;
    pub const FLASH: u16 = 2;
    pub const RCC: u16 = 3;
    pub const EXTI7_0: u16 = 4;
    pub const AWU: u16 = 5;
    pub const DMA1_CHANNEL1: u16 = 6;
    pub const DMA1_CHANNEL2: u16 = 7;
    pub const DMA1_CHANNEL3: u16 = 8;
    pub const DMA1_CHANNEL4: u16 = 9;
    pub const DMA1_CHANNEL5: u16 = 10;
    pub const DMA1_CHANNEL6: u16 = 11;
    pub const DMA1_CHANNEL7: u16 = 12;
    pub const ADC1: u16 = 13;
    pub const I2C1_EV: u16 = 14;
    pub const I2C1_ER: u16 = 15;
    pub const SPI1: u16 = 16;
    pub const USART1: u16 = 17;
    pub const TIM1_BRK: u16 = 18;
    pub const TIM1_UP: u16 = 19;
    pub const TIM1_TRG_COM: u16 = 20;
    pub const TIM1_CC: u16 = 21;
    pub const TIM2: u16 = 22;
    pub const IWDG: u16 = 23;
    /// SysTick is a core exception on ARM-shaped parts; the model gives it the
    /// next PFIC line so it travels the same vector table as everything else.
    pub const SYSTICK: u16 = 24;

    pub const COUNT: u16 = 32;
}

/// Register offsets from the PFIC base.
pub mod reg {
    pub const ISR: u32 = 0x000;
    pub const IPR: u32 = 0x020;
    pub const ITHRESDR: u32 = 0x040;
    pub const IENR: u32 = 0x100;
    pub const IRER: u32 = 0x180;
    pub const IPSR: u32 = 0x200;
    pub const IPRR: u32 = 0x280;
    pub const IPRIOR: u32 = 0x400;
}

#[derive(Debug)]
pub struct Pfic {
    regs: RegisterFile,
    /// Pending flag per source.
    pending: u32,
    /// Enabled bit per source.
    enabled: u32,
    /// Two-bit priority per source.
    priority: [u8; irq::COUNT as usize],
    threshold: u8,
}

impl Default for Pfic {
    fn default() -> Self {
        Self::new()
    }
}

impl Pfic {
    pub fn new() -> Self {
        Pfic {
            regs: RegisterFile::new(0x500 / 4),
            pending: 0,
            enabled: 0,
            priority: [3; irq::COUNT as usize],
            threshold: 0,
        }
    }

    /// A peripheral's interrupt line changed.
    pub fn set_line(&mut self, source: u16, high: bool) {
        let bit = 1u32 << (source & 31);
        if high {
            self.pending |= bit;
        }
    }

    /// The pending flag for one source, for a test or a debug surface.
    pub fn pending(&self, source: u16) -> bool {
        self.pending & (1u32 << (source & 31)) != 0
    }

    pub fn enabled(&self, source: u16) -> bool {
        self.enabled & (1u32 << (source & 31)) != 0
    }

    /// The highest-priority source that is ready to run.
    ///
    /// `current` is the priority of the handler running now, or
    /// [`crate::cpu::Core::NO_PRIORITY`] in thread mode.
    pub fn best_pending(&self, current: u8) -> Option<(u16, u8)> {
        let ready = self.pending & self.enabled;
        if ready == 0 {
            return None;
        }
        let mut best: Option<(u16, u8)> = None;
        for source in 0..irq::COUNT {
            if ready & (1u32 << (source & 31)) == 0 {
                continue;
            }
            let priority = self.priority[usize::from(source)] & 3;
            if self.threshold != 0 && priority >= self.threshold {
                continue;
            }
            if priority >= current {
                continue;
            }
            if best.is_none_or(|(_, best_priority)| priority < best_priority) {
                best = Some((source, priority));
            }
        }
        best
    }

    /// Acknowledge a delivered source: its pending flag clears, and the
    /// peripheral raises it again while its condition lasts.
    pub fn acknowledge(&mut self, source: u16) {
        self.pending &= !(1u32 << (source & 31));
    }

    /// Whether any line is latched, enabled or not. `wfi` wakes on this.
    pub fn any_pending(&self) -> bool {
        self.pending != 0
    }

    fn isr_word(&self, word: usize) -> u32 {
        let mut value = 0;
        for index in 0..4usize {
            let source = word * 4 + index;
            let bit = 1u32 << (source & 31);
            let mut field = 0;
            if self.pending & bit != 0 {
                field |= 1;
            }
            if self.enabled & bit != 0 {
                field |= 2;
            }
            value |= field << (index * 8);
        }
        value
    }

    fn ipr_word(&self, word: usize) -> u32 {
        let mut value = 0;
        for index in 0..4usize {
            let source = word * 4 + index;
            value |= u32::from(self.priority[source] & 3) << (index * 8 + 2);
        }
        value
    }

    pub fn read(&mut self, off: u32) -> u32 {
        if (reg::ISR..reg::ISR + 32).contains(&off) {
            return self.isr_word(((off - reg::ISR) / 4) as usize);
        }
        if (reg::IPR..reg::IPR + 32).contains(&off) {
            return self.ipr_word(((off - reg::IPR) / 4) as usize);
        }
        match off {
            reg::ITHRESDR => u32::from(self.threshold),
            reg::IENR => self.enabled,
            reg::IRER => self.enabled,
            reg::IPSR => self.pending,
            reg::IPRR => self.pending,
            off if (reg::IPRIOR..reg::IPRIOR + 32).contains(&off) => {
                let source = (off - reg::IPRIOR) as usize;
                u32::from(self.priority[source] & 3) << 6
            }
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        if (reg::ISR..reg::ISR + 32).contains(&off) {
            let word = ((off - reg::ISR) / 4) as usize;
            for index in 0..4usize {
                let source = word * 4 + index;
                let bit = 1u32 << (source & 31);
                let field = (value >> (index * 8)) & 0xff;
                if field & 1 == 0 {
                    self.pending &= !bit;
                }
                if field & 2 != 0 {
                    self.enabled |= bit;
                } else {
                    self.enabled &= !bit;
                }
            }
            return;
        }
        if (reg::IPR..reg::IPR + 32).contains(&off) {
            let word = ((off - reg::IPR) / 4) as usize;
            for index in 0..4usize {
                let field = (value >> (index * 8)) & 0xff;
                self.priority[word * 4 + index] = ((field >> 2) & 3) as u8;
            }
            return;
        }
        match off {
            reg::ITHRESDR => self.threshold = (value & 3) as u8,
            reg::IENR => self.enabled |= value,
            reg::IRER => self.enabled &= !value,
            reg::IPSR => self.pending |= value,
            reg::IPRR => self.pending &= !value,
            off if (reg::IPRIOR..reg::IPRIOR + 32).contains(&off) => {
                let source = (off - reg::IPRIOR) as usize;
                self.priority[source] = ((value >> 6) & 3) as u8;
            }
            off => self.regs.write(off, value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bit_views_and_the_field_views_agree() {
        let mut pfic = Pfic::new();
        pfic.write(reg::IENR, 1 << irq::USART1);
        assert!(pfic.enabled(irq::USART1));
        // USART1 is source 17: word 4, field 1.
        assert_eq!(pfic.read(reg::ISR + 16) & (0x02 << 8), 0x02 << 8);

        pfic.set_line(irq::USART1, true);
        assert_eq!(pfic.read(reg::IPSR) & (1 << irq::USART1), 1 << irq::USART1);
        assert_eq!(pfic.read(reg::ISR + 16) & (0x01 << 8), 0x01 << 8);
    }

    #[test]
    fn priority_orders_delivery() {
        let mut pfic = Pfic::new();
        pfic.write(reg::IENR, 0xffff_ffff);
        pfic.set_line(irq::TIM2, true);
        pfic.set_line(irq::USART1, true);
        // TIM2 = 22, USART1 = 17; give USART1 priority 0 and TIM2 priority 2.
        pfic.write(reg::IPRIOR + u32::from(irq::USART1), 0 << 6);
        pfic.write(reg::IPRIOR + u32::from(irq::TIM2), 2 << 6);
        assert_eq!(pfic.best_pending(4), Some((irq::USART1, 0)));

        pfic.acknowledge(irq::USART1);
        assert_eq!(pfic.best_pending(4), Some((irq::TIM2, 2)));
        // A handler at priority 2 does not pre-empt itself.
        assert_eq!(pfic.best_pending(2), None);
    }
}
