//! The AVR8 core: thirty-two registers, the status register, and the
//! instruction decoder for an ATmega328P.
//!
//! The core owns the register file, `SREG`, the stack pointer, the program
//! counter and flash. Everything outside that window — I/O registers and
//! SRAM — is reached through [`DataBus`], so the same core can be paired with
//! a bare memory in a unit test or with the full [`crate::Machine`].
//!
//! Program addresses are word addresses everywhere in this module. A fault
//! reports a byte address because that is what a map file and a HEX record
//! use; the conversion is in [`Fault`].

use core::fmt;

/// Flash size in bytes.
pub const FLASH_SIZE: usize = 32 * 1024;
/// Flash size in 16-bit words.
pub const FLASH_WORDS: usize = FLASH_SIZE / 2;
/// SRAM size in bytes.
pub const SRAM_SIZE: usize = 2 * 1024;
/// First data-space address of SRAM.
pub const SRAM_START: u16 = 0x0100;
/// Last data-space address (inclusive).
pub const SRAM_END: u16 = SRAM_START + SRAM_SIZE as u16 - 1;
/// Reset value of the stack pointer.
pub const RAMEND: u16 = SRAM_END;

/// `SREG` bit positions.
pub mod sreg {
    /// Carry.
    pub const C: u8 = 0;
    /// Zero.
    pub const Z: u8 = 1;
    /// Negative.
    pub const N: u8 = 2;
    /// Two's-complement overflow.
    pub const V: u8 = 3;
    /// Signed (N xor V).
    pub const S: u8 = 4;
    /// Half carry.
    pub const H: u8 = 5;
    /// Bit-copy storage.
    pub const T: u8 = 6;
    /// Global interrupt enable.
    pub const I: u8 = 7;
}

/// The status register.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sreg(u8);

impl Sreg {
    /// All flags clear, as after reset.
    pub const fn new() -> Self {
        Self(0)
    }

    pub const fn from_byte(value: u8) -> Self {
        Self(value)
    }

    pub const fn to_byte(self) -> u8 {
        self.0
    }

    pub fn get(&self, bit: u8) -> bool {
        self.0 & (1 << bit) != 0
    }

    pub fn set(&mut self, bit: u8, on: bool) {
        if on {
            self.0 |= 1 << bit;
        } else {
            self.0 &= !(1 << bit);
        }
    }

    pub fn carry(&self) -> bool {
        self.get(sreg::C)
    }
    pub fn zero(&self) -> bool {
        self.get(sreg::Z)
    }
    pub fn negative(&self) -> bool {
        self.get(sreg::N)
    }
    pub fn overflow(&self) -> bool {
        self.get(sreg::V)
    }
    pub fn sign(&self) -> bool {
        self.get(sreg::S)
    }
    pub fn half_carry(&self) -> bool {
        self.get(sreg::H)
    }
    pub fn t(&self) -> bool {
        self.get(sreg::T)
    }
    pub fn interrupt(&self) -> bool {
        self.get(sreg::I)
    }
}

impl fmt::Display for Sreg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SREG={:#04x}", self.0)
    }
}

/// A trap raised by the core instead of a panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The program counter left flash while fetching an opcode or operand.
    FlashOutOfBounds { addr: u32 },
    /// A load, store or stack access left the data address space.
    DataOutOfBounds { addr: u32 },
    /// A word that is not an implemented AVR instruction. `addr` is a byte
    /// address inside flash, the form a disassembly or a map file reports.
    UnknownOpcode { addr: u32, opcode: u16 },
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::FlashOutOfBounds { addr } => {
                write!(f, "flash access out of range at byte address {addr:#06x}")
            }
            Fault::DataOutOfBounds { addr } => {
                write!(f, "data access out of range at address {addr:#06x}")
            }
            Fault::UnknownOpcode { addr, opcode } => write!(
                f,
                "unknown opcode {opcode:#06x} at byte address {addr:#06x}"
            ),
        }
    }
}

impl std::error::Error for Fault {}

/// The part of the address space the core does not own.
///
/// Addresses below `0x20` and the three registers at `0x5d`–`0x5f` are handled
/// by [`Cpu`] itself; every other data-space address arrives here. An
/// implementation must reject addresses it cannot serve rather than silently
/// aliasing them.
pub trait DataBus {
    fn read_data(&mut self, addr: u16) -> Result<u8, Fault>;
    fn write_data(&mut self, addr: u16, value: u8) -> Result<(), Fault>;

    /// `WDR`: clear the watchdog counter. A bare memory has no watchdog, so
    /// the default is a no-op.
    fn watchdog_reset(&mut self) {}
}

/// The AVR8 register file and instruction interpreter.
pub struct Cpu {
    regs: [u8; 32],
    sreg: Sreg,
    sp: u16,
    /// Program counter in words.
    pc: u16,
    /// Retired cycles since reset.
    cycles: u64,
    flash: [u8; FLASH_SIZE],
    sleeping: bool,
    halted: bool,
    /// Set by [`Cpu::step`] when the core executes `SLEEP`.
    fault: Option<Fault>,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// A core in its reset state, with blank flash.
    pub fn new() -> Self {
        Self {
            regs: [0; 32],
            sreg: Sreg::new(),
            sp: RAMEND,
            pc: 0,
            cycles: 0,
            // Erased AVR flash reads as 0xffff, which is not a valid
            // instruction, so running off the end of a small image traps
            // instead of silently executing NOPs.
            flash: [0xff; FLASH_SIZE],
            sleeping: false,
            halted: false,
            fault: None,
        }
    }

