//! The on-chip peripherals: GPIO, the three timers, USART0, the ADC, SPI, TWI,
//! the EEPROM, the watchdog and the interrupt controller that ties them
//! together.
//!
//! Every peripheral exposes its registers through [`Peripherals::read`] and
//! [`Peripherals::write`], which the core reaches through
//! [`crate::cpu::DataBus`]. Bad addresses are ignored rather than panicking.
//!
//! # Interrupts
//!
//! [`vector`] names the ATmega328P vectors in datasheet order. The core asks
//! [`Peripherals::pending_interrupt`] before every instruction; when the `I`
//! flag is set and a pending vector exists, the core pushes the return
//! address, clears `I` and jumps to the vector's address (four cycles, the
//! ATmega328P's vector-entry cost with a 16-bit PC). `RETI` restores `I`.
//! Sources that have a flag cleared by hardware on vector entry clear it in
//! [`Peripherals::acknowledge`]; `RXC0`/`UDRE0` are level conditions and stay
//! pending until the ISR reads `UDR0` or writes it.
//!
//! # GPIO and timer outputs
//!
//! A pin with `DDR` set can be driven by its `PORT` latch or by a timer
//! compare output (`OC0A`, `OC0B`, `OC1A`, `OC1B`, `OC2A`, `OC2B`). The timer
//! level wins whenever the corresponding `COMnx` bits are non-zero, which is
//! what makes `analogWrite` move the pin. Every driven edge is reported to the
//! host and surfaces in [`Peripherals::take_outputs`].

pub mod adc;
pub mod eeprom;
pub mod spi;
pub mod timers;
pub mod twi;
pub mod usart;

use std::fmt;

pub use adc::Adc;
pub use eeprom::Eeprom;
pub use spi::{Spi, SpiTransfer};
pub use timers::{Timer0, Timer1, Timer2};
pub use twi::{I2cRead, I2cWrite, Twi};
pub use usart::Usart0;

/// Data-space addresses of the registers this module answers.
pub mod addr {
    pub const PINB: u16 = 0x23;
    pub const DDRB: u16 = 0x24;
    pub const PORTB: u16 = 0x25;
    pub const PINC: u16 = 0x26;
    pub const DDRC: u16 = 0x27;
    pub const PORTC: u16 = 0x28;
    pub const PIND: u16 = 0x29;
    pub const DDRD: u16 = 0x2a;
    pub const PORTD: u16 = 0x2b;
    pub const TIFR0: u16 = 0x35;
    pub const TIFR1: u16 = 0x36;
    pub const TIFR2: u16 = 0x37;
    pub const PCIFR: u16 = 0x3b;
    pub const EIFR: u16 = 0x3c;
    pub const EIMSK: u16 = 0x3d;
    pub const EECR: u16 = 0x3f;
    pub const EEDR: u16 = 0x40;
    pub const EEARL: u16 = 0x41;
    pub const EEARH: u16 = 0x42;
    pub const TCCR0A: u16 = 0x44;
    pub const TCCR0B: u16 = 0x45;
    pub const TCNT0: u16 = 0x46;
    pub const OCR0A: u16 = 0x47;
    pub const OCR0B: u16 = 0x48;
    pub const SPCR: u16 = 0x4c;
    pub const SPSR: u16 = 0x4d;
    pub const SPDR: u16 = 0x4e;
    pub const ACSR: u16 = 0x50;
    pub const SPMCSR: u16 = 0x57;
    pub const WDTCSR: u16 = 0x60;
    pub const PCICR: u16 = 0x68;
    pub const EICRA: u16 = 0x69;
    pub const PCMSK0: u16 = 0x6b;
    pub const PCMSK1: u16 = 0x6c;
    pub const PCMSK2: u16 = 0x6d;
    pub const TIMSK0: u16 = 0x6e;
    pub const TIMSK1: u16 = 0x6f;
    pub const TIMSK2: u16 = 0x70;
    pub const ADCL: u16 = 0x78;
    pub const ADCH: u16 = 0x79;
    pub const ADCSRA: u16 = 0x7a;
    pub const ADCSRB: u16 = 0x7b;
    pub const ADMUX: u16 = 0x7c;
    pub const TCCR1A: u16 = 0x80;
    pub const TCCR1B: u16 = 0x81;
    pub const TCCR1C: u16 = 0x82;
    pub const TCNT1L: u16 = 0x84;
    pub const TCNT1H: u16 = 0x85;
    pub const ICR1L: u16 = 0x86;
    pub const ICR1H: u16 = 0x87;
    pub const OCR1AL: u16 = 0x88;
    pub const OCR1AH: u16 = 0x89;
    pub const OCR1BL: u16 = 0x8a;
    pub const OCR1BH: u16 = 0x8b;
    pub const TCCR2A: u16 = 0xb0;
    pub const TCCR2B: u16 = 0xb1;
    pub const TCNT2: u16 = 0xb2;
    pub const OCR2A: u16 = 0xb3;
    pub const OCR2B: u16 = 0xb4;
    pub const TWBR: u16 = 0xb8;
    pub const TWSR: u16 = 0xb9;
    pub const TWAR: u16 = 0xba;
    pub const TWDR: u16 = 0xbb;
    pub const TWCR: u16 = 0xbc;
    pub const TWAMR: u16 = 0xbd;
    pub const UCSR0A: u16 = 0xc0;
    pub const UCSR0B: u16 = 0xc1;
    pub const UCSR0C: u16 = 0xc2;
    pub const UBRR0L: u16 = 0xc4;
    pub const UBRR0H: u16 = 0xc5;
    pub const UDR0: u16 = 0xc6;
}

