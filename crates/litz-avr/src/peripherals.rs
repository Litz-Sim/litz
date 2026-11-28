//! The on-chip peripherals: GPIO, the timers, the USARTs, the ADC, SPI, TWI,
//! the ATtiny's USI, the EEPROM, the watchdog and the interrupt controller
//! that ties them together.
//!
//! Every peripheral exposes its registers through [`Peripherals::read`] and
//! [`Peripherals::write`], which the core reaches through
//! [`crate::cpu::DataBus`]. A register a part does not have is not dispatched:
//! it falls through to plain storage and reads as zero, which matches the
//! datasheet's "reserved" behaviour.
//!
//! # Interrupts
//!
//! The [`vector`] module names the ATmega328P's vectors, and each device table
//! in [`crate::device`] carries the same information for its own part. The
//! core asks [`Peripherals::pending_interrupt`] before every instruction; when
//! the `I` flag is set and a pending vector exists, the core pushes the return
//! address, clears `I` and jumps to the vector's address. The entry cost and
//! the PC width come from the device, so an ATmega2560 pushes three bytes and
//! retires five cycles. `RETI` restores `I`. Sources that have a flag cleared
//! by hardware on vector entry clear it in [`Peripherals::acknowledge`];
//! `RXC`/`UDRE` are level conditions and stay pending until the ISR reads or
//! writes the data register.
//!
//! # GPIO and timer outputs
//!
//! A pin with `DDR` set can be driven by its `PORT` latch or by a timer
//! compare output. The timer level wins whenever the corresponding `COMnx`
//! bits are non-zero, which is what makes `analogWrite` move the pin. Every
//! driven edge is reported to the host and surfaces in
//! [`Peripherals::take_outputs`].

pub mod adc;
pub mod eeprom;
pub mod spi;
pub mod timers;
pub mod twi;
pub mod usart;
pub mod usi;

use std::fmt;

use crate::device::{ATMEGA328P, Device, NONE, Port};
pub use adc::Adc;
pub use eeprom::Eeprom;
pub use spi::{Spi, SpiTransfer};
pub use timers::{Timer, Timer0, Timer1, Timer2};
pub use twi::{I2cRead, I2cWrite, Twi};
pub use usart::Usart0;
pub use usi::Usi;

/// Data-space addresses of the ATmega328P registers this module answers.
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
    pub const TIFR3: u16 = 0x38;
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
    pub const EICRB: u16 = 0x6a;
    pub const PCMSK0: u16 = 0x6b;
    pub const PCMSK1: u16 = 0x6c;
    pub const PCMSK2: u16 = 0x6d;
    pub const TIMSK0: u16 = 0x6e;
    pub const TIMSK1: u16 = 0x6f;
    pub const TIMSK2: u16 = 0x70;
    pub const TIMSK3: u16 = 0x71;
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
    pub const UCSR1A: u16 = 0xc8;
    pub const UCSR1B: u16 = 0xc9;
    pub const UCSR1C: u16 = 0xca;
    pub const UBRR1L: u16 = 0xcc;
    pub const UBRR1H: u16 = 0xcd;
    pub const UDR1: u16 = 0xce;
    pub const TCCR3A: u16 = 0x90;
    pub const TCCR3B: u16 = 0x91;
    pub const TCNT3L: u16 = 0x94;
    pub const TCNT3H: u16 = 0x95;
    pub const OCR3AL: u16 = 0x98;
    pub const OCR3AH: u16 = 0x99;
    // ATtiny85 register addresses (data space).
    pub const USIDR: u16 = 0x2f;
    pub const USIBR: u16 = 0x30;
    pub const USISR: u16 = 0x2e;
    pub const USICR: u16 = 0x2d;
    pub const TCCR1_TINY: u16 = 0x50;
    pub const GTCCR_TINY: u16 = 0x4c;
    pub const OCR1C_TINY: u16 = 0x4d;
    pub const MCUCR_TINY: u16 = 0x55;
    pub const GIMSK_TINY: u16 = 0x5b;
    pub const GIFR_TINY: u16 = 0x5a;
}

/// The interrupt vectors of an ATmega328P, in datasheet priority order.
///
/// Vector `n` lives at byte address `4 * n` in flash (word address `2 * n`);
/// avr-gcc emits a four-byte `jmp` at each slot, which is why the spacing is
/// four bytes even though the datasheet's table numbers the slots in words.
/// The ATtiny85's slots are one word wide, so its table has a different
/// spacing; use [`word_address_for`] with a device for a part-specific
/// address.
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

    /// Byte address of a vector's slot on the ATmega328P.
    pub const fn byte_address(vector: u8) -> u16 {
        (vector as u16) * 4
    }

    /// Word address of a vector's slot on the ATmega328P.
    pub const fn word_address(vector: u8) -> u16 {
        (vector as u16) * 2
    }
}

/// Word address of a vector's slot on `device`.
pub fn word_address_for(device: &Device, vector: u8) -> u16 {
    u16::from(vector) * u16::from(device.vector_words)
}

