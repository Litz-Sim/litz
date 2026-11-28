//! ADC1: a 10-bit successive-approximation converter with eight channels, a
//! regular group, an injected group and the usual end-of-conversion flags.
//!
//! Conversion is instantaneous: writing `SWSTART` with `ADON` set fills `DR`
//! and sets `EOC`; `JSWSTART` converts the injected channel into `JDR1`. The
//! host supplies each channel's voltage in microvolts, and the sample is
//! `1023 * microvolts / 3_300_000`, clamped to ten bits.
//!
//! The channel-to-pad wiring is fixed in the model and documented on
//! [`crate::chip::analog_channel`]: PA1/PA2 are channels 1/0 and PC4, PD2..PD6
//! are channels 2 through 7.

use super::RegisterFile;

pub mod reg {
    pub const STATR: u32 = 0x00;
    pub const CTLR1: u32 = 0x04;
    pub const CTLR2: u32 = 0x08;
    pub const SAMPR1: u32 = 0x0c;
    pub const SAMPR2: u32 = 0x10;
    pub const JOFR1: u32 = 0x14;
    pub const JOFR2: u32 = 0x18;
    pub const JOFR3: u32 = 0x1c;
    pub const JOFR4: u32 = 0x20;
    pub const HTR: u32 = 0x24;
    pub const LTR: u32 = 0x28;
    pub const SQR1: u32 = 0x2c;
    pub const SQR2: u32 = 0x30;
    pub const SQR3: u32 = 0x34;
    pub const JSQR: u32 = 0x38;
    pub const JDR1: u32 = 0x3c;
    pub const JDR2: u32 = 0x40;
    pub const JDR3: u32 = 0x44;
    pub const JDR4: u32 = 0x48;
    pub const DR: u32 = 0x4c;
}

pub mod statr {
    pub const AWD: u32 = 1 << 0;
    pub const EOC: u32 = 1 << 1;
    pub const JEOC: u32 = 1 << 2;
    pub const JSTRT: u32 = 1 << 3;
    pub const STRT: u32 = 1 << 4;
}

pub mod ctlr1 {
    pub const EOCIE: u32 = 1 << 5;
    pub const AWDIE: u32 = 1 << 6;
    pub const JEOCIE: u32 = 1 << 7;
    pub const SCAN: u32 = 1 << 8;
}

pub mod ctlr2 {
    pub const ADON: u32 = 1 << 0;
    pub const CONT: u32 = 1 << 1;
    pub const CAL: u32 = 1 << 2;
    pub const RSTCAL: u32 = 1 << 3;
    pub const ALIGN: u32 = 1 << 11;
    pub const JSWSTART: u32 = 1 << 21;
    pub const SWSTART: u32 = 1 << 22;
}

/// The reference voltage in microvolts.
pub const REFERENCE_UV: u64 = 3_300_000;
pub const CHANNELS: usize = 8;

#[derive(Debug)]
pub struct Adc {
    regs: RegisterFile,
    statr: u32,
    ctlr2: u32,
    inputs: [u32; CHANNELS],
    dr: u16,
    jdr: [u16; 4],
    /// Cycles left before the next continuous conversion.
    continuous: u32,
}

impl Default for Adc {
    fn default() -> Self {
        Self::new()
    }
}

impl Adc {
    pub fn new() -> Self {
        Adc {
            regs: RegisterFile::new(0x100 / 4),
            statr: 0,
            ctlr2: 0,
            inputs: [0; CHANNELS],
            dr: 0,
            jdr: [0; 4],
            continuous: 0,
        }
    }

    pub fn set_input(&mut self, channel: u8, microvolts: u32) {
        if let Some(slot) = self.inputs.get_mut(usize::from(channel)) {
            *slot = microvolts;
        }
    }

    pub fn input(&self, channel: u8) -> u32 {
        self.inputs.get(usize::from(channel)).copied().unwrap_or(0)
    }

    fn sample(&self, channel: u8) -> u16 {
        let microvolts = u64::from(self.input(channel));
        (microvolts * 1023 / REFERENCE_UV).min(1023) as u16
    }

    fn regular_channel(&self) -> u8 {
        (self.regs.read(reg::SQR3) & 0x1f) as u8
    }