    /// Return registers, flags and counters to their reset values. Flash and
    /// SRAM are untouched.
    pub fn reset(&mut self) {
        self.regs = [0; 32];
        self.sreg = Sreg::new();
        self.sp = RAMEND;
        self.pc = 0;
        self.cycles = 0;
        self.sleeping = false;
        self.halted = false;
        self.fault = None;
    }

    // -- state accessors ----------------------------------------------------

    /// The register file.
    pub fn regs(&self) -> &[u8; 32] {
        &self.regs
    }

    /// Mutable register file, for a debugger or a test fixture.
    pub fn regs_mut(&mut self) -> &mut [u8; 32] {
        &mut self.regs
    }

    pub fn reg(&self, index: u8) -> u8 {
        self.regs[usize::from(index) & 0x1f]
    }

    pub fn set_reg(&mut self, index: u8, value: u8) {
        self.regs[usize::from(index) & 0x1f] = value;
    }

    pub fn sreg(&self) -> Sreg {
        self.sreg
    }

    pub fn set_sreg(&mut self, value: u8) {
        self.sreg = Sreg::from_byte(value);
    }

    /// Status register for tests and debuggers.
    pub fn sreg_mut(&mut self) -> &mut Sreg {
        &mut self.sreg
    }

    /// Stack pointer, in data space.
    pub fn sp(&self) -> u16 {
        self.sp
    }

    pub fn set_sp(&mut self, value: u16) {
        self.sp = value;
    }

    /// Program counter in words.
    pub fn pc(&self) -> u16 {
        self.pc
    }

    /// Program counter as a byte address.
    pub fn pc_bytes(&self) -> u32 {
        u32::from(self.pc) * 2
    }

    pub fn set_pc(&mut self, word: u16) {
        self.pc = word & (FLASH_WORDS as u16 - 1);
    }

    pub fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Whether `SLEEP` has stopped the core.
    pub fn sleeping(&self) -> bool {
        self.sleeping
    }

    /// Whether an unrecoverable trap has stopped the core.
    pub fn halted(&self) -> bool {
        self.halted
    }

    pub fn fault(&self) -> Option<Fault> {
        self.fault
    }

    pub fn flash(&self) -> &[u8; FLASH_SIZE] {
        &self.flash
    }

    pub fn flash_mut(&mut self) -> &mut [u8; FLASH_SIZE] {
        &mut self.flash
    }

    /// Whether `SREG.I` lets an interrupt through.
    pub fn interrupts_enabled(&self) -> bool {
        self.sreg.interrupt()
    }

    /// Enter an interrupt handler: push the return address, clear `I`, jump to
    /// the vector's word address and retire the vector-entry cycles.
    ///
    /// `vector` is the datasheet vector number; vector `n` lives at word
    /// address `2 * n` (byte `4 * n`). The caller clears the peripheral flag
    /// with `Peripherals::acknowledge` after this succeeds.
    pub fn take_interrupt<B: DataBus + ?Sized>(
        &mut self,
        vector: u8,
        bus: &mut B,
    ) -> Result<u8, Fault> {
        if let Err(fault) = self.push_return_address(bus) {
            self.fault = Some(fault);
            self.halted = true;
            return Err(fault);
        }
        self.sreg.set(sreg::I, false);
        self.sleeping = false;
        self.pc = (u16::from(vector) * 2) & (FLASH_WORDS as u16 - 1);
        self.cycles = self
            .cycles
            .wrapping_add(u64::from(crate::peripherals::INTERRUPT_CYCLES));
        Ok(crate::peripherals::INTERRUPT_CYCLES)
    }

    /// Retire one idle cycle while the core is in `SLEEP`, so a peripheral
    /// clock driven by the host or a timer can still make progress.
    pub fn idle_cycle(&mut self) {
        self.cycles = self.cycles.wrapping_add(1);
    }

    // -- stepping -----------------------------------------------------------

    /// Execute one instruction. Returns the number of cycles retired, which is
    /// zero when the core is asleep or halted. A sleeping core is woken and
    /// vectored by [`crate::Machine::step`], not here.
    pub fn step<B: DataBus + ?Sized>(&mut self, bus: &mut B) -> Result<u8, Fault> {
        if self.sleeping || self.halted {
            return Ok(0);
        }
        let at = self.pc_bytes();
        let opcode = self.fetch_word()?;
        match self.execute(opcode, at, bus) {
            Ok(cycles) => {
                self.cycles = self.cycles.wrapping_add(u64::from(cycles));
                Ok(cycles)
            }
            Err(fault) => {
                self.fault = Some(fault);
                self.halted = true;
                Err(fault)
            }
        }
    }

    fn fetch_word(&mut self) -> Result<u16, Fault> {
        let word = usize::from(self.pc) & (FLASH_WORDS - 1);
        let at = word * 2;
        if at + 1 >= FLASH_SIZE {
            return Err(Fault::FlashOutOfBounds { addr: at as u32 });
        }
        let value = u16::from_le_bytes([self.flash[at], self.flash[at + 1]]);
        self.pc = ((word + 1) & (FLASH_WORDS - 1)) as u16;
        Ok(value)
    }

