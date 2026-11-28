//! An STC89C52RC: the 8051 core, its memories, SFRs, timers, serial port and
//! interrupt controller.
//!
//! Program addresses are code addresses everywhere in this module. A fault
//! reports the code address of the opcode or the bit that could not be
//! accessed, never a silent NOP.

use std::collections::VecDeque;
use std::fmt;

use litz_core::{Drive, Level};

use crate::hex::{self, HexError};

/// The STC89C52RC's oscillator frequency on the classic 11.0592 MHz crystal.
pub const CPU_HZ: u64 = 11_059_200;
/// Oscillator clocks in one classic 12T machine cycle.
pub const OSCILLATOR_CYCLES_PER_MACHINE: u64 = 12;
/// Code flash size in bytes.
pub const CODE_SIZE: usize = 8192;
/// Internal RAM size in bytes, including the indirect-only upper half.
pub const IRAM_SIZE: usize = 256;
/// External data memory size in bytes.
pub const XDATA_SIZE: usize = 65536;
/// Debug address where internal RAM starts, for [`crate::Stc89c52`]'s
/// `debug_read_memory`.
pub const DEBUG_IRAM_BASE: u32 = 0x2000;
/// Debug address where XDATA starts.
pub const DEBUG_XDATA_BASE: u32 = 0x1_0000;

/// `INT0` is `P3.2`.
pub const INT0_GPIO: u8 = 26;
/// `INT1` is `P3.3`.
pub const INT1_GPIO: u8 = 27;
/// `T0` is `P3.4`.
pub const T0_GPIO: u8 = 28;
/// `T1` is `P3.5`.
pub const T1_GPIO: u8 = 29;
/// `T2` is `P1.0`.
pub const T2_GPIO: u8 = 8;
/// `T2EX` is `P1.1`.
pub const T2EX_GPIO: u8 = 9;

/// The special function registers this part implements.
pub mod sfr {
    pub const P0: u8 = 0x80;
    pub const SP: u8 = 0x81;
    pub const DPL: u8 = 0x82;
    pub const DPH: u8 = 0x83;
    pub const DPL1: u8 = 0x84;
    pub const DPH1: u8 = 0x85;
    pub const PCON: u8 = 0x87;
    pub const TCON: u8 = 0x88;
    pub const TMOD: u8 = 0x89;
    pub const TL0: u8 = 0x8A;
    pub const TL1: u8 = 0x8B;
    pub const TH0: u8 = 0x8C;
    pub const TH1: u8 = 0x8D;
    pub const AUXR: u8 = 0x8E;
    pub const P1: u8 = 0x90;
    pub const SCON: u8 = 0x98;
    pub const SBUF: u8 = 0x99;
    pub const P2: u8 = 0xA0;
    pub const AUXR1: u8 = 0xA2;
    pub const IE: u8 = 0xA8;
    pub const P3: u8 = 0xB0;
    pub const IP: u8 = 0xB8;
    pub const T2CON: u8 = 0xC8;
    pub const RCAP2L: u8 = 0xCA;
    pub const RCAP2H: u8 = 0xCB;
    pub const TL2: u8 = 0xCC;
    pub const TH2: u8 = 0xCD;
    pub const PSW: u8 = 0xD0;
    pub const ACC: u8 = 0xE0;
    pub const B: u8 = 0xF0;
}

/// `PSW` bit masks.
pub mod psw {
    pub const CY: u8 = 0x80;
    pub const AC: u8 = 0x40;
    pub const F0: u8 = 0x20;
    pub const RS1: u8 = 0x10;
    pub const RS0: u8 = 0x08;
    pub const OV: u8 = 0x04;
    pub const F1: u8 = 0x02;
    pub const P: u8 = 0x01;
}

/// `TCON` bit masks.
pub mod tcon {
    pub const IT0: u8 = 0x01;
    pub const IE0: u8 = 0x02;
    pub const IT1: u8 = 0x04;
    pub const IE1: u8 = 0x08;
    pub const TR0: u8 = 0x10;
    pub const TF0: u8 = 0x20;
    pub const TR1: u8 = 0x40;
    pub const TF1: u8 = 0x80;
}

/// `T2CON` bit masks.
pub mod t2con {
    pub const TF2: u8 = 0x80;
    pub const EXF2: u8 = 0x40;
    pub const RCLK: u8 = 0x20;
    pub const TCLK: u8 = 0x10;
    pub const EXEN2: u8 = 0x08;
    pub const TR2: u8 = 0x04;
    pub const CT2: u8 = 0x02;
    pub const CPRL2: u8 = 0x01;
}

/// `SCON` bit masks.
pub mod scon {
    pub const SM0: u8 = 0x80;
    pub const SM1: u8 = 0x40;
    pub const SM2: u8 = 0x20;
    pub const REN: u8 = 0x10;
    pub const TB8: u8 = 0x08;
    pub const RB8: u8 = 0x04;
    pub const TI: u8 = 0x02;
    pub const RI: u8 = 0x01;
}

/// `IE` bit masks.
pub mod ie {
    pub const EX0: u8 = 0x01;
    pub const ET0: u8 = 0x02;
    pub const EX1: u8 = 0x04;
    pub const ET1: u8 = 0x08;
    pub const ES: u8 = 0x10;
    pub const ET2: u8 = 0x20;
    pub const EA: u8 = 0x80;
}

/// `IP` bit masks.
pub mod ip {
    pub const PX0: u8 = 0x01;
    pub const PT0: u8 = 0x02;
    pub const PX1: u8 = 0x04;
    pub const PT1: u8 = 0x08;
    pub const PS: u8 = 0x10;
    pub const PT2: u8 = 0x20;
}

/// Interrupt vectors, in polling order.
pub const VECTORS: [u16; 6] = [0x0003, 0x000B, 0x0013, 0x001B, 0x0023, 0x002B];

/// Whether an SFR at `addr` is bit addressable on this part.
pub fn is_bit_addressable_sfr(addr: u8) -> bool {
    matches!(
        addr,
        0x80 | 0x88 | 0x90 | 0x98 | 0xA0 | 0xA8 | 0xB0 | 0xB8 | 0xC8 | 0xD0 | 0xE0 | 0xF0
    )
}

/// A trap raised by the core instead of a panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The program counter left the 8 KiB code array.
    CodeOutOfBounds { addr: u32 },
    /// An XDATA access left the 64 KiB space. `MOVX` cannot produce this, but
    /// a host poke can.
    XdataOutOfBounds { addr: u32 },
    /// A bit operand named an address that is not a bit of RAM or a
    /// bit-addressable SFR.
    InvalidBit { bit: u8 },
    /// An opcode the MCS-51 does not define (`0xA5` on this part).
    UnknownOpcode { addr: u32, opcode: u8 },
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::CodeOutOfBounds { addr } => {
                write!(f, "code fetch outside 8 KiB at {addr:#06x}")
            }
            Fault::XdataOutOfBounds { addr } => {
                write!(f, "XDATA access outside 64 KiB at {addr:#06x}")
            }
            Fault::InvalidBit { bit } => write!(f, "bit address {bit:#04x} is not addressable"),
            Fault::UnknownOpcode { addr, opcode } => {
                write!(f, "unknown opcode {opcode:#04x} at {addr:#06x}")
            }
        }
    }
}

impl std::error::Error for Fault {}

/// Why [`Machine::run`] came back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The oscillator-cycle budget was spent.
    Budget,
    /// The core is in idle or power-down mode and no enabled interrupt can
    /// wake it.
    Idle,
    /// The core trapped; the detail is in [`Machine::fault`].
    Trap,
}

/// An image could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The Intel HEX text was malformed.
    Hex(HexError),
    /// A raw binary ran past the end of code flash.
    BinaryOutOfRange { offset: u64, length: usize },
    /// A HEX record addressed outside the 8 KiB code array.
    NotInFlash,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Hex(error) => write!(f, "{error}"),
            LoadError::BinaryOutOfRange { offset, length } => write!(
                f,
                "binary of {length} bytes at {offset:#x} does not fit in {CODE_SIZE} bytes of code"
            ),
            LoadError::NotInFlash => write!(f, "the image does not fit in the 8 KiB code array"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<HexError> for LoadError {
    fn from(error: HexError) -> Self {
        LoadError::Hex(error)
    }
}

/// One port write edge, with the drive strength the port presents.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PortEdge {
    gpio: u8,
    high: bool,
    drive: Drive,
}

/// A pending interrupt source selected by [`Machine::pending_interrupt`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    /// 0 INT0, 1 T0, 2 INT1, 3 T1, 4 serial, 5 T2.
    source: u8,
    vector: u16,
    high: bool,
}

