//! The parts of an AVR the core and peripherals need to know: memory sizes,
//! data-space layout, port and pin numbering, the register map, the interrupt
//! vector table and the instruction-set features of each device.
//!
//! An AVR8 datasheet is written once per family, and the differences that
//! matter to an interpreter are all tables: how much flash and SRAM there is,
//! which address each peripheral register sits at, which vectors exist and in
//! what order, and whether the core has a hardware multiplier, absolute
//! `JMP`/`CALL`, `ELPM`/`EIND` or `SPM`. [`Device`] is those tables, so the
//! core and the peripherals stay one implementation for every part.
//!
//! The four devices here are the ones this crate models. Register addresses
//! come from each datasheet's register summary; vector numbers come from its
//! reset-and-interrupt-vectors table. A register or vector a part does not have
//! is `None`/[`NONE`] and is therefore absent from that part's data space or
//! vector table.

/// An address that no register on this device uses.
pub const NONE: u16 = 0xffff;

/// Instruction-set features that the AVR8 family varies by device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Features {
    /// `MUL`, `MULS` and `MULSU` are present.
    pub mul: bool,
    /// Absolute `JMP`/`CALL` are present (the part has more than 8 KiB of
    /// flash and a vector slot wide enough for them).
    pub jmp_call: bool,
    /// `ELPM` is present.
    pub elpm: bool,
    /// `EIJMP`/`EICALL` and the `EIND` register are present.
    pub eind: bool,
    /// The `RAMPZ` register is present for `ELPM`/`SPM`.
    pub rampz: bool,
    /// `SPM` exists in the instruction set.
    pub spm: bool,
}

/// How a host-facing pin number is derived from a port bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinNumbering {
    /// The Arduino Uno header: `D0..D7` are `PORTD[0..8]`, `D8..D13` are
    /// `PORTB[0..6]`, `A0..A7` are `PORTC[0..6]` and the two extra analog
    /// inputs.
    ArduinoUno,
    /// `pin = port position * 8 + bit`, with the ports in datasheet order.
    PortLinear,
}

/// One GPIO port and the three registers that reach it.
#[derive(Clone, Copy, Debug)]
pub struct Port {
    /// Index into the GPIO state arrays.
    pub index: usize,
    /// `PINn` in the data space.
    pub pin: u16,
    /// `DDRn` in the data space.
    pub ddr: u16,
    /// `PORTn` in the data space.
    pub port: u16,
    /// Bits the package brings out.
    pub mask: u8,
    /// Position in the datasheet's port order, used by
    /// [`PinNumbering::PortLinear`].
    pub number: u8,
    /// The datasheet letter (`"B"`, `"J"`, …), used by
    /// [`PinNumbering::ArduinoUno`].
    pub letter: &'static str,
}

/// The kind of counter a timer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerKind {
    /// The ordinary 8-bit timer with `TCCRnA`/`TCCRnB`.
    Standard8,
    /// The 16-bit timer with capture and three compare channels.
    Long16,
    /// The ATtiny85's 8-bit Timer/Counter1 with a single `TCCR1` and `TOP`
    /// taken from `OCR1C`.
    Tiny8,
    /// The ATmega32U4's 10-bit high-speed Timer/Counter4.
    HighSpeed4,
}

/// Which prescaler table a timer's clock-select bits use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prescaler {
    /// `/1`, `/8`, `/64`, `/256`, `/1024`; 6 and 7 are external clocks.
    Standard,
    /// Timer2's table, which inserts `/32` and `/128`.
    Timer2,
    /// The four-bit table of the high-speed and ATtiny85 timers:
    /// `1 << (cs - 1)`.
    Power2,
}

/// The bit each interrupt flag occupies in its `TIFR` and `TIMSK` register.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlagMasks {
    pub overflow: u8,
    pub compare_a: u8,
    pub compare_b: u8,
    pub compare_c: u8,
    pub capture: u8,
}

/// Where a timer's compare output leaves the chip.
#[derive(Clone, Copy, Debug)]
pub struct ComparePin {
    /// GPIO state index of the port.
    pub port: usize,
    /// Bit within the port.
    pub bit: u8,
}

/// The addresses of one timer's registers. Unused fields are [`NONE`].
#[derive(Clone, Copy, Debug, Default)]
pub struct TimerRegs {
    /// `TCCRnA..TCCRnE`, or the single `TCCR1` on the ATtiny85.
    pub control: [u16; 5],
    /// `GTCCR`, which holds Timer1's B-side control on the ATtiny85.
    pub gtccr: u16,
    /// `TCNTn` (the low byte on a 16-bit timer).
    pub count: u16,
    /// `TCNTnH` on a 16-bit timer.
    pub count_high: u16,
    /// `OCRnA..OCRnD`, low bytes.
    pub compare: [u16; 4],
    /// High bytes for the compare registers of a 16-bit timer.
    pub compare_high: [u16; 4],
    /// `ICRn` low/high.
    pub capture: u16,
    pub capture_high: u16,
    /// The 32U4's `TC4H` temporary high byte.
    pub temp_high: u16,
    pub timsk: u16,
    pub tifr: u16,
}

/// The vector numbers a timer owns. Numbers are zero-based indexes into the
/// device's vector table.
#[derive(Clone, Copy, Debug, Default)]
pub struct TimerVectors {
    pub overflow: Option<u8>,
    pub compare_a: Option<u8>,
    pub compare_b: Option<u8>,
    pub compare_c: Option<u8>,
    pub capture: Option<u8>,
}

/// One timer in the device's inventory.
#[derive(Clone, Copy, Debug)]
pub struct TimerSpec {
    /// Timer number as the datasheet names it.
    pub index: u8,
    pub kind: TimerKind,
    pub prescaler: Prescaler,
    pub regs: TimerRegs,
    pub vectors: TimerVectors,
    pub masks: FlagMasks,
    /// Compare output for channels A, B and C.
    pub outputs: [Option<ComparePin>; 3],
    /// The input-capture pin, when the timer has a capture unit.
    pub capture_pin: Option<ComparePin>,
}

/// One USART's registers and vectors.
#[derive(Clone, Copy, Debug, Default)]
pub struct UsartSpec {
    pub control_a: u16,
    pub control_b: u16,
    pub control_c: u16,
    pub baud_low: u16,
    pub baud_high: u16,
    pub data: u16,
    pub vector_rx: Option<u8>,
    pub vector_udre: Option<u8>,
    pub vector_tx: Option<u8>,
}

/// The ADC's register block and channel selection layout.
#[derive(Clone, Copy, Debug)]
pub struct AdcConfig {
    pub control: u16,
    pub control_b: u16,
    pub mux: u16,
    pub data_low: u16,
    pub data_high: u16,
    /// Bits of `ADMUX` that select the channel.
    pub mux_mask: u8,
    /// Bits of `ADCSRB` that extend the channel selection, if any.
    pub mux5_mask: Option<u8>,
    /// On the ATmega2560 a channel code of 32 and above selects `ADC8..ADC15`;
    /// the single-ended channel is code - 24.
    pub high_bank: bool,
    /// `ADMUX` bit that left-adjusts the result.
    pub adlar_bit: u8,
    /// Value of the reference bits that selects the internal 1.1 V source.
    pub internal_ref_mask: u8,
    pub internal_ref_value: u8,
    /// Voltage of that internal reference, in microvolts.
    pub internal_ref_microvolts: u32,
    /// The MUX code that reads the 1.1 V bandgap, if the part has one.
    pub bandgap_mux: Option<u8>,
    /// The MUX code that reads ground.
    pub ground_mux: Option<u8>,
    /// Channels the host can inject a voltage into.
    pub channels: u8,
}