    fn fetch_operand_word(&mut self) -> Result<u16, Fault> {
        self.fetch_word()
    }

    // -- data space ---------------------------------------------------------

    fn read_data<B: DataBus + ?Sized>(&mut self, addr: u16, bus: &mut B) -> Result<u8, Fault> {
        match addr {
            0x0000..=0x001f => Ok(self.regs[usize::from(addr)]),
            0x5d => Ok(self.sp as u8),
            0x5e => Ok((self.sp >> 8) as u8),
            0x5f => Ok(self.sreg.to_byte()),
            _ => bus.read_data(addr),
        }
    }

    fn write_data<B: DataBus + ?Sized>(
        &mut self,
        addr: u16,
        value: u8,
        bus: &mut B,
    ) -> Result<(), Fault> {
        match addr {
            0x0000..=0x001f => {
                self.regs[usize::from(addr)] = value;
                Ok(())
            }
            0x5d => {
                self.sp = (self.sp & 0xff00) | u16::from(value);
                Ok(())
            }
            0x5e => {
                self.sp = (self.sp & 0x00ff) | (u16::from(value) << 8);
                Ok(())
            }
            0x5f => {
                self.sreg = Sreg::from_byte(value);
                Ok(())
            }
            _ => bus.write_data(addr, value),
        }
    }

    fn push<B: DataBus + ?Sized>(&mut self, value: u8, bus: &mut B) -> Result<(), Fault> {
        let addr = self.sp;
        self.write_data(addr, value, bus)?;
        self.sp = self.sp.wrapping_sub(1);
        Ok(())
    }

    fn pop<B: DataBus + ?Sized>(&mut self, bus: &mut B) -> Result<u8, Fault> {
        self.sp = self.sp.wrapping_add(1);
        let addr = self.sp;
        self.read_data(addr, bus)
    }

    fn push_return_address<B: DataBus + ?Sized>(&mut self, bus: &mut B) -> Result<(), Fault> {
        // The low byte is pushed first, so it sits at the higher address and
        // is popped last.
        self.push(self.pc as u8, bus)?;
        self.push((self.pc >> 8) as u8, bus)
    }

    fn pop_return_address<B: DataBus + ?Sized>(&mut self, bus: &mut B) -> Result<u16, Fault> {
        let high = self.pop(bus)?;
        let low = self.pop(bus)?;
        Ok(u16::from_le_bytes([low, high]) & (FLASH_WORDS as u16 - 1))
    }

    fn pair(&self, low: u8) -> u16 {
        u16::from_le_bytes([self.regs[usize::from(low)], self.regs[usize::from(low) + 1]])
    }

    fn set_pair(&mut self, low: u8, value: u16) {
        let bytes = value.to_le_bytes();
        self.regs[usize::from(low)] = bytes[0];
        self.regs[usize::from(low) + 1] = bytes[1];
    }

    // -- flag helpers -------------------------------------------------------

    fn set_zn(&mut self, result: u8) {
        self.sreg.set(sreg::N, result & 0x80 != 0);
        self.sreg.set(sreg::Z, result == 0);
    }

    /// `S = N xor V`.
    fn set_s(&mut self) {
        let n = self.sreg.get(sreg::N);
        let v = self.sreg.get(sreg::V);
        self.sreg.set(sreg::S, n ^ v);
    }

    fn flags_add(&mut self, rd: u8, rr: u8, result: u8) {
        let carry = ((rd & rr) | (rr & !result) | (!result & rd)) & 0x80 != 0;
        let half = ((rd & rr) | (rr & !result) | (!result & rd)) & 0x08 != 0;
        let overflow = ((rd & rr & !result) | (!rd & !rr & result)) & 0x80 != 0;
        self.sreg.set(sreg::C, carry);
        self.sreg.set(sreg::H, half);
        self.sreg.set(sreg::V, overflow);
        self.set_zn(result);
        self.set_s();
    }

    /// Subtract flags for the carry-chaining forms (`SBC`, `SBCI`, `CPC`).
    ///
    /// The AVR keeps `Z` set only while every byte of a multi-byte operation
    /// has produced zero, so a 16- or 32-bit compare can end with one branch.
    fn flags_sub_chained(&mut self, rd: u8, rr: u8, result: u8) {
        let zero = self.sreg.zero();
        self.flags_sub(rd, rr, result);
        self.sreg.set(sreg::Z, result == 0 && zero);
    }

    fn flags_sub(&mut self, rd: u8, rr: u8, result: u8) {
        let borrow = (!rd & rr) | (rr & result) | (result & !rd);
        let overflow = ((rd & !rr & !result) | (!rd & rr & result)) & 0x80 != 0;
        self.sreg.set(sreg::C, borrow & 0x80 != 0);
        self.sreg.set(sreg::H, borrow & 0x08 != 0);
        self.sreg.set(sreg::V, overflow);
        self.set_zn(result);
        self.set_s();
    }

    // -- instruction execution ---------------------------------------------