/// Byte address of a vector's slot on `device`.
pub fn byte_address_for(device: &Device, vector: u8) -> u32 {
    u32::from(vector) * u32::from(device.vector_words) * 2
}

/// Cycles an interrupt entry costs on an ATmega328P after a 16-bit PC is
/// pushed. Other parts carry their own entry cost in the device table.
pub const INTERRUPT_CYCLES: u8 = 4;

/// The ATmega328P's GPIO ports, named as the datasheet names them.
pub const PORT_B: usize = 0;
pub const PORT_C: usize = 1;
pub const PORT_D: usize = 2;

/// Which port and bit an Arduino Uno pin maps to.
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

/// The most ports any modelled part has (the ATmega2560's A–L).
const MAX_PORTS: usize = 11;

/// GPIO ports: direction, output latch, external input levels and the timer
/// compare-output overrides. The port index is the port's position in the
/// device table.
pub struct Gpio {
    device: &'static Device,
    ddr: [u8; MAX_PORTS],
    port: [u8; MAX_PORTS],
    input: [u8; MAX_PORTS],
    /// Level a timer wants on a pin, when a compare output is connected.
    overridden: [Option<bool>; MAX_PORTS * 8],
    /// Last level reported per port/bit, so a callback sees edges only.
    reported: [Option<bool>; MAX_PORTS * 8],
    events: Vec<(u8, bool)>,
    callback: Option<Box<dyn FnMut(u8, bool)>>,
}

impl Default for Gpio {
    fn default() -> Self {
        Self::new(&ATMEGA328P)
    }
}

impl Gpio {
    /// GPIO for `device`, with every pin an input and every latch low.
    pub fn new(device: &'static Device) -> Self {
        Self {
            device,
            ddr: [0; MAX_PORTS],
            port: [0; MAX_PORTS],
            input: [0; MAX_PORTS],
            overridden: [None; MAX_PORTS * 8],
            reported: [None; MAX_PORTS * 8],
            events: Vec::new(),
            callback: None,
        }
    }

    /// Install a callback for output edges. It receives the host-facing pin
    /// number and the level the chip now drives.
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

