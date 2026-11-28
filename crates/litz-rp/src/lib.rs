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
//! PIO0 and PIO1 each run four state machines with their own clock dividers,
//! program memory, shift registers, delays, side-set, autopush/autopull and
//! the full nine-instruction set, and their pins drive and sample the pads
//! through IO_BANK0's function selects. SIO's interpolators compute the lane
//! result from both accumulators with the shift, mask, sign, cross-input and
//! blend controls, and the inter-core FIFOs carry words in both directions.
//!
//! Core 1 is a second [`cpu::Core`] with its own NVIC, `SysTick` and `VTOR`:
//! the word core 0 writes to SIO's FIFO releases it through the bootrom's
//! launch block, and from then on both cores step against one clock, the
//! machine advancing by the longer of their instructions.
//!
//! # What is approximate
//!
//! PIO's instruction timing is one system clock per instruction after the
//! divider, with the programmed delays and the stall of a blocking `PULL` or
//! `PUSH` accounted for; the pipeline's one-instruction skew on a pin edge is
//! not reproduced, and a side-set's extra cycle is folded into the delay.
//! The ADC samples on its own schedule rather than at the end of a converted
//! cycle. A UART transmitter's frames are timed bit by bit at the baud its
//! divisor names, but a byte the host sends arrives at the receiver at once
//! rather than a bit at a time. The hardware divider is exact: SIO implements the signed and
//! unsigned divide, its quotient and remainder read back, and `DIV_CSR.READY`
//! is always set; the RP2040 has no square-root peripheral to model.
//!
//! None of these traps. The named [`cpu::Fault`]s are reserved for undefined
//! or unimplemented instructions, a bus access with no target, and a halt.
#![forbid(unsafe_code)]

pub mod bus;
pub mod chip;
pub mod cpu;
pub mod machine;
pub mod peripherals;
pub mod thumb;

pub use chip::{CPU_HZ, Rp2040, SIO_PINS};
pub use cpu::{Bus, Core, Fault};
pub use machine::{Machine, Stop};