    fn execute<B: DataBus + ?Sized>(
        &mut self,
        opcode: u16,
        at: u32,
        bus: &mut B,
    ) -> Result<u8, Fault> {
        // Exact single-word opcodes first.
        match opcode {
            0x0000 => return Ok(1),                // NOP
            0x9508 => return self.ret(bus, false), // RET
            0x9518 => return self.ret(bus, true),  // RETI
            0x9409 => return self.ijmp(),          // IJMP
            0x9419 => return self.ijmp(),          // EIJMP (same 32 KiB view)
            0x9509 => return self.icall(bus),      // ICALL
            0x9519 => return self.icall(bus),      // EICALL
            0x9588 => {
                self.sleeping = true;
                return Ok(1);
            }
            0x9598 => {
                self.halted = true;
                return Ok(1);
            }
            0x95a8 => {
                bus.watchdog_reset();
                return Ok(1);
            }
            0x95c8 => return self.lpm(None, bus), // LPM
            _ => {}
        }

        // MOVW / MULS / MULSU.
        if opcode & 0xff00 == 0x0100 {
            let d = ((opcode >> 4) & 0x0f) as u8 * 2;
            let r = (opcode & 0x0f) as u8 * 2;
            let value = self.pair(r);
            self.set_pair(d, value);
            return Ok(1);
        }
        if opcode & 0xff00 == 0x0200 {
            let d = 16 + (((opcode >> 4) as u8) & 0x0f);
            let r = 16 + (opcode & 0x0f) as u8;
            let product = i16::from(self.regs[usize::from(d)] as i8)
                .wrapping_mul(i16::from(self.regs[usize::from(r)] as i8));
            self.store_product(product as u16);
            return Ok(2);
        }
        if opcode & 0xff88 == 0x0300 {
            let d = 16 + (((opcode >> 4) as u8) & 0x07);
            let r = 16 + (opcode & 0x07) as u8;
            let product = i16::from(self.regs[usize::from(d)] as i8)
                .wrapping_mul(i16::from(self.regs[usize::from(r)]));
            self.store_product(product as u16);
            return Ok(2);
        }

        // ADIW / SBIW: 16-bit immediate add to a register pair. avr-gcc uses
        // these constantly for pointer arithmetic.
        if opcode & 0xff00 == 0x9600 {
            return self.adiw(opcode);
        }
        if opcode & 0xff00 == 0x9700 {
            return self.sbiw(opcode);
        }

        // Two-register ALU operations.
        let d = ((opcode >> 4) & 0x1f) as u8;
        let r = ((opcode & 0x0f) | ((opcode >> 5) & 0x10)) as u8;
        match opcode & 0xfc00 {
            0x0400 => return self.cpc(d, r), // CPC
            0x0800 => return self.sbc(d, r), // SBC
            0x0c00 => return self.add(d, r), // ADD (LSL is ADD Rd,Rd)
            0x1000 => return self.cpse(d, r, bus),
            0x1400 => return self.cp(d, r),  // CP
            0x1800 => return self.sub(d, r), // SUB
            0x1c00 => return self.adc(d, r), // ADC (ROL is ADC Rd,Rd)
            0x2000 => return self.logic(d, r, Logic::And),
            0x2400 => return self.logic(d, r, Logic::Eor),
            0x2800 => return self.logic(d, r, Logic::Or),
            0x2c00 => {
                self.regs[usize::from(d)] = self.regs[usize::from(r)];
                return Ok(1);
            }
            0x9c00 => {
                let product =
                    u16::from(self.regs[usize::from(d)]) * u16::from(self.regs[usize::from(r)]);
                self.store_product(product);
                return Ok(2);
            }
            _ => {}
        }

        // Immediate ALU operations, `KKKK dddd KKKK`.
        let k = (((opcode >> 4) & 0x00f0) | (opcode & 0x000f)) as u8;
        let id = 16 + ((opcode >> 4) & 0x0f) as u8;
        match opcode & 0xf000 {
            0x3000 => return self.cpi(id, k),  // CPI
            0x4000 => return self.sbci(id, k), // SBCI
            0x5000 => return self.subi(id, k), // SUBI
            0x6000 => return self.iori(id, k), // ORI / SBR
            0x7000 => return self.andi(id, k), // ANDI / CBR
            0xe000 => return self.ldi(id, k),  // LDI / SER
            _ => {}
        }

        // LDD/STD: displacement addressing off Y or Z.
        if opcode & 0xd000 == 0x8000 {
            let q = ((opcode >> 8) & 0x20) | ((opcode >> 7) & 0x18) | (opcode & 0x07);
            let store = opcode & 0x0200 != 0;
            let base = if opcode & 0x0008 != 0 { 28 } else { 30 };
            let address = self.pair(base).wrapping_add(q);
            if store {
                self.write_data(address, self.regs[usize::from(d)], bus)?;
            } else {
                self.regs[usize::from(d)] = self.read_data(address, bus)?;
            }
            return Ok(2);
        }

        // The indirect load/store group.
        if opcode & 0xf000 == 0x9000 {
            match opcode & 0xfe0f {
                0x9000 => {
                    // LDS Rd, k
                    let addr = self.fetch_operand_word()?;
                    self.regs[usize::from(d)] = self.read_data(addr, bus)?;
                    return Ok(2);
                }
                0x9200 => {
                    // STS k, Rr
                    let addr = self.fetch_operand_word()?;
                    self.write_data(addr, self.regs[usize::from(d)], bus)?;
                    return Ok(2);
                }
                0x900c => return self.ld_ptr(d, 26, PtrMode::Plain, bus),
                0x900d => return self.ld_ptr(d, 26, PtrMode::Post, bus),
                0x900e => return self.ld_ptr(d, 26, PtrMode::Pre, bus),
                0x920c => return self.st_ptr(26, d, PtrMode::Plain, bus),
                0x920d => return self.st_ptr(26, d, PtrMode::Post, bus),
                0x920e => return self.st_ptr(26, d, PtrMode::Pre, bus),
                0x9001 => return self.ld_ptr(d, 30, PtrMode::Post, bus),
                0x9002 => return self.ld_ptr(d, 30, PtrMode::Pre, bus),
                0x9009 => return self.ld_ptr(d, 28, PtrMode::Post, bus),
                0x900a => return self.ld_ptr(d, 28, PtrMode::Pre, bus),
                0x9201 => return self.st_ptr(30, d, PtrMode::Post, bus),
                0x9202 => return self.st_ptr(30, d, PtrMode::Pre, bus),
                0x9209 => return self.st_ptr(28, d, PtrMode::Post, bus),
                0x920a => return self.st_ptr(28, d, PtrMode::Pre, bus),
                0x9004 => return self.lpm(Some(d), bus), // LPM Rd, Z
                0x9005 => {
                    // LPM Rd, Z+
                    let cycles = self.lpm(Some(d), bus)?;
                    let z = self.pair(30).wrapping_add(1);
                    self.set_pair(30, z);
                    return Ok(cycles);
                }
                0x900f => {
                    // POP Rd
                    self.regs[usize::from(d)] = self.pop(bus)?;
                    return Ok(2);
                }
                0x920f => {
                    // PUSH Rr
                    self.push(self.regs[usize::from(d)], bus)?;
                    return Ok(2);
                }
                _ => {}
            }
            match opcode & 0xff00 {
                0x9800 => return self.sbi_like(opcode, 0, bus), // CBI
                0x9a00 => return self.sbi_like(opcode, 1, bus), // SBI
                0x9900 => return self.sbic_like(opcode, false, bus), // SBIC
                0x9b00 => return self.sbic_like(opcode, true, bus), // SBIS
                _ => {}
            }
        }

        // The 0x9400 one-register / control group.
        if opcode & 0xff8f == 0x9408 {
            let bit = ((opcode >> 4) & 0x07) as u8;
            self.sreg.set(bit, true);
            return Ok(1);
        }
        if opcode & 0xff8f == 0x9488 {
            let bit = ((opcode >> 4) & 0x07) as u8;
            self.sreg.set(bit, false);
            return Ok(1);
        }
        if opcode & 0xfe0e == 0x940c {
            let target = self.jump_target(opcode)?;
            self.pc = target;
            return Ok(3);
        }
        if opcode & 0xfe0e == 0x940e {
            let target = self.jump_target(opcode)?;
            self.push_return_address(bus)?;
            self.pc = target;
            return Ok(4);
        }
        if opcode & 0xfe0f == 0x9400 {
            let reg = usize::from(d);
            match opcode & 0x000f {
                0x0 => {
                    // COM
                    let result = !self.regs[reg];
                    self.regs[reg] = result;
                    self.sreg.set(sreg::C, true);
                    self.sreg.set(sreg::V, false);
                    self.set_zn(result);
                    self.set_s();
                }
                0x1 => {
                    // NEG
                    let old = self.regs[reg];
                    let result = 0u8.wrapping_sub(old);
                    self.regs[reg] = result;
                    self.sreg.set(sreg::C, old != 0);
                    self.sreg.set(sreg::V, result == 0x80);
                    self.sreg.set(sreg::H, (old & 0x08) | (result & 0x08) != 0);
                    self.set_zn(result);
                    self.set_s();
                }
                0x2 => {
                    // SWAP
                    let value = self.regs[reg];
                    self.regs[reg] = value.rotate_left(4);
                }
                0x3 => {
                    // INC
                    let result = self.regs[reg].wrapping_add(1);
                    self.regs[reg] = result;
                    self.sreg.set(sreg::V, result == 0x80);
                    self.set_zn(result);
                    self.set_s();
                }
                0x5 => {
                    // ASR
                    let value = self.regs[reg];
                    let carry = value & 0x01 != 0;
                    let result = (value >> 1) | (value & 0x80);
                    self.regs[reg] = result;
                    self.sreg.set(sreg::C, carry);
                    self.sreg.set(sreg::N, result & 0x80 != 0);
                    self.sreg.set(sreg::Z, result == 0);
                    self.sreg.set(sreg::V, (result & 0x80 != 0) ^ carry);
                    self.set_s();
                }
                0x6 => {
                    // LSR
                    let value = self.regs[reg];
                    let carry = value & 0x01 != 0;
                    let result = value >> 1;
                    self.regs[reg] = result;
                    self.sreg.set(sreg::C, carry);
                    self.sreg.set(sreg::N, false);
                    self.sreg.set(sreg::Z, result == 0);
                    self.sreg.set(sreg::V, carry);
                    self.sreg.set(sreg::S, carry);
                }
                0x7 => {
                    // ROR
                    let value = self.regs[reg];
                    let carry = value & 0x01 != 0;
                    let old_carry = if self.sreg.carry() { 0x80 } else { 0 };
                    let result = (value >> 1) | old_carry;
                    self.regs[reg] = result;
                    self.sreg.set(sreg::C, carry);
                    self.sreg.set(sreg::N, result & 0x80 != 0);
                    self.sreg.set(sreg::Z, result == 0);
                    self.sreg.set(sreg::V, (result & 0x80 != 0) ^ carry);
                    self.set_s();
                }
                0xa => {
                    // DEC
                    let result = self.regs[reg].wrapping_sub(1);
                    self.regs[reg] = result;
                    self.sreg.set(sreg::V, result == 0x7f);
                    self.set_zn(result);
                    self.set_s();
                }
                _ => return Err(Fault::UnknownOpcode { addr: at, opcode }),
            }
            return Ok(1);
        }

        // IN / OUT.
        if opcode & 0xf800 == 0xb000 {
            let io = ((opcode >> 5) & 0x30) | (opcode & 0x0f);
            self.regs[usize::from(d)] = self.read_data(0x20 + io, bus)?;
            return Ok(1);
        }
        if opcode & 0xf800 == 0xb800 {
            let io = ((opcode >> 5) & 0x30) | (opcode & 0x0f);
            self.write_data(0x20 + io, self.regs[usize::from(d)], bus)?;
            return Ok(1);
        }

        // RJMP / RCALL.
        if opcode & 0xf000 == 0xc000 {
            let offset = sign_extend(opcode & 0x0fff, 12);
            self.pc = wrap_pc(self.pc as i32 + offset);
            return Ok(2);
        }
        if opcode & 0xf000 == 0xd000 {
            let offset = sign_extend(opcode & 0x0fff, 12);
            self.push_return_address(bus)?;
            self.pc = wrap_pc(self.pc as i32 + offset);
            return Ok(3);
        }

        // Bit and branch instructions.
        if opcode & 0xfe08 == 0xf800 {
            let bit = (opcode & 0x07) as u8;
            let reg = usize::from(d);
            let on = self.sreg.t();
            if on {
                self.regs[reg] |= 1 << bit;
            } else {
                self.regs[reg] &= !(1 << bit);
            }
            return Ok(1);
        }
        if opcode & 0xfe08 == 0xfa00 {
            let bit = (opcode & 0x07) as u8;
            self.sreg
                .set(sreg::T, self.regs[usize::from(d)] & (1 << bit) != 0);
            return Ok(1);
        }
        if opcode & 0xfe08 == 0xfc00 {
            // SBRC: skip if bit clear.
            let bit = (opcode & 0x07) as u8;
            if self.regs[usize::from(d)] & (1 << bit) == 0 {
                return self.skip_next(bus);
            }
            return Ok(1);
        }
        if opcode & 0xfe08 == 0xfe00 {
            // SBRS: skip if bit set.
            let bit = (opcode & 0x07) as u8;
            if self.regs[usize::from(d)] & (1 << bit) != 0 {
                return self.skip_next(bus);
            }
            return Ok(1);
        }
        if opcode & 0xfc00 == 0xf000 {
            let bit = (opcode & 0x07) as u8;
            let offset = sign_extend((opcode >> 3) & 0x7f, 7);
            if self.sreg.get(bit) {
                self.pc = wrap_pc(self.pc as i32 + offset);
                return Ok(2);
            }
            return Ok(1);
        }
        if opcode & 0xfc00 == 0xf400 {
            let bit = (opcode & 0x07) as u8;
            let offset = sign_extend((opcode >> 3) & 0x7f, 7);
            if !self.sreg.get(bit) {
                self.pc = wrap_pc(self.pc as i32 + offset);
                return Ok(2);
            }
            return Ok(1);
        }

        Err(Fault::UnknownOpcode { addr: at, opcode })
    }

