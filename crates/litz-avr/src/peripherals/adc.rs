//! The 10-bit analog-to-digital converter.
//!
//! The ADC models the register protocol faithfully enough for the Arduino
//! `analogRead` path: `ADMUX` selects the reference and channel, writing
//! `ADSC` starts a conversion, `ADSC` reads back as one until the conversion
//! finishes, and `ADIF`/`ADIE` raise the ADC interrupt. A conversion takes
//! thirteen ADC clocks, and the ADC clock is the CPU clock divided by the
//! `ADPS` prescaler.
//!
//! How the channel is encoded in `ADMUX` (and `ADCSRB` on the parts that need
//! a fifth MUX bit), which reference codes select the internal source and
//! which codes read the bandgap or ground all come from the part's
//! [`AdcConfig`]. On the ATmega2560 and ATmega32U4, `MUX5 = 1` moves the
//! channel into the ADC8..ADC15 bank.
//!
//! # Injecting a voltage
//!
//! The converter has no pins of its own to sample. The host supplies a voltage
//! per channel with [`Adc::set_input`] (or
//! [`crate::Machine::set_analog_input`]), in **microvolts**. A board's analog
//! solver returns volts; multiply by `1_000_000` before injecting:
//!
//! ```
//! use litz_avr::Machine;
//! let mut machine = Machine::new();
//! // 2.5 V on ADC0, from an analog solve of 2.5.
//! machine.set_analog_input(0, 2_500_000);
//! ```
//!
//! The reference is `AVcc` (5 V by default, settable with
//! [`Adc::set_reference`]) or the part's internal 1.1 V source for the
//! reference code its table names. Differential modes, gain stages and the
//! temperature channel are not modelled: those codes read the injected
//! single-ended channel of the same number, or zero.

use crate::device::{ATMEGA328P, AdcConfig};

/// CPU cycles per ADC clock for each `ADPS` value.
fn adc_divisor(adps: u8) -> u32 {
    match adps & 0x07 {
        0 | 1 => 2,
        other => 1 << other,
    }
}

/// The ADC register block, result registers and injected channel voltages.
pub struct Adc {
    config: &'static AdcConfig,
    pub admux: u8,
    /// Stored bits of `ADCSRA`; `ADSC` and `ADIF` are kept separately.
    adcsra: u8,
    pub adcsrb: u8,
    pub adcl: u8,
    pub adch: u8,
    /// Injected voltage per channel, in microvolts.
    inputs: [u32; 16],
    /// `AVcc`/`AREF` reference, in microvolts.
    reference: u32,
    remaining: u32,
    converting: bool,
    adif: bool,
}

impl Default for Adc {
    fn default() -> Self {
        Self::new(&ATMEGA328P.adc)
    }
}

impl Adc {
    /// An ADC with the register layout of `config`.
    pub fn new(config: &'static AdcConfig) -> Self {
        Self {
            config,
            admux: 0,
            adcsra: 0,
            adcsrb: 0,
            adcl: 0,
            adch: 0,
            inputs: [0; 16],
            reference: 5_000_000,
            remaining: 0,
            converting: false,
            adif: false,
        }
    }