    fn port_spec(&self, index: usize) -> Option<&'static Port> {
        self.device.ports.get(index)
    }

    pub fn ddr(&self, port: usize) -> u8 {
        self.ddr.get(port).copied().unwrap_or(0)
    }

    pub fn port(&self, port: usize) -> u8 {
        self.port.get(port).copied().unwrap_or(0)
    }

    /// Level driven back into `PINx`: output pins read the level the chip
    /// drives (the timer's compare output when one is connected, otherwise the
    /// latch); input pins read the external level.
    pub fn pin(&self, port: usize) -> u8 {
        if self.port_spec(port).is_none() {
            return 0;
        }
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
        if self.port_spec(port).is_none() || self.ddr[port] & (1 << bit) == 0 {
            return None;
        }
        let index = port * 8 + usize::from(bit);
        Some(self.overridden[index].unwrap_or(self.port[port] & (1 << bit) != 0))
    }

    /// Connect (or disconnect) a timer compare output. The level only reaches
    /// the pin when `DDR` makes it an output.
    pub(crate) fn set_override(&mut self, port: usize, bit: u8, level: Option<bool>) {
        if self.port_spec(port).is_none() {
            return;
        }
        let index = port * 8 + usize::from(bit);
        if self.overridden[index] == level {
            return;
        }
        self.overridden[index] = level;
        self.report(port);
    }

    pub fn set_ddr(&mut self, port: usize, value: u8) {
        if self.port_spec(port).is_none() {
            return;
        }
        self.ddr[port] = value;
        self.report(port);
    }

    pub fn set_port(&mut self, port: usize, value: u8) {
        if self.port_spec(port).is_none() {
            return;
        }
        self.port[port] = value;
        self.report(port);
    }

    /// Writing a one to `PINx` toggles the corresponding `PORTx` bit, which is
    /// how firmware implements a fast pin toggle.
    pub fn toggle_via_pin(&mut self, port: usize, value: u8) {
        if self.port_spec(port).is_none() {
            return;
        }
        self.port[port] ^= value;
        self.report(port);
    }

    /// Set the level a host drives onto a host-facing pin. Pins configured as
    /// outputs ignore the host; the board is not allowed to fight the chip.
    pub fn set_input(&mut self, pin: u8, high: bool) {
        let device = self.device;
        if let Some((port, bit)) = device.port_of_pin(pin)
            && device.ports.get(port).is_some()
        {
            if high {
                self.input[port] |= 1 << bit;
            } else {
                self.input[port] &= !(1 << bit);
            }
        }
    }

    /// The raw external input latch for a port, used by tests.
    pub fn input(&self, port: usize) -> u8 {
        self.input.get(port).copied().unwrap_or(0)
    }

    fn report(&mut self, port: usize) {
        let device = self.device;
        for bit in 0..8u8 {
            let index = port * 8 + usize::from(bit);
            let level = self.driven_level(port, bit);
            if self.reported[index] == level {
                continue;
            }
            self.reported[index] = level;
            if let Some(high) = level
                && let Some(pin) = device.pin_of_port(port, bit)
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

    /// Watchdog timeouts are measured in this part's CPU cycles. The watchdog
    /// runs from its own ~128 kHz oscillator, so one watchdog cycle is 125 CPU
    /// cycles at 16 MHz; WDP 0 is 2K watchdog cycles (16 ms), WDP 9 is 1024K.
    fn timeout_cycles(&self, cpu_hz: u64) -> u64 {
        let code = ((self.control >> 2) & 0x08) | (self.control & 0x07);
        (2048u64 << u32::from(code)) * cpu_hz / 128_000
    }

    fn advance(&mut self, cycles: u64, cpu_hz: u64) {
        if !self.enabled() {
            self.counter = 0;
            return;
        }
        self.counter = self.counter.saturating_add(cycles);
        if self.counter >= self.timeout_cycles(cpu_hz) {
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
    device: &'static Device,
    pub gpio: Gpio,
    pub timer0: Timer,
    pub timer1: Timer,
    pub timer2: Timer,
    pub timer3: Timer,
    pub timer4: Timer,
    pub timer5: Timer,
    pub usart0: Usart0,
    pub usart1: Usart0,
    pub usart2: Usart0,
    pub usart3: Usart0,
    pub adc: Adc,
    pub spi: Spi,
    pub twi: Twi,
    pub usi: Usi,
    pub eeprom: Eeprom,
    pub watchdog: Watchdog,
    pub eicra: u8,
    pub eicrb: u8,
    pub eimsk: u8,
    pub eifr: u8,
    /// The ATtiny's combined `GIMSK`/`GIFR`, which carry `INT0`, `PCIE` and
    /// their flags.
    pub gimsk: u8,
    pub gifr: u8,
    pub mcucr: u8,
    pub pcicr: u8,
    pub pcifr: u8,
    pub pcmsk: [u8; 3],
    pub spmcsr: u8,
    pub acsr: u8,
    last_pins: [u8; MAX_PORTS],
    last_capture: [bool; 6],
    last_aco: bool,
    /// Backing store for I/O registers that are modelled as plain storage.
    io: Vec<u8>,
}

impl Default for Peripherals {
    fn default() -> Self {
        Self::new()
    }
}

impl Peripherals {
    /// An ATmega328P peripheral set.
    pub fn new() -> Self {
        Self::for_device(&ATMEGA328P)
    }

    /// The peripheral set of `device`.
    pub fn for_device(device: &'static Device) -> Self {
        let mut timers: [Timer; 6] = core::array::from_fn(|_| Timer::empty());
        for spec in device.timers {
            if let Some(slot) = timers.get_mut(usize::from(spec.index)) {
                *slot = Timer::new(spec);
            }
        }
        let [timer0, timer1, timer2, timer3, timer4, timer5] = timers;
        let write_cycles = (device.cpu_hz / 1000 * 3400 / 1000) as u32;
        Self {
            device,
            gpio: Gpio::new(device),
            timer0,
            timer1,
            timer2,
            timer3,
            timer4,
            timer5,
            usart0: Usart0::default(),
            usart1: Usart0::default(),
            usart2: Usart0::default(),
            usart3: Usart0::default(),
            adc: Adc::new(&device.adc),
            spi: Spi::new(),
            twi: Twi::new(),
            usi: Usi::new(),
            eeprom: Eeprom::new(device.eeprom_size, write_cycles),
            watchdog: Watchdog::default(),
            eicra: 0,
            eicrb: 0,
            eimsk: 0,
            eifr: 0,
            gimsk: 0,
            gifr: 0,
            mcucr: 0,
            pcicr: 0,
            pcifr: 0,
            pcmsk: [0; 3],
            spmcsr: 0,
            acsr: 0,
            last_pins: [0; MAX_PORTS],
            last_capture: [false; 6],
            last_aco: false,
            io: vec![0; 0x1e0],
        }
    }

    /// The device this peripheral set belongs to.
    pub fn device(&self) -> &'static Device {
        self.device
    }

    fn timer(&self, index: usize) -> &Timer {
        match index {
            0 => &self.timer0,
            1 => &self.timer1,
            2 => &self.timer2,
            3 => &self.timer3,
            4 => &self.timer4,
            _ => &self.timer5,
        }
    }

    fn timer_mut(&mut self, index: usize) -> &mut Timer {
        match index {
            0 => &mut self.timer0,
            1 => &mut self.timer1,
            2 => &mut self.timer2,
            3 => &mut self.timer3,
            4 => &mut self.timer4,
            _ => &mut self.timer5,
        }
    }

    fn usart_mut(&mut self, index: usize) -> &mut Usart0 {
        match index {
            0 => &mut self.usart0,
            1 => &mut self.usart1,
            2 => &mut self.usart2,
            _ => &mut self.usart3,
        }
    }

    fn usart(&self, index: usize) -> &Usart0 {
        match index {
            0 => &self.usart0,
            1 => &self.usart1,
            2 => &self.usart2,
            _ => &self.usart3,
        }
    }

    fn in_range(&self, addr: u16) -> Option<usize> {
        let index = usize::from(addr.checked_sub(0x20)?);
        (index < self.io.len()).then_some(index)
    }

    /// Read one peripheral register. `addr` is a data-space address; a
    /// register the device does not have reads as zero.
    pub fn read(&mut self, addr: u16) -> u8 {
        let Some(index) = self.in_range(addr) else {
            return 0;
        };
        let regs = self.device.regs;
        let ports = self.device.ports;
        let usarts = self.device.usarts;

        for port in ports {
            if addr == port.pin {
                return self.gpio.pin(port.index);
            }
            if addr == port.ddr {
                return self.gpio.ddr(port.index);
            }
            if addr == port.port {
                return self.gpio.port(port.index);
            }
        }

        if regs.shared_timer_irq {
            let first = self.timer0.spec().map(|spec| spec.regs);
            if let Some(first) = first {
                if addr == first.timsk {
                    return self.timer0.timsk | self.timer1.timsk;
                }
                if addr == first.tifr {
                    return self.timer0.tifr | self.timer1.tifr;
                }
            }
        }
        for counter in 0..6 {
            if let Some(value) = self.timer_mut(counter).read_register(addr) {
                return value;
            }
        }

        for (number, usart) in usarts.iter().enumerate() {
            if usart.control_a != NONE && addr == usart.control_a {
                return self.usart(number).status();
            }
            if usart.control_b != NONE && addr == usart.control_b {
                return self.usart(number).ucsr_b;
            }
            if usart.control_c != NONE && addr == usart.control_c {
                return self.usart(number).ucsr_c;
            }
            if usart.baud_low != NONE && addr == usart.baud_low {
                return self.usart(number).ubrr_l;
            }
            if usart.baud_high != NONE && addr == usart.baud_high {
                return self.usart(number).ubrr_h;
            }
            if usart.data != NONE && addr == usart.data {
                return self.usart_mut(number).read_data();
            }
        }

        let adc = self.device.adc;
        if addr == adc.control {
            return self.adc.read_control();
        }
        if addr == adc.control_b {
            return self.adc.adcsrb;
        }
        if addr == adc.mux {
            return self.adc.admux;
        }
        if addr == adc.data_low {
            return self.adc.adcl;
        }
        if addr == adc.data_high {
            return self.adc.adch;
        }

        if regs.spi_status != NONE && addr == regs.spi_status {
            return self.spi.read_status();
        }
        if regs.spi_control != NONE && addr == regs.spi_control {
            return self.spi.spcr;
        }
        if regs.spi_data != NONE && addr == regs.spi_data {
            return self.spi.read_data();
        }
        if addr == regs.analog_comparator {
            return self.acsr;
        }
        if addr == regs.spm_control {
            return self.spmcsr;
        }
        if addr == regs.watchdog_control {
            return self.watchdog.read_control();
        }
        if regs.twi_status != NONE && addr == regs.twi_status {
            return self.twi.read_status();
        }
        if regs.twi_bit_rate != NONE && addr == regs.twi_bit_rate {
            return self.twi.twbr;
        }
        if regs.twi_address != NONE && addr == regs.twi_address {
            return self.twi.twar;
        }
        if regs.twi_data != NONE && addr == regs.twi_data {
            return self.twi.twdr;
        }
        if regs.twi_control != NONE && addr == regs.twi_control {
            return self.twi.read_control();
        }
        if regs.twi_mask != NONE && addr == regs.twi_mask {
            return self.twi.twamr;
        }
        if regs.eeprom_control != NONE && addr == regs.eeprom_control {
            return self.eeprom.read_control();
        }
        if regs.eeprom_data != NONE && addr == regs.eeprom_data {
            return self.eeprom.eedr;
        }
        if regs.eeprom_address_low != NONE && addr == regs.eeprom_address_low {
            return self.eeprom.read_low();
        }
        if regs.eeprom_address_high != NONE && addr == regs.eeprom_address_high {
            return self.eeprom.read_high();
        }
        if regs.usi_data != NONE && addr == regs.usi_data {
            return self.usi.usidr;
        }
        if regs.usi_buffer != NONE && addr == regs.usi_buffer {
            return self.usi.usibr;
        }
        if regs.usi_status != NONE && addr == regs.usi_status {
            return self.usi.read_status();
        }
        if regs.usi_control != NONE && addr == regs.usi_control {
            return self.usi.usicr;
        }

        if regs.ext_int_control_a != NONE && addr == regs.ext_int_control_a {
            return self.eicra;
        }
        if regs.ext_int_control_b != NONE && addr == regs.ext_int_control_b {
            return self.eicrb;
        }
        if regs.mcu_control != NONE && addr == regs.mcu_control {
            return self.mcucr;
        }
        if regs.ext_int_mask != NONE && addr == regs.ext_int_mask {
            return self.eimsk;
        }
        if regs.ext_int_flags != NONE && addr == regs.ext_int_flags {
            return self.eifr;
        }
        if regs.general_int_mask != NONE && addr == regs.general_int_mask {
            return self.gimsk;
        }
        if regs.general_int_flags != NONE && addr == regs.general_int_flags {
            return self.gifr;
        }
        if regs.pin_change_control != NONE && addr == regs.pin_change_control {
            return self.pcicr;
        }
        if regs.pin_change_flags != NONE && addr == regs.pin_change_flags {
            return self.pcifr;
        }
        for (slot, &mask) in regs.pin_change_mask.iter().enumerate() {
            if mask != NONE && addr == mask {
                return self.pcmsk[slot];
            }
        }

        self.io[index]
    }

    /// Write one peripheral register. A register the device does not have is
    /// plain storage.
    pub fn write(&mut self, addr: u16, value: u8) {
        let Some(index) = self.in_range(addr) else {
            return;
        };
        let regs = self.device.regs;
        let ports = self.device.ports;
        let usarts = self.device.usarts;

        let mut handled = true;
        let mut port_written = None;
        for port in ports {
            if addr == port.pin {
                self.gpio.toggle_via_pin(port.index, value);
                port_written = Some(port.index);
            } else if addr == port.ddr {
                self.gpio.set_ddr(port.index, value);
                port_written = Some(port.index);
            } else if addr == port.port {
                self.gpio.set_port(port.index, value);
                port_written = Some(port.index);
            }
        }
        if port_written.is_none() {
            handled = false;
        }

        if !handled && regs.shared_timer_irq {
            if let Some(first) = self.timer0.spec().map(|spec| spec.regs) {
                if addr == first.timsk {
                    self.timer0.timsk = value;
                    self.timer1.timsk = value;
                    handled = true;
                } else if addr == first.tifr {
                    self.timer0.tifr &= !value;
                    self.timer1.tifr &= !value;
                    handled = true;
                }
            }
        }
        if !handled {
            for counter in 0..6 {
                if self.timer_mut(counter).write_register(addr, value) {
                    handled = true;
                    break;
                }
            }
        }

        if !handled {
            for (number, usart) in usarts.iter().enumerate() {
                if usart.control_a != NONE && addr == usart.control_a {
                    self.usart_mut(number).write_status(value);
                    handled = true;
                } else if usart.control_b != NONE && addr == usart.control_b {
                    self.usart_mut(number).ucsr_b = value;
                    handled = true;
                } else if usart.control_c != NONE && addr == usart.control_c {
                    self.usart_mut(number).ucsr_c = value;
                    handled = true;
                } else if usart.baud_low != NONE && addr == usart.baud_low {
                    self.usart_mut(number).ubrr_l = value;
                    handled = true;
                } else if usart.baud_high != NONE && addr == usart.baud_high {
                    self.usart_mut(number).ubrr_h = value;
                    handled = true;
                } else if usart.data != NONE && addr == usart.data {
                    self.usart_mut(number).write_data(value);
                    handled = true;
                }
                if handled {
                    break;
                }
            }
        }

        if !handled {
            let adc = self.device.adc;
            if addr == adc.control {
                self.adc.write_control(value);
                handled = true;
            } else if addr == adc.control_b {
                self.adc.adcsrb = value;
                handled = true;
            } else if addr == adc.mux {
                self.adc.admux = value;
                handled = true;
            }
        }

        if !handled {
            if regs.spi_control != NONE && addr == regs.spi_control {
                self.spi.spcr = value;
                handled = true;
            } else if regs.spi_status != NONE && addr == regs.spi_status {
                self.spi.write_status(value);
                handled = true;
            } else if regs.spi_data != NONE && addr == regs.spi_data {
                self.spi.write_data(value);
                handled = true;
            } else if addr == regs.analog_comparator {
                self.write_acsr(value);
                handled = true;
            } else if addr == regs.spm_control {
                self.spmcsr = value;
                handled = true;
            } else if addr == regs.watchdog_control {
                self.watchdog.write_control(value);
                handled = true;
            } else if regs.twi_bit_rate != NONE && addr == regs.twi_bit_rate {
                self.twi.twbr = value;
                handled = true;
            } else if regs.twi_status != NONE && addr == regs.twi_status {
                self.twi.write_status(value);
                handled = true;
            } else if regs.twi_address != NONE && addr == regs.twi_address {
                self.twi.twar = value;
                handled = true;
            } else if regs.twi_data != NONE && addr == regs.twi_data {
                self.twi.twdr = value;
                handled = true;
            } else if regs.twi_control != NONE && addr == regs.twi_control {
                self.twi.write_control(value);
                handled = true;
            } else if regs.twi_mask != NONE && addr == regs.twi_mask {
                self.twi.twamr = value;
                handled = true;
            } else if regs.eeprom_control != NONE && addr == regs.eeprom_control {
                self.eeprom.write_control(value);
                handled = true;
            } else if regs.eeprom_data != NONE && addr == regs.eeprom_data {
                self.eeprom.eedr = value;
                handled = true;
            } else if regs.eeprom_address_low != NONE && addr == regs.eeprom_address_low {
                self.eeprom.write_low(value);
                handled = true;
            } else if regs.eeprom_address_high != NONE && addr == regs.eeprom_address_high {
                self.eeprom.write_high(value);
                handled = true;
            } else if regs.usi_data != NONE && addr == regs.usi_data {
                self.usi.usidr = value;
                handled = true;
            } else if regs.usi_buffer != NONE && addr == regs.usi_buffer {
                self.usi.usibr = value;
                handled = true;
            } else if regs.usi_status != NONE && addr == regs.usi_status {
                self.usi.write_status(value);
                handled = true;
            } else if regs.usi_control != NONE && addr == regs.usi_control {
                self.usi.write_control(value);
                handled = true;
            }
        }

        if !handled {
            if regs.ext_int_control_a != NONE && addr == regs.ext_int_control_a {
                self.eicra = value;
            } else if regs.ext_int_control_b != NONE && addr == regs.ext_int_control_b {
                self.eicrb = value;
            } else if regs.mcu_control != NONE && addr == regs.mcu_control {
                self.mcucr = value;
            } else if regs.ext_int_mask != NONE && addr == regs.ext_int_mask {
                self.eimsk = value;
            } else if regs.ext_int_flags != NONE && addr == regs.ext_int_flags {
                self.eifr &= !value;
            } else if regs.general_int_mask != NONE && addr == regs.general_int_mask {
                self.gimsk = value;
            } else if regs.general_int_flags != NONE && addr == regs.general_int_flags {
                self.gifr &= !value;
            } else if regs.pin_change_control != NONE && addr == regs.pin_change_control {
                self.pcicr = value;
            } else if regs.pin_change_flags != NONE && addr == regs.pin_change_flags {
                self.pcifr &= !value;
            } else {
                for (slot, &mask) in regs.pin_change_mask.iter().enumerate() {
                    if mask != NONE && addr == mask {
                        self.pcmsk[slot] = value;
                        handled = true;
                        break;
                    }
                }
                if !handled {
                    self.io[index] = value;
                }
            }
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
        for counter in 0..6 {
            self.timer_mut(counter).advance(cycles);
        }
        self.adc.advance(cycles);
        self.spi.advance(cycles);
        self.eeprom.advance(cycles);
        self.watchdog.advance(cycles, self.device.cpu_hz);
        self.sync_oc();
        self.sample_pins();
        self.update_comparator();
    }

    /// Drive the compare-output pins from the timer state. Called after every
    /// advance and every register write so `analogWrite` takes effect on the
    /// very next edge.
    pub(crate) fn sync_oc(&mut self) {
        let mut levels = [(0usize, 0u8, None); 24];
        let mut count = 0;
        let timers = self.device.timers;
        for spec in timers {
            let timer = self.timer(usize::from(spec.index));
            for (channel, output) in spec.outputs.iter().enumerate() {
                if let Some(pin) = output {
                    levels[count] = (pin.port, pin.bit, timer.oc_level_of(channel));
                    count += 1;
                }
            }
        }
        for (port, bit, level) in &levels[..count] {
            self.gpio.set_override(*port, *bit, *level);
        }
    }

    /// Sample every pin after a change: pin-change flags, external interrupt
    /// flags, input capture and the analog comparator all watch pin levels.
    fn sample_pins(&mut self) {
        let device = self.device;
        let port_count = device.ports.len().min(MAX_PORTS);
        let old = self.last_pins;
        let mut now = [0u8; MAX_PORTS];
        for port in 0..port_count {
            now[port] = self.gpio.pin(port);
        }

        let pin_flag_register = device.regs.general_int_flags != NONE;
        for change in device.pin_changes {
            let changed = now[change.port] ^ old[change.port];
            let enabled = if pin_flag_register {
                self.gimsk & change.enable_mask != 0
            } else {
                self.pcicr & change.enable_mask != 0
            };
            if changed & self.pcmsk[change.mask_index] != 0 && enabled {
                if pin_flag_register {
                    self.gifr |= change.flag_mask;
                } else {
                    self.pcifr |= change.flag_mask;
                }
            }
        }

        for interrupt in device.ext_ints {
            let old_level = old[interrupt.port] & (1 << interrupt.bit) != 0;
            let new_level = now[interrupt.port] & (1 << interrupt.bit) != 0;
            let sense = match interrupt.sense_register {
                0x69 => self.eicra,
                0x6a => self.eicrb,
                0x55 => self.mcucr,
                _ => 0,
            };
            let isc = (sense >> interrupt.sense_shift) & 0x03;
            let fire = match isc {
                0 => !new_level,
                1 => old_level != new_level,
                2 => old_level && !new_level,
                _ => !old_level && new_level,
            };
            if fire {
                if device.regs.general_int_flags != NONE {
                    self.gifr |= 1 << interrupt.mask_bit;
                } else {
                    self.eifr |= 1 << interrupt.mask_bit;
                }
            }
        }

        for spec in device.timers {
            let Some(pin) = spec.capture_pin else {
                continue;
            };
            let level = now[pin.port] & (1 << pin.bit) != 0;
            let index = usize::from(spec.index).min(5);
            let rising = self.timer(index).tccr_b & 0x40 != 0;
            if level != self.last_capture[index] && ((rising && level) || (!rising && !level)) {
                self.timer_mut(index).capture();
            }
            self.last_capture[index] = level;
        }

        self.last_pins = now;
    }

    fn update_comparator(&mut self) {
        let disabled = self.acsr & 0x80 != 0;
        let bandgap = self.acsr & 0x40 != 0;
        let comparator = self.device.comparator;
        let output = match comparator {
            Some((positive, negative)) if !disabled => {
                let pins = self.gpio.pin(positive.port);
                let positive_high = pins & (1 << positive.bit) != 0;
                let negative_high = if bandgap {
                    false
                } else {
                    self.gpio.pin(negative.port) & (1 << negative.bit) != 0
                };
                positive_high && !negative_high
            }
            _ => false,
        };
        if !disabled && output != self.last_aco {
            self.acsr |= 0x10; // ACI
        }
        self.acsr = (self.acsr & !0x20) | if output { 0x20 } else { 0 };
        self.last_aco = output;
    }

    fn ext_flag(&self, bit: u8) -> bool {
        if self.device.regs.general_int_flags != NONE {
            self.gifr & bit != 0
        } else {
            self.eifr & bit != 0
        }
    }

    fn ext_enabled(&self, bit: u8) -> bool {
        if self.device.regs.general_int_mask != NONE {
            self.gimsk & bit != 0
        } else {
            self.eimsk & bit != 0
        }
    }

    fn pin_change_flag(&self, bit: u8) -> bool {
        if self.device.regs.general_int_flags != NONE {
            self.gifr & bit != 0
        } else {
            self.pcifr & bit != 0
        }
    }

    fn pin_change_enabled(&self, bit: u8) -> bool {
        if self.device.regs.general_int_mask != NONE {
            self.gimsk & bit != 0
        } else {
            self.pcicr & bit != 0
        }
    }

    /// The highest-priority pending interrupt, or `None`. The scan returns the
    /// lowest vector number whose enable is set and whose flag is raised, which
    /// is the order the part's datasheet gives.
    pub fn pending_interrupt(&self) -> Option<u8> {
        let mut best: Option<u8> = None;
        let mut take = |vector: u8| {
            best = Some(match best {
                Some(current) => current.min(vector),
                None => vector,
            });
        };

        for interrupt in self.device.ext_ints {
            if self.ext_flag(1 << interrupt.mask_bit) && self.ext_enabled(1 << interrupt.mask_bit) {
                take(interrupt.vector);
            }
        }
        for change in self.device.pin_changes {
            if self.pin_change_flag(change.flag_mask) && self.pin_change_enabled(change.enable_mask)
            {
                take(change.vector);
            }
        }
        if self.watchdog.interrupt_pending() {
            take(self.device.vectors.watchdog);
        }
        for spec in self.device.timers {
            let timer = self.timer(usize::from(spec.index));
            for (vector, mask) in timer.interrupt_sources() {
                if timer.tifr & mask != 0 && timer.timsk & mask != 0 {
                    take(vector);
                }
            }
        }
        if let Some(vector) = self.device.vectors.spi
            && self.spi.interrupt_pending()
        {
            take(vector);
        }
        for (number, usart) in self.device.usarts.iter().enumerate() {
            let state = self.usart(number);
            if let Some(vector) = usart.vector_rx
                && state.rx_interrupt_enabled()
                && state.rxc()
            {
                take(vector);
            }
            if let Some(vector) = usart.vector_udre
                && state.udre_interrupt_enabled()
                && state.udre()
            {
                take(vector);
            }
            if let Some(vector) = usart.vector_tx
                && state.tx_interrupt_enabled()
                && state.txc()
            {
                take(vector);
            }
        }
        if self.adc.interrupt_pending() {
            take(self.device.vectors.adc);
        }
        if self.eeprom.interrupt_pending() {
            take(self.device.vectors.eeprom);
        }
        if self.acsr & 0x10 != 0 && self.acsr & 0x08 != 0 {
            take(self.device.vectors.analog_comparator);
        }
        if let Some(vector) = self.device.vectors.twi
            && self.twi.interrupt_pending()
        {
            take(vector);
        }
        if let Some(vector) = self.device.vectors.spm
            && self.spmcsr & 0x80 != 0
        {
            take(vector);
        }
        if let Some(vector) = self.device.vectors.usi_start
            && self.usi.start_pending()
        {
            take(vector);
        }
        if let Some(vector) = self.device.vectors.usi_overflow
            && self.usi.overflow_pending()
        {
            take(vector);
        }
        // The USB controller's registers are storage only, so its two vectors
        // never become pending.
        best
    }

    /// Clear the flag a vector owns, as hardware does when it services the
    /// interrupt. Level-triggered sources are left alone; their ISR clears
    /// them by touching the data register.
    pub fn acknowledge(&mut self, vector: u8) {
        let device = self.device;
        let regs = device.regs;
        let vectors = device.vectors;
        for interrupt in device.ext_ints {
            if interrupt.vector == vector {
                if regs.general_int_flags != NONE {
                    self.gifr &= !(1 << interrupt.mask_bit);
                } else {
                    self.eifr &= !(1 << interrupt.mask_bit);
                }
                return;
            }
        }
        for change in device.pin_changes {
            if change.vector == vector {
                if regs.general_int_flags != NONE {
                    self.gifr &= !change.flag_mask;
                } else {
                    self.pcifr &= !change.flag_mask;
                }
                return;
            }
        }
        if vector == vectors.watchdog {
            self.watchdog.acknowledge();
            return;
        }
        let timers = device.timers;
        for spec in timers {
            let timer = self.timer_mut(usize::from(spec.index));
            let sources: Vec<(u8, u8)> = timer.interrupt_sources().collect();
            for (source, mask) in sources {
                if source == vector {
                    timer.tifr &= !mask;
                    return;
                }
            }
        }
        if vectors.spi == Some(vector) {
            self.spi.acknowledge();
            return;
        }
        for (number, usart) in device.usarts.iter().enumerate() {
            if usart.vector_tx == Some(vector) {
                self.usart_mut(number).acknowledge_tx();
                return;
            }
        }
        if vector == vectors.adc {
            self.adc.acknowledge();
            return;
        }
        if vector == vectors.eeprom {
            self.eeprom.acknowledge();
            return;
        }
        if vector == vectors.analog_comparator {
            self.acsr &= !0x10;
            return;
        }
        if vectors.twi == Some(vector) {
            self.twi.acknowledge();
            return;
        }
        if let (Some(start), Some(overflow)) = (vectors.usi_start, vectors.usi_overflow) {
            self.usi.acknowledge(vector, start, overflow);
        }
    }

    /// Whether any enabled source could wake a sleeping core. A source that is
    /// enabled but has no way to change state on its own (a stopped timer, an
    /// idle ADC) is not counted.
    pub fn any_interrupt_enabled(&self) -> bool {
        let mut enabled = self.watchdog.can_wake()
            || self.spi.can_wake()
            || self.adc.can_wake()
            || self.eeprom.can_wake()
            || self.acsr & 0x08 != 0
            || self.twi.can_wake()
            || self.spmcsr & 0x80 != 0
            || self.usi.any_enabled();
        for interrupt in self.device.ext_ints {
            enabled |= self.ext_enabled(1 << interrupt.mask_bit);
        }
        for change in self.device.pin_changes {
            enabled |= self.pin_change_enabled(change.enable_mask);
        }
        for spec in self.device.timers {
            let timer = self.timer(usize::from(spec.index));
            enabled |= timer.running() && timer.timsk != 0;
        }
        for (number, usart) in self.device.usarts.iter().enumerate() {
            let state = self.usart(number);
            enabled |= usart.vector_rx.is_some() && state.rx_interrupt_enabled();
            enabled |=
                usart.vector_udre.is_some() && state.udre_interrupt_enabled() && state.udre();
            enabled |= usart.vector_tx.is_some() && state.tx_interrupt_enabled() && state.txc();
        }
        enabled
    }

    /// Queue a byte for the console USART's receive path.
    pub fn receive(&mut self, value: u8) {
        let console = self.device.console_usart;
        self.usart_mut(console).receive_byte(value);
    }

    /// Bytes the firmware has written to the console USART, drained.
    pub fn take_console(&mut self) -> Vec<u8> {
        let console = self.device.console_usart;
        self.usart_mut(console).take()
    }

    /// Install the GPIO output callback.
    pub fn set_gpio_callback(&mut self, callback: Box<dyn FnMut(u8, bool)>) {
        self.gpio.set_callback(callback);
    }

    /// Output edges observed since the last call.
    pub fn take_outputs(&mut self) -> Vec<(u8, bool)> {
        self.gpio.take_events()
    }

    /// The high-time fraction each compare-output pin is programmed to, in the
    /// part's host pin numbering.
    pub fn pwm_duties(&self) -> Vec<(u8, f64)> {
        let mut duties = Vec::new();
        for spec in self.device.timers {
            let timer = self.timer(usize::from(spec.index));
            for (channel, output) in spec.outputs.iter().enumerate() {
                let Some(pin) = output else {
                    continue;
                };
                let Some(duty) = timer.duty_of(channel) else {
                    continue;
                };
                if let Some(host) = self.device.pin_of_port(pin.port, pin.bit) {
                    duties.push((host, duty));
                }
            }
        }
        duties
    }

    /// Drive a host-facing pin from the host.
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
            .field("device", &self.device.name)
            .field("ddr_b", &self.gpio.ddr(0))
            .field("port_b", &self.gpio.port(0))
            .field("tcnt0", &self.timer0.count)
            .field("tifr0", &self.timer0.tifr)
            .field("tcnt1", &self.timer1.count)
            .field("tifr1", &self.timer1.tifr)
            .field("tcnt2", &self.timer2.count)
            .field("tifr2", &self.timer2.tifr)
            .finish()
    }
}