    // -- instruction families ----------------------------------------------

    fn add(&mut self, d: u8, r: u8) -> Result<u8, Fault> {
        let rd = self.regs[usize::from(d)];
        let rr = self.regs[usize::from(r)];
        let result = rd.wrapping_add(rr);
        self.regs[usize::from(d)] = result;
        self.flags_add(rd, rr, result);
        Ok(1)
    }

    fn adc(&mut self, d: u8, r: u8) -> Result<u8, Fault> {
        let rd = self.regs[usize::from(d)];
        let rr = self.regs[usize::from(r)];
        let carry = u8::from(self.sreg.carry());
        let result = rd.wrapping_add(rr).wrapping_add(carry);
        self.regs[usize::from(d)] = result;
        self.flags_add(rd, rr, result);
        Ok(1)
    }

    fn sub(&mut self, d: u8, r: u8) -> Result<u8, Fault> {
        let rd = self.regs[usize::from(d)];
        let rr = self.regs[usize::from(r)];
        let result = rd.wrapping_sub(rr);
        self.regs[usize::from(d)] = result;
        self.flags_sub(rd, rr, result);
        Ok(1)
    }

    fn sbc(&mut self, d: u8, r: u8) -> Result<u8, Fault> {
        let rd = self.regs[usize::from(d)];
        let rr = self.regs[usize::from(r)];
        let carry = u8::from(self.sreg.carry());
        let result = rd.wrapping_sub(rr).wrapping_sub(carry);
        self.regs[usize::from(d)] = result;
        self.flags_sub_chained(rd, rr, result);
        Ok(1)
    }