/// One STC89C52RC.
pub struct Machine {
    code: [u8; CODE_SIZE],
    iram: [u8; IRAM_SIZE],
    xdata: Vec<u8>,
    /// The SFR file, indexed by `addr - 0x80`.
    sfr: [u8; 128],
    pc: u16,
    /// Oscillator clocks retired since reset.
    oscillator: u64,
    fault: Option<Fault>,
    /// What an external driver holds on each of the 32 port pins. `true` is
    /// high, the released/weak-pull-up state.
    external: [bool; 32],
    outputs: Vec<PortEdge>,
    console: Vec<u8>,
    rx_queue: VecDeque<u8>,
    rx_buffer: u8,
    tx_busy: bool,
    tx_byte: u8,
    tx_accumulator: u64,
    tx_period: u64,
    tx_bits: u8,
    tx_bit_index: u8,
    tx_queue: VecDeque<u8>,
    rx_busy: bool,
    rx_byte: u8,
    rx_accumulator: u64,
    rx_period: u64,
    rx_bits: u8,
    rx_bit_index: u8,
    /// Priority level of each active ISR, youngest last.
    isr_levels: [u8; 8],
    isr_depth: usize,
    /// Whether an ISR at each priority level is in service.
    active: [bool; 2],
    breakpoints: Vec<u16>,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    /// A machine at reset with erased (0xFF) code and zeroed RAM.
    pub fn new() -> Self {
        let mut machine = Self {
            code: [0xFF; CODE_SIZE],
            iram: [0; IRAM_SIZE],
            xdata: vec![0; XDATA_SIZE],
            sfr: [0; 128],
            pc: 0,
            oscillator: 0,
            fault: None,
            external: [true; 32],
            outputs: Vec::new(),
            console: Vec::new(),
            rx_queue: VecDeque::new(),
            rx_buffer: 0,
            tx_busy: false,
            tx_byte: 0,
            tx_accumulator: 0,
            tx_period: 12,
            tx_bits: 8,
            tx_bit_index: 0,
            tx_queue: VecDeque::new(),
            rx_busy: false,
            rx_byte: 0,
            rx_accumulator: 0,
            rx_period: 12,
            rx_bits: 8,
            rx_bit_index: 0,
            isr_levels: [0; 8],
            isr_depth: 0,
            active: [false; 2],
            breakpoints: Vec::new(),
        };
        machine.reset_registers();
        machine
    }

    /// Reset registers and peripherals. Code flash survives.
    pub fn reset(&mut self) {
        self.iram = [0; IRAM_SIZE];
        self.xdata.fill(0);
        self.sfr = [0; 128];
        self.pc = 0;
        self.oscillator = 0;
        self.fault = None;
        self.external = [true; 32];
        self.outputs.clear();
        self.console.clear();
        self.rx_queue.clear();
        self.rx_buffer = 0;
        self.tx_busy = false;
        self.tx_accumulator = 0;
        self.tx_bit_index = 0;
        self.tx_queue.clear();
        self.rx_busy = false;
        self.rx_accumulator = 0;
        self.rx_bit_index = 0;
        self.isr_depth = 0;
        self.active = [false; 2];
        self.reset_registers();
    }

    /// Point the core at the reset vector and put the peripherals back to
    /// their reset state. Code and breakpoints survive.
    pub fn boot(&mut self) {
        self.reset();
    }

    fn reset_registers(&mut self) {
        // Quasi-bidirectional ports come out of reset with every latch high.
        for port in 0..4u8 {
            self.sfr[usize::from(port) * 0x10] = 0xFF;
        }
        self.sfr[usize::from(sfr::SP - 0x80)] = 0x07;
        self.sfr[usize::from(sfr::PCON - 0x80)] = 0;
    }

    // -- memory and loads --------------------------------------------------

    pub fn code(&self) -> &[u8; CODE_SIZE] {
        &self.code
    }

    pub fn code_mut(&mut self) -> &mut [u8; CODE_SIZE] {
        &mut self.code
    }

    pub fn iram(&self) -> &[u8; IRAM_SIZE] {
        &self.iram
    }

    pub fn xdata(&self) -> &[u8] {
        &self.xdata
    }

    /// Load Intel HEX text into code flash.
    pub fn load_hex(&mut self, text: &str) -> Result<(), LoadError> {
        let image = hex::parse_hex(text)?;
        if image.len() > CODE_SIZE {
            return Err(LoadError::NotInFlash);
        }
        self.code[..image.len()].copy_from_slice(&image);
        Ok(())
    }

    /// Load a raw binary at a byte offset into code flash.
    pub fn load_bin(&mut self, bytes: &[u8], offset: u64) -> Result<(), LoadError> {
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let end = start.checked_add(bytes.len());
        match end {
            Some(end) if end <= CODE_SIZE => {
                self.code[start..end].copy_from_slice(bytes);
                Ok(())
            }
            _ => Err(LoadError::BinaryOutOfRange {
                offset,
                length: bytes.len(),
            }),
        }
    }

    /// Load either format: Intel HEX is detected from its first non-blank
    /// byte, a raw binary is placed at `offset`.
    pub fn load_image(&mut self, bytes: &[u8], offset: u64) -> Result<(), LoadError> {
        if hex::looks_like_hex(bytes) {
            let text = core::str::from_utf8(bytes)
                .map_err(|_| LoadError::Hex(HexError::NotHex { line: 1 }))?;
            self.load_hex(text)
        } else {
            self.load_bin(bytes, offset)
        }
    }

    // -- registers and clocks ---------------------------------------------

    pub fn pc(&self) -> u16 {
        self.pc
    }

    pub fn set_pc(&mut self, pc: u16) {
        self.pc = pc;
    }

    /// Oscillator clocks since reset.
    pub fn cycles(&self) -> u64 {
        self.oscillator
    }

    /// Machine cycles since reset.
    pub fn machine_cycles(&self) -> u64 {
        self.oscillator / OSCILLATOR_CYCLES_PER_MACHINE
    }

    pub fn fault(&self) -> Option<Fault> {
        self.fault
    }

    pub fn acc(&self) -> u8 {
        self.sfr[usize::from(sfr::ACC - 0x80)]
    }

    pub fn set_acc(&mut self, value: u8) {
        self.sfr[usize::from(sfr::ACC - 0x80)] = value;
    }

    pub fn b(&self) -> u8 {
        self.sfr[usize::from(sfr::B - 0x80)]
    }

    pub fn set_b(&mut self, value: u8) {
        self.sfr[usize::from(sfr::B - 0x80)] = value;
    }

    pub fn sp(&self) -> u8 {
        self.sfr[usize::from(sfr::SP - 0x80)]
    }

    pub fn set_sp(&mut self, value: u8) {
        self.sfr[usize::from(sfr::SP - 0x80)] = value;
    }

    /// `PSW` with the parity bit computed from `ACC`.
    pub fn psw(&self) -> u8 {
        (self.sfr[usize::from(sfr::PSW - 0x80)] & !psw::P) | parity(self.acc())
    }

    pub fn set_psw(&mut self, value: u8) {
        self.sfr[usize::from(sfr::PSW - 0x80)] = value & !psw::P;
    }

    pub fn carry(&self) -> bool {
        self.sfr[usize::from(sfr::PSW - 0x80)] & psw::CY != 0
    }

    pub fn auxiliary_carry(&self) -> bool {
        self.sfr[usize::from(sfr::PSW - 0x80)] & psw::AC != 0
    }

    pub fn overflow(&self) -> bool {
        self.sfr[usize::from(sfr::PSW - 0x80)] & psw::OV != 0
    }

    /// The selected register bank, 0–3.
    pub fn bank(&self) -> u8 {
        (self.sfr[usize::from(sfr::PSW - 0x80)] >> 3) & 0x03
    }

    /// `Rn` in the selected bank.
    pub fn register(&self, n: u8) -> u8 {
        self.iram[usize::from(self.bank() * 8 + (n & 7))]
    }

    pub fn set_register(&mut self, n: u8, value: u8) {
        self.iram[usize::from(self.bank() * 8 + (n & 7))] = value;
    }

    /// The active data pointer, selected by `DPS` in `AUXR1`.
    pub fn dptr(&self) -> u16 {
        if self.sfr[usize::from(sfr::AUXR1 - 0x80)] & 0x01 != 0 {
            (u16::from(self.sfr[usize::from(sfr::DPH1 - 0x80)]) << 8)
                | u16::from(self.sfr[usize::from(sfr::DPL1 - 0x80)])
        } else {
            (u16::from(self.sfr[usize::from(sfr::DPH - 0x80)]) << 8)
                | u16::from(self.sfr[usize::from(sfr::DPL - 0x80)])
        }
    }