/// The interrupt vectors of an ATmega328P, in datasheet priority order.
///
/// Vector `n` lives at byte address `4 * n` in flash (word address `2 * n`);
/// avr-gcc emits a four-byte `jmp` at each slot, which is why the spacing is
/// four bytes even though the datasheet's table numbers the slots in words.
///
/// Unknown sources are not routed; a vector with no handler falls through to
/// whatever the firmware put there.
pub mod vector {
    pub const RESET: u8 = 0;
    pub const INT0: u8 = 1;
    pub const INT1: u8 = 2;
    pub const PCINT0: u8 = 3;
    pub const PCINT1: u8 = 4;
    pub const PCINT2: u8 = 5;
    pub const WDT: u8 = 6;
    pub const TIMER2_COMPA: u8 = 7;
    pub const TIMER2_COMPB: u8 = 8;
    pub const TIMER2_OVF: u8 = 9;
    pub const TIMER1_CAPT: u8 = 10;
    pub const TIMER1_COMPA: u8 = 11;
    pub const TIMER1_COMPB: u8 = 12;
    pub const TIMER1_OVF: u8 = 13;
    pub const TIMER0_COMPA: u8 = 14;
    pub const TIMER0_COMPB: u8 = 15;
    pub const TIMER0_OVF: u8 = 16;
    pub const SPI_STC: u8 = 17;
    pub const USART_RX: u8 = 18;
    pub const USART_UDRE: u8 = 19;
    pub const USART_TX: u8 = 20;
    pub const ADC: u8 = 21;
    pub const EE_READY: u8 = 22;
    pub const ANALOG_COMP: u8 = 23;
    pub const TWI: u8 = 24;
    pub const SPM_READY: u8 = 25;
    /// One past the last vector.
    pub const COUNT: u8 = 26;

    /// Byte address of a vector's slot in flash.
    pub const fn byte_address(vector: u8) -> u16 {
        (vector as u16) * 4
    }

    /// Word address of a vector's slot in flash.
    pub const fn word_address(vector: u8) -> u16 {
        (vector as u16) * 2
    }
}

/// Cycles an interrupt entry costs on an ATmega328P after a 16-bit PC is
/// pushed.
pub const INTERRUPT_CYCLES: u8 = 4;

/// The three GPIO ports, named as the datasheet names them.
pub const PORT_B: usize = 0;
pub const PORT_C: usize = 1;
pub const PORT_D: usize = 2;

/// Which port and bit a host-facing digital pin maps to.
///
/// The numbering is the Arduino Uno's: D0–D7 are `PORTD[0..8]`, D8–D13 are
/// `PORTB[0..6]`, A0–A5 are `PORTC[0..6]`.
pub fn digital_to_port(pin: u8) -> Option<(usize, u8)> {
    match pin {
        0..=7 => Some((PORT_D, pin)),
        8..=13 => Some((PORT_B, pin - 8)),
        14..=19 => Some((PORT_C, pin - 14)),
        _ => None,
    }
}

/// The inverse of [`digital_to_port`]. PB6/PB7 and PC6/PC7 have no Arduino
/// digital number and return `None`.
pub fn port_to_digital(port: usize, bit: u8) -> Option<u8> {
    match (port, bit) {
        (PORT_D, bit @ 0..=7) => Some(bit),
        (PORT_B, bit @ 0..=5) => Some(8 + bit),
        (PORT_C, bit @ 0..=5) => Some(14 + bit),
        _ => None,
    }
}

/// GPIO ports B, C and D: direction, output latch, external input levels and
/// the timer compare-output overrides.
#[derive(Default)]
pub struct Gpio {
    ddr: [u8; 3],
    port: [u8; 3],
    input: [u8; 3],
    /// Level a timer wants on a pin, when a compare output is connected.
    overridden: [Option<bool>; 24],
    /// Last level reported per port/bit, so a callback sees edges only.
    reported: [Option<bool>; 24],
    events: Vec<(u8, bool)>,
    callback: Option<Box<dyn FnMut(u8, bool)>>,
}

impl Gpio {
    /// Install a callback for output edges. It receives the Arduino digital
    /// pin number and the level the chip now drives.
    pub fn set_callback(&mut self, callback: Box<dyn FnMut(u8, bool)>) {
        self.callback = Some(callback);
    }