    fn cp(&mut self, d: u8, r: u8) -> Result<u8, Fault> {
        let rd = self.regs[usize::from(d)];
        let rr = self.regs[usize::from(r)];
        self.flags_sub(rd, rr, rd.wrapping_sub(rr));
        Ok(1)
    }

    fn cpc(&mut self, d: u8, r: u8) -> Result<u8, Fault> {
        let rd = self.regs[usize::from(d)];
        let rr = self.regs[usize::from(r)];
        let carry = u8::from(self.sreg.carry());
        let result = rd.wrapping_sub(rr).wrapping_sub(carry);
        self.flags_sub_chained(rd, rr, result);
        Ok(1)
    }

    fn cpi(&mut self, d: u8, k: u8) -> Result<u8, Fault> {
        let rd = self.regs[usize::from(d)];
        self.flags_sub(rd, k, rd.wrapping_sub(k));
        Ok(1)
    }

    fn subi(&mut self, d: u8, k: u8) -> Result<u8, Fault> {
        let rd = self.regs[usize::from(d)];
        let result = rd.wrapping_sub(k);
        self.regs[usize::from(d)] = result;
        self.flags_sub(rd, k, result);
        Ok(1)
    }

    fn sbci(&mut self, d: u8, k: u8) -> Result<u8, Fault> {
        let rd = self.regs[usize::from(d)];
        let carry = u8::from(self.sreg.carry());
        let result = rd.wrapping_sub(k).wrapping_sub(carry);
        self.regs[usize::from(d)] = result;
        self.flags_sub_chained(rd, k, result);
        Ok(1)
    }