    /// The part's ADC table.
    pub fn config(&self) -> &'static AdcConfig {
        self.config
    }

    /// Set the voltage the converter reads on `channel`, in microvolts. Values
    /// above the reference are clamped to full scale.
    pub fn set_input(&mut self, channel: u8, microvolts: u32) {
        self.inputs[usize::from(channel) & 0x0f] = microvolts;
    }

    /// The injected voltage on `channel`, in microvolts.
    pub fn input(&self, channel: u8) -> u32 {
        self.inputs[usize::from(channel) & 0x0f]
    }

    /// Voltage of the external reference, in microvolts. Defaults to 5 V.
    pub fn set_reference(&mut self, microvolts: u32) {
        if microvolts > 0 {
            self.reference = microvolts;
        }
    }

    pub fn reference_microvolts(&self) -> u32 {
        self.reference
    }

    pub fn converting(&self) -> bool {
        self.converting
    }

    /// `ADCSRA` as firmware reads it.
    pub(crate) fn read_control(&self) -> u8 {
        (self.adcsra & !0x40)
            | if self.converting { 0x40 } else { 0 }
            | if self.adif { 0x10 } else { 0 }
    }

    pub(crate) fn write_control(&mut self, value: u8) {
        let start = value & 0x40 != 0;
        let enabled = value & 0x80 != 0;
        let clear_flag = value & 0x10 != 0;
        self.adcsra = value & !0x40;
        if clear_flag {
            self.adif = false;
        }
        if !enabled {
            self.converting = false;
            self.remaining = 0;
            return;
        }
        if start && !self.converting {
            self.start_conversion();
        }
    }

    fn start_conversion(&mut self) {
        let divisor = adc_divisor(self.adcsra & 0x07);
        // Thirteen ADC clocks per conversion; the datasheet's 13.5 for the
        // first free-running sample is not worth a separate state.
        self.remaining = 13 * divisor;
        self.converting = true;
    }

    pub(crate) fn advance(&mut self, cycles: u64) {
        if !self.converting {
            return;
        }
        if cycles >= u64::from(self.remaining) {
            self.complete();
        } else {
            self.remaining -= cycles as u32;
        }
    }

    fn complete(&mut self) {
        let value = self.sample();
        if self.admux & (1 << self.config.adlar_bit) != 0 {
            // ADLAR: left-adjusted, ADCH holds bits 9:2.
            self.adcl = (value << 6) as u8;
            self.adch = (value >> 2) as u8;
        } else {
            self.adcl = value as u8;
            self.adch = (value >> 8) as u8;
        }
        self.converting = false;
        self.remaining = 0;
        self.adif = true;
        // Free-running auto-trigger (ADATE set and ADTS = 0 in ADCSRB).
        if self.adcsra & 0x20 != 0 && self.adcsrb & 0x07 == 0 {
            self.start_conversion();
        }
    }

    /// The channel code the MUX bits and `MUX5` select.
    fn channel_code(&self) -> u16 {
        let mut code = u16::from(self.admux & self.config.mux_mask);
        if let Some(mask) = self.config.mux5_mask
            && self.adcsrb & mask != 0
        {
            code |= 0x20;
        }
        code
    }

    /// The single-ended channel a code names, after the high-bank offset.
    fn channel_of(code: u16) -> u8 {
        (if code >= 32 { code - 24 } else { code }) as u8
    }

    fn sample(&self) -> u16 {
        let code = self.channel_code();
        let microvolts = if self.config.bandgap_mux == Some(code as u8) {
            1_100_000
        } else if self.config.ground_mux == Some(code as u8) {
            0
        } else {
            self.inputs[usize::from(Self::channel_of(code)) & 0x0f]
        };
        let reference =
            if self.admux & self.config.internal_ref_mask == self.config.internal_ref_value {
                self.config.internal_ref_microvolts
            } else {
                self.reference
            };
        let scaled = (u64::from(microvolts) * 1024) / u64::from(reference);
        scaled.min(1023) as u16
    }

    pub(crate) fn interrupt_enabled(&self) -> bool {
        self.adcsra & 0x08 != 0
    }

    pub(crate) fn interrupt_pending(&self) -> bool {
        self.interrupt_enabled() && self.adif
    }

    pub(crate) fn acknowledge(&mut self) {
        self.adif = false;
    }

    /// Whether a running conversion or free-running mode could raise the
    /// interrupt while the core sleeps.
    pub(crate) fn can_wake(&self) -> bool {
        self.interrupt_enabled()
            && self.adcsra & 0x80 != 0
            && (self.converting || (self.adcsra & 0x20 != 0 && self.adcsrb & 0x07 == 0))
    }
}