/// One external interrupt: its vector, pin, sense register and enable bit.
#[derive(Clone, Copy, Debug)]
pub struct ExtIntSpec {
    pub vector: u8,
    /// GPIO state index of the pin.
    pub port: usize,
    pub bit: u8,
    /// `EICRA`, `EICRB` or the ATtiny's `MCUCR`.
    pub sense_register: u16,
    /// Bit pair within that register.
    pub sense_shift: u8,
    /// Bit in `EIMSK` (or `GIMSK` on the ATtiny).
    pub mask_bit: u8,
}

/// One pin-change group.
#[derive(Clone, Copy, Debug)]
pub struct PinChangeSpec {
    pub vector: u8,
    /// GPIO state index of the port the group watches.
    pub port: usize,
    /// Index into the peripheral's `PCMSK` array.
    pub mask_index: usize,
    /// Bit in `PCICR` (or `GIMSK`) that enables the group.
    pub enable_mask: u8,
    /// Bit in `PCIFR` (or `GIFR`) that records the request.
    pub flag_mask: u8,
}

/// The plain register addresses shared by all device tables.
#[derive(Clone, Copy, Debug)]
pub struct Regs {
    pub eeprom_control: u16,
    pub eeprom_data: u16,
    pub eeprom_address_low: u16,
    pub eeprom_address_high: u16,
    pub spm_control: u16,
    pub watchdog_control: u16,
    pub analog_comparator: u16,
    pub spi_control: u16,
    pub spi_status: u16,
    pub spi_data: u16,
    pub twi_bit_rate: u16,
    pub twi_status: u16,
    pub twi_address: u16,
    pub twi_data: u16,
    pub twi_control: u16,
    pub twi_mask: u16,
    pub usi_data: u16,
    pub usi_buffer: u16,
    pub usi_status: u16,
    pub usi_control: u16,
    pub ext_int_control_a: u16,
    pub ext_int_control_b: u16,
    pub ext_int_mask: u16,
    pub ext_int_flags: u16,
    /// The ATtiny's combined `GIMSK`, which carries both `INT0` and `PCIE`.
    pub general_int_mask: u16,
    /// The ATtiny's combined `GIFR`.
    pub general_int_flags: u16,
    /// The ATtiny's `MCUCR`, which holds `ISC01:00`.
    pub mcu_control: u16,
    pub pin_change_control: u16,
    pub pin_change_flags: u16,
    pub pin_change_mask: [u16; 3],
    /// `TIFR0..TIFR5`.
    pub tifr: [u16; 6],
    /// `TIMSK0..TIMSK5`.
    pub timsk: [u16; 6],
    /// On the ATtiny85 both timers share `TIMSK` and `TIFR`.
    pub shared_timer_irq: bool,
    pub rampz: u16,
    pub eind: u16,
    /// First address of the USB device-controller block, storage only.
    pub usb: u16,
    /// Number of USB registers beginning at `usb`.
    pub usb_len: usize,
}

/// The whole vector table of one part: the names in datasheet order, plus the
/// source-to-vector numbers the peripherals use.
#[derive(Clone, Copy, Debug)]
pub struct Vectors {
    pub names: &'static [&'static str],
    pub watchdog: u8,
    pub analog_comparator: u8,
    pub adc: u8,
    pub eeprom: u8,
    pub spi: Option<u8>,
    pub twi: Option<u8>,
    pub spm: Option<u8>,
    pub usi_start: Option<u8>,
    pub usi_overflow: Option<u8>,
    pub usb_general: Option<u8>,
    pub usb_endpoint: Option<u8>,
}

impl Vectors {
    /// Number of vectors, reserved slots included.
    pub const fn count(&self) -> u8 {
        self.names.len() as u8
    }
}

/// One AVR device as an interpreter needs to see it.
#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub name: &'static str,
    pub cpu_hz: u64,
    pub flash_size: usize,
    pub sram_size: usize,
    pub eeprom_size: usize,
    pub sram_start: u16,
    pub ramend: u16,
    /// Bytes of return address pushed on the stack (two or three).
    pub pc_bytes: u8,
    /// Words occupied by one vector slot (one or two).
    pub vector_words: u8,
    pub interrupt_cycles: u8,
    pub call_cycles: u8,
    pub ret_cycles: u8,
    pub features: Features,
    pub numbering: PinNumbering,
    pub ports: &'static [Port],
    /// `(board gpio, ADC channel)` pairs the adapter publishes analog inputs
    /// on.
    pub analog: &'static [(u8, u8)],
    pub regs: Regs,
    pub vectors: Vectors,
    pub timers: &'static [TimerSpec],
    /// Indexed by USART number; `NONE` addresses mean the part has none.
    pub usarts: [UsartSpec; 4],
    /// Which USART the console is attached to.
    pub console_usart: usize,
    pub adc: AdcConfig,
    pub ext_ints: &'static [ExtIntSpec],
    pub pin_changes: &'static [PinChangeSpec],
    /// Analog comparator positive and negative input pins, when the part
    /// brings both out.
    pub comparator: Option<(ComparePin, ComparePin)>,
    pub usb: bool,
}

impl Device {
    /// Flash size in instruction words.
    pub const fn flash_words(&self) -> u32 {
        (self.flash_size / 2) as u32
    }

    /// The port and bit a host-facing pin number names, when the package
    /// brings that bit out.
    pub fn port_of_pin(&self, pin: u8) -> Option<(usize, u8)> {
        match self.numbering {
            PinNumbering::ArduinoUno => match pin {
                0..=7 => Some((2, pin)),
                8..=13 => Some((0, pin - 8)),
                14..=19 => Some((1, pin - 14)),
                _ => None,
            },
            PinNumbering::PortLinear => {
                let number = pin / 8;
                let bit = pin % 8;
                self.ports
                    .iter()
                    .find(|port| port.number == number && port.mask & (1 << bit) != 0)
                    .map(|port| (port.index, bit))
            }
        }
    }

    /// The host-facing pin number for a port bit, when there is one.
    pub fn pin_of_port(&self, index: usize, bit: u8) -> Option<u8> {
        let port = self.ports.iter().find(|port| port.index == index)?;
        if port.mask & (1 << bit) == 0 {
            return None;
        }
        match self.numbering {
            PinNumbering::ArduinoUno => match (port.letter, bit) {
                ("B", 0..=5) => Some(8 + bit),
                ("C", 0..=5) => Some(14 + bit),
                ("D", 0..=7) => Some(bit),
                _ => None,
            },
            PinNumbering::PortLinear => Some(port.number * 8 + bit),
        }
    }

    /// The ADC channel a board analog input is wired to.
    pub fn analog_channel(&self, gpio: u8) -> Option<u8> {
        self.analog
            .iter()
            .find(|&&(pin, _)| pin == gpio)
            .map(|&(_, channel)| channel)
    }

    /// A timer by its datasheet number.
    pub fn timer(&self, index: u8) -> Option<&'static TimerSpec> {
        self.timers.iter().find(|timer| timer.index == index)
    }

    /// A USART by its datasheet number.
    pub fn usart(&self, index: usize) -> &UsartSpec {
        &self.usarts[index]
    }

    /// A GPIO port by datasheet letter.
    pub fn port(&self, letter: &str) -> Option<&'static Port> {
        self.ports.iter().find(|port| port.letter == letter)
    }
}

