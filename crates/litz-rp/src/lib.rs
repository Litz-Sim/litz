//! A pure-Rust RP2040 for Litz.
//!
//! The RP2040 is a dual-core ARM Cortex-M0+ at 125 MHz with 264 KiB of SRAM,
//! no internal flash (code lives on a QSPI part behind the XIP cache) and a
//! peripheral set built around the SIO block, the 64-bit microsecond timer,
//! the IO mux and the PIO state machines.
//!
//! The crate is one of four chip backends beside `litz-avr`, the `esp32sim`
//! adapters and the wasm components, and it implements [`litz_board::Chip`] the
//! same way, so a Pico can sit on a diagram next to an ESP32 or an ATmega328P
//! without the board runtime knowing which is which.
//!
//! # What runs
//!
//! [`cpu`] is an ARMv6-M interpreter with the exception model: NVIC lines,
//! `SysTick`, stacked frames, `EXC_RETURN`, MSP/PSP banking. [`machine`] owns
//! the clock — one instruction, its cycles, then every peripheral ticked by
//! that amount, with a pending interrupt delivered between instructions and
//! never inside one.
//!
//! Boot follows the silicon: the ROM copies the first 256 bytes of flash into
//! SRAM, calls that copy with `LR` pointing back at itself, and reads the
//! application's vector table from the flash immediately after it once stage 2
//! returns. [`Machine::boot`] performs the same handoff, so a stock `boot2`
//! image and an image built here both run unchanged, and a program compiled by
//! `arm-none-eabi-gcc -mcpu=cortex-m0plus` runs as compiled.
//!
//! The blocks behind [`peripherals`] are SIO, IO_BANK0/PADS, the 64-bit timer
//! with four alarms, TICKS, the watchdog, RESETS, CLOCKS, PWM, ADC, DMA, the
//! two PL011 UARTs, SPI0/1, I2C0/1, the XIP controller and the QSPI SSI
//! sequence `boot2` uses, plus the system control space (`NVIC`, `SysTick`,
//! `SCB`).
//!
//! # What is not modelled yet
//!
//! PIO, the USB device controller, the interpolators, the hardware
//! divider/square-root peripherals, and core 1 — the SIO FIFO and spinlocks
//! exist for single-core firmware, but releasing core 1 starts nothing. A
//! firmware that depends on any of these traps with a named fault rather than
//! running into behaviour the model invented.
#![forbid(unsafe_code)]

pub mod bus;
pub mod chip;
pub mod cpu;
pub mod machine;
pub mod peripherals;

pub use chip::{CPU_HZ, Rp2040, SIO_PINS};
pub use cpu::{Bus, Core, Fault};
pub use machine::{Machine, Stop};