    fn injected_channel(&self) -> u8 {
        (self.regs.read(reg::JSQR) & 0x1f) as u8
    }

    fn convert_regular(&mut self) {
        self.dr = self.sample(self.regular_channel());
        self.statr |= statr::EOC;
    }

    fn convert_injected(&mut self) {
        let sample = self.sample(self.injected_channel());
        self.jdr[0] = sample;
        self.statr |= statr::JEOC;
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::STATR => self.statr,
            reg::DR => u32::from(self.dr),
            reg::JDR1 => u32::from(self.jdr[0]),
            reg::JDR2 => u32::from(self.jdr[1]),
            reg::JDR3 => u32::from(self.jdr[2]),
            reg::JDR4 => u32::from(self.jdr[3]),
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::STATR => {
                // EOC and JEOC are write-zero-to-clear, the STM32 handshake.
                self.statr &= value & (statr::EOC | statr::JEOC);
            }
            reg::CTLR2 => {
                let was_on = self.ctlr2 & ctlr2::ADON != 0;
                self.ctlr2 = value
                    & (ctlr2::ADON | ctlr2::CONT | ctlr2::ALIGN | ctlr2::SWSTART | ctlr2::JSWSTART);
                if value & ctlr2::RSTCAL != 0 {
                    // Calibration completes at once.
                    self.ctlr2 &= !ctlr2::RSTCAL;
                }
                let on = self.ctlr2 & ctlr2::ADON != 0;
                if on && value & ctlr2::SWSTART != 0 {
                    self.convert_regular();
                }
                if on && value & ctlr2::JSWSTART != 0 {
                    self.convert_injected();
                }
                if !was_on && on {
                    self.continuous = 0;
                }
                // SWSTART/JSWSTART are self-clearing command bits.
                self.ctlr2 &= !(ctlr2::SWSTART | ctlr2::JSWSTART);
            }
            reg::SQR1
            | reg::SQR2
            | reg::SQR3
            | reg::JSQR
            | reg::SAMPR1
            | reg::SAMPR2
            | reg::HTR
            | reg::LTR
            | reg::JOFR1
            | reg::JOFR2
            | reg::JOFR3
            | reg::JOFR4 => self.regs.write(off, value),
            reg::CTLR1 => self.regs.write(off, value),
            off => self.regs.write(off, value),
        }
    }

    pub fn tick(&mut self, _cycles: u64) {
        if self.ctlr2 & ctlr2::ADON == 0 || self.ctlr2 & ctlr2::CONT == 0 {
            return;
        }
        // One conversion per advance keeps a continuous scan visibly moving
        // without inventing an ADC clock.
        self.continuous = self.continuous.wrapping_add(1);
        self.convert_regular();
    }

    pub fn irq(&self) -> bool {
        let control = self.regs.read(reg::CTLR1);
        (control & ctlr1::EOCIE != 0 && self.statr & statr::EOC != 0)
            || (control & ctlr1::JEOCIE != 0 && self.statr & statr::JEOC != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversion_scales_to_ten_bits() {
        let mut adc = Adc::new();
        adc.set_input(0, 1_650_000);
        adc.write(reg::CTLR2, ctlr2::ADON | ctlr2::SWSTART);
        assert_eq!(adc.read(reg::DR), 511);
        assert_ne!(adc.read(reg::STATR) & statr::EOC, 0);
        adc.write(reg::STATR, 0);
        assert_eq!(adc.read(reg::STATR) & statr::EOC, 0);
    }

    #[test]
    fn full_scale_and_channel_select() {
        let mut adc = Adc::new();
        adc.set_input(3, 3_300_000);
        adc.write(reg::SQR3, 3);
        adc.write(reg::CTLR2, ctlr2::ADON | ctlr2::SWSTART);
        assert_eq!(adc.read(reg::DR), 1023);
    }

    #[test]
    fn the_injected_group_writes_its_own_data_register() {
        let mut adc = Adc::new();
        adc.set_input(5, 1_000_000);
        adc.write(reg::JSQR, 5);
        adc.write(reg::CTLR2, ctlr2::ADON | ctlr2::JSWSTART);
        assert_eq!(adc.read(reg::JDR1), 310);
        assert_ne!(adc.read(reg::STATR) & statr::JEOC, 0);
    }
}