    /// Take the callback so a reset can reinstate it: the callback belongs to
    /// the host, not to the chip.
    pub fn take_callback(&mut self) -> Option<Box<dyn FnMut(u8, bool)>> {
        self.callback.take()
    }

    /// Remove and return every output edge since the last call.
    pub fn take_events(&mut self) -> Vec<(u8, bool)> {
        core::mem::take(&mut self.events)
    }

    pub fn ddr(&self, port: usize) -> u8 {
        self.ddr[port]
    }

    pub fn port(&self, port: usize) -> u8 {
        self.port[port]
    }

    /// Level driven back into `PINx`: output pins read the level the chip
    /// drives (the timer's compare output when one is connected, otherwise the
    /// latch); input pins read the external level.
    pub fn pin(&self, port: usize) -> u8 {
        let mut value = 0u8;
        for bit in 0..8u8 {
            let high = if self.ddr[port] & (1 << bit) != 0 {
                self.driven_level(port, bit) == Some(true)
            } else {
                self.input[port] & (1 << bit) != 0
            };
            if high {
                value |= 1 << bit;
            }
        }
        value
    }

    /// What the chip drives on one pin, or `None` when the pin is an input.
    pub fn driven_level(&self, port: usize, bit: u8) -> Option<bool> {
        if self.ddr[port] & (1 << bit) == 0 {
            return None;
        }
        let index = port * 8 + usize::from(bit);
        Some(self.overridden[index].unwrap_or(self.port[port] & (1 << bit) != 0))
    }

    /// Connect (or disconnect) a timer compare output. The level only reaches
    /// the pin when `DDR` makes it an output.
    pub(crate) fn set_override(&mut self, port: usize, bit: u8, level: Option<bool>) {
        let index = port * 8 + usize::from(bit);
        if self.overridden[index] == level {
            return;
        }
        self.overridden[index] = level;
        self.report(port);
    }

    pub fn set_ddr(&mut self, port: usize, value: u8) {
        self.ddr[port] = value;
        self.report(port);
    }

    pub fn set_port(&mut self, port: usize, value: u8) {
        self.port[port] = value;
        self.report(port);
    }

    /// Writing a one to `PINx` toggles the corresponding `PORTx` bit, which is
    /// how firmware implements a fast pin toggle.
    pub fn toggle_via_pin(&mut self, port: usize, value: u8) {
        self.port[port] ^= value;
        self.report(port);
    }

    /// Set the level a host drives onto a digital pin. Pins configured as
    /// outputs ignore the host; the board is not allowed to fight the chip.
    pub fn set_input(&mut self, pin: u8, high: bool) {
        if let Some((port, bit)) = digital_to_port(pin) {
            if high {
                self.input[port] |= 1 << bit;
            } else {
                self.input[port] &= !(1 << bit);
            }
        }
    }

    /// The raw external input latch for a port, used by tests.
    pub fn input(&self, port: usize) -> u8 {
        self.input[port]
    }

    fn report(&mut self, port: usize) {
        for bit in 0..8u8 {
            let index = port * 8 + usize::from(bit);
            let level = self.driven_level(port, bit);
            if self.reported[index] == level {
                continue;
            }
            self.reported[index] = level;
            if let Some(high) = level
                && let Some(pin) = port_to_digital(port, bit)
            {
                self.events.push((pin, high));
                if let Some(callback) = self.callback.as_mut() {
                    callback(pin, high);
                }
            }
        }
    }
}

/// The watchdog timer. Only the interrupt mode is modelled: a timeout sets
/// `WDIF`, and `WDR` clears the counter. A watchdog configured for system
/// reset does not reset the core.
#[derive(Default)]
pub struct Watchdog {
    control: u8,
    counter: u64,
    flag: bool,
}

impl Watchdog {
    /// `WDTCSR` as firmware reads it.
    pub fn read_control(&self) -> u8 {
        (self.control & !0x80) | if self.flag { 0x80 } else { 0 }
    }

    pub fn write_control(&mut self, value: u8) {
        // WDIF is write-one-to-clear; the other bits are storage.
        let flag = self.flag && value & 0x80 == 0;
        self.control = value & 0x7f;
        self.flag = flag;
        if self.control & 0x48 == 0 {
            self.counter = 0;
        }
    }

    pub(crate) fn reset(&mut self) {
        self.counter = 0;
    }

    fn enabled(&self) -> bool {
        self.control & 0x48 != 0 // WDE or WDIE
    }

    fn timeout_cycles(&self) -> u64 {
        // The watchdog runs from its own ~128 kHz oscillator, so one watchdog
        // cycle is 125 CPU cycles at 16 MHz. WDP 0 is 2K watchdog cycles
        // (16 ms); WDP 9 is 1024K (8 s).
        let code = ((self.control >> 2) & 0x08) | (self.control & 0x07);
        (2048u64 << u32::from(code)) * 125
    }

    fn advance(&mut self, cycles: u64) {
        if !self.enabled() {
            self.counter = 0;
            return;
        }
        self.counter = self.counter.saturating_add(cycles);
        if self.counter >= self.timeout_cycles() {
            self.counter = 0;
            self.flag = true;
        }
    }