const fn timer_regs(
    control: [u16; 5],
    gtccr: u16,
    count: u16,
    count_high: u16,
    compare: [u16; 4],
    compare_high: [u16; 4],
    capture: u16,
    capture_high: u16,
    timsk: u16,
    tifr: u16,
) -> TimerRegs {
    TimerRegs {
        control,
        gtccr,
        count,
        count_high,
        compare,
        compare_high,
        capture,
        capture_high,
        temp_high: NONE,
        timsk,
        tifr,
    }
}

const fn usart(
    control_a: u16,
    control_b: u16,
    control_c: u16,
    baud_low: u16,
    baud_high: u16,
    data: u16,
    vector_rx: Option<u8>,
    vector_udre: Option<u8>,
    vector_tx: Option<u8>,
) -> UsartSpec {
    UsartSpec {
        control_a,
        control_b,
        control_c,
        baud_low,
        baud_high,
        data,
        vector_rx,
        vector_udre,
        vector_tx,
    }
}

const NO_USART: UsartSpec = UsartSpec {
    control_a: NONE,
    control_b: NONE,
    control_c: NONE,
    baud_low: NONE,
    baud_high: NONE,
    data: NONE,
    vector_rx: None,
    vector_udre: None,
    vector_tx: None,
};

// -- ATmega328P ------------------------------------------------------------

/// The ATmega328P: 32 KiB flash, 2 KiB SRAM, 1 KiB EEPROM, ports B/C/D.
pub static ATMEGA328P: Device = Device {
    name: "atmega328p",
    cpu_hz: 16_000_000,
    flash_size: 32 * 1024,
    sram_size: 2 * 1024,
    eeprom_size: 1024,
    sram_start: 0x0100,
    ramend: 0x08ff,
    pc_bytes: 2,
    vector_words: 2,
    interrupt_cycles: 4,
    call_cycles: 4,
    ret_cycles: 4,
    features: Features {
        mul: true,
        jmp_call: true,
        elpm: false,
        eind: false,
        rampz: false,
        spm: true,
    },
    numbering: PinNumbering::ArduinoUno,
    ports: &[
        Port {
            index: 0,
            pin: 0x23,
            ddr: 0x24,
            port: 0x25,
            mask: 0xff,
            number: 1,
            letter: "B",
        },
        Port {
            index: 1,
            pin: 0x26,
            ddr: 0x27,
            port: 0x28,
            mask: 0xff,
            number: 2,
            letter: "C",
        },
        Port {
            index: 2,
            pin: 0x29,
            ddr: 0x2a,
            port: 0x2b,
            mask: 0xff,
            number: 3,
            letter: "D",
        },
    ],
    analog: &[
        (14, 0),
        (15, 1),
        (16, 2),
        (17, 3),
        (18, 4),
        (19, 5),
        (20, 6),
        (21, 7),
    ],
    regs: Regs {
        eeprom_control: 0x3f,
        eeprom_data: 0x40,
        eeprom_address_low: 0x41,
        eeprom_address_high: 0x42,
        spm_control: 0x57,
        watchdog_control: 0x60,
        analog_comparator: 0x50,
        spi_control: 0x4c,
        spi_status: 0x4d,
        spi_data: 0x4e,
        twi_bit_rate: 0xb8,
        twi_status: 0xb9,
        twi_address: 0xba,
        twi_data: 0xbb,
        twi_control: 0xbc,
        twi_mask: 0xbd,
        usi_data: NONE,
        usi_buffer: NONE,
        usi_status: NONE,
        usi_control: NONE,
        ext_int_control_a: 0x69,
        ext_int_control_b: NONE,
        ext_int_mask: 0x3d,
        ext_int_flags: 0x3c,
        general_int_mask: NONE,
        general_int_flags: NONE,
        mcu_control: NONE,
        pin_change_control: 0x68,
        pin_change_flags: 0x3b,
        pin_change_mask: [0x6b, 0x6c, 0x6d],
        tifr: [0x35, 0x36, 0x37, NONE, NONE, NONE],
        timsk: [0x6e, 0x6f, 0x70, NONE, NONE, NONE],
        shared_timer_irq: false,
        rampz: NONE,
        eind: NONE,
        usb: NONE,
        usb_len: 0,
    },
    vectors: Vectors {
        names: &[
            "RESET",
            "INT0",
            "INT1",
            "PCINT0",
            "PCINT1",
            "PCINT2",
            "WDT",
            "TIMER2_COMPA",
            "TIMER2_COMPB",
            "TIMER2_OVF",
            "TIMER1_CAPT",
            "TIMER1_COMPA",
            "TIMER1_COMPB",
            "TIMER1_OVF",
            "TIMER0_COMPA",
            "TIMER0_COMPB",
            "TIMER0_OVF",
            "SPI_STC",
            "USART_RX",
            "USART_UDRE",
            "USART_TX",
            "ADC",
            "EE_READY",
            "ANALOG_COMP",
            "TWI",
            "SPM_READY",
        ],
        watchdog: 6,
        analog_comparator: 23,
        adc: 21,
        eeprom: 22,
        spi: Some(17),
        twi: Some(24),
        spm: Some(25),
        usi_start: None,
        usi_overflow: None,
        usb_general: None,
        usb_endpoint: None,
    },
    timers: &[
        TimerSpec {
            index: 0,
            kind: TimerKind::Standard8,
            prescaler: Prescaler::Standard,
            regs: timer_regs(
                [0x44, 0x45, NONE, NONE, NONE],
                NONE,
                0x46,
                NONE,
                [0x47, 0x48, NONE, NONE],
                [NONE; 4],
                NONE,
                NONE,
                0x6e,
                0x35,
            ),
            vectors: TimerVectors {
                overflow: Some(16),
                compare_a: Some(14),
                compare_b: Some(15),
                compare_c: None,
                capture: None,
            },
            masks: FlagMasks {
                overflow: 0x01,
                compare_a: 0x02,
                compare_b: 0x04,
                ..EMPTY_MASKS
            },
            capture_pin: None,
            outputs: [
                Some(ComparePin { port: 2, bit: 6 }),
                Some(ComparePin { port: 2, bit: 5 }),
                None,
            ],
        },
        TimerSpec {
            index: 1,
            kind: TimerKind::Long16,
            prescaler: Prescaler::Standard,
            regs: timer_regs(
                [0x80, 0x81, 0x82, NONE, NONE],
                NONE,
                0x84,
                0x85,
                [0x88, 0x8a, NONE, NONE],
                [0x89, 0x8b, NONE, NONE],
                0x86,
                0x87,
                0x6f,
                0x36,
            ),
            vectors: TimerVectors {
                overflow: Some(13),
                compare_a: Some(11),
                compare_b: Some(12),
                compare_c: None,
                capture: Some(10),
            },
            masks: FlagMasks {
                overflow: 0x01,
                compare_a: 0x02,
                compare_b: 0x04,
                compare_c: 0x08,
                capture: 0x20,
            },
            capture_pin: Some(ComparePin { port: 0, bit: 0 }),
            outputs: [
                Some(ComparePin { port: 0, bit: 1 }),
                Some(ComparePin { port: 0, bit: 2 }),
                None,
            ],
        },
        TimerSpec {
            index: 2,
            kind: TimerKind::Standard8,
            prescaler: Prescaler::Timer2,
            regs: timer_regs(
                [0xb0, 0xb1, NONE, NONE, NONE],
                NONE,
                0xb2,
                NONE,
                [0xb3, 0xb4, NONE, NONE],
                [NONE; 4],
                NONE,
                NONE,
                0x70,
                0x37,
            ),
            vectors: TimerVectors {
                overflow: Some(9),
                compare_a: Some(7),
                compare_b: Some(8),
                compare_c: None,
                capture: None,
            },
            masks: FlagMasks {
                overflow: 0x01,
                compare_a: 0x02,
                compare_b: 0x04,
                ..EMPTY_MASKS
            },
            capture_pin: None,
            outputs: [
                Some(ComparePin { port: 0, bit: 3 }),
                Some(ComparePin { port: 2, bit: 3 }),
                None,
            ],
        },
    ],
    usarts: [
        usart(
            0xc0,
            0xc1,
            0xc2,
            0xc4,
            0xc5,
            0xc6,
            Some(18),
            Some(19),
            Some(20),
        ),
        NO_USART,
        NO_USART,
        NO_USART,
    ],
    console_usart: 0,
    adc: AdcConfig {
        control: 0x7a,
        control_b: 0x7b,
        mux: 0x7c,
        data_low: 0x78,
        data_high: 0x79,
        mux_mask: 0x0f,
        mux5_mask: None,
        high_bank: false,
        adlar_bit: 5,
        internal_ref_mask: 0xc0,
        internal_ref_value: 0xc0,
        internal_ref_microvolts: 1_100_000,
        bandgap_mux: Some(14),
        ground_mux: Some(15),
        channels: 16,
    },
    ext_ints: &[
        ExtIntSpec {
            vector: 1,
            port: 2,
            bit: 2,
            sense_register: 0x69,
            sense_shift: 0,
            mask_bit: 0,
        },
        ExtIntSpec {
            vector: 2,
            port: 2,
            bit: 3,
            sense_register: 0x69,
            sense_shift: 2,
            mask_bit: 1,
        },
    ],
    pin_changes: &[
        PinChangeSpec {
            vector: 3,
            port: 0,
            mask_index: 0,
            enable_mask: 0x01,
            flag_mask: 0x01,
        },
        PinChangeSpec {
            vector: 4,
            port: 1,
            mask_index: 1,
            enable_mask: 0x02,
            flag_mask: 0x02,
        },
        PinChangeSpec {
            vector: 5,
            port: 2,
            mask_index: 2,
            enable_mask: 0x04,
            flag_mask: 0x04,
        },
    ],
    comparator: Some((
        ComparePin { port: 2, bit: 6 },
        ComparePin { port: 2, bit: 7 },
    )),
    usb: false,
};