    fn logic(&mut self, d: u8, r: u8, kind: Logic) -> Result<u8, Fault> {
        let rd = self.regs[usize::from(d)];
        let rr = self.regs[usize::from(r)];
        let result = match kind {
            Logic::And => rd & rr,
            Logic::Or => rd | rr,
            Logic::Eor => rd ^ rr,
        };
        self.regs[usize::from(d)] = result;
        self.sreg.set(sreg::V, false);
        self.set_zn(result);
        self.set_s();
        Ok(1)
    }

    /// The pair and immediate of an `ADIW`/`SBIW`.
    fn pair_immediate(opcode: u16) -> (u8, u16) {
        let low = 24 + 2 * (((opcode >> 4) & 0x03) as u8);
        let k = ((opcode >> 2) & 0x30) | (opcode & 0x0f);
        (low, k)
    }

    fn adiw(&mut self, opcode: u16) -> Result<u8, Fault> {
        let (low, k) = Self::pair_immediate(opcode);
        let old = self.pair(low);
        let result = old.wrapping_add(k);
        self.set_pair(low, result);
        let old_high = (old >> 15) as u8;
        let new_high = (result >> 15) as u8;
        self.sreg.set(sreg::V, (1 - old_high) & new_high != 0);
        self.sreg.set(sreg::C, old_high & (1 - new_high) != 0);
        self.sreg.set(sreg::N, new_high != 0);
        self.sreg.set(sreg::Z, result == 0);
        self.set_s();
        Ok(2)
    }

    fn sbiw(&mut self, opcode: u16) -> Result<u8, Fault> {
        let (low, k) = Self::pair_immediate(opcode);
        let old = self.pair(low);
        let result = old.wrapping_sub(k);
        self.set_pair(low, result);
        let old_high = (old >> 15) as u8;
        let new_high = (result >> 15) as u8;
        self.sreg.set(sreg::V, (1 - new_high) & old_high != 0);
        self.sreg.set(sreg::C, new_high & (1 - old_high) != 0);
        self.sreg.set(sreg::N, new_high != 0);
        self.sreg.set(sreg::Z, result == 0);
        self.set_s();
        Ok(2)
    }

    fn ldi(&mut self, d: u8, k: u8) -> Result<u8, Fault> {
        self.regs[usize::from(d)] = k;
        Ok(1)
    }

    fn andi(&mut self, d: u8, k: u8) -> Result<u8, Fault> {
        let result = self.regs[usize::from(d)] & k;
        self.regs[usize::from(d)] = result;
        self.sreg.set(sreg::V, false);
        self.set_zn(result);
        self.set_s();
        Ok(1)
    }

    fn iori(&mut self, d: u8, k: u8) -> Result<u8, Fault> {
        let result = self.regs[usize::from(d)] | k;
        self.regs[usize::from(d)] = result;
        self.sreg.set(sreg::V, false);
        self.set_zn(result);
        self.set_s();
        Ok(1)
    }

    fn cpse<B: DataBus + ?Sized>(&mut self, d: u8, r: u8, bus: &mut B) -> Result<u8, Fault> {
        if self.regs[usize::from(d)] == self.regs[usize::from(r)] {
            self.skip_next(bus)
        } else {
            Ok(1)
        }
    }

    fn store_product(&mut self, product: u16) {
        let bytes = product.to_le_bytes();
        self.regs[0] = bytes[0];
        self.regs[1] = bytes[1];
        self.sreg.set(sreg::C, product & 0x8000 != 0);
        self.sreg.set(sreg::Z, product == 0);
    }