    fn interrupt_pending(&self) -> bool {
        self.flag && self.control & 0x40 != 0
    }

    fn can_wake(&self) -> bool {
        self.control & 0x40 != 0
    }

    fn acknowledge(&mut self) {
        self.flag = false;
    }
}

/// Every peripheral behind the data bus.
pub struct Peripherals {
    pub gpio: Gpio,
    pub timer0: Timer0,
    pub timer1: Timer1,
    pub timer2: Timer2,
    pub usart0: Usart0,
    pub adc: Adc,
    pub spi: Spi,
    pub twi: Twi,
    pub eeprom: Eeprom,
    pub watchdog: Watchdog,
    pub eicra: u8,
    pub eimsk: u8,
    pub eifr: u8,
    pub pcicr: u8,
    pub pcifr: u8,
    pub pcmsk: [u8; 3],
    pub spmcsr: u8,
    pub acsr: u8,
    last_pins: [u8; 3],
    last_icp1: bool,
    last_aco: bool,
    /// Backing store for I/O registers that are modelled as plain storage.
    io: [u8; 0xe0],
}

impl Default for Peripherals {
    fn default() -> Self {
        Self {
            gpio: Gpio::default(),
            timer0: Timer0::default(),
            timer1: Timer1::default(),
            timer2: Timer2::default(),
            usart0: Usart0::default(),
            adc: Adc::default(),
            spi: Spi::new(),
            twi: Twi::new(),
            eeprom: Eeprom::default(),
            watchdog: Watchdog::default(),
            eicra: 0,
            eimsk: 0,
            eifr: 0,
            pcicr: 0,
            pcifr: 0,
            pcmsk: [0; 3],
            spmcsr: 0,
            acsr: 0,
            last_pins: [0; 3],
            last_icp1: false,
            last_aco: false,
            io: [0; 0xe0],
        }
    }
}

impl Peripherals {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read one peripheral register. `addr` is a data-space address in
    /// `0x20..=0xff`; anything else reads as zero.
    pub fn read(&mut self, addr: u16) -> u8 {
        let Some(index) = addr.checked_sub(0x20).map(usize::from) else {
            return 0;
        };
        if index >= self.io.len() {
            return 0;
        }
        match addr {
            addr::PINB => self.gpio.pin(PORT_B),
            addr::PINC => self.gpio.pin(PORT_C),
            addr::PIND => self.gpio.pin(PORT_D),
            addr::DDRB => self.gpio.ddr(PORT_B),
            addr::DDRC => self.gpio.ddr(PORT_C),
            addr::DDRD => self.gpio.ddr(PORT_D),
            addr::PORTB => self.gpio.port(PORT_B),
            addr::PORTC => self.gpio.port(PORT_C),
            addr::PORTD => self.gpio.port(PORT_D),
            addr::TIFR0 => self.timer0.tifr,
            addr::TIFR1 => self.timer1.tifr,
            addr::TIFR2 => self.timer2.tifr,
            addr::PCIFR => self.pcifr,
            addr::EIFR => self.eifr,
            addr::EIMSK => self.eimsk,
            addr::EICRA => self.eicra,
            addr::PCICR => self.pcicr,
            addr::PCMSK0 => self.pcmsk[0],
            addr::PCMSK1 => self.pcmsk[1],
            addr::PCMSK2 => self.pcmsk[2],
            addr::TCCR0A => self.timer0.tccr_a,
            addr::TCCR0B => self.timer0.tccr_b,
            addr::TCNT0 => self.timer0.count,
            addr::OCR0A => self.timer0.ocr_a,
            addr::OCR0B => self.timer0.ocr_b,
            addr::TIMSK0 => self.timer0.timsk,
            addr::TCCR1A => self.timer1.tccr_a,
            addr::TCCR1B => self.timer1.tccr_b,
            addr::TCCR1C => self.timer1.tccr_c,
            addr::TCNT1L => self.timer1.count as u8,
            addr::TCNT1H => (self.timer1.count >> 8) as u8,
            addr::ICR1L => self.timer1.icr as u8,
            addr::ICR1H => (self.timer1.icr >> 8) as u8,
            addr::OCR1AL => self.timer1.ocr_a as u8,
            addr::OCR1AH => (self.timer1.ocr_a >> 8) as u8,
            addr::OCR1BL => self.timer1.ocr_b as u8,
            addr::OCR1BH => (self.timer1.ocr_b >> 8) as u8,
            addr::TIMSK1 => self.timer1.timsk,
            addr::TCCR2A => self.timer2.tccr_a,
            addr::TCCR2B => self.timer2.tccr_b,
            addr::TCNT2 => self.timer2.count,
            addr::OCR2A => self.timer2.ocr_a,
            addr::OCR2B => self.timer2.ocr_b,
            addr::TIMSK2 => self.timer2.timsk,
            addr::SPCR => self.spi.spcr,
            addr::SPSR => self.spi.read_status(),
            addr::SPDR => self.spi.read_data(),
            addr::ACSR => self.acsr,
            addr::SPMCSR => self.spmcsr,
            addr::WDTCSR => self.watchdog.read_control(),
            addr::ADCL => self.adc.adcl,
            addr::ADCH => self.adc.adch,
            addr::ADCSRA => self.adc.read_control(),
            addr::ADCSRB => self.adc.adcsrb,
            addr::ADMUX => self.adc.admux,
            addr::TWBR => self.twi.twbr,
            addr::TWSR => self.twi.read_status(),
            addr::TWAR => self.twi.twar,
            addr::TWDR => self.twi.twdr,
            addr::TWCR => self.twi.read_control(),
            addr::TWAMR => self.twi.twamr,
            addr::EECR => self.eeprom.read_control(),
            addr::EEDR => self.eeprom.eedr,
            addr::EEARL => self.eeprom.read_low(),
            addr::EEARH => self.eeprom.read_high(),
            addr::UCSR0A => self.usart0.status(),
            addr::UCSR0B => self.usart0.ucsr_b,
            addr::UCSR0C => self.usart0.ucsr_c,
            addr::UBRR0L => self.usart0.ubrr_l,
            addr::UBRR0H => self.usart0.ubrr_h,
            addr::UDR0 => self.usart0.read_data(),
            _ => self.io[index],
        }
    }