    pub fn set_dptr(&mut self, value: u16) {
        if self.sfr[usize::from(sfr::AUXR1 - 0x80)] & 0x01 != 0 {
            self.sfr[usize::from(sfr::DPH1 - 0x80)] = (value >> 8) as u8;
            self.sfr[usize::from(sfr::DPL1 - 0x80)] = value as u8;
        } else {
            self.sfr[usize::from(sfr::DPH - 0x80)] = (value >> 8) as u8;
            self.sfr[usize::from(sfr::DPL - 0x80)] = value as u8;
        }
    }

    /// `DPS` (the dual-data-pointer selector, `AUXR1` bit 0).
    pub fn dps(&self) -> u8 {
        self.sfr[usize::from(sfr::AUXR1 - 0x80)] & 0x01
    }

    // -- direct and indirect memory ---------------------------------------

    /// Read a direct data address. `rmw` selects the port latch instead of the
    /// pin, which is what the 8051's read-modify-write instructions do.
    pub fn read_direct(&self, addr: u8, rmw: bool) -> u8 {
        if addr < 0x80 {
            return self.iram[usize::from(addr)];
        }
        match addr {
            sfr::P0 | sfr::P1 | sfr::P2 | sfr::P3 => {
                let latch = self.sfr[usize::from(addr - 0x80)];
                if rmw {
                    return latch;
                }
                let port = (addr - sfr::P0) / 0x10;
                let mut out = 0u8;
                for bit in 0..8 {
                    let mask = 1u8 << bit;
                    if latch & mask != 0 && self.external[usize::from(port * 8 + bit)] {
                        out |= mask;
                    }
                }
                out
            }
            sfr::SBUF => self.rx_buffer,
            sfr::PSW => self.psw(),
            _ => self.sfr[usize::from(addr - 0x80)],
        }
    }

    /// Write a direct data address, with the side effects of the ports,
    /// `SBUF`, `PSW` and `AUXR1`.
    pub fn write_direct(&mut self, addr: u8, value: u8) {
        if addr < 0x80 {
            self.iram[usize::from(addr)] = value;
            return;
        }
        match addr {
            sfr::P0 | sfr::P1 | sfr::P2 | sfr::P3 => {
                let port = (addr - sfr::P0) / 0x10;
                self.write_port(port, value);
            }
            sfr::SBUF => self.start_transmit(value),
            sfr::PSW => self.sfr[usize::from(addr - 0x80)] = value & !psw::P,
            sfr::AUXR1 => self.sfr[usize::from(addr - 0x80)] = value & 0x01,
            sfr::TCON => {
                self.sfr[usize::from(addr - 0x80)] = value;
                self.refresh_level_interrupts();
            }
            _ => self.sfr[usize::from(addr - 0x80)] = value,
        }
    }

    /// Read an SFR as a program would (`PSW` carries parity, ports read pins).
    pub fn read_sfr(&self, addr: u8) -> u8 {
        self.read_direct(addr, false)
    }

    /// Write an SFR as a program would.
    pub fn write_sfr(&mut self, addr: u8, value: u8) {
        self.write_direct(addr, value);
    }

    pub fn read_iram(&self, addr: u8) -> u8 {
        self.iram[usize::from(addr)]
    }

    pub fn write_iram(&mut self, addr: u8, value: u8) {
        self.iram[usize::from(addr)] = value;
    }

    pub fn read_xdata(&self, addr: u16) -> u8 {
        self.xdata[usize::from(addr)]
    }

    pub fn write_xdata(&mut self, addr: u16, value: u8) {
        self.xdata[usize::from(addr)] = value;
    }

    // -- ports -------------------------------------------------------------

    fn write_port(&mut self, port: u8, value: u8) {
        let index = usize::from(port) * 0x10;
        let old = self.sfr[index];
        if old == value {
            return;
        }
        self.sfr[index] = value;
        for bit in 0..8 {
            let mask = 1u8 << bit;
            if (old ^ value) & mask != 0 {
                let high = value & mask != 0;
                let drive = if high {
                    if port == 0 {
                        // P0 has no internal pull-up.
                        Drive::HIGH_Z
                    } else {
                        Drive::weak(Level::High)
                    }
                } else {
                    Drive::strong(Level::Low)
                };
                self.outputs.push(PortEdge {
                    gpio: port * 8 + bit,
                    high,
                    drive,
                });
            }
        }
        if port == 3 {
            self.refresh_level_interrupts();
        }
    }

    /// Set the level an external driver holds on `gpio`.
    pub fn set_input(&mut self, gpio: u8, high: bool) {
        if gpio >= 32 {
            return;
        }
        let old_pin = self.gpio_high(gpio);
        self.external[usize::from(gpio)] = high;
        let new_pin = self.gpio_high(gpio);
        if old_pin && !new_pin {
            self.on_falling_edge(gpio);
        }
        self.refresh_level_interrupts();
    }

    /// The level the chip sees on `gpio`: the latch ANDed with the external
    /// level, which is what a quasi-bidirectional pin reads back.
    pub fn gpio_high(&self, gpio: u8) -> bool {
        if gpio >= 32 {
            return false;
        }
        let port = gpio / 8;
        let bit = gpio % 8;
        let latch = self.sfr[usize::from(port) * 0x10];
        latch & (1u8 << bit) != 0 && self.external[usize::from(gpio)]
    }

    /// The external level on `gpio`, ignoring the latch.
    pub fn external_high(&self, gpio: u8) -> bool {
        gpio < 32 && self.external[usize::from(gpio)]
    }

    /// Every port pin's current drive, for seeding a board.
    pub fn port_drives(&self) -> Vec<(u8, Drive)> {
        let mut drives = Vec::with_capacity(32);
        for port in 0..4u8 {
            let latch = self.sfr[usize::from(port) * 0x10];
            for bit in 0..8 {
                let high = latch & (1u8 << bit) != 0;
                let drive = if high {
                    if port == 0 {
                        Drive::HIGH_Z
                    } else {
                        Drive::weak(Level::High)
                    }
                } else {
                    Drive::strong(Level::Low)
                };
                drives.push((port * 8 + bit, drive));
            }
        }
        drives
    }

    /// Output edges since the last call, as `(gpio, high)`.
    pub fn take_outputs(&mut self) -> Vec<(u8, bool)> {
        std::mem::take(&mut self.outputs)
            .into_iter()
            .map(|edge| (edge.gpio, edge.high))
            .collect()
    }

    /// Output edges since the last call, with their drive strength.
    pub fn take_output_drives(&mut self) -> Vec<(u8, Drive)> {
        std::mem::take(&mut self.outputs)
            .into_iter()
            .map(|edge| (edge.gpio, edge.drive))
            .collect()
    }

    /// Discard any queued port edges.
    pub fn clear_outputs(&mut self) {
        self.outputs.clear();
    }

    // -- console and serial ------------------------------------------------

    /// Queue bytes for the serial receiver. They are only accepted while
    /// `REN` is set.
    pub fn write_console(&mut self, bytes: &[u8]) {
        for byte in bytes {
            if self.rx_queue.len() < 4096 {
                self.rx_queue.push_back(*byte);
            }
        }
    }

