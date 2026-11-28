//! A pure-Rust AVR8 interpreter for Litz.
//!
//! The crate is one AVR8 core and one peripheral implementation, configured by
//! a per-device table. It models four parts:
//!
//! | Part | Flash | SRAM | EEPROM | Ports | Distinctive hardware |
//! |------|-------|------|--------|-------|----------------------|
//! | ATmega328P | 32 KiB | 2 KiB | 1 KiB | B, C, D | Arduino Uno numbering |
//! | ATmega2560 | 256 KiB | 8 KiB | 4 KiB | A–L | 22-bit PC, `RAMPZ`/`EIND`, `ELPM`, four USARTs, six timers |
//! | ATtiny85 | 8 KiB | 512 B | 512 B | B | no `MUL`, no `JMP`/`CALL`, USI instead of SPI/TWI |
//! | ATmega32U4 | 32 KiB | 2.5 KiB | 1 KiB | B–F | USART1, Timer4, USB registers as storage |
//!
//! Everything is written from the instruction set and the datasheets, with no
//! external emulator. It compiles natively and for `wasm32-unknown-unknown`,
//! and each part implements [`litz_board::Chip`] so the board runtime can drive
//! it beside every other chip backend.
//!
//! # The device table
//!
//! [`device`] holds one [`Device`] per part: flash/SRAM/EEPROM sizes, the
//! data-space boundary, the port and pin numbering, every peripheral register
//! address, the interrupt vector table and the instruction-set features. The
//! core and the peripherals read that table instead of hard-coding an
//! ATmega328P, so a register a part does not have simply is not dispatched
//! (it reads as zero) and its vector slot does not exist. [`Machine::new`] is
//! still the ATmega328P; [`Machine::for_device`] takes any other table, and
//! [`Atmega2560`], [`Attiny85`] and [`Atmega32u4`] are the board-facing
//! types.
//!
//! # What is implemented
//!
//! * The AVR8 base instruction set avr-gcc emits: arithmetic and logic
//!   (`ADD`/`ADC`/`ADIW`, `SUB`/`SBC`/`SBIW`, `CP`/`CPC`/`CPI`, the bitwise
//!   operations, `COM`, `NEG`, `INC`, `DEC`, `SWAP`, the shifts and rotates),
//!   moves and immediates, the full `LD`/`ST`/`LDD`/`STD`/`LDS`/`STS` group,
//!   `PUSH`/`POP`, the branch and skip instructions,
//!   `RJMP`/`IJMP`/`RCALL`/`ICALL`/`RET`/`RETI`, and the bit instructions.
//! * Per-part instruction gates: `MUL`/`MULS`/`MULSU` exist everywhere except
//!   the ATtiny85; absolute `JMP`/`CALL` exist on the three larger parts;
//!   `ELPM`, `EIJMP`/`EICALL`, `RAMPZ` and `EIND` exist where the datasheet
//!   puts them. An instruction a part lacks traps as an unknown opcode rather
//!   than being silently executed.
//! * Per-part addressing: the ATmega2560's program counter is 22 bits, so an
//!   interrupt pushes three return-address bytes, `JMP`/`CALL` read the sixth
//!   address bit from their first word, and `EIND` supplies bit 16 for
//!   `EIJMP`/`EICALL`.
//! * Cycle counts per instruction, with the part's own vector-entry and
//!   call/return costs (four cycles on the 328P and ATtiny85, five on the
//!   2560 and 32U4).
//! * [`Machine::run`] with a cycle budget, idle on `SLEEP` when no enabled
//!   interrupt can wake the core, and a named [`Fault`] on an unknown opcode
//!   or an out-of-range access.
//!
//! ## Interrupts
//!
//! [`vector`] names the ATmega328P's 26 vectors; each device table has its own
//! names and order, and [`word_address_for`] computes a part's slot address.
//! The 328P, 2560 and 32U4 use two-word (four-byte) slots; the ATtiny85 uses
//! one-word slots, so its vectors are two bytes apart. The core checks
//! [`Peripherals::pending_interrupt`] before every instruction and takes the
//! lowest-numbered pending, enabled source, which is the datasheet's priority.
//! Wired sources on the parts that have them:
//!
//! * External interrupts with their `EICRA`/`EICRB` (or the ATtiny's `MCUCR`)
//!   sense modes, and pin-change groups over their `PCMSK` masks.
//! * Every timer's overflow, compare A/B/C and input capture.
//! * Every USART's receive-complete, data-register-empty and
//!   transmit-complete sources; the ATtiny's USI start and overflow.
//! * ADC conversion complete, EEPROM ready, analog comparator, TWI, SPI
//!   transfer-complete and SPM-ready.
//! * The watchdog (`WDIE`).
//!
//! Vector flags are cleared on entry the way hardware clears them; the USART
//! receive and empty conditions are level-triggered and remain pending until
//! the ISR touches the data register. A sleeping core wakes when an enabled
//! source is pending.
//!
//! ## Timers and PWM
//!
//! All timers use one counter model, configured by the part's table: the
//! standard 8-bit timers with `TCCRA`/`TCCRB`, the 16-bit timers with capture
//! and up to three compare channels, the ATtiny85's Timer1 with `OCR1C` as
//! `TOP` and the ATmega32U4's 10-bit high-speed Timer4. Normal, CTC, fast PWM
//! and phase-correct PWM are supported; the prescaler tables follow each
//! part (including Timer2's `/32` and `/128`, and the four-bit `1 << (CS-1)`
//! table). Each timer has an [`overflows`](Timer::overflows) count suitable
//! for `millis()`.
//!
//! Compare outputs drive the real pins when `COMnx` connects them, so a
//! firmware `analogWrite` produces an actual duty cycle, reported as ordinary
//! GPIO edges and, through [`Machine::pwm_duties`], as the duty the pin is
//! switching at.
//!
//! ## GPIO
//!
//! Every port the package brings out, with `PINn`/`DDRn`/`PORTn` semantics
//! (writing one to a `PINn` bit toggles the latch). Output edges are reported
//! as `(pin, level)` in the part's host numbering: the Arduino Uno's header
//! numbering on the 328P, and `port position * 8 + bit` on the others.
//!
//! ## ADC
//!
//! 10-bit successive approximation with the part's single-ended channel
//! encoding, `ADLAR`, the AVcc/external and internal 1.1 V references, the
//! `ADPS` prescaler, conversion timing (13 ADC clocks), `ADSC` self-clear,
//! free-running mode and `ADIF`/`ADIE`. The ATmega2560 and 32U4 also decode
//! `MUX5` into their `ADC8+` bank. There are no analog pins to sample, so the
//! host injects a voltage per channel in microvolts:
//!
//! ```
//! # use litz_avr::Machine;
//! let mut machine = Machine::new();
//! machine.set_analog_input(0, 2_500_000); // 2.5 V from the board's analog solve
//! ```
//!
//! A board's analog solution reports volts; multiply by `1_000_000` to
//! microvolts. [`Machine::set_analog_reference`] sets the external reference
//! (5 V by default).
//!
//! ## Board attachment
//!
//! Each chip type has `with_board`/`with_shared_board`, which attach it to a
//! [`litz_board::Board`](https://docs.rs/litz-board) behind
//! `Rc<RefCell<Board>>`. Board GPIO numbers are the part's host numbering,
//! which is also what [`Machine::set_input`] and the output events use. For
//! the ATmega328P they are the Arduino Uno's header numbers, which
//! [`digital_to_port`] maps to port bits:
//!
//! | Board GPIO | Arduino pin | Port bit | Notes |
//! |------------|-------------|----------|-------|
//! | 0–7        | D0–D7       | `PORTD[0..8]` | D2/D3 are `INT0`/`INT1` |
//! | 8–13       | D8–D13      | `PORTB[0..6]` | D13 is `PB5` (the Uno LED), D10 is `SS` |
//! | 14–19      | A0–A5       | `PORTC[0..6]` | also ADC channels 0–5 |
//!
//! Each [`litz_board::Chip::run`] pass applies scheduled host actions, advances
//! the board to the chip's time, drains its resolved pin levels into
//! [`Machine::set_input`], publishes the board's analog voltages, runs the
//! core for the budget (shortened when a board part or host input is due
//! sooner), and then publishes the chip's output edges and PWM duties as
//! strong drives. Cycles become nanoseconds at the part's clock.
//!
//! Buses are bridged too: the TWI master calls the board's `i2c0` devices
//! through `I2c::write`/`read`/`write_read`, and the SPI master calls
//! `Board::spi_transfer` on controller 0 with the chip-select pin the
//! peripheral reports.
//!
//! ## SPI, TWI, USI and EEPROM
//!
//! SPI is a master with `SPCR`/`SPSR`/`SPDR`, all four clock dividers,
//! `SPIF`/`WCOL` and a host callback shaped `spi_transfer(cs_pin, tx) -> rx`
//! ([`Machine::set_spi_callback`]). TWI is a master state machine with the
//! status codes a polled or interrupt-driven `Wire` uses and the callbacks
//! `i2c_write(addr, bytes)` and `i2c_read(addr, len) -> byte`. The EEPROM is
//! sized per part with `EECR`/`EEAR`/`EEDR`, the `EEMPE` write-enable window
//! and a 3.4 ms write that holds `EEPE` set, scaled to the part's clock.
//! USART receive bytes enter through [`Machine::write_console`] and interrupt;
//! transmitted bytes join the console buffer. The ATtiny85 replaces SPI/TWI
//! with the USI shift register ([`Peripherals::usi`]).
//!
//! # What is deliberately simplified
//!
//! * **Clock sources and power modes.** Each part runs at the clock in its
//!   table (16 MHz for the three ATmegas, 8 MHz for the ATtiny85); `CLKPR`,
//!   the system-clock prescaler and external crystals are ignored. `SLEEP`
//!   wakes on interrupts but the individual sleep modes, `SMCR` and
//!   power-reduction (`PRR`) are not modelled.
//! * **Self-programming.** `SPM` traps even though the `SPM_READY` vector
//!   exists, so flash is read-only.
//! * **USB.** The ATmega32U4's USB device controller registers are storage;
//!   no USB traffic is generated, so its two vectors never become pending.
//! * **Bus timing.** USART is byte-granular (no shift register, baud
//!   generator or framing), TWI operations complete immediately rather than
//!   over SCL cycles, and SPI counts clock cycles but does not model
//!   CPOL/CPHA edges. The ATtiny's USI shifts on `USITC` strobes only.
//! * **Analog detail.** Differential ADC channels, gain stages and the
//!   temperature channel read the injected single-ended channel of the same
//!   number or zero; the analog comparator compares its two pins as logic
//!   levels, not voltages.
//! * **Timer detail.** External clock inputs stop a timer; Timer2's
//!   asynchronous mode is synchronous; the ATmega32U4 Timer4 dead-time
//!   generator and complementary outputs are not modelled; force-output
//!   strobes are ignored.
//! * **Watchdog system reset.** A timeout raises `WDIF`; the reset mode that
//!   would restart the core is not modelled.
//!
//! # Example
//!
//! ```
//! use litz_avr::{Machine, Stop};
//!
//! // ldi r16, 0xff ; ldi r17, 0x01 ; add r16, r17 ; rjmp .
//! let program: [u16; 4] = [0xef0f, 0xe011, 0x0f01, 0xcfff];
//! let mut machine = Machine::new();
//! let mut bytes = Vec::new();
//! for word in program {
//!     bytes.extend_from_slice(&word.to_le_bytes());
//! }
//! machine.load_bin(&bytes, 0).unwrap();
//! machine.boot();
//! machine.run(16);
//! assert_eq!(machine.cpu().reg(16), 0x00);
//! assert!(machine.cpu().sreg().carry());
//! assert_eq!(machine.run(1), Stop::Budget);
//! ```
#![forbid(unsafe_code)]