    /// Write one peripheral register. Bad addresses are ignored.
    pub fn write(&mut self, addr: u16, value: u8) {
        let Some(index) = addr.checked_sub(0x20).map(usize::from) else {
            return;
        };
        if index >= self.io.len() {
            return;
        }
        match addr {
            addr::PINB => self.gpio.toggle_via_pin(PORT_B, value),
            addr::PINC => self.gpio.toggle_via_pin(PORT_C, value),
            addr::PIND => self.gpio.toggle_via_pin(PORT_D, value),
            addr::DDRB => self.gpio.set_ddr(PORT_B, value),
            addr::DDRC => self.gpio.set_ddr(PORT_C, value),
            addr::DDRD => self.gpio.set_ddr(PORT_D, value),
            addr::PORTB => self.gpio.set_port(PORT_B, value),
            addr::PORTC => self.gpio.set_port(PORT_C, value),
            addr::PORTD => self.gpio.set_port(PORT_D, value),
            // Interrupt flags are write-one-to-clear.
            addr::TIFR0 => self.timer0.tifr &= !value,
            addr::TIFR1 => self.timer1.tifr &= !value,
            addr::TIFR2 => self.timer2.tifr &= !value,
            addr::PCIFR => self.pcifr &= !value,
            addr::EIFR => self.eifr &= !value,
            addr::EIMSK => self.eimsk = value,
            addr::EICRA => self.eicra = value,
            addr::PCICR => self.pcicr = value,
            addr::PCMSK0 => self.pcmsk[0] = value,
            addr::PCMSK1 => self.pcmsk[1] = value,
            addr::PCMSK2 => self.pcmsk[2] = value,
            addr::TCCR0A => self.timer0.tccr_a = value,
            addr::TCCR0B => self.timer0.tccr_b = value,
            addr::TCNT0 => self.timer0.count = value,
            addr::OCR0A => self.timer0.ocr_a = value,
            addr::OCR0B => self.timer0.ocr_b = value,
            addr::TIMSK0 => self.timer0.timsk = value,
            addr::TCCR1A => self.timer1.tccr_a = value,
            addr::TCCR1B => self.timer1.tccr_b = value,
            addr::TCCR1C => self.timer1.tccr_c = value,
            addr::TCNT1L => self.timer1.count = (self.timer1.count & 0xff00) | u16::from(value),
            addr::TCNT1H => {
                self.timer1.count = (self.timer1.count & 0x00ff) | (u16::from(value) << 8);
            }
            addr::ICR1L => self.timer1.icr = (self.timer1.icr & 0xff00) | u16::from(value),
            addr::ICR1H => self.timer1.icr = (self.timer1.icr & 0x00ff) | (u16::from(value) << 8),
            addr::OCR1AL => self.timer1.ocr_a = (self.timer1.ocr_a & 0xff00) | u16::from(value),
            addr::OCR1AH => {
                self.timer1.ocr_a = (self.timer1.ocr_a & 0x00ff) | (u16::from(value) << 8);
            }
            addr::OCR1BL => self.timer1.ocr_b = (self.timer1.ocr_b & 0xff00) | u16::from(value),
            addr::OCR1BH => {
                self.timer1.ocr_b = (self.timer1.ocr_b & 0x00ff) | (u16::from(value) << 8);
            }
            addr::TIMSK1 => self.timer1.timsk = value,
            addr::TCCR2A => self.timer2.tccr_a = value,
            addr::TCCR2B => self.timer2.tccr_b = value,
            addr::TCNT2 => self.timer2.count = value,
            addr::OCR2A => self.timer2.ocr_a = value,
            addr::OCR2B => self.timer2.ocr_b = value,
            addr::TIMSK2 => self.timer2.timsk = value,
            addr::SPCR => self.spi.spcr = value,
            addr::SPSR => self.spi.write_status(value),
            addr::SPDR => self.spi.write_data(value),
            addr::ACSR => self.write_acsr(value),
            addr::SPMCSR => self.spmcsr = value,
            addr::WDTCSR => self.watchdog.write_control(value),
            addr::ADCSRA => self.adc.write_control(value),
            addr::ADCSRB => self.adc.adcsrb = value,
            addr::ADMUX => self.adc.admux = value,
            addr::TWBR => self.twi.twbr = value,
            addr::TWSR => self.twi.write_status(value),
            addr::TWAR => self.twi.twar = value,
            addr::TWDR => self.twi.twdr = value,
            addr::TWCR => self.twi.write_control(value),
            addr::TWAMR => self.twi.twamr = value,
            addr::EECR => self.eeprom.write_control(value),
            addr::EEDR => self.eeprom.eedr = value,
            addr::EEARL => self.eeprom.write_low(value),
            addr::EEARH => self.eeprom.write_high(value),
            addr::UCSR0A => self.usart0.write_status(value),
            addr::UCSR0B => self.usart0.ucsr_b = value,
            addr::UCSR0C => self.usart0.ucsr_c = value,
            addr::UBRR0L => self.usart0.ubrr_l = value,
            addr::UBRR0H => self.usart0.ubrr_h = value,
            addr::UDR0 => self.usart0.write_data(value),
            _ => self.io[index] = value,
        }
        self.sync_oc();
        self.sample_pins();
        self.update_comparator();
    }

