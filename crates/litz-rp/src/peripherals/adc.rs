//! The ADC: a 4-channel 12-bit successive-approximation converter with a
//! temperature sensor and a byte-wide FIFO.
//!
//! A conversion is instantaneous once started: `START_ONCE` clears, the result
//! appears in `RESULT`, and the sample is pushed into the four-entry FIFO when
//! `FCS.EN` is set. `READY` stays high because there is never a conversion in
//! flight. `START_MANY` converts once per [`Adc::tick`], which is fast enough
//! that a firmware polling the FIFO sees samples appear without the model
//! having to invent an ADC clock.
//!
//! The host supplies the voltage per channel in microvolts through
//! [`Adc::set_input`]. A full-scale 3.3 V input reads 4095; the conversion is
//! `4095 * microvolts / 3_300_000`, clamped to the 12-bit range. (The task
//! gives the scaling as `3300 * mv / 3_300_000`; that expression yields
//! millivolts, so this model applies the same ratio to the 12-bit full scale,
//! which is the value a firmware expects in `RESULT`.)

use std::collections::VecDeque;

use super::RegisterFile;

/// Register offsets from the ADC base.
pub mod reg {
    pub const CS: u32 = 0x00;
    pub const RESULT: u32 = 0x04;
    pub const FCS: u32 = 0x08;
    pub const FIFO: u32 = 0x0c;
    pub const DIV: u32 = 0x10;
}

/// `CS` bits.
pub mod cs {
    pub const EN: u32 = 1 << 0;
    pub const TS_EN: u32 = 1 << 1;
    pub const START_ONCE: u32 = 1 << 2;
    pub const START_MANY: u32 = 1 << 3;
    pub const AINSEL_SHIFT: u32 = 5;
    pub const AINSEL_MASK: u32 = 0x7 << AINSEL_SHIFT;
    pub const READY: u32 = 1 << 8;
    pub const ERR: u32 = 1 << 9;
}

/// `FCS` bits.
pub mod fcs {
    pub const EN: u32 = 1 << 0;
    pub const SHIFT: u32 = 1 << 1;
    pub const ERR: u32 = 1 << 2;
    pub const DREQ_EN: u32 = 1 << 3;
    pub const THRESH_SHIFT: u32 = 16;
    pub const THRESH_MASK: u32 = 0xf << THRESH_SHIFT;
}

/// The reference voltage in microvolts.
pub const REFERENCE_UV: u64 = 3_300_000;
/// Channels 0 through 3 are the external pins, channel 4 the temperature
/// sensor.
pub const CHANNELS: usize = 5;
/// The FIFO holds four samples.
const FIFO_DEPTH: usize = 4;

