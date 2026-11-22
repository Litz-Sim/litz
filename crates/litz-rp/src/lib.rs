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
//! The blocks behind [`peripherals`] are SIO (GPIO, the inter-core FIFOs,
//! spinlocks and the hardware divider), IO_BANK0/PADS, the 64-bit timer with
//! four alarms, TICKS, the watchdog, RESETS, CLOCKS, PWM, ADC, DMA, the two
//! PL011 UARTs, SPI0/1, I2C0/1, the XIP controller and the QSPI SSI sequence
//! `boot2` uses, the USB device controller and its CDC console, plus the
//! system control space (`NVIC`, `SysTick`, `SCB`).
//!
//! # What is not modelled yet
//!
//! PIO and its state machines, the interpolators, and core 1. PIO0 and PIO1
//! share a scratch register file: their registers accept writes, read back
//! what was written and otherwise read zero, but no state machine runs and no
//! FIFO moves. The interpolator registers in SIO fall into scratch storage the
//! same way, so a lane's accumulators and shifters never advance. Releasing
//! core 1 starts no second core: the SIO FIFO and spinlocks exist for
//! single-core firmware, and `CPUID` always reports core 0.
//!
//! The hardware divider is not a gap: SIO implements the signed and unsigned
//! divide, its quotient and remainder read back, and `DIV_CSR.READY` is always
//! set. The RP2040 has no square-root peripheral to model.
//!
//! None of these traps. Firmware that touches a gap keeps running against the
//! inert version above; the named [`cpu::Fault`]s are reserved for undefined
//! or unimplemented instructions, a bus access with no target, and a halt.
#![forbid(unsafe_code)]

pub mod bus;
pub mod chip;
pub mod cpu;
pub mod machine;
pub mod peripherals;

pub use chip::{CPU_HZ, Rp2040, SIO_PINS};
pub use cpu::{Bus, Core, Fault};
pub use machine::{Machine, Stop};