    fn write_acsr(&mut self, value: u8) {
        // ACI is write-one-to-clear; ACO is read-only and set by the
        // comparator.
        let aci = self.acsr & 0x10 & !value;
        self.acsr = (value & !0x30) | aci | (self.acsr & 0x20);
    }

    /// Advance every peripheral by the cycles one instruction (or one
    /// interrupt entry) retired.
    pub fn advance(&mut self, cycles: u8) {
        let cycles = u64::from(cycles);
        self.timer0.advance(cycles);
        self.timer1.advance(cycles);
        self.timer2.advance(cycles);
        self.adc.advance(cycles);
        self.spi.advance(cycles);
        self.eeprom.advance(cycles);
        self.watchdog.advance(cycles);
        self.sync_oc();
        self.sample_pins();
        self.update_comparator();
    }

    /// Drive the six compare-output pins from the timer state. Called after
    /// every advance and every register write so `analogWrite` takes effect on
    /// the very next edge.
    pub(crate) fn sync_oc(&mut self) {
        let outputs = [
            (PORT_D, 6u8, self.timer0.oc_a_level()),
            (PORT_D, 5u8, self.timer0.oc_b_level()),
            (PORT_B, 1u8, self.timer1.oc_a_level()),
            (PORT_B, 2u8, self.timer1.oc_b_level()),
            (PORT_B, 3u8, self.timer2.oc_a_level()),
            (PORT_D, 3u8, self.timer2.oc_b_level()),
        ];
        for (port, bit, level) in outputs {
            self.gpio.set_override(port, bit, level);
        }
    }

    /// Sample every pin after a change: pin-change flags, external interrupt
    /// flags, Timer1 input capture and the analog comparator all watch pin
    /// levels.
    fn sample_pins(&mut self) {
        let old = self.last_pins;
        let mut now = [0u8; 3];
        for (port, slot) in now.iter_mut().enumerate() {
            *slot = self.gpio.pin(port);
        }
        for port in 0..3 {
            let changed = now[port] ^ old[port];
            if changed != 0 {
                let enable = 1u8 << port;
                if self.pcicr & enable != 0 && changed & self.pcmsk[port] != 0 {
                    self.pcifr |= enable;
                }
            }
        }
        self.update_external_interrupt(0, 0x04, old[PORT_D], now[PORT_D]);
        self.update_external_interrupt(1, 0x08, old[PORT_D], now[PORT_D]);

        let icp1 = now[PORT_B] & 0x01 != 0;
        let rising = self.timer1.tccr_b & 0x40 != 0;
        if icp1 != self.last_icp1 && ((rising && icp1) || (!rising && !icp1)) {
            self.timer1.capture();
        }
        self.last_icp1 = icp1;
        self.last_pins = now;
    }

    fn update_external_interrupt(&mut self, index: u8, mask: u8, old: u8, now: u8) {
        let isc = (self.eicra >> (index * 2)) & 0x03;
        let changed = (old ^ now) & mask != 0;
        let high = now & mask != 0;
        let fire = match isc {
            0 => !high,            // low level
            1 => changed,          // any logical change
            2 => changed && !high, // falling edge
            _ => changed && high,  // rising edge
        };
        if fire {
            self.eifr |= 1 << index;
        }
    }