const EMPTY_MASKS: FlagMasks = FlagMasks {
    overflow: 0,
    compare_a: 0,
    compare_b: 0,
    compare_c: 0,
    capture: 0,
};

// -- ATmega2560 ------------------------------------------------------------

/// The ATmega2560: 256 KiB flash, 8 KiB SRAM, 4 KiB EEPROM, ports A through L.
pub static ATMEGA2560: Device = Device {
    name: "atmega2560",
    cpu_hz: 16_000_000,
    flash_size: 256 * 1024,
    sram_size: 8 * 1024,
    eeprom_size: 4 * 1024,
    sram_start: 0x0200,
    ramend: 0x21ff,
    pc_bytes: 3,
    vector_words: 2,
    interrupt_cycles: 5,
    call_cycles: 5,
    ret_cycles: 5,
    features: Features {
        mul: true,
        jmp_call: true,
        elpm: true,
        eind: true,
        rampz: true,
        spm: true,
    },
    numbering: PinNumbering::PortLinear,
    ports: &[
        Port {
            index: 0,
            pin: 0x20,
            ddr: 0x21,
            port: 0x22,
            mask: 0xff,
            number: 0,
            letter: "A",
        },
        Port {
            index: 1,
            pin: 0x23,
            ddr: 0x24,
            port: 0x25,
            mask: 0xff,
            number: 1,
            letter: "B",
        },
        Port {
            index: 2,
            pin: 0x26,
            ddr: 0x27,
            port: 0x28,
            mask: 0xff,
            number: 2,
            letter: "C",
        },
        Port {
            index: 3,
            pin: 0x29,
            ddr: 0x2a,
            port: 0x2b,
            mask: 0xff,
            number: 3,
            letter: "D",
        },
        Port {
            index: 4,
            pin: 0x2c,
            ddr: 0x2d,
            port: 0x2e,
            mask: 0xff,
            number: 4,
            letter: "E",
        },
        Port {
            index: 5,
            pin: 0x2f,
            ddr: 0x30,
            port: 0x31,
            mask: 0xff,
            number: 5,
            letter: "F",
        },
        Port {
            index: 6,
            pin: 0x32,
            ddr: 0x33,
            port: 0x34,
            mask: 0x3f,
            number: 6,
            letter: "G",
        },
        Port {
            index: 7,
            pin: 0x100,
            ddr: 0x101,
            port: 0x102,
            mask: 0xff,
            number: 7,
            letter: "H",
        },
        Port {
            index: 8,
            pin: 0x103,
            ddr: 0x104,
            port: 0x105,
            mask: 0xff,
            number: 8,
            letter: "J",
        },
        Port {
            index: 9,
            pin: 0x106,
            ddr: 0x107,
            port: 0x108,
            mask: 0xff,
            number: 9,
            letter: "K",
        },
        Port {
            index: 10,
            pin: 0x109,
            ddr: 0x10a,
            port: 0x10b,
            mask: 0xff,
            number: 10,
            letter: "L",
        },
    ],
    analog: &[
        (40, 0),
        (41, 1),
        (42, 2),
        (43, 3),
        (44, 4),
        (45, 5),
        (46, 6),
        (47, 7),
        (72, 8),
        (73, 9),
        (74, 10),
        (75, 11),
        (76, 12),
        (77, 13),
        (78, 14),
        (79, 15),
    ],
    regs: Regs {
        eeprom_control: 0x3f,
        eeprom_data: 0x40,
        eeprom_address_low: 0x41,
        eeprom_address_high: 0x42,
        spm_control: 0x57,
        watchdog_control: 0x60,
        analog_comparator: 0x50,
        spi_control: 0x4c,
        spi_status: 0x4d,
        spi_data: 0x4e,
        twi_bit_rate: 0xb8,
        twi_status: 0xb9,
        twi_address: 0xba,
        twi_data: 0xbb,
        twi_control: 0xbc,
        twi_mask: 0xbd,
        usi_data: NONE,
        usi_buffer: NONE,
        usi_status: NONE,
        usi_control: NONE,
        ext_int_control_a: 0x69,
        ext_int_control_b: 0x6a,
        ext_int_mask: 0x3d,
        ext_int_flags: 0x3c,
        general_int_mask: NONE,
        general_int_flags: NONE,
        mcu_control: NONE,
        pin_change_control: 0x68,
        pin_change_flags: 0x3b,
        pin_change_mask: [0x6b, 0x6c, 0x6d],
        tifr: [0x35, 0x36, 0x37, 0x38, 0x39, 0x3a],
        timsk: [0x6e, 0x6f, 0x70, 0x71, 0x72, 0x73],
        shared_timer_irq: false,
        rampz: 0x5b,
        eind: 0x5c,
        usb: NONE,
        usb_len: 0,
    },
    vectors: Vectors {
        names: &[
            "RESET",
            "INT0",
            "INT1",
            "INT2",
            "INT3",
            "INT4",
            "INT5",
            "INT6",
            "INT7",
            "PCINT0",
            "PCINT1",
            "PCINT2",
            "WDT",
            "TIMER2_COMPA",
            "TIMER2_COMPB",
            "TIMER2_OVF",
            "TIMER1_CAPT",
            "TIMER1_COMPA",
            "TIMER1_COMPB",
            "TIMER1_COMPC",
            "TIMER1_OVF",
            "TIMER0_COMPA",
            "TIMER0_COMPB",
            "TIMER0_OVF",
            "SPI_STC",
            "USART0_RX",
            "USART0_UDRE",
            "USART0_TX",
            "ANALOG_COMP",
            "ADC",
            "EE_READY",
            "TIMER3_CAPT",
            "TIMER3_COMPA",
            "TIMER3_COMPB",
            "TIMER3_COMPC",
            "TIMER3_OVF",
            "USART1_RX",
            "USART1_UDRE",
            "USART1_TX",
            "TWI",
            "SPM_READY",
            "TIMER4_CAPT",
            "TIMER4_COMPA",
            "TIMER4_COMPB",
            "TIMER4_COMPC",
            "TIMER4_OVF",
            "TIMER5_CAPT",
            "TIMER5_COMPA",
            "TIMER5_COMPB",
            "TIMER5_COMPC",
            "TIMER5_OVF",
            "USART2_RX",
            "USART2_UDRE",
            "USART2_TX",
            "USART3_RX",
            "USART3_UDRE",
            "USART3_TX",
        ],
        watchdog: 12,
        analog_comparator: 28,
        adc: 29,
        eeprom: 30,
        spi: Some(24),
        twi: Some(39),
        spm: Some(40),
        usi_start: None,
        usi_overflow: None,
        usb_general: None,
        usb_endpoint: None,
    },
    timers: &[
        TimerSpec {
            index: 0,
            kind: TimerKind::Standard8,
            prescaler: Prescaler::Standard,
            regs: timer_regs(
                [0x44, 0x45, NONE, NONE, NONE],
                NONE,
                0x46,
                NONE,
                [0x47, 0x48, NONE, NONE],
                [NONE; 4],
                NONE,
                NONE,
                0x6e,
                0x35,
            ),
            vectors: TimerVectors {
                overflow: Some(23),
                compare_a: Some(21),
                compare_b: Some(22),
                compare_c: None,
                capture: None,
            },
            masks: FlagMasks {
                overflow: 0x01,
                compare_a: 0x02,
                compare_b: 0x04,
                ..EMPTY_MASKS
            },
            capture_pin: None,
            outputs: [
                Some(ComparePin { port: 1, bit: 7 }),
                Some(ComparePin { port: 6, bit: 5 }),
                None,
            ],
        },
        TimerSpec {
            index: 1,
            kind: TimerKind::Long16,
            prescaler: Prescaler::Standard,
            regs: timer_regs(
                [0x80, 0x81, 0x82, NONE, NONE],
                NONE,
                0x84,
                0x85,
                [0x88, 0x8a, 0x8c, NONE],
                [0x89, 0x8b, 0x8d, NONE],
                0x86,
                0x87,
                0x6f,
                0x36,
            ),
            vectors: TimerVectors {
                overflow: Some(20),
                compare_a: Some(17),
                compare_b: Some(18),
                compare_c: Some(19),
                capture: Some(16),
            },
            masks: FlagMasks {
                overflow: 0x01,
                compare_a: 0x02,
                compare_b: 0x04,
                compare_c: 0x08,
                capture: 0x20,
            },
            capture_pin: Some(ComparePin { port: 3, bit: 4 }),
            outputs: [
                Some(ComparePin { port: 1, bit: 5 }),
                Some(ComparePin { port: 1, bit: 6 }),
                Some(ComparePin { port: 1, bit: 7 }),
            ],
        },
        TimerSpec {
            index: 2,
            kind: TimerKind::Standard8,
            prescaler: Prescaler::Timer2,
            regs: timer_regs(
                [0xb0, 0xb1, NONE, NONE, NONE],
                NONE,
                0xb2,
                NONE,
                [0xb3, 0xb4, NONE, NONE],
                [NONE; 4],
                NONE,
                NONE,
                0x70,
                0x37,
            ),
            vectors: TimerVectors {
                overflow: Some(15),
                compare_a: Some(13),
                compare_b: Some(14),
                compare_c: None,
                capture: None,
            },
            masks: FlagMasks {
                overflow: 0x01,
                compare_a: 0x02,
                compare_b: 0x04,
                ..EMPTY_MASKS
            },
            capture_pin: None,
            outputs: [
                Some(ComparePin { port: 1, bit: 4 }),
                Some(ComparePin { port: 7, bit: 6 }),
                None,
            ],
        },
        TimerSpec {
            index: 3,
            kind: TimerKind::Long16,
            prescaler: Prescaler::Standard,
            regs: timer_regs(
                [0x90, 0x91, 0x92, NONE, NONE],
                NONE,
                0x94,
                0x95,
                [0x98, 0x9a, 0x9c, NONE],
                [0x99, 0x9b, 0x9d, NONE],
                0x96,
                0x97,
                0x71,
                0x38,
            ),
            vectors: TimerVectors {
                overflow: Some(35),
                compare_a: Some(32),
                compare_b: Some(33),
                compare_c: Some(34),
                capture: Some(31),
            },
            masks: FlagMasks {
                overflow: 0x01,
                compare_a: 0x02,
                compare_b: 0x04,
                compare_c: 0x08,
                capture: 0x20,
            },
            capture_pin: Some(ComparePin { port: 4, bit: 7 }),
            outputs: [
                Some(ComparePin { port: 4, bit: 3 }),
                Some(ComparePin { port: 4, bit: 4 }),
                Some(ComparePin { port: 4, bit: 5 }),
            ],
        },
        TimerSpec {
            index: 4,
            kind: TimerKind::Long16,
            prescaler: Prescaler::Standard,
            regs: timer_regs(
                [0xa0, 0xa1, 0xa2, NONE, NONE],
                NONE,
                0xa4,
                0xa5,
                [0xa8, 0xaa, 0xac, NONE],
                [0xa9, 0xab, 0xad, NONE],
                0xa6,
                0xa7,
                0x72,
                0x39,
            ),
            vectors: TimerVectors {
                overflow: Some(45),
                compare_a: Some(42),
                compare_b: Some(43),
                compare_c: Some(44),
                capture: Some(41),
            },
            masks: FlagMasks {
                overflow: 0x01,
                compare_a: 0x02,
                compare_b: 0x04,
                compare_c: 0x08,
                capture: 0x20,
            },
            capture_pin: Some(ComparePin { port: 10, bit: 0 }),
            outputs: [
                Some(ComparePin { port: 7, bit: 3 }),
                Some(ComparePin { port: 7, bit: 4 }),
                Some(ComparePin { port: 7, bit: 5 }),
            ],
        },
        TimerSpec {
            index: 5,
            kind: TimerKind::Long16,
            prescaler: Prescaler::Standard,
            regs: timer_regs(
                [0x120, 0x121, 0x122, NONE, NONE],
                NONE,
                0x124,
                0x125,
                [0x128, 0x12a, 0x12c, NONE],
                [0x129, 0x12b, 0x12d, NONE],
                0x126,
                0x127,
                0x73,
                0x3a,
            ),
            vectors: TimerVectors {
                overflow: Some(50),
                compare_a: Some(47),
                compare_b: Some(48),
                compare_c: Some(49),
                capture: Some(46),
            },
            masks: FlagMasks {
                overflow: 0x01,
                compare_a: 0x02,
                compare_b: 0x04,
                compare_c: 0x08,
                capture: 0x20,
            },
            capture_pin: Some(ComparePin { port: 10, bit: 1 }),
            outputs: [
                Some(ComparePin { port: 10, bit: 3 }),
                Some(ComparePin { port: 10, bit: 4 }),
                Some(ComparePin { port: 10, bit: 5 }),
            ],
        },
    ],
    usarts: [
        usart(
            0xc0,
            0xc1,
            0xc2,
            0xc4,
            0xc5,
            0xc6,
            Some(25),
            Some(26),
            Some(27),
        ),
        usart(
            0xc8,
            0xc9,
            0xca,
            0xcc,
            0xcd,
            0xce,
            Some(36),
            Some(37),
            Some(38),
        ),
        usart(
            0xd0,
            0xd1,
            0xd2,
            0xd4,
            0xd5,
            0xd6,
            Some(51),
            Some(52),
            Some(53),
        ),
        usart(
            0x130,
            0x131,
            0x132,
            0x134,
            0x135,
            0x136,
            Some(54),
            Some(55),
            Some(56),
        ),
    ],
    console_usart: 0,
    adc: AdcConfig {
        control: 0x7a,
        control_b: 0x7b,
        mux: 0x7c,
        data_low: 0x78,
        data_high: 0x79,
        mux_mask: 0x1f,
        mux5_mask: Some(0x08),
        high_bank: true,
        adlar_bit: 5,
        internal_ref_mask: 0xc0,
        internal_ref_value: 0x80,
        internal_ref_microvolts: 1_100_000,
        bandgap_mux: Some(0x1e),
        ground_mux: Some(0x1f),
        channels: 16,
    },
    ext_ints: &[
        ExtIntSpec {
            vector: 1,
            port: 3,
            bit: 0,
            sense_register: 0x69,
            sense_shift: 0,
            mask_bit: 0,
        },
        ExtIntSpec {
            vector: 2,
            port: 3,
            bit: 1,
            sense_register: 0x69,
            sense_shift: 2,
            mask_bit: 1,
        },
        ExtIntSpec {
            vector: 3,
            port: 3,
            bit: 2,
            sense_register: 0x69,
            sense_shift: 4,
            mask_bit: 2,
        },
        ExtIntSpec {
            vector: 4,
            port: 3,
            bit: 3,
            sense_register: 0x69,
            sense_shift: 6,
            mask_bit: 3,
        },
        ExtIntSpec {
            vector: 5,
            port: 4,
            bit: 4,
            sense_register: 0x6a,
            sense_shift: 0,
            mask_bit: 4,
        },
        ExtIntSpec {
            vector: 6,
            port: 4,
            bit: 5,
            sense_register: 0x6a,
            sense_shift: 2,
            mask_bit: 5,
        },
        ExtIntSpec {
            vector: 7,
            port: 4,
            bit: 6,
            sense_register: 0x6a,
            sense_shift: 4,
            mask_bit: 6,
        },
        ExtIntSpec {
            vector: 8,
            port: 4,
            bit: 7,
            sense_register: 0x6a,
            sense_shift: 6,
            mask_bit: 7,
        },
    ],
    pin_changes: &[
        PinChangeSpec {
            vector: 9,
            port: 1,
            mask_index: 0,
            enable_mask: 0x01,
            flag_mask: 0x01,
        },
        PinChangeSpec {
            vector: 10,
            port: 8,
            mask_index: 1,
            enable_mask: 0x02,
            flag_mask: 0x02,
        },
        PinChangeSpec {
            vector: 11,
            port: 9,
            mask_index: 2,
            enable_mask: 0x04,
            flag_mask: 0x04,
        },
    ],
    comparator: Some((
        ComparePin { port: 4, bit: 2 },
        ComparePin { port: 4, bit: 3 },
    )),
    usb: false,
};