    fn ld_ptr<B: DataBus + ?Sized>(
        &mut self,
        d: u8,
        base: u8,
        mode: PtrMode,
        bus: &mut B,
    ) -> Result<u8, Fault> {
        let (address, mode_cycles) = match mode {
            PtrMode::Plain => (self.pair(base), 1),
            PtrMode::Post => {
                let address = self.pair(base);
                self.set_pair(base, address.wrapping_add(1));
                (address, 2)
            }
            PtrMode::Pre => {
                let address = self.pair(base).wrapping_sub(1);
                self.set_pair(base, address);
                (address, 2)
            }
        };
        self.regs[usize::from(d)] = self.read_data(address, bus)?;
        Ok(mode_cycles)
    }

    fn st_ptr<B: DataBus + ?Sized>(
        &mut self,
        base: u8,
        r: u8,
        mode: PtrMode,
        bus: &mut B,
    ) -> Result<u8, Fault> {
        let (address, mode_cycles) = match mode {
            PtrMode::Plain => (self.pair(base), 1),
            PtrMode::Post => {
                let address = self.pair(base);
                self.set_pair(base, address.wrapping_add(1));
                (address, 2)
            }
            PtrMode::Pre => {
                let address = self.pair(base).wrapping_sub(1);
                self.set_pair(base, address);
                (address, 2)
            }
        };
        self.write_data(address, self.regs[usize::from(r)], bus)?;
        Ok(mode_cycles)
    }

    fn lpm<B: DataBus + ?Sized>(
        &mut self,
        destination: Option<u8>,
        _bus: &mut B,
    ) -> Result<u8, Fault> {
        let address = usize::from(self.pair(30));
        let byte = if address < FLASH_SIZE {
            self.flash[address]
        } else {
            return Err(Fault::FlashOutOfBounds {
                addr: address as u32,
            });
        };
        match destination {
            Some(register) => self.regs[usize::from(register)] = byte,
            None => self.regs[0] = byte,
        }
        Ok(3)
    }

    fn sbi_like<B: DataBus + ?Sized>(
        &mut self,
        opcode: u16,
        set: u8,
        bus: &mut B,
    ) -> Result<u8, Fault> {
        let addr = 0x20 + ((opcode >> 3) & 0x1f);
        let bit = (opcode & 0x07) as u8;
        let mut value = bus.read_data(addr)?;
        if set != 0 {
            value |= 1 << bit;
        } else {
            value &= !(1 << bit);
        }
        bus.write_data(addr, value)?;
        Ok(2)
    }

    fn sbic_like<B: DataBus + ?Sized>(
        &mut self,
        opcode: u16,
        want_set: bool,
        bus: &mut B,
    ) -> Result<u8, Fault> {
        let addr = 0x20 + ((opcode >> 3) & 0x1f);
        let bit = (opcode & 0x07) as u8;
        let value = bus.read_data(addr)?;
        if (value & (1 << bit) != 0) == want_set {
            self.skip_next(bus)
        } else {
            Ok(1)
        }
    }

    fn skip_next<B: DataBus + ?Sized>(&mut self, bus: &mut B) -> Result<u8, Fault> {
        let next = self.fetch_word()?;
        if is_two_word(next) {
            self.fetch_operand_word()?;
            Ok(3)
        } else {
            let _ = bus;
            Ok(2)
        }
    }

    fn ijmp(&mut self) -> Result<u8, Fault> {
        self.pc = self.pair(30) & (FLASH_WORDS as u16 - 1);
        Ok(2)
    }

    fn icall<B: DataBus + ?Sized>(&mut self, bus: &mut B) -> Result<u8, Fault> {
        let target = self.pair(30) & (FLASH_WORDS as u16 - 1);
        self.push_return_address(bus)?;
        self.pc = target;
        Ok(3)
    }

    fn ret<B: DataBus + ?Sized>(&mut self, bus: &mut B, interrupt: bool) -> Result<u8, Fault> {
        self.pc = self.pop_return_address(bus)?;
        if interrupt {
            self.sreg.set(sreg::I, true);
        }
        Ok(4)
    }

    /// JMP/CALL immediate target: the high address bits live in the first
    /// word, the low sixteen in the operand word.
    fn jump_target(&mut self, first: u16) -> Result<u16, Fault> {
        let low = self.fetch_operand_word()?;
        let high = (u32::from((first >> 4) & 0x1f) << 17) | (u32::from(first & 1) << 16);
        Ok(((high | u32::from(low)) & 0x3fff) as u16)
    }
}

#[derive(Clone, Copy)]
enum Logic {
    And,
    Or,
    Eor,
}

#[derive(Clone, Copy)]
enum PtrMode {
    Plain,
    Post,
    Pre,
}

fn sign_extend(value: u16, bits: u32) -> i32 {
    let shift = 32 - bits;
    (i32::from(value) << shift) >> shift
}

fn wrap_pc(value: i32) -> u16 {
    (value & (FLASH_WORDS as i32 - 1)) as u16
}

fn is_two_word(opcode: u16) -> bool {
    opcode & 0xfe0f == 0x9000
        || opcode & 0xfe0f == 0x9200
        || opcode & 0xfe0e == 0x940c
        || opcode & 0xfe0e == 0x940e
}