    fn update_comparator(&mut self) {
        let disabled = self.acsr & 0x80 != 0;
        let bandgap = self.acsr & 0x40 != 0;
        let pins = self.gpio.pin(PORT_D);
        let positive = pins & 0x40 != 0; // AIN0 = PD6
        let negative = if bandgap { false } else { pins & 0x80 != 0 }; // AIN1 = PD7
        let output = !disabled && positive && !negative;
        if !disabled && output != self.last_aco {
            self.acsr |= 0x10; // ACI
        }
        self.acsr = (self.acsr & !0x20) | if output { 0x20 } else { 0 };
        self.last_aco = output;
    }

    /// The highest-priority pending interrupt, or `None`. Only sources whose
    /// enable bits are set are considered, and the scan is in vector order, so
    /// the first match is the one hardware would take.
    pub fn pending_interrupt(&self) -> Option<u8> {
        if self.eifr & 0x01 != 0 && self.eimsk & 0x01 != 0 {
            return Some(vector::INT0);
        }
        if self.eifr & 0x02 != 0 && self.eimsk & 0x02 != 0 {
            return Some(vector::INT1);
        }
        for (bit, vector) in [
            (0x01, vector::PCINT0),
            (0x02, vector::PCINT1),
            (0x04, vector::PCINT2),
        ] {
            if self.pcifr & bit != 0 && self.pcicr & bit != 0 {
                return Some(vector);
            }
        }
        if self.watchdog.interrupt_pending() {
            return Some(vector::WDT);
        }
        if self.timer2.tifr & 0x02 != 0 && self.timer2.timsk & 0x02 != 0 {
            return Some(vector::TIMER2_COMPA);
        }
        if self.timer2.tifr & 0x04 != 0 && self.timer2.timsk & 0x04 != 0 {
            return Some(vector::TIMER2_COMPB);
        }
        if self.timer2.tifr & 0x01 != 0 && self.timer2.timsk & 0x01 != 0 {
            return Some(vector::TIMER2_OVF);
        }
        if self.timer1.tifr & 0x20 != 0 && self.timer1.timsk & 0x20 != 0 {
            return Some(vector::TIMER1_CAPT);
        }
        if self.timer1.tifr & 0x02 != 0 && self.timer1.timsk & 0x02 != 0 {
            return Some(vector::TIMER1_COMPA);
        }
        if self.timer1.tifr & 0x04 != 0 && self.timer1.timsk & 0x04 != 0 {
            return Some(vector::TIMER1_COMPB);
        }
        if self.timer1.tifr & 0x01 != 0 && self.timer1.timsk & 0x01 != 0 {
            return Some(vector::TIMER1_OVF);
        }
        if self.timer0.tifr & 0x02 != 0 && self.timer0.timsk & 0x02 != 0 {
            return Some(vector::TIMER0_COMPA);
        }
        if self.timer0.tifr & 0x04 != 0 && self.timer0.timsk & 0x04 != 0 {
            return Some(vector::TIMER0_COMPB);
        }
        if self.timer0.tifr & 0x01 != 0 && self.timer0.timsk & 0x01 != 0 {
            return Some(vector::TIMER0_OVF);
        }
        if self.spi.interrupt_pending() {
            return Some(vector::SPI_STC);
        }
        if self.usart0.rx_interrupt_enabled() && self.usart0.rxc() {
            return Some(vector::USART_RX);
        }
        if self.usart0.udre_interrupt_enabled() && self.usart0.udre() {
            return Some(vector::USART_UDRE);
        }
        if self.usart0.tx_interrupt_enabled() && self.usart0.txc() {
            return Some(vector::USART_TX);
        }
        if self.adc.interrupt_pending() {
            return Some(vector::ADC);
        }
        if self.eeprom.interrupt_pending() {
            return Some(vector::EE_READY);
        }
        if self.acsr & 0x10 != 0 && self.acsr & 0x08 != 0 {
            return Some(vector::ANALOG_COMP);
        }
        if self.twi.interrupt_pending() {
            return Some(vector::TWI);
        }
        if self.spmcsr & 0x80 != 0 {
            return Some(vector::SPM_READY);
        }
        None
    }

    /// Clear the flag a vector owns, as hardware does when it services the
    /// interrupt. Level-triggered sources (`RXC0`, `UDRE0`) are left alone;
    /// their ISR clears them by touching `UDR0`.
    pub fn acknowledge(&mut self, vector: u8) {
        match vector {
            vector::INT0 => self.eifr &= !0x01,
            vector::INT1 => self.eifr &= !0x02,
            vector::PCINT0 => self.pcifr &= !0x01,
            vector::PCINT1 => self.pcifr &= !0x02,
            vector::PCINT2 => self.pcifr &= !0x04,
            vector::WDT => self.watchdog.acknowledge(),
            vector::TIMER2_COMPA => self.timer2.tifr &= !0x02,
            vector::TIMER2_COMPB => self.timer2.tifr &= !0x04,
            vector::TIMER2_OVF => self.timer2.tifr &= !0x01,
            vector::TIMER1_CAPT => self.timer1.tifr &= !0x20,
            vector::TIMER1_COMPA => self.timer1.tifr &= !0x02,
            vector::TIMER1_COMPB => self.timer1.tifr &= !0x04,
            vector::TIMER1_OVF => self.timer1.tifr &= !0x01,
            vector::TIMER0_COMPA => self.timer0.tifr &= !0x02,
            vector::TIMER0_COMPB => self.timer0.tifr &= !0x04,
            vector::TIMER0_OVF => self.timer0.tifr &= !0x01,
            vector::SPI_STC => self.spi.acknowledge(),
            vector::USART_TX => self.usart0.acknowledge_tx(),
            vector::ADC => self.adc.acknowledge(),
            vector::EE_READY => self.eeprom.acknowledge(),
            vector::ANALOG_COMP => self.acsr &= !0x10,
            vector::TWI => self.twi.acknowledge(),
            _ => {}
        }
    }