// -- ATtiny85 --------------------------------------------------------------

/// The ATtiny85: 8 KiB flash, 512 B SRAM, 512 B EEPROM, port B, no multiplier
/// and no absolute jump.
pub static ATTINY85: Device = Device {
    name: "attiny85",
    cpu_hz: 8_000_000,
    flash_size: 8 * 1024,
    sram_size: 512,
    eeprom_size: 512,
    sram_start: 0x0060,
    ramend: 0x025f,
    pc_bytes: 2,
    vector_words: 1,
    interrupt_cycles: 4,
    call_cycles: 4,
    ret_cycles: 4,
    features: Features {
        mul: false,
        jmp_call: false,
        elpm: false,
        eind: false,
        rampz: false,
        spm: true,
    },
    numbering: PinNumbering::PortLinear,
    ports: &[Port {
        index: 0,
        pin: 0x36,
        ddr: 0x37,
        port: 0x38,
        mask: 0x3f,
        number: 0,
        letter: "B",
    }],
    analog: &[(5, 0), (2, 1), (4, 2), (3, 3)],
    regs: Regs {
        eeprom_control: 0x3c,
        eeprom_data: 0x3d,
        eeprom_address_low: 0x3e,
        eeprom_address_high: 0x3f,
        spm_control: 0x57,
        watchdog_control: 0x41,
        analog_comparator: 0x28,
        spi_control: NONE,
        spi_status: NONE,
        spi_data: NONE,
        twi_bit_rate: NONE,
        twi_status: NONE,
        twi_address: NONE,
        twi_data: NONE,
        twi_control: NONE,
        twi_mask: NONE,
        usi_data: 0x2f,
        usi_buffer: 0x30,
        usi_status: 0x2e,
        usi_control: 0x2d,
        ext_int_control_a: NONE,
        ext_int_control_b: NONE,
        ext_int_mask: NONE,
        ext_int_flags: NONE,
        general_int_mask: 0x5b,
        general_int_flags: 0x5a,
        mcu_control: 0x55,
        pin_change_control: NONE,
        pin_change_flags: NONE,
        pin_change_mask: [0x35, NONE, NONE],
        tifr: [0x58, 0x58, NONE, NONE, NONE, NONE],
        timsk: [0x59, 0x59, NONE, NONE, NONE, NONE],
        shared_timer_irq: true,
        rampz: NONE,
        eind: NONE,
        usb: NONE,
        usb_len: 0,
    },
    vectors: Vectors {
        names: &[
            "RESET",
            "INT0",
            "PCINT0",
            "TIMER1_COMPA",
            "TIMER1_OVF",
            "TIMER0_OVF",
            "EE_RDY",
            "ANA_COMP",
            "ADC",
            "TIMER1_COMPB",
            "TIMER0_COMPA",
            "TIMER0_COMPB",
            "WDT",
            "USI_START",
            "USI_OVF",
        ],
        watchdog: 12,
        analog_comparator: 7,
        adc: 8,
        eeprom: 6,
        spi: None,
        twi: None,
        spm: None,
        usi_start: Some(13),
        usi_overflow: Some(14),
        usb_general: None,
        usb_endpoint: None,
    },
    timers: &[
        TimerSpec {
            index: 0,
            kind: TimerKind::Standard8,
            prescaler: Prescaler::Standard,
            regs: timer_regs(
                [0x4a, 0x53, NONE, NONE, NONE],
                NONE,
                0x52,
                NONE,
                [0x49, 0x48, NONE, NONE],
                [NONE; 4],
                NONE,
                NONE,
                0x59,
                0x58,
            ),
            vectors: TimerVectors {
                overflow: Some(5),
                compare_a: Some(10),
                compare_b: Some(11),
                compare_c: None,
                capture: None,
            },
            masks: FlagMasks {
                overflow: 0x02,
                compare_a: 0x10,
                compare_b: 0x08,
                ..EMPTY_MASKS
            },
            capture_pin: None,
            outputs: [
                Some(ComparePin { port: 0, bit: 0 }),
                Some(ComparePin { port: 0, bit: 1 }),
                None,
            ],
        },
        TimerSpec {
            index: 1,
            kind: TimerKind::Tiny8,
            prescaler: Prescaler::Power2,
            regs: timer_regs(
                [0x50, NONE, NONE, NONE, NONE],
                0x4c,
                0x4f,
                NONE,
                [0x4e, 0x4b, 0x4d, NONE],
                [NONE; 4],
                NONE,
                NONE,
                0x59,
                0x58,
            ),
            vectors: TimerVectors {
                overflow: Some(4),
                compare_a: Some(3),
                compare_b: Some(9),
                compare_c: None,
                capture: None,
            },
            masks: FlagMasks {
                overflow: 0x04,
                compare_a: 0x40,
                compare_b: 0x20,
                ..EMPTY_MASKS
            },
            capture_pin: None,
            outputs: [
                Some(ComparePin { port: 0, bit: 1 }),
                Some(ComparePin { port: 0, bit: 4 }),
                None,
            ],
        },
    ],
    usarts: [NO_USART, NO_USART, NO_USART, NO_USART],
    console_usart: 0,
    adc: AdcConfig {
        control: 0x26,
        control_b: 0x23,
        mux: 0x27,
        data_low: 0x24,
        data_high: 0x25,
        mux_mask: 0x0f,
        mux5_mask: None,
        high_bank: false,
        adlar_bit: 5,
        internal_ref_mask: 0xd0,
        internal_ref_value: 0x40,
        internal_ref_microvolts: 1_100_000,
        bandgap_mux: Some(0x0c),
        ground_mux: Some(0x0d),
        channels: 4,
    },
    ext_ints: &[ExtIntSpec {
        vector: 1,
        port: 0,
        bit: 2,
        sense_register: 0x55,
        sense_shift: 0,
        mask_bit: 6,
    }],
    pin_changes: &[PinChangeSpec {
        vector: 2,
        port: 0,
        mask_index: 0,
        enable_mask: 0x20,
        flag_mask: 0x20,
    }],
    comparator: Some((
        ComparePin { port: 0, bit: 0 },
        ComparePin { port: 0, bit: 1 },
    )),
    usb: false,
};