pub mod chip;
pub mod cpu;
pub mod device;
pub mod hex;
pub mod machine;
pub mod peripherals;

pub use chip::{
    ANALOG_FIRST_GPIO, ANALOG_LAST_GPIO, Atmega32u4, Atmega328p, Atmega2560, Attiny85, AvrChip,
    CPU_HZ, I2C_CONTROLLER, SPI_CONTROLLER, analog_channel,
};
pub use cpu::{
    Cpu, DataBus, FLASH_SIZE, FLASH_WORDS, Fault, RAMEND, SRAM_END, SRAM_SIZE, SRAM_START, Sreg,
};
pub use device::{
    ATMEGA32U4, ATMEGA328P, ATMEGA2560, ATTINY85, AdcConfig, ComparePin, Device, ExtIntSpec,
    Features, FlagMasks, PinChangeSpec, Port, Regs, TimerKind, TimerRegs, TimerSpec, TimerVectors,
    UsartSpec, Vectors,
};
pub use hex::{HexError, load_hex, looks_like_hex, parse_hex};
pub use machine::{LoadError, Machine, Stop};
pub use peripherals::eeprom::{EEPROM_SIZE, WRITE_CYCLES as EEPROM_WRITE_CYCLES};
pub use peripherals::usi::Usi;
pub use peripherals::{
    Adc, Eeprom, Gpio, I2cRead, I2cWrite, INTERRUPT_CYCLES, PORT_B, PORT_C, PORT_D, Peripherals,
    Spi, SpiTransfer, Timer, Timer0, Timer1, Timer2, Twi, Usart0, Watchdog, byte_address_for,
    digital_to_port, port_to_digital, vector, word_address_for,
};
