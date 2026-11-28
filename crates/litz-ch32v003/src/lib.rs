//! A CH32V003 for Litz.
//!
//! The crate is the WCH part end to end: the QingKe V2A RV32EC core, the PFIC,
//! the GPIO/EXTI/AFIO block, TIM1, TIM2, SysTick, USART1, I2C1, SPI1, ADC1,
//! RCC, PWR, the independent and window watchdogs, the flash controller and a
//! [`litz_board::Chip`] adapter that drives a board beside every other
//! backend.
//!
//! # Shape
//!
//! * [`cpu`] decodes and executes RV32E plus the compressed encodings.
//! * [`bus`] maps flash, SRAM, the peripheral blocks and the PFIC.
//! * [`machine`] owns the clock: instruction, peripheral tick, interrupt.
//! * [`chip`] attaches a board and implements [`litz_board::Chip`].
//! * [`disasm`] is the RV32EC disassembler behind the debug surface.
//!
//! # Pin numbering
//!
//! Board GPIO numbers are the 18 pads, `0..=7` = PC0..PC7, `8..=15` =
//! PD0..PD7, 16 = PA1, 17 = PA2. See [`chip::SIO_PINS`] and
//! [`chip::analog_channel`].
//!
//! # What is modelled, and what is not
//!
//! The core, traps and PFIC delivery, GPIO with pulls and alternate-function
//! release, EXTI on both edges, the timers with PWM, USART 8N1 with board and
//! console bytes, I²C and SPI masters bridged to the board, the 10-bit ADC,
//! SysTick, the clock tree's arithmetic, and both watchdogs' resets are
//! modelled. Bus timing is not: transactions complete on the register write
//! that ends them, clock gates are storage rather than switches, and the
//! option bytes are registers. The vector table is 32-bit addresses with the
//! reset vector at entry 0 and PFIC source `n` at entry `n + 1`.
#![forbid(unsafe_code)]

pub mod bus;
pub mod chip;
pub mod cpu;
pub mod disasm;
pub mod machine;
pub mod peripherals;

pub use chip::{Ch32v003, SIO_PINS, analog_channel};
pub use cpu::{Core, Fault};
pub use machine::{CPU_HZ, Machine, Stop};
