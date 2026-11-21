//! A pure-Rust ATmega328P interpreter for Litz.
//!
//! The crate is an AVR8 core plus the peripherals an Arduino Uno sketch needs,
//! written from the instruction set and the datasheet with no external
//! emulator and no generated JavaScript. It compiles natively and for
//! `wasm32-unknown-unknown`, and it implements [`litz_board::Chip`] so the
//! board runtime can drive it beside every other chip backend.
//!
//! # What is implemented
//!
//! * 32 KiB flash, 2 KiB SRAM, the 32-register file, `SREG`, and a 16-bit
//!   stack pointer reset to `RAMEND`.
//! * The AVR8 base instruction set avr-gcc emits:
//!   * arithmetic and logic: `ADD`, `ADC`, `ADDI`, `SUB`, `SBC`, `SUBI`,
//!     `SBCI`, `CP`, `CPC`, `CPI`, `AND`, `ANDI`, `OR`, `ORI`, `EOR`, `COM`,
//!     `NEG`, `INC`, `DEC`, `SWAP`, `ASR`, `LSR`, `ROR`, `ADIW`, `SBIW`
//!     (`LSL`/`ROL` are the `ADD`/`ADC` aliases).
//!   * moves and immediates: `MOV`, `MOVW`, `LDI`, `SBR`, `CBR`.
//!   * loads and stores: `LD`/`ST` through `X`, `Y` and `Z` with pre- and
//!     post-increment, `LDD`/`STD` displacement, `LDS`/`STS`, `LPM` (all
//!     forms), `PUSH`, `POP`.
//!   * control flow: `RJMP`, `IJMP`, `JMP`, `RCALL`, `ICALL`, `CALL`, `RET`,
//!     `RETI`, and every branch alias (`BREQ`, `BRNE`, `BRCS`, `BRCC`,
//!     `BRSH`, `BRLO`, `BRGE`, `BRLT`, `BRMI`, `BRPL`, `BRHS`, `BRHC`,
//!     `BRTS`, `BRTC`, `BRVS`, `BRVC`, `BRIE`, `BRID`).
//!   * skipping: `CPSE`, `SBRC`, `SBRS`, `SBIC`, `SBIS`.
//!   * bits: `BLD`, `BST`, `SBI`, `CBI`, `BSET`, `BCLR` (`SEI`/`CLI`).
//!   * multiply: `MUL`, `MULS`, `MULSU`.
//!   * miscellaneous: `NOP`, `SLEEP`, `WDR` (clears the watchdog counter).
//! * Cycle counts per instruction, including the two-cycle branch taken, the
//!   three-cycle skip of a two-word instruction and the four-cycle interrupt
//!   entry.
//! * [`Machine::run`] with a cycle budget, idle on `SLEEP` when no enabled
//!   interrupt can wake the core, and a named [`Fault`] on an unknown opcode
//!   or an out-of-range access. The core never silently treats an unknown
//!   word as `NOP`.
//!
//! ## Interrupts
//!
//! [`vector`] names all 26 ATmega328P interrupt vectors in datasheet priority
//! order. Vector `n` occupies byte address `4n` (word address `2n`), the
//! layout avr-gcc emits with a four-byte `jmp` per slot. The core checks
//! [`Peripherals::pending_interrupt`] before every instruction; when `I` is set
//! it pushes the return address, clears `I`, jumps to the vector and retires
//! [`INTERRUPT_CYCLES`] cycles. `RETI` restores `I`. Wired sources:
//!
//! * `INT0`/`INT1` (D2/PD2 and D3/PD3) with all four `EICRA` sense modes
//!   (`EIFR`/`EIMSK`), and `PCINT0`/`PCINT1`/`PCINT2` over ports B/C/D with
//!   `PCMSK0..2` masks (`PCIFR`/`PCICR`).
//! * Timer0/Timer1/Timer2 overflow, compare A/B and Timer1 input capture, via
//!   `TIMSK0..2`/`TIFR0..2`.
//! * USART0 receive-complete, data-register-empty and transmit-complete
//!   (`RXCIE0`/`UDRIE0`/`TXCIE0`).
//! * ADC conversion complete, EEPROM ready, analog comparator, TWI, SPI
//!   transfer-complete and SPM-ready.
//! * The watchdog (`WDIE`).
//!
//! Vector flags are cleared on entry the way hardware clears them; `RXC0` and
//! `UDRE0` are level conditions and remain pending until the ISR touches
//! `UDR0`. A sleeping core wakes when an enabled source is pending. (`SLEEP`
//! keeps the peripheral clock advancing one cycle per [`Machine::step`].)
//!
//! ## Timers and PWM
//!
//! Timer0 and Timer2 are 8-bit with normal, CTC, fast-PWM and phase-correct
//! modes, all five internal prescalers (Timer2 uses its own /32 and /128
//! steps), compare flags and a [`Timer0::overflows`]/[`Timer2::overflows`]
//! counter suitable for `millis()`. Timer1 is 16-bit with all sixteen waveform
//! modes (normal, CTC, fast PWM and phase correct, with `TOP` taken from
//! `OCRnA` or `ICR1`) plus input capture into `ICR1` on `ICP1` (PB0, D8).
//!
//! The compare outputs drive the real pins when `COMnx` connects them:
//! `OC0A` = D6, `OC0B` = D5, `OC1A` = D9, `OC1B` = D10, `OC2A` = D11,
//! `OC2B` = D3. A firmware `analogWrite` therefore produces an actual duty
//! cycle, reported as ordinary GPIO edges.
//!
//! ## GPIO
//!
//! `PINB`/`DDRB`/`PORTB`, `PINC`/`DDRC`/`PORTC` and `PIND`/`DDRD`/`PORTD`.
//! Output edges are reported as `(pin, level)` with the Arduino Uno digital
//! numbering (D0–D7, D8–D13, A0–A5) through a callback or
//! [`Machine::take_outputs`].
//!
//! ## ADC
//!
//! 10-bit successive approximation with `MUX0..3` single-ended channels,
//! `ADLAR`, the AVcc/external and internal 1.1 V references, the `ADPS`
//! prescaler, conversion timing (13 ADC clocks), `ADSC` self-clear,
//! free-running mode and `ADIF`/`ADIE`. There are no analog pins to sample, so
//! the host injects a voltage per channel in microvolts:
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
//! [`Atmega328p::with_board`] attaches the chip to a
//! [`litz_board::Board`](https://docs.rs/litz-board) and keeps it behind
//! `Rc<RefCell<Board>>`, so a test or UI shares the same parts. Board GPIO
//! numbers are the Arduino Uno's header numbering, which is also what
//! [`Machine::set_input`] and the output events use; [`digital_to_port`] then
//! maps each to a port bit:
//!
//! | Board GPIO | Arduino pin | Port bit | Notes |
//! |------------|-------------|----------|-------|
//! | 0–7        | D0–D7       | `PORTD[0..8]` | D2/D3 are `INT0`/`INT1` |
//! | 8–13       | D8–D13      | `PORTB[0..6]` | D13 is `PB5` (the Uno LED), D10 is `SS` |
//! | 14–19      | A0–A5       | `PORTC[0..6]` | also ADC channels 0–5 |
//!
//! Each [`litz_board::Chip::run`] pass, in order:
//!
//! 1. applies scheduled host actions whose time has come;
//! 2. advances the board to the chip's time, then drains
//!    `board.take_chip_inputs()` into [`Machine::set_input`];
//! 3. publishes `board.analog_inputs()` on `A0..A5` as microvolts;
//! 4. runs the core for the budget, shortened when a board part's
//!    `next_event` (or a scheduled input) is due sooner;
//! 5. publishes [`Machine::take_outputs`] as `Drive::strong` on the board and
//!    advances the board once more, so events the new drives caused are ready
//!    for the next pass.
//!
//! The run loop turns cycles into nanoseconds at [`CPU_HZ`]; a board deadline
//! is rounded up to a cycle so an edge is never missed between passes. A board
//! part that schedules its own edges (an HC-SR04's ECHO, a sensor's frame)
//! therefore reaches the CPU instead of being collapsed into one settle.
//!
//! Buses are bridged too: the TWI master calls the board's `i2c0` devices
//! through `I2c::write`/`read`/`write_read`, and the SPI master calls
//! `Board::spi_transfer` on controller 0 with the chip-select pin the
//! peripheral reports (D10 unless changed).
//!
//! ## SPI, TWI and EEPROM
//!
//! SPI is a master with `SPCR`/`SPSR`/`SPDR`, all four clock dividers,
//! `SPIF`/`WCOL` and a host callback shaped `spi_transfer(cs_pin, tx) -> rx`
//! ([`Machine::set_spi_callback`]). TWI is a master state machine with the
//! status codes a polled or interrupt-driven `Wire` uses and the callbacks
//! `i2c_write(addr, bytes)` and `i2c_read(addr, len) -> byte`
//! ([`Machine::set_i2c_write_callback`], [`Machine::set_i2c_read_callback`]).
//! The EEPROM is 1 KiB with `EECR`/`EEAR`/`EEDR`, the `EEMPE` write-enable
//! window and a 3.4 ms (54 400-cycle) write that holds `EEPE` set. USART0
//! receive bytes enter through [`Machine::write_console`], set `RXC0` and
//! interrupt; transmitted bytes join the console buffer
//! ([`Machine::take_console`]).
//!
//! ## Everything else
//!
//! The watchdog interrupt mode with `WDR`, the analog comparator's digital
//! comparison of AIN0/AIN1, and Intel HEX / raw binary loading with checksum
//! and range validation.
//!
//! # What is still missing
//!
//! * **Clock sources and power modes.** The core always runs at 16 MHz;
//!   `CLKPR`, the system-clock prescaler and external crystals are ignored.
//!   `SLEEP` wakes on interrupts but the individual sleep modes, `SMCR` and
//!   power-reduction (`PRR`) are not modelled.
//! * **SPM** (self-programming), `ELPM` and the reduced-core opcodes. The
//!   `SPM_READY` vector exists but an `SPM` instruction still traps.
//! * **Watchdog system reset.** A timeout raises `WDIF`; the reset mode that
//!   would restart the core is not modelled.
//! * **Bus timing.** USART is byte-granular (no shift register, baud generator
//!   or framing), TWI operations complete immediately rather than over SCL
//!   cycles, and SPI counts clock cycles but does not model CPOL/CPHA edges.
//! * **Slave modes, differential ADC channels, Timer2 asynchronous mode,
//!   external clock inputs, input-capture noise cancelling, and the analog
//!   comparator's actual voltage comparison** (it compares the two pins as
//!   logic levels).
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
pub mod hex;
pub mod machine;
pub mod peripherals;

pub use chip::{
    ANALOG_FIRST_GPIO, ANALOG_LAST_GPIO, Atmega328p, CPU_HZ, I2C_CONTROLLER, SPI_CONTROLLER,
    analog_channel,
};
pub use cpu::{
    Cpu, DataBus, FLASH_SIZE, FLASH_WORDS, Fault, RAMEND, SRAM_END, SRAM_SIZE, SRAM_START, Sreg,
};
pub use hex::{HexError, load_hex, looks_like_hex, parse_hex};
pub use machine::{LoadError, Machine, Stop};
pub use peripherals::eeprom::{EEPROM_SIZE, WRITE_CYCLES as EEPROM_WRITE_CYCLES};
pub use peripherals::{
    Adc, Eeprom, Gpio, I2cRead, I2cWrite, INTERRUPT_CYCLES, PORT_B, PORT_C, PORT_D, Peripherals,
    Spi, SpiTransfer, Timer0, Timer1, Timer2, Twi, Usart0, Watchdog, digital_to_port,
    port_to_digital, vector,
};