// -- ATmega32U4 ------------------------------------------------------------

/// The ATmega32U4: 32 KiB flash, 2560 B SRAM, 1 KiB EEPROM, ports B through F,
/// with the USB device controller's registers present as storage.
pub static ATMEGA32U4: Device = Device {
    name: "atmega32u4",
    cpu_hz: 16_000_000,
    flash_size: 32 * 1024,
    sram_size: 2560,
    eeprom_size: 1024,
    sram_start: 0x0100,
    ramend: 0x0aff,
    pc_bytes: 2,
    vector_words: 2,
    interrupt_cycles: 5,
    call_cycles: 5,
    ret_cycles: 5,
    features: Features {
        mul: true,
        jmp_call: true,
        elpm: true,
        eind: false,
        rampz: true,
        spm: true,
    },
    numbering: PinNumbering::PortLinear,
    ports: &[
        Port {
            index: 0,
            pin: 0x23,
            ddr: 0x24,
            port: 0x25,
            mask: 0xff,
            number: 0,
            letter: "B",
        },
        Port {
            index: 1,
            pin: 0x26,
            ddr: 0x27,
            port: 0x28,
            mask: 0xc0,
            number: 1,
            letter: "C",
        },
        Port {
            index: 2,
            pin: 0x29,
            ddr: 0x2a,
            port: 0x2b,
            mask: 0xff,
            number: 2,
            letter: "D",
        },
        Port {
            index: 3,
            pin: 0x2c,
            ddr: 0x2d,
            port: 0x2e,
            mask: 0x44,
            number: 3,
            letter: "E",
        },
        Port {
            index: 4,
            pin: 0x2f,
            ddr: 0x30,
            port: 0x31,
            mask: 0xcf,
            number: 4,
            letter: "F",
        },
    ],
    analog: &[
        (32, 0),
        (33, 1),
        (34, 2),
        (35, 3),
        (36, 4),
        (37, 5),
        (38, 6),
        (39, 7),
        (20, 8),
        (22, 9),
        (23, 10),
        (4, 11),
    ],
    regs: Regs {
        eeprom_control: 0x3f,
        eeprom_data: 0x40,
        eeprom_address_low: 0x41,
        eeprom_address_high: 0x42,
        spm_control: 0x57,
        watchdog_control: 0x60,
        analog_comparator: 0x50,
        spi_control: 0x4c,
        spi_status: 0x4d,
        spi_data: 0x4e,
        twi_bit_rate: 0xb8,
        twi_status: 0xb9,
        twi_address: 0xba,
        twi_data: 0xbb,
        twi_control: 0xbc,
        twi_mask: 0xbd,
        usi_data: NONE,
        usi_buffer: NONE,
        usi_status: NONE,
        usi_control: NONE,
        ext_int_control_a: 0x69,
        ext_int_control_b: 0x6a,
        ext_int_mask: 0x3d,
        ext_int_flags: 0x3c,
        general_int_mask: NONE,
        general_int_flags: NONE,
        mcu_control: NONE,
        pin_change_control: 0x68,
        pin_change_flags: 0x3b,
        pin_change_mask: [0x6b, NONE, NONE],
        tifr: [0x35, 0x36, NONE, 0x38, 0x39, NONE],
        timsk: [0x6e, 0x6f, NONE, 0x71, 0x72, NONE],
        shared_timer_irq: false,
        rampz: 0x5b,
        eind: NONE,
        usb: 0xd7,
        usb_len: 0x29,
    },
    vectors: Vectors {
        names: &[
            "RESET",
            "INT0",
            "INT1",
            "INT2",
            "INT3",
            "RESERVED",
            "RESERVED",
            "INT6",
            "RESERVED",
            "PCINT0",
            "USB_GENERAL",
            "USB_ENDPOINT",
            "WDT",
            "RESERVED",
            "RESERVED",
            "RESERVED",
            "TIMER1_CAPT",
            "TIMER1_COMPA",
            "TIMER1_COMPB",
            "TIMER1_COMPC",
            "TIMER1_OVF",
            "TIMER0_COMPA",
            "TIMER0_COMPB",
            "TIMER0_OVF",
            "SPI_STC",
            "USART1_RX",
            "USART1_UDRE",
            "USART1_TX",
            "ANALOG_COMP",
            "ADC",
            "EE_READY",
            "TIMER3_CAPT",
            "TIMER3_COMPA",
            "TIMER3_COMPB",
            "TIMER3_COMPC",
            "TIMER3_OVF",
            "TWI",
            "SPM_READY",
            "TIMER4_COMPA",
            "TIMER4_COMPB",
            "TIMER4_COMPD",
            "TIMER4_OVF",
            "TIMER4_FPF",
        ],
        watchdog: 12,
        analog_comparator: 28,
        adc: 29,
        eeprom: 30,
        spi: Some(24),
        twi: Some(36),
        spm: Some(37),
        usi_start: None,
        usi_overflow: None,
        usb_general: Some(10),
        usb_endpoint: Some(11),
    },
    timers: &[
        TimerSpec {
            index: 0,
            kind: TimerKind::Standard8,
            prescaler: Prescaler::Standard,
            regs: timer_regs(
                [0x44, 0x45, NONE, NONE, NONE],
                NONE,
                0x46,
                NONE,
                [0x47, 0x48, NONE, NONE],
                [NONE; 4],
                NONE,
                NONE,
                0x6e,
                0x35,
            ),
            vectors: TimerVectors {
                overflow: Some(23),
                compare_a: Some(21),
                compare_b: Some(22),
                compare_c: None,
                capture: None,
            },
            masks: FlagMasks {
                overflow: 0x01,
                compare_a: 0x02,
                compare_b: 0x04,
                ..EMPTY_MASKS
            },
            capture_pin: None,
            outputs: [
                Some(ComparePin { port: 0, bit: 7 }),
                Some(ComparePin { port: 2, bit: 0 }),
                None,
            ],
        },
        TimerSpec {
            index: 1,
            kind: TimerKind::Long16,
            prescaler: Prescaler::Standard,
            regs: timer_regs(
                [0x80, 0x81, 0x82, NONE, NONE],
                NONE,
                0x84,
                0x85,
                [0x88, 0x8a, 0x8c, NONE],
                [0x89, 0x8b, 0x8d, NONE],
                0x86,
                0x87,
                0x6f,
                0x36,
            ),
            vectors: TimerVectors {
                overflow: Some(20),
                compare_a: Some(17),
                compare_b: Some(18),
                compare_c: Some(19),
                capture: Some(16),
            },
            masks: FlagMasks {
                overflow: 0x01,
                compare_a: 0x02,
                compare_b: 0x04,
                compare_c: 0x08,
                capture: 0x20,
            },
            capture_pin: Some(ComparePin { port: 2, bit: 4 }),
            outputs: [
                Some(ComparePin { port: 0, bit: 5 }),
                Some(ComparePin { port: 0, bit: 6 }),
                Some(ComparePin { port: 0, bit: 7 }),
            ],
        },
        TimerSpec {
            index: 3,
            kind: TimerKind::Long16,
            prescaler: Prescaler::Standard,
            regs: timer_regs(
                [0x90, 0x91, 0x92, NONE, NONE],
                NONE,
                0x94,
                0x95,
                [0x98, 0x9a, 0x9c, NONE],
                [0x99, 0x9b, 0x9d, NONE],
                0x96,
                0x97,
                0x71,
                0x38,
            ),
            vectors: TimerVectors {
                overflow: Some(35),
                compare_a: Some(32),
                compare_b: Some(33),
                compare_c: Some(34),
                capture: Some(31),
            },
            masks: FlagMasks {
                overflow: 0x01,
                compare_a: 0x02,
                compare_b: 0x04,
                compare_c: 0x08,
                capture: 0x20,
            },
            capture_pin: Some(ComparePin { port: 1, bit: 7 }),
            outputs: [Some(ComparePin { port: 1, bit: 6 }), None, None],
        },
        TimerSpec {
            index: 4,
            kind: TimerKind::HighSpeed4,
            prescaler: Prescaler::Power2,
            regs: TimerRegs {
                control: [0xc0, 0xc1, 0xc2, 0xc3, 0xc4],
                gtccr: NONE,
                count: 0xbe,
                count_high: NONE,
                compare: [0xcf, 0xd0, 0xd1, 0xd2],
                compare_high: [NONE; 4],
                capture: NONE,
                capture_high: NONE,
                temp_high: 0xbf,
                timsk: 0x72,
                tifr: 0x39,
            },
            vectors: TimerVectors {
                overflow: Some(41),
                compare_a: Some(38),
                compare_b: Some(39),
                compare_c: Some(40),
                capture: None,
            },
            masks: FlagMasks {
                overflow: 0x04,
                compare_a: 0x40,
                compare_b: 0x20,
                compare_c: 0x80,
                capture: 0,
            },
            capture_pin: None,
            outputs: [None, None, Some(ComparePin { port: 2, bit: 7 })],
        },
    ],
    usarts: [
        NO_USART,
        usart(
            0xc8,
            0xc9,
            0xca,
            0xcc,
            0xcd,
            0xce,
            Some(25),
            Some(26),
            Some(27),
        ),
        NO_USART,
        NO_USART,
    ],
    console_usart: 1,
    adc: AdcConfig {
        control: 0x7a,
        control_b: 0x7b,
        mux: 0x7c,
        data_low: 0x78,
        data_high: 0x79,
        mux_mask: 0x1f,
        mux5_mask: Some(0x20),
        high_bank: true,
        adlar_bit: 5,
        internal_ref_mask: 0xc0,
        internal_ref_value: 0xc0,
        internal_ref_microvolts: 2_560_000,
        bandgap_mux: Some(0x1e),
        ground_mux: Some(0x1f),
        channels: 12,
    },
    ext_ints: &[
        ExtIntSpec {
            vector: 1,
            port: 2,
            bit: 0,
            sense_register: 0x69,
            sense_shift: 0,
            mask_bit: 0,
        },
        ExtIntSpec {
            vector: 2,
            port: 2,
            bit: 1,
            sense_register: 0x69,
            sense_shift: 2,
            mask_bit: 1,
        },
        ExtIntSpec {
            vector: 3,
            port: 2,
            bit: 2,
            sense_register: 0x69,
            sense_shift: 4,
            mask_bit: 2,
        },
        ExtIntSpec {
            vector: 4,
            port: 2,
            bit: 3,
            sense_register: 0x69,
            sense_shift: 6,
            mask_bit: 3,
        },
        ExtIntSpec {
            vector: 7,
            port: 3,
            bit: 6,
            sense_register: 0x6a,
            sense_shift: 4,
            mask_bit: 6,
        },
    ],
    pin_changes: &[PinChangeSpec {
        vector: 9,
        port: 0,
        mask_index: 0,
        enable_mask: 0x01,
        flag_mask: 0x01,
    }],
    comparator: None,
    usb: true,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_tables_line_up_with_the_contents_lists() {
        for device in [&ATMEGA328P, &ATMEGA2560, &ATTINY85, &ATMEGA32U4] {
            for timer in device.timers {
                for vector in [
                    timer.vectors.overflow,
                    timer.vectors.compare_a,
                    timer.vectors.compare_b,
                    timer.vectors.compare_c,
                    timer.vectors.capture,
                ]
                .into_iter()
                .flatten()
                {
                    assert!(
                        usize::from(vector) < device.vectors.names.len(),
                        "{} timer {} names vector {vector}",
                        device.name,
                        timer.index
                    );
                }
            }
            for usart in &device.usarts {
                for vector in [usart.vector_rx, usart.vector_udre, usart.vector_tx]
                    .into_iter()
                    .flatten()
                {
                    assert!(usize::from(vector) < device.vectors.names.len());
                }
            }
        }
    }

    #[test]
    fn the_port_numbering_follows_each_part() {
        assert_eq!(ATMEGA328P.port_of_pin(13), Some((0, 5)));
        assert_eq!(ATMEGA328P.port_of_pin(2), Some((2, 2)));

        assert_eq!(ATMEGA2560.port_of_pin(0), Some((0, 0)));
        assert_eq!(ATMEGA2560.port_of_pin(80), Some((10, 0)));
        assert_eq!(ATMEGA2560.pin_of_port(10, 7), Some(87));

        assert_eq!(ATTINY85.port_of_pin(5), Some((0, 5)));
        assert_eq!(ATTINY85.port_of_pin(6), None);

        assert_eq!(ATMEGA32U4.port_of_pin(32), Some((4, 0)));
        assert_eq!(ATMEGA32U4.pin_of_port(4, 7), Some(39));
        assert_eq!(ATMEGA32U4.port_of_pin(8), None, "PORTE bit 0 is not bonded");
    }
}