    /// Whether any enabled source could wake a sleeping core. A source that is
    /// enabled but has no way to change state on its own (a stopped timer, an
    /// idle ADC) is not counted.
    pub fn any_interrupt_enabled(&self) -> bool {
        self.eimsk != 0
            || self.pcicr != 0
            || self.watchdog.can_wake()
            || (self.timer0.running() && self.timer0.timsk & 0x07 != 0)
            || (self.timer1.running() && self.timer1.timsk & 0x27 != 0)
            || (self.timer2.running() && self.timer2.timsk & 0x07 != 0)
            || self.spi.can_wake()
            || self.usart0.rx_interrupt_enabled()
            || (self.usart0.udre_interrupt_enabled() && self.usart0.udre())
            || (self.usart0.tx_interrupt_enabled() && self.usart0.txc())
            || self.adc.can_wake()
            || self.eeprom.can_wake()
            || self.acsr & 0x08 != 0
            || self.twi.can_wake()
            || self.spmcsr & 0x80 != 0
    }

    /// Queue a byte for the firmware's receive path.
    pub fn receive(&mut self, value: u8) {
        self.usart0.receive_byte(value);
    }

    /// Bytes the firmware has written to `UDR0`, drained.
    pub fn take_console(&mut self) -> Vec<u8> {
        self.usart0.take()
    }

    /// Install the GPIO output callback.
    pub fn set_gpio_callback(&mut self, callback: Box<dyn FnMut(u8, bool)>) {
        self.gpio.set_callback(callback);
    }

    /// Output edges observed since the last call.
    pub fn take_outputs(&mut self) -> Vec<(u8, bool)> {
        self.gpio.take_events()
    }

    /// The high-time fraction each compare-output pin is programmed to, in
    /// board (Arduino header) numbering: `OC0A` D6, `OC0B` D5, `OC1A` D9,
    /// `OC1B` D10, `OC2A` D11, `OC2B` D3. A timer whose compare output is
    /// disconnected or stopped contributes nothing.
    pub fn pwm_duties(&self) -> Vec<(u8, f64)> {
        [
            (6, self.timer0.oc_a_duty()),
            (5, self.timer0.oc_b_duty()),
            (9, self.timer1.oc_a_duty()),
            (10, self.timer1.oc_b_duty()),
            (11, self.timer2.oc_a_duty()),
            (3, self.timer2.oc_b_duty()),
        ]
        .into_iter()
        .filter_map(|(gpio, duty)| duty.map(|duty| (gpio, duty)))
        .collect()
    }

    /// Drive an Arduino digital pin from the host.
    pub fn set_input(&mut self, pin: u8, high: bool) {
        self.gpio.set_input(pin, high);
        self.sync_oc();
        self.sample_pins();
        self.update_comparator();
    }

    /// Set the voltage on an ADC channel, in microvolts.
    pub fn set_analog_input(&mut self, channel: u8, microvolts: u32) {
        self.adc.set_input(channel, microvolts);
    }

    /// The watchdog instruction `WDR` clears the watchdog counter.
    pub fn watchdog_reset(&mut self) {
        self.watchdog.reset();
    }

    pub(crate) fn take_gpio_callback(&mut self) -> Option<Box<dyn FnMut(u8, bool)>> {
        self.gpio.take_callback()
    }

    pub(crate) fn take_spi_callback(&mut self) -> Option<spi::SpiTransfer> {
        self.spi.take_callback()
    }

    pub(crate) fn take_i2c_callbacks(&mut self) -> (Option<twi::I2cWrite>, Option<twi::I2cRead>) {
        self.twi.take_callbacks()
    }
}

impl fmt::Debug for Peripherals {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Peripherals")
            .field("ddr_b", &self.gpio.ddr(PORT_B))
            .field("port_b", &self.gpio.port(PORT_B))
            .field("tcnt0", &self.timer0.count)
            .field("tifr0", &self.timer0.tifr)
            .field("tcnt1", &self.timer1.count)
            .field("tifr1", &self.timer1.tifr)
            .field("tcnt2", &self.timer2.count)
            .field("tifr2", &self.timer2.tifr)
            .finish()
    }
}
