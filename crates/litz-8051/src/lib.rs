//! A pure-Rust MCS-51 (8051) interpreter for Litz: an STC89C52RC backend.
//!
//! The crate contains a complete 8051 core, the STC89C52RC's special function
//! registers, its four quasi-bidirectional ports, Timer 0/1/2, the full-duplex
//! serial port, the six interrupt sources with two priority levels, and an
//! adapter that implements [`litz_board::Chip`](https://docs.rs/litz-board) so
//! the board runtime can drive it beside every other chip backend.
//!
//! # The part
//!
//! * **Core**: every opcode the MCS-51 defines, all five addressing modes,
//!   bit-addressable RAM at `0x20..=0x2F` and bit-addressable SFRs, the
//!   carry/auxiliary-carry/overflow flags, and the full `PSW` bank select.
//! * **Memory**: 8 KiB of code, 256 B of internal RAM (the upper 128 bytes are
//!   indirect-only, as on the part), and 64 KiB of zero-initialised XDATA.
//! * **SFRs**: `ACC`, `B`, `PSW`, `SP`, `DPL`/`DPH`, the STC dual data pointer
//!   (`DPL1`/`DPH1` and `DPS` in `AUXR1`), `P0`–`P3`, `TCON`, `TMOD`, Timer 2's
//!   `T2CON`/`TH2`/`TL2`/`RCAP2L`/`RCAP2H`, `SCON`/`SBUF`, `PCON`, `IE`, `IP`,
//!   `AUXR`, and plain storage for the auxiliary registers.
//! * **Timers**: Timer 0/1 in modes 0/1/2 (and Timer 0 mode 3), with the
//!   classic 12-oscillator-clock machine cycle, overflow flags, external
//!   counter mode on `T0`/`T1`, and gate mode. Timer 2 supports auto-reload,
//!   capture, and baud-generator modes with `TF2`/`EXF2`/`EXEN2`.
//! * **UART**: modes 0–3, `TI`/`RI`, mode 1/3 baud from Timer 1 (`SMOD`) or
//!   Timer 2, mode 2's fixed baud, frame timing measured in simulated cycles,
//!   and a receive queue fed by [`Machine::write_console`].
//! * **Interrupts**: vectors `0x0003`, `0x000B`, `0x0013`, `0x001B`, `0x0023`
//!   and `0x002B`; `IE`/`IP` gating; the documented same-level polling order;
//!   hardware flag clearing where the part does it; and the priority latch
//!   restored by `RETI`.
//! * **INT0/INT1** in falling-edge and low-level modes.
//!
//! # Clock and cycles
//!
//! The STC89C52RC here runs at 11.0592 MHz with the classic 12T machine cycle:
//! one machine cycle is twelve oscillator clocks. [`Machine::cycles`] counts
//! oscillator clocks, which is also the unit [`litz_board::Chip::run`] budgets
//! in. [`Machine::step`] returns the machine cycles an instruction retired.
//!
//! # GPIO numbering
//!
//! Board GPIO numbers are `port * 8 + bit`, so `P0.0` is GPIO 0, `P1.0` is
//! GPIO 8, `P3.2` (`INT0`) is GPIO 26 and `P3.7` is GPIO 31. [`Machine::set_input`]
//! and [`Machine::gpio_high`] use the same numbering, as does the board
//! adapter. The special pins are:
//!
//! | Pin | GPIO | Port bit |
//! |-----|------|----------|
//! | `RXD` | 24 | `P3.0` |
//! | `TXD` | 25 | `P3.1` |
//! | `INT0` | 26 | `P3.2` |
//! | `INT1` | 27 | `P3.3` |
//! | `T0` | 28 | `P3.4` |
//! | `T1` | 29 | `P3.5` |
//! | `WR` | 30 | `P3.6` |
//! | `RD` | 31 | `P3.7` |
//! | `T2` | 8 | `P1.0` |
//! | `T2EX` | 9 | `P1.1` |
//!
//! # Debug address map
//!
//! [`litz_board::Chip::debug_read_memory`] is addressed as:
//!
//! * `0x0000..0x1FFF` — code flash, the same addresses the program counter uses.
//! * `0x2000..0x20FF` — internal RAM.
//! * `0x1_0000..0x1_FFFF` — XDATA.
//!
//! [`litz_board::Chip::debug_register_file`] is a fixed 33-word mapping:
//! `ACC`, `B`, `DPL`, `DPH`, `SP`, `PSW`, `DPL1`, `DPH1`, `DPS` in slots 0–8;
//! the currently selected register bank's `R0..R7` in slots 9–16; zeros in
//! slots 17–31; and the program counter in slot 32.
#![forbid(unsafe_code)]

pub mod chip;
pub mod disasm;
pub mod hex;
pub mod machine;

pub use chip::{CPU_HZ, Stc89c52};
pub use disasm::disassemble;
pub use hex::{HexError, load_hex, looks_like_hex, parse_hex};
pub use machine::{
    CODE_SIZE, DEBUG_IRAM_BASE, DEBUG_XDATA_BASE, Fault, INT0_GPIO, INT1_GPIO, IRAM_SIZE,
    LoadError, Machine, OSCILLATOR_CYCLES_PER_MACHINE, Stop, T0_GPIO, T1_GPIO, T2_GPIO, T2EX_GPIO,
    VECTORS, XDATA_SIZE, ie, ip, parity, psw, scon, sfr, t2con, tcon,
};