#[derive(Debug)]
pub struct Adc {
    regs: RegisterFile,
    cs: u32,
    fcs: u32,
    div: u32,
    result: u16,
    fifo: VecDeque<u16>,
    inputs: [u32; CHANNELS],
    overrun: bool,
    start_many: bool,
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
            cs: 0,
            fcs: 0,
            div: 0,
            result: 0,
            fifo: VecDeque::new(),
            inputs: [0; CHANNELS],
            overrun: false,
            start_many: false,
        }
    }

    /// Set the voltage the converter samples on `channel`, in microvolts.
    pub fn set_input(&mut self, channel: u8, microvolts: u32) {
        if let Some(slot) = self.inputs.get_mut(usize::from(channel)) {
            *slot = microvolts;
        }
    }

    /// The injected voltage on `channel`, in microvolts.
    pub fn input(&self, channel: u8) -> u32 {
        self.inputs.get(usize::from(channel)).copied().unwrap_or(0)
    }

    fn threshold(&self) -> usize {
        (((self.fcs & fcs::THRESH_MASK) >> fcs::THRESH_SHIFT) as usize) + 1
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            reg::CS => {
                // Conversions finish in the same cycle, so the block is always
                // ready.
                self.cs & !(cs::START_ONCE | cs::READY | cs::ERR)
                    | cs::READY
                    | if self.overrun { cs::ERR } else { 0 }
            }
            reg::RESULT => u32::from(self.result),
            reg::FCS => self.fcs | if self.overrun { fcs::ERR } else { 0 },
            reg::FIFO => u32::from(self.fifo.pop_front().unwrap_or(0)),
            reg::DIV => self.div,
            off => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            reg::CS => {
                self.cs = value & !(cs::START_ONCE | cs::START_MANY | cs::READY | cs::ERR);
                let enabled = self.cs & cs::EN != 0;
                if enabled && value & cs::START_ONCE != 0 {
                    self.convert();
                }
                self.start_many = enabled && value & cs::START_MANY != 0;
            }
            reg::RESULT => {}
            reg::FCS => {
                self.fcs = value & (fcs::EN | fcs::SHIFT | fcs::DREQ_EN | fcs::THRESH_MASK);
                // The overrun flag is write-one-to-clear.
                if value & fcs::ERR != 0 {
                    self.overrun = false;
                }
            }
            reg::FIFO => {}
            reg::DIV => self.div = value & 0xffff,
            off => self.regs.write(off, value),
        }
    }

    pub fn tick(&mut self, _cycles: u64) {
        if self.start_many && self.cs & cs::EN != 0 {
            self.convert();
        }
    }

    pub fn irq(&self) -> bool {
        self.overrun || self.fifo.len() >= self.threshold()
    }

    fn convert(&mut self) {
        let channel = ((self.cs & cs::AINSEL_MASK) >> cs::AINSEL_SHIFT) as usize;
        let microvolts = self.inputs.get(channel).copied().unwrap_or(0);
        let sample = (u64::from(microvolts) * 4095 / REFERENCE_UV).min(4095);
        self.result = sample as u16;
        if self.fcs & fcs::EN != 0 {
            let value = if self.fcs & fcs::SHIFT != 0 {
                self.result >> 4
            } else {
                self.result
            };
            if self.fifo.len() >= FIFO_DEPTH {
                self.overrun = true;
                self.fifo.pop_front();
            }
            self.fifo.push_back(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_scales_to_twelve_bits() {
        let mut adc = Adc::new();
        adc.set_input(0, 1_650_000);
        adc.write(reg::CS, cs::EN | cs::START_ONCE);
        assert_eq!(adc.read(reg::RESULT), 2047);
        assert_eq!(adc.read(reg::CS) & cs::START_ONCE, 0);
        assert_ne!(adc.read(reg::CS) & cs::READY, 0);
    }

    #[test]
    fn full_scale_and_alsel_select() {
        let mut adc = Adc::new();
        adc.set_input(0, 0);
        adc.set_input(3, 3_300_000);
        adc.write(reg::CS, cs::EN | (3 << cs::AINSEL_SHIFT) | cs::START_ONCE);
        assert_eq!(adc.read(reg::RESULT), 4095);

        adc.write(reg::CS, cs::EN | (0 << cs::AINSEL_SHIFT) | cs::START_ONCE);
        assert_eq!(adc.read(reg::RESULT), 0);
    }

    #[test]
    fn fifo_threshold_raises_the_interrupt() {
        let mut adc = Adc::new();
        adc.set_input(0, 1_000_000);
        adc.write(reg::FCS, fcs::EN | (1 << fcs::THRESH_SHIFT));
        adc.write(reg::CS, cs::EN | cs::START_ONCE);
        assert!(!adc.irq(), "one sample is below a threshold of two");
        adc.write(reg::CS, cs::EN | cs::START_ONCE);
        assert!(adc.irq());
        assert_eq!(adc.read(reg::FIFO), 1240);
        assert_ne!(adc.read(reg::FIFO), 0);
        assert_eq!(adc.read(reg::FIFO), 0, "FIFO drained");
    }

    #[test]
    fn shifting_gives_an_eight_bit_sample() {
        let mut adc = Adc::new();
        adc.set_input(1, 3_300_000);
        adc.write(reg::FCS, fcs::EN | fcs::SHIFT);
        adc.write(reg::CS, cs::EN | (1 << cs::AINSEL_SHIFT) | cs::START_ONCE);
        assert_eq!(adc.read(reg::FIFO), 255);
    }
}