    /// Bytes the firmware has transmitted, drained.
    pub fn take_console(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.console)
    }

    /// The last byte the serial receiver wrote to `SBUF`.
    pub fn sbuf(&self) -> u8 {
        self.rx_buffer
    }

    fn start_transmit(&mut self, byte: u8) {
        if self.tx_busy {
            if self.tx_queue.len() < 1024 {
                self.tx_queue.push_back(byte);
            }
            return;
        }
        self.tx_busy = true;
        self.tx_byte = byte;
        self.tx_accumulator = 0;
        self.tx_period = self.uart_period(true);
        self.tx_bits = self.uart_frame_bits();
        self.tx_bit_index = 0;
    }

    fn finish_transmit(&mut self) {
        self.tx_busy = false;
        self.console.push(self.tx_byte);
        let index = usize::from(sfr::SCON - 0x80);
        self.sfr[index] |= scon::TI;
        if let Some(next) = self.tx_queue.pop_front() {
            self.start_transmit(next);
        }
    }

    fn uart_mode(&self) -> u8 {
        (self.sfr[usize::from(sfr::SCON - 0x80)] >> 6) & 0x03
    }

    fn uart_frame_bits(&self) -> u8 {
        match self.uart_mode() {
            0 => 8,
            1 => 10,
            _ => 11,
        }
    }

    fn uart_period(&self, transmit: bool) -> u64 {
        let smod = self.sfr[usize::from(sfr::PCON - 0x80)] & 0x80 != 0;
        match self.uart_mode() {
            0 => 12,
            2 => {
                if smod {
                    32
                } else {
                    64
                }
            }
            _ => {
                let t2 = self.sfr[usize::from(sfr::T2CON - 0x80)];
                let use_timer2 = if transmit {
                    t2 & t2con::TCLK != 0
                } else {
                    t2 & t2con::RCLK != 0
                };
                if use_timer2 {
                    let reload = (u16::from(self.sfr[usize::from(sfr::RCAP2H - 0x80)]) << 8)
                        | u16::from(self.sfr[usize::from(sfr::RCAP2L - 0x80)]);
                    32 * (65536 - u64::from(reload))
                } else {
                    let th1 = u64::from(self.sfr[usize::from(sfr::TH1 - 0x80)]);
                    let base = 384 * (256 - th1.min(255));
                    if smod { base / 2 } else { base }
                }
            }
        }
        .max(1)
    }

    fn tick_serial(&mut self) {
        if self.tx_busy {
            self.tx_accumulator += OSCILLATOR_CYCLES_PER_MACHINE;
            while self.tx_busy && self.tx_accumulator >= self.tx_period {
                self.tx_accumulator -= self.tx_period;
                self.tx_bit_index += 1;
                if self.tx_bit_index >= self.tx_bits {
                    self.finish_transmit();
                }
            }
        }

        let scon = self.sfr[usize::from(sfr::SCON - 0x80)];
        if !self.rx_busy
            && scon & scon::REN != 0
            && scon & scon::RI == 0
            && !self.rx_queue.is_empty()
        {
            let byte = self.rx_queue.pop_front().unwrap_or(0);
            self.rx_busy = true;
            self.rx_byte = byte;
            self.rx_accumulator = 0;
            self.rx_period = self.uart_period(false);
            self.rx_bits = self.uart_frame_bits();
            self.rx_bit_index = 0;
        }

        if self.rx_busy {
            self.rx_accumulator += OSCILLATOR_CYCLES_PER_MACHINE;
            while self.rx_busy && self.rx_accumulator >= self.rx_period {
                self.rx_accumulator -= self.rx_period;
                self.rx_bit_index += 1;
                if self.rx_bit_index >= self.rx_bits {
                    self.finish_receive();
                }
            }
        }
    }

    fn finish_receive(&mut self) {
        self.rx_busy = false;
        self.rx_buffer = self.rx_byte;
        let index = usize::from(sfr::SCON - 0x80);
        self.sfr[index] |= scon::RB8;
        let mode = self.uart_mode();
        let sm2 = self.sfr[index] & scon::SM2 != 0;
        let rb8 = self.sfr[index] & scon::RB8 != 0;
        if !(mode >= 2 && sm2 && !rb8) {
            self.sfr[index] |= scon::RI;
        }
    }

    // -- interrupts --------------------------------------------------------

    fn tcon_byte(&self) -> u8 {
        self.sfr[usize::from(sfr::TCON - 0x80)]
    }

    fn t2con_byte(&self) -> u8 {
        self.sfr[usize::from(sfr::T2CON - 0x80)]
    }

    fn scn_byte(&self) -> u8 {
        self.sfr[usize::from(sfr::SCON - 0x80)]
    }

    fn idle_flag(&self) -> bool {
        self.sfr[usize::from(sfr::PCON - 0x80)] & 0x01 != 0
    }

    fn power_down_flag(&self) -> bool {
        self.sfr[usize::from(sfr::PCON - 0x80)] & 0x02 != 0
    }

    fn interrupts_enabled(&self) -> bool {
        self.sfr[usize::from(sfr::IE - 0x80)] & ie::EA != 0
    }

    /// Whether the core is stopped and no enabled interrupt can wake it.
    pub fn is_idle(&self) -> bool {
        if !(self.idle_flag() || self.power_down_flag()) {
            return false;
        }
        let enabled = self.sfr[usize::from(sfr::IE - 0x80)];
        if enabled & ie::EA == 0 {
            return true;
        }
        if self.power_down_flag() {
            // Power-down stops the oscillator, so only INT0/INT1 can wake it.
            return enabled & (ie::EX0 | ie::EX1) == 0;
        }
        enabled & 0x3F == 0
    }

    fn pending_interrupt(&self) -> Option<Pending> {
        let enabled = self.sfr[usize::from(sfr::IE - 0x80)];
        if enabled & ie::EA == 0 {
            return None;
        }
        // A high-priority ISR is never preempted.
        if self.active[1] {
            return None;
        }
        let ip = self.sfr[usize::from(sfr::IP - 0x80)];
        let tcon = self.tcon_byte();
        let t2 = self.t2con_byte();
        let scon = self.scn_byte();
        let flags = [
            tcon & tcon::IE0 != 0,
            tcon & tcon::TF0 != 0,
            tcon & tcon::IE1 != 0,
            tcon & tcon::TF1 != 0,
            scon & (scon::RI | scon::TI) != 0,
            t2 & (t2con::TF2 | t2con::EXF2) != 0,
        ];
        let enables = [ie::EX0, ie::ET0, ie::EX1, ie::ET1, ie::ES, ie::ET2];
        let priorities = [ip::PX0, ip::PT0, ip::PX1, ip::PT1, ip::PS, ip::PT2];

        let low_active = self.active[0];
        for high_pass in [true, false] {
            if !high_pass && low_active {
                // A low-priority ISR blocks its own level.
                continue;
            }
            for (index, (&flag, &enable)) in flags.iter().zip(enables.iter()).enumerate() {
                if !flag || enabled & enable == 0 {
                    continue;
                }
                let high = ip & priorities[index] != 0;
                if high == high_pass {
                    return Some(Pending {
                        source: index as u8,
                        vector: VECTORS[index],
                        high,
                    });
                }
            }
        }
        None
    }

    fn service_interrupt(&mut self, pending: Pending) -> u8 {
        self.push_pc();
        self.pc = pending.vector;
        let level = usize::from(pending.high);
        self.active[level] = true;
        if self.isr_depth < self.isr_levels.len() {
            self.isr_levels[self.isr_depth] = pending.high as u8;
            self.isr_depth += 1;
        }
        self.clear_interrupt_flag(pending.source);
        2
    }

    fn clear_interrupt_flag(&mut self, source: u8) {
        let index = usize::from(sfr::TCON - 0x80);
        match source {
            0 => {
                if self.tcon_byte() & tcon::IT0 != 0 {
                    self.sfr[index] &= !tcon::IE0;
                }
            }
            1 => self.sfr[index] &= !tcon::TF0,
            2 => {
                if self.tcon_byte() & tcon::IT1 != 0 {
                    self.sfr[index] &= !tcon::IE1;
                }
            }
            3 => self.sfr[index] &= !tcon::TF1,
            // Serial and Timer 2 flags are cleared by software.
            _ => {}
        }
    }

    /// Make level-triggered external interrupt flags follow their pins.
    fn refresh_level_interrupts(&mut self) {
        let index = usize::from(sfr::TCON - 0x80);
        let mut tcon = self.sfr[index];
        if tcon & tcon::IT0 == 0 {
            if self.gpio_high(INT0_GPIO) {
                tcon &= !tcon::IE0;
            } else {
                tcon |= tcon::IE0;
            }
        }
        if tcon & tcon::IT1 == 0 {
            if self.gpio_high(INT1_GPIO) {
                tcon &= !tcon::IE1;
            } else {
                tcon |= tcon::IE1;
            }
        }
        self.sfr[index] = tcon;
    }

    fn on_falling_edge(&mut self, gpio: u8) {
        match gpio {
            INT0_GPIO => {
                let index = usize::from(sfr::TCON - 0x80);
                if self.sfr[index] & tcon::IT0 != 0 {
                    self.sfr[index] |= tcon::IE0;
                }
            }
            INT1_GPIO => {
                let index = usize::from(sfr::TCON - 0x80);
                if self.sfr[index] & tcon::IT1 != 0 {
                    self.sfr[index] |= tcon::IE1;
                }
            }
            T0_GPIO => {
                let tmod = self.sfr[usize::from(sfr::TMOD - 0x80)];
                if tmod & 0x04 != 0 && self.timer0_clocked() {
                    self.bump_timer0(tmod & 0x03);
                }
            }
            T1_GPIO => {
                let tmod = self.sfr[usize::from(sfr::TMOD - 0x80)];
                let mode1 = (tmod >> 4) & 0x03;
                if tmod & 0x40 != 0 && mode1 != 3 && self.timer1_clocked() {
                    self.bump_timer1(mode1, tmod & 0x03 != 3);
                }
            }
            T2_GPIO => {
                let t2 = self.t2con_byte();
                if t2 & t2con::CT2 != 0 && t2 & t2con::TR2 != 0 {
                    self.bump_timer2();
                }
            }
            T2EX_GPIO => self.t2ex_event(),
            _ => {}
        }
    }

    // -- timers ------------------------------------------------------------

    fn timer0_clocked(&self) -> bool {
        let tmod = self.sfr[usize::from(sfr::TMOD - 0x80)];
        let tr0 = self.tcon_byte() & tcon::TR0 != 0;
        let gate = tmod & 0x08 != 0;
        tr0 && (!gate || self.gpio_high(INT0_GPIO))
    }

    fn timer1_clocked(&self) -> bool {
        let tmod = self.sfr[usize::from(sfr::TMOD - 0x80)];
        let tr1 = self.tcon_byte() & tcon::TR1 != 0;
        let gate = tmod & 0x80 != 0;
        tr1 && (!gate || self.gpio_high(INT1_GPIO))
    }

    fn set_tf0(&mut self) {
        self.sfr[usize::from(sfr::TCON - 0x80)] |= tcon::TF0;
    }

    fn set_tf1(&mut self) {
        self.sfr[usize::from(sfr::TCON - 0x80)] |= tcon::TF1;
    }

    fn set_tf2(&mut self) {
        self.sfr[usize::from(sfr::T2CON - 0x80)] |= t2con::TF2;
    }

    fn bump_timer0(&mut self, mode: u8) {
        match mode {
            0 => {
                let value = (u16::from(self.sfr[usize::from(sfr::TH0 - 0x80)]) << 5)
                    | u16::from(self.sfr[usize::from(sfr::TL0 - 0x80)] & 0x1F);
                let next = (value + 1) & 0x1FFF;
                if value == 0x1FFF {
                    self.set_tf0();
                }
                self.sfr[usize::from(sfr::TH0 - 0x80)] = (next >> 5) as u8;
                self.sfr[usize::from(sfr::TL0 - 0x80)] = (next & 0x1F) as u8;
            }
            1 => {
                let value = (u16::from(self.sfr[usize::from(sfr::TH0 - 0x80)]) << 8)
                    | u16::from(self.sfr[usize::from(sfr::TL0 - 0x80)]);
                let next = value.wrapping_add(1);
                if next == 0 {
                    self.set_tf0();
                }
                self.sfr[usize::from(sfr::TH0 - 0x80)] = (next >> 8) as u8;
                self.sfr[usize::from(sfr::TL0 - 0x80)] = next as u8;
            }
            2 => {
                let next = self.sfr[usize::from(sfr::TL0 - 0x80)].wrapping_add(1);
                self.sfr[usize::from(sfr::TL0 - 0x80)] = next;
                if next == 0 {
                    self.sfr[usize::from(sfr::TL0 - 0x80)] = self.sfr[usize::from(sfr::TH0 - 0x80)];
                    self.set_tf0();
                }
            }
            _ => {
                let next = self.sfr[usize::from(sfr::TL0 - 0x80)].wrapping_add(1);
                self.sfr[usize::from(sfr::TL0 - 0x80)] = next;
                if next == 0 {
                    self.set_tf0();
                }
            }
        }
    }

    fn bump_th0(&mut self) {
        let next = self.sfr[usize::from(sfr::TH0 - 0x80)].wrapping_add(1);
        self.sfr[usize::from(sfr::TH0 - 0x80)] = next;
        if next == 0 {
            self.set_tf1();
        }
    }

    fn bump_timer1(&mut self, mode: u8, set_flag: bool) {
        match mode {
            0 => {
                let value = (u16::from(self.sfr[usize::from(sfr::TH1 - 0x80)]) << 5)
                    | u16::from(self.sfr[usize::from(sfr::TL1 - 0x80)] & 0x1F);
                let next = (value + 1) & 0x1FFF;
                if value == 0x1FFF && set_flag {
                    self.set_tf1();
                }
                self.sfr[usize::from(sfr::TH1 - 0x80)] = (next >> 5) as u8;
                self.sfr[usize::from(sfr::TL1 - 0x80)] = (next & 0x1F) as u8;
            }
            1 => {
                let value = (u16::from(self.sfr[usize::from(sfr::TH1 - 0x80)]) << 8)
                    | u16::from(self.sfr[usize::from(sfr::TL1 - 0x80)]);
                let next = value.wrapping_add(1);
                if next == 0 && set_flag {
                    self.set_tf1();
                }
                self.sfr[usize::from(sfr::TH1 - 0x80)] = (next >> 8) as u8;
                self.sfr[usize::from(sfr::TL1 - 0x80)] = next as u8;
            }
            2 => {
                let next = self.sfr[usize::from(sfr::TL1 - 0x80)].wrapping_add(1);
                self.sfr[usize::from(sfr::TL1 - 0x80)] = next;
                if next == 0 {
                    self.sfr[usize::from(sfr::TL1 - 0x80)] = self.sfr[usize::from(sfr::TH1 - 0x80)];
                    if set_flag {
                        self.set_tf1();
                    }
                }
            }
            _ => {}
        }
    }

    fn timer2_value(&self) -> u16 {
        (u16::from(self.sfr[usize::from(sfr::TH2 - 0x80)]) << 8)
            | u16::from(self.sfr[usize::from(sfr::TL2 - 0x80)])
    }

    fn set_timer2(&mut self, value: u16) {
        self.sfr[usize::from(sfr::TH2 - 0x80)] = (value >> 8) as u8;
        self.sfr[usize::from(sfr::TL2 - 0x80)] = value as u8;
    }

    fn rcap2_value(&self) -> u16 {
        (u16::from(self.sfr[usize::from(sfr::RCAP2H - 0x80)]) << 8)
            | u16::from(self.sfr[usize::from(sfr::RCAP2L - 0x80)])
    }

    fn set_rcap2(&mut self, value: u16) {
        self.sfr[usize::from(sfr::RCAP2H - 0x80)] = (value >> 8) as u8;
        self.sfr[usize::from(sfr::RCAP2L - 0x80)] = value as u8;
    }

    fn bump_timer2(&mut self) {
        let value = self.timer2_value();
        let next = value.wrapping_add(1);
        if next != 0 {
            self.set_timer2(next);
            return;
        }
        let t2 = self.t2con_byte();
        let baud_generator = t2 & (t2con::RCLK | t2con::TCLK) != 0;
        // In baud-generator mode the overflow clocks the serial port and does
        // not raise TF2.
        if !baud_generator {
            self.set_tf2();
        }
        if baud_generator || t2 & t2con::CPRL2 == 0 {
            self.set_timer2(self.rcap2_value());
        } else {
            self.set_timer2(0);
        }
    }

    fn t2ex_event(&mut self) {
        let t2 = self.t2con_byte();
        if t2 & t2con::EXEN2 == 0 {
            return;
        }
        let baud_generator = t2 & (t2con::RCLK | t2con::TCLK) != 0;
        if t2 & t2con::CPRL2 != 0 && !baud_generator {
            let value = self.timer2_value();
            self.set_rcap2(value);
        } else {
            let value = self.rcap2_value();
            self.set_timer2(value);
        }
        self.sfr[usize::from(sfr::T2CON - 0x80)] |= t2con::EXF2;
    }

    fn tick_timers(&mut self) {
        let tmod = self.sfr[usize::from(sfr::TMOD - 0x80)];
        let mode0 = tmod & 0x03;
        let mode1 = (tmod >> 4) & 0x03;

        if tmod & 0x04 == 0 && self.timer0_clocked() {
            self.bump_timer0(mode0);
        }
        if mode0 == 3 {
            // TH0 is an 8-bit timer under TR1 in Timer 0 mode 3.
            if self.tcon_byte() & tcon::TR1 != 0 {
                self.bump_th0();
            }
        }
        if mode1 != 3 && tmod & 0x40 == 0 && self.timer1_clocked() {
            self.bump_timer1(mode1, mode0 != 3);
        }

        let t2 = self.t2con_byte();
        if t2 & t2con::TR2 != 0 && t2 & t2con::CT2 == 0 {
            self.bump_timer2();
        }
    }

    fn advance_machine_cycles(&mut self, cycles: u8) {
        for _ in 0..cycles {
            self.oscillator = self.oscillator.wrapping_add(OSCILLATOR_CYCLES_PER_MACHINE);
            self.tick_timers();
            self.tick_serial();
        }
    }

    // -- stepping ----------------------------------------------------------

    /// Execute one instruction, or service a pending interrupt first. Returns
    /// the machine cycles retired.
    pub fn step(&mut self) -> Result<u8, Fault> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }

        let stopped = self.idle_flag() || self.power_down_flag();
        if stopped {
            if let Some(pending) = self.pending_interrupt() {
                let can_wake =
                    !self.power_down_flag() || pending.source == 0 || pending.source == 2;
                if can_wake {
                    let index = usize::from(sfr::PCON - 0x80);
                    self.sfr[index] &= !0x03;
                    let cycles = self.service_interrupt(pending);
                    self.advance_machine_cycles(cycles);
                    return Ok(cycles);
                }
            }
            if self.power_down_flag() {
                // The oscillator is off; only host-visible time moves.
                self.oscillator = self.oscillator.wrapping_add(OSCILLATOR_CYCLES_PER_MACHINE);
            } else {
                self.advance_machine_cycles(1);
            }
            return Ok(1);
        }

        if self.interrupts_enabled()
            && let Some(pending) = self.pending_interrupt()
        {
            let cycles = self.service_interrupt(pending);
            self.advance_machine_cycles(cycles);
            return Ok(cycles);
        }

        match self.execute() {
            Ok(cycles) => {
                self.advance_machine_cycles(cycles);
                Ok(cycles)
            }
            Err(fault) => {
                self.fault = Some(fault);
                Err(fault)
            }
        }
    }

    /// Run for at most `budget` oscillator clocks.
    pub fn run(&mut self, budget: u64) -> Stop {
        let start = self.oscillator;
        loop {
            if self.fault.is_some() {
                return Stop::Trap;
            }
            if self.is_idle() {
                return Stop::Idle;
            }
            if self.oscillator.wrapping_sub(start) >= budget {
                return Stop::Budget;
            }
            if self.step().is_err() {
                return Stop::Trap;
            }
        }
    }

    // -- stack -------------------------------------------------------------

    fn push(&mut self, value: u8) {
        let sp = self.sp().wrapping_add(1);
        self.set_sp(sp);
        self.iram[usize::from(sp)] = value;
    }

    fn pop(&mut self) -> u8 {
        let sp = self.sp();
        let value = self.iram[usize::from(sp)];
        self.set_sp(sp.wrapping_sub(1));
        value
    }

    fn push_pc(&mut self) {
        self.push(self.pc as u8);
        self.push((self.pc >> 8) as u8);
    }

    fn pop_pc(&mut self) {
        let high = self.pop();
        let low = self.pop();
        self.pc = (u16::from(high) << 8) | u16::from(low);
    }

    // -- fetch -------------------------------------------------------------

    fn fetch8(&mut self) -> Result<u8, Fault> {
        let addr = usize::from(self.pc);
        if addr >= CODE_SIZE {
            return Err(Fault::CodeOutOfBounds {
                addr: u32::from(self.pc),
            });
        }
        let byte = self.code[addr];
        self.pc = self.pc.wrapping_add(1);
        Ok(byte)
    }

    fn fetch16(&mut self) -> Result<u16, Fault> {
        let high = self.fetch8()?;
        let low = self.fetch8()?;
        Ok((u16::from(high) << 8) | u16::from(low))
    }

    fn relative(&mut self) -> Result<i8, Fault> {
        Ok(self.fetch8()? as i8)
    }

    fn jump_relative(&mut self, offset: i8) {
        self.pc = self.pc.wrapping_add(offset as i16 as u16);
    }

    // -- operand helpers ---------------------------------------------------

    fn read_indirect(&self, n: u8) -> u8 {
        self.iram[usize::from(self.register(n))]
    }

    fn write_indirect(&mut self, n: u8, value: u8) {
        let addr = self.register(n);
        self.iram[usize::from(addr)] = value;
    }

    fn read_bit(&self, bit: u8, rmw: bool) -> Result<bool, Fault> {
        if bit < 0x80 {
            let addr = 0x20 + (bit >> 3);
            let mask = 1u8 << (bit & 7);
            Ok(self.iram[usize::from(addr)] & mask != 0)
        } else {
            let addr = bit & 0xF8;
            if !is_bit_addressable_sfr(addr) {
                return Err(Fault::InvalidBit { bit });
            }
            let mask = 1u8 << (bit & 7);
            Ok(self.read_direct(addr, rmw) & mask != 0)
        }
    }

    fn write_bit(&mut self, bit: u8, on: bool) -> Result<(), Fault> {
        if bit < 0x80 {
            let addr = 0x20 + (bit >> 3);
            let mask = 1u8 << (bit & 7);
            if on {
                self.iram[usize::from(addr)] |= mask;
            } else {
                self.iram[usize::from(addr)] &= !mask;
            }
            Ok(())
        } else {
            let addr = bit & 0xF8;
            if !is_bit_addressable_sfr(addr) {
                return Err(Fault::InvalidBit { bit });
            }
            let mask = 1u8 << (bit & 7);
            let old = self.read_direct(addr, true);
            let value = if on { old | mask } else { old & !mask };
            self.write_direct(addr, value);
            Ok(())
        }
    }

    fn set_flag(&mut self, mask: u8, on: bool) {
        let index = usize::from(sfr::PSW - 0x80);
        if on {
            self.sfr[index] |= mask;
        } else {
            self.sfr[index] &= !mask;
        }
    }

    fn set_carry(&mut self, on: bool) {
        self.set_flag(psw::CY, on);
    }

    // -- ALU ---------------------------------------------------------------

    fn add_a(&mut self, value: u8, with_carry: bool) {
        let a = self.acc();
        let carry_in = u8::from(with_carry && self.carry());
        let sum = u16::from(a) + u16::from(value) + u16::from(carry_in);
        let result = sum as u8;
        let carry = sum > 0xFF;
        let auxiliary =
            (u16::from(a & 0x0F) + u16::from(value & 0x0F) + u16::from(carry_in)) > 0x0F;
        let overflow = ((a ^ result) & (value ^ result) & 0x80) != 0;
        self.set_acc(result);
        self.set_carry(carry);
        self.set_flag(psw::AC, auxiliary);
        self.set_flag(psw::OV, overflow);
    }

    fn subb_a(&mut self, value: u8, with_carry: bool) {
        let a = self.acc();
        let borrow_in = i16::from(u8::from(with_carry && self.carry()));
        let difference = i16::from(a) - i16::from(value) - borrow_in;
        let result = difference as u8;
        let carry = difference < 0;
        let auxiliary = i16::from(a & 0x0F) - i16::from(value & 0x0F) - borrow_in < 0;
        let overflow = ((a ^ value) & (a ^ result) & 0x80) != 0;
        self.set_acc(result);
        self.set_carry(carry);
        self.set_flag(psw::AC, auxiliary);
        self.set_flag(psw::OV, overflow);
    }

    fn da_a(&mut self) {
        let a = self.acc();
        let carry_in = self.carry();
        let ac_in = self.auxiliary_carry();
        let low = (a & 0x0F) > 9 || ac_in;
        let high = a > 0x99 || carry_in;
        let correction = (if low { 0x06 } else { 0 }) | (if high { 0x60 } else { 0 });
        self.set_acc(a.wrapping_add(correction));
        self.set_flag(psw::AC, low);
        self.set_carry(high);
    }

    fn multiply(&mut self) {
        let product = u16::from(self.acc()) * u16::from(self.b());
        self.set_acc(product as u8);
        self.set_b((product >> 8) as u8);
        self.set_carry(false);
        self.set_flag(psw::OV, product > 0xFF);
    }

    fn divide(&mut self) {
        let divisor = self.b();
        if divisor == 0 {
            self.set_carry(false);
            self.set_flag(psw::OV, true);
            return;
        }
        let dividend = u16::from(self.acc());
        self.set_acc((dividend / u16::from(divisor)) as u8);
        self.set_b((dividend % u16::from(divisor)) as u8);
        self.set_carry(false);
        self.set_flag(psw::OV, false);
    }

    fn ajmp(&mut self, opcode: u8) -> Result<(), Fault> {
        let low = u16::from(self.fetch8()?);
        self.pc = (self.pc & 0xF800) | (u16::from(opcode & 0xE0) << 3) | low;
        Ok(())
    }

    fn acall(&mut self, opcode: u8) -> Result<(), Fault> {
        let low = u16::from(self.fetch8()?);
        let target = (self.pc & 0xF800) | (u16::from(opcode & 0xE0) << 3) | low;
        self.push_pc();
        self.pc = target;
        Ok(())
    }

    fn movc_pc(&mut self) -> Result<(), Fault> {
        let addr = self.pc.wrapping_add(u16::from(self.acc()));
        let byte = *self
            .code
            .get(usize::from(addr))
            .ok_or(Fault::CodeOutOfBounds {
                addr: u32::from(addr),
            })?;
        self.set_acc(byte);
        Ok(())
    }

    fn movc_dptr(&mut self) -> Result<(), Fault> {
        let addr = self.dptr().wrapping_add(u16::from(self.acc()));
        let byte = *self
            .code
            .get(usize::from(addr))
            .ok_or(Fault::CodeOutOfBounds {
                addr: u32::from(addr),
            })?;
        self.set_acc(byte);
        Ok(())
    }

    // -- the interpreter ---------------------------------------------------

    fn execute(&mut self) -> Result<u8, Fault> {
        let opcode_at = self.pc;
        let opcode = self.fetch8()?;
        // `Rn` instructions use the low three opcode bits; `@Ri`
        // instructions use the low bit (R0 or R1).
        let n = opcode & 0x07;
        let ri = opcode & 0x01;

        match opcode {
            0x00 => Ok(1),
            0x02 => {
                let target = self.fetch16()?;
                self.pc = target;
                Ok(2)
            }
            0x12 => {
                let target = self.fetch16()?;
                self.push_pc();
                self.pc = target;
                Ok(2)
            }
            0x22 => {
                self.pop_pc();
                Ok(2)
            }
            0x32 => {
                self.pop_pc();
                if self.isr_depth > 0 {
                    self.isr_depth -= 1;
                    let level = usize::from(self.isr_levels[self.isr_depth]);
                    self.active[level] = false;
                }
                Ok(2)
            }
            0x03 => {
                let a = self.acc();
                self.set_acc(a.rotate_right(1));
                Ok(1)
            }
            0x13 => {
                let a = self.acc();
                let carry = u8::from(self.carry());
                self.set_acc((a >> 1) | (carry << 7));
                self.set_carry(a & 0x01 != 0);
                Ok(1)
            }
            0x23 => {
                let a = self.acc();
                self.set_acc(a.rotate_left(1));
                Ok(1)
            }
            0x33 => {
                let a = self.acc();
                let carry = u8::from(self.carry());
                self.set_acc((a << 1) | carry);
                self.set_carry(a & 0x80 != 0);
                Ok(1)
            }
            0x04 => {
                let a = self.acc().wrapping_add(1);
                self.set_acc(a);
                Ok(1)
            }
            0x05 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, true).wrapping_add(1);
                self.write_direct(addr, value);
                Ok(1)
            }
            0x06 | 0x07 => {
                let value = self.read_indirect(ri).wrapping_add(1);
                self.write_indirect(ri, value);
                Ok(1)
            }
            0x08..=0x0F => {
                let value = self.register(n).wrapping_add(1);
                self.set_register(n, value);
                Ok(1)
            }
            0x10 => {
                let bit = self.fetch8()?;
                let offset = self.relative()?;
                if self.read_bit(bit, true)? {
                    self.write_bit(bit, false)?;
                    self.jump_relative(offset);
                }
                Ok(2)
            }
            0x14 => {
                let a = self.acc().wrapping_sub(1);
                self.set_acc(a);
                Ok(1)
            }
            0x15 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, true).wrapping_sub(1);
                self.write_direct(addr, value);
                Ok(1)
            }
            0x16 | 0x17 => {
                let value = self.read_indirect(ri).wrapping_sub(1);
                self.write_indirect(ri, value);
                Ok(1)
            }
            0x18..=0x1F => {
                let value = self.register(n).wrapping_sub(1);
                self.set_register(n, value);
                Ok(1)
            }
            0x20 => {
                let bit = self.fetch8()?;
                let offset = self.relative()?;
                if self.read_bit(bit, false)? {
                    self.jump_relative(offset);
                }
                Ok(2)
            }
            0x24 => {
                let value = self.fetch8()?;
                self.add_a(value, false);
                Ok(1)
            }
            0x25 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, false);
                self.add_a(value, false);
                Ok(1)
            }
            0x26 | 0x27 => {
                let value = self.read_indirect(ri);
                self.add_a(value, false);
                Ok(1)
            }
            0x28..=0x2F => {
                let value = self.register(n);
                self.add_a(value, false);
                Ok(1)
            }
            0x30 => {
                let bit = self.fetch8()?;
                let offset = self.relative()?;
                if !self.read_bit(bit, false)? {
                    self.jump_relative(offset);
                }
                Ok(2)
            }
            0x34 => {
                let value = self.fetch8()?;
                self.add_a(value, true);
                Ok(1)
            }
            0x35 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, false);
                self.add_a(value, true);
                Ok(1)
            }
            0x36 | 0x37 => {
                let value = self.read_indirect(ri);
                self.add_a(value, true);
                Ok(1)
            }
            0x38..=0x3F => {
                let value = self.register(n);
                self.add_a(value, true);
                Ok(1)
            }
            0x40 => {
                let offset = self.relative()?;
                if self.carry() {
                    self.jump_relative(offset);
                }
                Ok(2)
            }
            0x42 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, true) | self.acc();
                self.write_direct(addr, value);
                Ok(1)
            }
            0x43 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, true) | self.fetch8()?;
                self.write_direct(addr, value);
                Ok(2)
            }
            0x44 => {
                let value = self.fetch8()? | self.acc();
                self.set_acc(value);
                Ok(1)
            }
            0x45 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, false) | self.acc();
                self.set_acc(value);
                Ok(1)
            }
            0x46 | 0x47 => {
                let value = self.read_indirect(ri) | self.acc();
                self.set_acc(value);
                Ok(1)
            }
            0x48..=0x4F => {
                let value = self.register(n) | self.acc();
                self.set_acc(value);
                Ok(1)
            }
            0x50 => {
                let offset = self.relative()?;
                if !self.carry() {
                    self.jump_relative(offset);
                }
                Ok(2)
            }
            0x52 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, true) & self.acc();
                self.write_direct(addr, value);
                Ok(1)
            }
            0x53 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, true) & self.fetch8()?;
                self.write_direct(addr, value);
                Ok(2)
            }
            0x54 => {
                let value = self.fetch8()? & self.acc();
                self.set_acc(value);
                Ok(1)
            }
            0x55 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, false) & self.acc();
                self.set_acc(value);
                Ok(1)
            }
            0x56 | 0x57 => {
                let value = self.read_indirect(ri) & self.acc();
                self.set_acc(value);
                Ok(1)
            }
            0x58..=0x5F => {
                let value = self.register(n) & self.acc();
                self.set_acc(value);
                Ok(1)
            }
            0x60 => {
                let offset = self.relative()?;
                if self.acc() == 0 {
                    self.jump_relative(offset);
                }
                Ok(2)
            }
            0x62 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, true) ^ self.acc();
                self.write_direct(addr, value);
                Ok(1)
            }
            0x63 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, true) ^ self.fetch8()?;
                self.write_direct(addr, value);
                Ok(2)
            }
            0x64 => {
                let value = self.fetch8()? ^ self.acc();
                self.set_acc(value);
                Ok(1)
            }
            0x65 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, false) ^ self.acc();
                self.set_acc(value);
                Ok(1)
            }
            0x66 | 0x67 => {
                let value = self.read_indirect(ri) ^ self.acc();
                self.set_acc(value);
                Ok(1)
            }
            0x68..=0x6F => {
                let value = self.register(n) ^ self.acc();
                self.set_acc(value);
                Ok(1)
            }
            0x70 => {
                let offset = self.relative()?;
                if self.acc() != 0 {
                    self.jump_relative(offset);
                }
                Ok(2)
            }
            0x72 => {
                let bit = self.fetch8()?;
                let value = self.carry() || self.read_bit(bit, false)?;
                self.set_carry(value);
                Ok(2)
            }
            0x73 => {
                let target = self.dptr().wrapping_add(u16::from(self.acc()));
                self.pc = target;
                Ok(2)
            }
            0x74 => {
                let value = self.fetch8()?;
                self.set_acc(value);
                Ok(1)
            }
            0x75 => {
                let addr = self.fetch8()?;
                let value = self.fetch8()?;
                self.write_direct(addr, value);
                Ok(2)
            }
            0x76 | 0x77 => {
                let value = self.fetch8()?;
                self.write_indirect(ri, value);
                Ok(1)
            }
            0x78..=0x7F => {
                let value = self.fetch8()?;
                self.set_register(n, value);
                Ok(1)
            }
            0x80 => {
                let offset = self.relative()?;
                self.jump_relative(offset);
                Ok(2)
            }
            0x82 => {
                let bit = self.fetch8()?;
                let value = self.carry() && self.read_bit(bit, false)?;
                self.set_carry(value);
                Ok(2)
            }
            0x83 => {
                self.movc_pc()?;
                Ok(2)
            }
            0x84 => {
                self.divide();
                Ok(4)
            }
            0x85 => {
                let source = self.fetch8()?;
                let destination = self.fetch8()?;
                let value = self.read_direct(source, false);
                self.write_direct(destination, value);
                Ok(2)
            }
            0x86 | 0x87 => {
                let addr = self.fetch8()?;
                let value = self.read_indirect(ri);
                self.write_direct(addr, value);
                Ok(2)
            }
            0x88..=0x8F => {
                let addr = self.fetch8()?;
                let value = self.register(n);
                self.write_direct(addr, value);
                Ok(2)
            }
            0x90 => {
                let value = self.fetch16()?;
                self.set_dptr(value);
                Ok(2)
            }
            0x92 => {
                let bit = self.fetch8()?;
                let value = self.carry();
                self.write_bit(bit, value)?;
                Ok(2)
            }
            0x93 => {
                self.movc_dptr()?;
                Ok(2)
            }
            0x94 => {
                let value = self.fetch8()?;
                self.subb_a(value, false);
                Ok(1)
            }
            0x95 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, false);
                self.subb_a(value, false);
                Ok(1)
            }
            0x96 | 0x97 => {
                let value = self.read_indirect(ri);
                self.subb_a(value, false);
                Ok(1)
            }
            0x98..=0x9F => {
                let value = self.register(n);
                self.subb_a(value, false);
                Ok(1)
            }
            0xA0 => {
                let bit = self.fetch8()?;
                let value = self.carry() || !self.read_bit(bit, false)?;
                self.set_carry(value);
                Ok(2)
            }
            0xA2 => {
                let bit = self.fetch8()?;
                let value = self.read_bit(bit, false)?;
                self.set_carry(value);
                Ok(1)
            }
            0xA3 => {
                let value = self.dptr().wrapping_add(1);
                self.set_dptr(value);
                Ok(2)
            }
            0xA4 => {
                self.multiply();
                Ok(4)
            }
            0xA5 => Err(Fault::UnknownOpcode {
                addr: u32::from(opcode_at),
                opcode,
            }),
            0xA6 | 0xA7 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, false);
                self.write_indirect(ri, value);
                Ok(2)
            }
            0xA8..=0xAF => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, false);
                self.set_register(n, value);
                Ok(2)
            }
            0xB0 => {
                let bit = self.fetch8()?;
                let value = self.carry() && !self.read_bit(bit, false)?;
                self.set_carry(value);
                Ok(2)
            }
            0xB2 => {
                let bit = self.fetch8()?;
                let value = self.read_bit(bit, true)?;
                self.write_bit(bit, !value)?;
                Ok(1)
            }
            0xB3 => {
                let value = !self.carry();
                self.set_carry(value);
                Ok(1)
            }
            0xB4 => {
                let value = self.fetch8()?;
                let offset = self.relative()?;
                let a = self.acc();
                self.set_carry(a < value);
                if a != value {
                    self.jump_relative(offset);
                }
                Ok(2)
            }
            0xB5 => {
                let addr = self.fetch8()?;
                let offset = self.relative()?;
                let a = self.acc();
                let value = self.read_direct(addr, false);
                self.set_carry(a < value);
                if a != value {
                    self.jump_relative(offset);
                }
                Ok(2)
            }
            0xB6 | 0xB7 => {
                let value = self.fetch8()?;
                let offset = self.relative()?;
                let indirect = self.read_indirect(ri);
                self.set_carry(indirect < value);
                if indirect != value {
                    self.jump_relative(offset);
                }
                Ok(2)
            }
            0xB8..=0xBF => {
                let value = self.fetch8()?;
                let offset = self.relative()?;
                let register = self.register(n);
                self.set_carry(register < value);
                if register != value {
                    self.jump_relative(offset);
                }
                Ok(2)
            }
            0xC0 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, false);
                self.push(value);
                Ok(2)
            }
            0xC2 => {
                let bit = self.fetch8()?;
                self.write_bit(bit, false)?;
                Ok(1)
            }
            0xC3 => {
                self.set_carry(false);
                Ok(1)
            }
            0xC4 => {
                let a = self.acc();
                self.set_acc(a.rotate_left(4));
                Ok(1)
            }
            0xC5 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, true);
                let a = self.acc();
                self.write_direct(addr, a);
                self.set_acc(value);
                Ok(1)
            }
            0xC6 | 0xC7 => {
                let value = self.read_indirect(ri);
                let a = self.acc();
                self.write_indirect(ri, a);
                self.set_acc(value);
                Ok(1)
            }
            0xC8..=0xCF => {
                let value = self.register(n);
                let a = self.acc();
                self.set_register(n, a);
                self.set_acc(value);
                Ok(1)
            }
            0xD0 => {
                let addr = self.fetch8()?;
                let value = self.pop();
                self.write_direct(addr, value);
                Ok(2)
            }
            0xD2 => {
                let bit = self.fetch8()?;
                self.write_bit(bit, true)?;
                Ok(1)
            }
            0xD3 => {
                self.set_carry(true);
                Ok(1)
            }
            0xD4 => {
                self.da_a();
                Ok(1)
            }
            0xD5 => {
                let addr = self.fetch8()?;
                let offset = self.relative()?;
                let value = self.read_direct(addr, true).wrapping_sub(1);
                self.write_direct(addr, value);
                if value != 0 {
                    self.jump_relative(offset);
                }
                Ok(2)
            }
            0xD6 | 0xD7 => {
                let value = self.read_indirect(ri);
                let a = self.acc();
                self.write_indirect(ri, (value & 0xF0) | (a & 0x0F));
                self.set_acc((a & 0xF0) | (value & 0x0F));
                Ok(1)
            }
            0xD8..=0xDF => {
                let offset = self.relative()?;
                let value = self.register(n).wrapping_sub(1);
                self.set_register(n, value);
                if value != 0 {
                    self.jump_relative(offset);
                }
                Ok(2)
            }
            0xE0 => {
                let addr = self.dptr();
                let value = self.xdata[usize::from(addr)];
                self.set_acc(value);
                Ok(2)
            }
            0xE2 | 0xE3 => {
                let high = u16::from(self.sfr[usize::from(sfr::P2 - 0x80)]);
                let addr = (high << 8) | u16::from(self.register(ri));
                let value = self.xdata[usize::from(addr)];
                self.set_acc(value);
                Ok(2)
            }
            0xE4 => {
                self.set_acc(0);
                Ok(1)
            }
            0xE5 => {
                let addr = self.fetch8()?;
                let value = self.read_direct(addr, false);
                self.set_acc(value);
                Ok(1)
            }
            0xE6 | 0xE7 => {
                let value = self.read_indirect(ri);
                self.set_acc(value);
                Ok(1)
            }
            0xE8..=0xEF => {
                let value = self.register(n);
                self.set_acc(value);
                Ok(1)
            }
            0xF0 => {
                let addr = self.dptr();
                let value = self.acc();
                self.xdata[usize::from(addr)] = value;
                Ok(2)
            }
            0xF2 | 0xF3 => {
                let high = u16::from(self.sfr[usize::from(sfr::P2 - 0x80)]);
                let addr = (high << 8) | u16::from(self.register(ri));
                let value = self.acc();
                self.xdata[usize::from(addr)] = value;
                Ok(2)
            }
            0xF4 => {
                let value = !self.acc();
                self.set_acc(value);
                Ok(1)
            }
            0xF5 => {
                let addr = self.fetch8()?;
                let value = self.acc();
                self.write_direct(addr, value);
                Ok(1)
            }
            0xF6 | 0xF7 => {
                let value = self.acc();
                self.write_indirect(ri, value);
                Ok(1)
            }
            0xF8..=0xFF => {
                let value = self.acc();
                self.set_register(n, value);
                Ok(1)
            }
            _ if opcode & 0x1F == 0x01 => {
                self.ajmp(opcode)?;
                Ok(2)
            }
            _ if opcode & 0x1F == 0x11 => {
                self.acall(opcode)?;
                Ok(2)
            }
            _ => Err(Fault::UnknownOpcode {
                addr: u32::from(opcode_at),
                opcode,
            }),
        }
    }

    // -- debug -------------------------------------------------------------

    /// Replace the breakpoint set.
    pub fn set_breakpoints(&mut self, pcs: impl IntoIterator<Item = u32>) {
        self.breakpoints = pcs.into_iter().map(|pc| pc as u16).collect();
        self.breakpoints.sort_unstable();
        self.breakpoints.dedup();
    }

    pub fn clear_breakpoints(&mut self) {
        self.breakpoints.clear();
    }

    pub fn has_breakpoint(&self, pc: u16) -> bool {
        self.breakpoints.binary_search(&pc).is_ok()
    }

    /// Read `len` bytes starting at a debug address.
    pub fn debug_read_memory(&self, addr: u32, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len.min(4096));
        let end = addr.saturating_add(len as u32);
        let mut at = addr;
        while at < end {
            let (base, data): (u32, &[u8]) = if at < CODE_SIZE as u32 {
                (0, &self.code)
            } else if (DEBUG_IRAM_BASE..DEBUG_IRAM_BASE + IRAM_SIZE as u32).contains(&at) {
                (DEBUG_IRAM_BASE, &self.iram)
            } else if (DEBUG_XDATA_BASE..DEBUG_XDATA_BASE + XDATA_SIZE as u32).contains(&at) {
                (DEBUG_XDATA_BASE, &self.xdata)
            } else {
                break;
            };
            let offset = (at - base) as usize;
            let take = (data.len() - offset).min((end - at) as usize);
            if take == 0 {
                break;
            }
            out.extend_from_slice(&data[offset..offset + take]);
            at += take as u32;
        }
        out
    }
}

/// The parity of a byte: 1 when it has an odd number of set bits.
pub fn parity(value: u8) -> u8 {
    value.count_ones() as u8 & 1
}
