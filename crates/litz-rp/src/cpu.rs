//! The ARMv6-M core: registers, exception entry and the Thumb decoder.
//!
//! The Cortex-M0+ executes Thumb only, and only the part of Thumb-2 that
//! ARMv6-M defines: every 16-bit encoding in the ARMv6-M manual plus the
//! handful of 32-bit encodings (`BL`, `MRS`/`MSR`, `DSB`/`DMB`/`ISB`). That is
//! a small, closed instruction set, which is why the decoder is a match over
//! the 16-bit opcode word rather than a table.
//!
//! The split with the rest of the chip is the usual one: the core knows
//! registers, flags, the stack and the exception model; every address it
//! touches goes through [`Bus`], so memory, the PPB (NVIC, SysTick, SCB) and
//! the peripherals are the machine's business.

use core::fmt;

/// A core-level failure. A firmware bug or a gap in the model surfaces here and
/// the runner reports it, rather than the core inventing an instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The decoder met an encoding ARMv6-M does not define.
    Undefined { pc: u32, opcode: u16 },
    /// A 32-bit encoding the core has not implemented yet.
    Unimplemented { pc: u32, opcode: u32 },
    /// A load, store or fetch outside the map, or an alignment the core
    /// rejects (word and halfword accesses must be aligned).
    Bus { addr: u32, write: bool },
    /// The core executed something that never returns in a simulator: the
    /// semihosting `BKPT`, or a `WFI` the machine cannot idle on.
    Halted,
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::Undefined { pc, opcode } => {
                write!(f, "undefined instruction {opcode:#06x} at {pc:#010x}")
            }
            Fault::Unimplemented { pc, opcode } => {
                write!(f, "unimplemented instruction {opcode:#010x} at {pc:#010x}")
            }
            Fault::Bus { addr, write } => write!(
                f,
                "{} of {addr:#010x} has no target",
                if *write { "write" } else { "read" }
            ),
            Fault::Halted => write!(f, "core halted"),
        }
    }
}

/// What the core needs from the rest of the chip.
///
/// The widths are separate methods rather than one method with a size, because
/// a peripheral register and a byte in SRAM go down different paths on this
/// chip and the bus wants to say so.
pub trait Bus {
    fn read8(&mut self, addr: u32) -> Result<u32, Fault>;
    fn read16(&mut self, addr: u32) -> Result<u32, Fault>;
    fn read32(&mut self, addr: u32) -> Result<u32, Fault>;
    fn write8(&mut self, addr: u32, value: u32) -> Result<(), Fault>;
    fn write16(&mut self, addr: u32, value: u32) -> Result<(), Fault>;
    fn write32(&mut self, addr: u32, value: u32) -> Result<(), Fault>;
}

/// `xPSR` bit positions.
pub mod psr {
    pub const N: u32 = 1 << 31;
    pub const Z: u32 = 1 << 30;
    pub const C: u32 = 1 << 29;
    pub const V: u32 = 1 << 28;
    pub const T: u32 = 1 << 24;
    /// The exception number, in the low byte of `IPSR`.
    pub const IPSR_MASK: u32 = 0x3f;
}

/// The two `EXC_RETURN` values a handler sees in `LR`.
pub const EXC_RETURN_MSP: u32 = 0xffff_fff9;
pub const EXC_RETURN_PSP: u32 = 0xffff_fffd;

/// The Cortex-M0+ register file.
///
/// `r[13]` is the stack pointer the instruction sees (MSP or PSP, chosen by
/// `CONTROL.SPSEL` outside handler mode); both banks are kept so `MSP`/`PSP`
/// stay meaningful across a switch.
#[derive(Clone, Debug)]
pub struct Core {
    pub r: [u32; 16],
    pub xpsr: u32,
    /// `PRIMASK`: mask every configurable exception but NMI and HardFault.
    pub primask: bool,
    /// `CONTROL`: bit 0 is nPRIV, bit 1 is SPSEL (0 = MSP, 1 = PSP).
    pub control: u32,
    pub msp: u32,
    pub psp: u32,
    /// The exception number currently executing (0 in thread mode). Kept
    /// outside `xpsr` so a handler can be entered while the stacked `xPSR`
    /// still describes the interrupted thread.
    pub exception: u16,
    /// Set by `WFI`; the machine clears it when an interrupt arrives.
    pub waiting: bool,
}

impl Default for Core {
    fn default() -> Self {
        Self::new()
    }
}

impl Core {
    pub const fn new() -> Self {
        Core {
            r: [0; 16],
            xpsr: psr::T,
            primask: false,
            control: 0,
            msp: 0,
            psp: 0,
            exception: 0,
            waiting: false,
        }
    }

    /// Take the reset values from the vector table the machine installed at
    /// `vectors`: initial `SP` at +0, reset handler at +4.
    pub fn reset<B: Bus>(&mut self, bus: &mut B) -> Result<(), Fault> {
        *self = Core::new();
        self.msp = bus.read32(0)?;
        let entry = bus.read32(4)?;
        self.r[13] = self.msp;
        self.r[15] = entry & !1;
        self.xpsr = psr::T;
        Ok(())
    }

    pub fn pc(&self) -> u32 {
        self.r[15]
    }

    pub fn sp(&self) -> u32 {
        self.r[13]
    }

    pub fn in_handler(&self) -> bool {
        self.exception != 0
    }

    /// Whether a configurable exception may be taken right now.
    pub fn accepts_interrupt(&self) -> bool {
        !self.primask || self.exception >= 2
    }

    /// Execute one instruction and return the cycles it consumed.
    ///
    /// ARMv6-M timing is mostly one cycle per instruction; the multi-cycle
    /// cases (branch, `BX`/`BLX`, the block transfers and exception entry) are
    /// counted where the decoder has the answer, which is what the machine
    /// reports as elapsed time.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> Result<u32, Fault> {
        let pc = self.r[15];
        let hw = (bus.read16(pc)? & 0xffff) as u16;
        // The only multi-halfword encodings ARMv6-M defines start with
        // `11101`, `11110` or `11111`; everything else is a single halfword.
        if hw >> 11 >= 0b11101 {
            let hw2 = (bus.read16(pc.wrapping_add(2))? & 0xffff) as u16;
            self.r[15] = pc.wrapping_add(4);
            return self.execute32(pc, hw, hw2);
        }
        self.r[15] = pc.wrapping_add(2);
        self.execute16(pc, hw, bus)
    }

    /// The 16-bit half of the ISA. `pc` is the address of `hw`, and `r[15]`
    /// already holds the fall-through address when this is called.
    fn execute16<B: Bus>(&mut self, pc: u32, hw: u16, bus: &mut B) -> Result<u32, Fault> {
        let undefined = Fault::Undefined { pc, opcode: hw };
        match hw >> 12 {
            0x0 | 0x1 => {
                // `000xx`: shifts by an immediate, `00011`: add/subtract by
                // register or by a three-bit immediate.
                let kind = (hw >> 11) & 3;
                if kind != 3 {
                    let imm = u32::from((hw >> 6) & 0x1f);
                    let rm = usize::from((hw >> 3) & 7);
                    let rd = usize::from(hw & 7);
                    let shift = match kind {
                        0 => Shift::Lsl,
                        1 => Shift::Lsr,
                        _ => Shift::Asr,
                    };
                    let (value, carry) = shift_imm(self.r[rm], imm, shift);
                    self.set_nz(value);
                    if let Some(carry) = carry {
                        self.set_c(carry);
                    }
                    self.write_reg(rd, value);
                    return Ok(1);
                }
                let op = (hw >> 9) & 3;
                let rn = usize::from((hw >> 3) & 7);
                let rd = usize::from(hw & 7);
                let rhs = if op < 2 {
                    self.r[usize::from((hw >> 6) & 7)]
                } else {
                    u32::from((hw >> 6) & 7)
                };
                let value = if op & 1 == 0 {
                    self.add_with_carry(self.r[rn], rhs, false)
                } else {
                    self.sub_flags(self.r[rn], rhs)
                };
                self.write_reg(rd, value);
                Ok(1)
            }
            0x2 | 0x3 => {
                // `00100` MOVS, `00101` CMP, `00110` ADDS, `00111` SUBS, all
                // against an eight-bit immediate.
                let group = hw >> 11;
                let imm = u32::from(hw & 0xff);
                let rd = usize::from((hw >> 8) & 7);
                match group {
                    4 => {
                        self.set_nz(imm);
                        self.write_reg(rd, imm);
                    }
                    5 => {
                        self.sub_flags(self.r[rd], imm);
                    }
                    6 => {
                        let value = self.add_with_carry(self.r[rd], imm, false);
                        self.write_reg(rd, value);
                    }
                    7 => {
                        let value = self.sub_flags(self.r[rd], imm);
                        self.write_reg(rd, value);
                    }
                    _ => return Err(undefined),
                }
                Ok(1)
            }
            0x4 => match hw >> 10 {
                // `010000`: the data-processing instructions.
                0b010000 => {
                    let op = (hw >> 6) & 0xf;
                    let rm = usize::from((hw >> 3) & 7);
                    let rdn = usize::from(hw & 7);
                    let a = self.r[rdn];
                    let b = self.r[rm];
                    match op {
                        0 => {
                            let value = a & b;
                            self.set_nz(value);
                            self.write_reg(rdn, value);
                        }
                        1 => {
                            let value = a ^ b;
                            self.set_nz(value);
                            self.write_reg(rdn, value);
                        }
                        2 => self.write_shift(rdn, a, b, Shift::Lsl),
                        3 => self.write_shift(rdn, a, b, Shift::Lsr),
                        4 => self.write_shift(rdn, a, b, Shift::Asr),
                        5 => {
                            let value = self.add_with_carry(a, b, self.flag(psr::C));
                            self.write_reg(rdn, value);
                        }
                        6 => {
                            let value = self.add_with_carry(a, !b, self.flag(psr::C));
                            self.write_reg(rdn, value);
                        }
                        7 => self.write_shift(rdn, a, b, Shift::Ror),
                        8 => self.set_nz(a & b),
                        9 => {
                            let value = self.sub_flags(0, b);
                            self.write_reg(rdn, value);
                        }
                        10 => {
                            self.sub_flags(a, b);
                        }
                        11 => {
                            self.add_with_carry(a, b, false);
                        }
                        12 => {
                            let value = a | b;
                            self.set_nz(value);
                            self.write_reg(rdn, value);
                        }
                        13 => {
                            let value = a.wrapping_mul(b);
                            self.set_nz(value);
                            self.write_reg(rdn, value);
                        }
                        14 => {
                            let value = a & !b;
                            self.set_nz(value);
                            self.write_reg(rdn, value);
                        }
                        _ => {
                            let value = !b;
                            self.set_nz(value);
                            self.write_reg(rdn, value);
                        }
                    }
                    Ok(1)
                }
                // `010001`: high-register data processing and `BX`/`BLX`.
                0b010001 => {
                    let op = (hw >> 8) & 3;
                    let rdn = usize::from(((hw >> 7) & 1) << 3 | (hw & 7));
                    let rm = usize::from((hw >> 3) & 0xf);
                    match op {
                        0 => {
                            let value =
                                self.reg_or_pc(rdn, pc).wrapping_add(self.reg_or_pc(rm, pc));
                            if rdn == 15 {
                                self.jump_or_return(bus, value)?;
                                Ok(2)
                            } else {
                                self.write_reg(rdn, value);
                                Ok(1)
                            }
                        }
                        1 => {
                            let a = self.reg_or_pc(rdn, pc);
                            let b = self.reg_or_pc(rm, pc);
                            self.sub_flags(a, b);
                            Ok(1)
                        }
                        2 => {
                            let value = self.reg_or_pc(rm, pc);
                            if rdn == 15 {
                                self.jump_or_return(bus, value)?;
                                Ok(2)
                            } else {
                                self.write_reg(rdn, value);
                                Ok(1)
                            }
                        }
                        _ => {
                            // Both `BX` and `BLX` require the low three bits to
                            // be zero; bit 7 selects the link.
                            if hw & 7 != 0 {
                                return Err(undefined);
                            }
                            let target = self.reg_or_pc(rm, pc);
                            if hw & 0x80 != 0 {
                                self.r[14] = pc.wrapping_add(2) | 1;
                            }
                            self.jump_or_return(bus, target)?;
                            Ok(2)
                        }
                    }
                }
                // `01001`: load a word relative to the aligned PC.
                _ => {
                    let rt = usize::from((hw >> 8) & 7);
                    let offset = u32::from(hw & 0xff) * 4;
                    let base = pc.wrapping_add(4) & !3;
                    let value = load(bus, base.wrapping_add(offset), 4)?;
                    self.write_reg(rt, value);
                    Ok(1)
                }
            },
            0x5 => {
                // Register-offset loads and stores.
                let op = (hw >> 9) & 7;
                let rm = usize::from((hw >> 6) & 7);
                let rn = usize::from((hw >> 3) & 7);
                let rt = usize::from(hw & 7);
                let addr = self.r[rn].wrapping_add(self.r[rm]);
                match op {
                    0 => store(bus, addr, self.r[rt], 4)?,
                    1 => store(bus, addr, self.r[rt], 2)?,
                    2 => store(bus, addr, self.r[rt], 1)?,
                    3 => {
                        let value = sx8(load(bus, addr, 1)?);
                        self.write_reg(rt, value);
                    }
                    4 => {
                        let value = load(bus, addr, 4)?;
                        self.write_reg(rt, value);
                    }
                    5 => {
                        let value = load(bus, addr, 2)?;
                        self.write_reg(rt, value);
                    }
                    6 => {
                        let value = load(bus, addr, 1)?;
                        self.write_reg(rt, value);
                    }
                    _ => {
                        let value = sx16(load(bus, addr, 2)?);
                        self.write_reg(rt, value);
                    }
                }
                Ok(1)
            }
            0x6..=0x8 => {
                // Immediate-offset loads and stores, scaled by the access size.
                let group = hw >> 11;
                let imm5 = u32::from((hw >> 6) & 0x1f);
                let rn = usize::from((hw >> 3) & 7);
                let rt = usize::from(hw & 7);
                match group {
                    12 => store(bus, self.r[rn].wrapping_add(imm5 * 4), self.r[rt], 4)?,
                    13 => {
                        let value = load(bus, self.r[rn].wrapping_add(imm5 * 4), 4)?;
                        self.write_reg(rt, value);
                    }
                    14 => store(bus, self.r[rn].wrapping_add(imm5), self.r[rt], 1)?,
                    15 => {
                        let value = load(bus, self.r[rn].wrapping_add(imm5), 1)?;
                        self.write_reg(rt, value);
                    }
                    16 => store(bus, self.r[rn].wrapping_add(imm5 * 2), self.r[rt], 2)?,
                    17 => {
                        let value = load(bus, self.r[rn].wrapping_add(imm5 * 2), 2)?;
                        self.write_reg(rt, value);
                    }
                    _ => return Err(undefined),
                }
                Ok(1)
            }
            0x9 => {
                // Word load or store relative to the active stack pointer.
                let rt = usize::from((hw >> 8) & 7);
                let offset = u32::from(hw & 0xff) * 4;
                let addr = self.r[13].wrapping_add(offset);
                if hw & 0x0800 != 0 {
                    let value = load(bus, addr, 4)?;
                    self.write_reg(rt, value);
                } else {
                    store(bus, addr, self.r[rt], 4)?;
                }
                Ok(1)
            }
            0xA => {
                // `10100` ADR and `10101` ADD Rd, SP, #imm.
                let rd = usize::from((hw >> 8) & 7);
                let offset = u32::from(hw & 0xff) * 4;
                match hw >> 11 {
                    0b10100 => {
                        let value = (pc.wrapping_add(4) & !3).wrapping_add(offset);
                        self.write_reg(rd, value);
                    }
                    0b10101 => {
                        let value = self.r[13].wrapping_add(offset);
                        self.write_reg(rd, value);
                    }
                    _ => return Err(undefined),
                }
                Ok(1)
            }
            0xB => {
                // The miscellaneous group, whose sub-encodings share no
                // contiguous field, so each is matched by its own mask.
                if hw & 0xff00 == 0xb000 {
                    // ADD or SUB SP, #imm7 * 4.
                    let offset = u32::from(hw & 0x7f) * 4;
                    let value = if hw & 0x80 != 0 {
                        self.r[13].wrapping_sub(offset)
                    } else {
                        self.r[13].wrapping_add(offset)
                    };
                    self.write_reg(13, value);
                    return Ok(1);
                }
                if hw & 0xff00 == 0xb200 {
                    // SXTH, SXTB, UXTH, UXTB.
                    let op = (hw >> 6) & 3;
                    let rm = usize::from((hw >> 3) & 7);
                    let rd = usize::from(hw & 7);
                    let value = match op {
                        0 => sx16(self.r[rm]),
                        1 => sx8(self.r[rm]),
                        2 => self.r[rm] & 0xffff,
                        _ => self.r[rm] & 0xff,
                    };
                    self.set_nz(value);
                    self.write_reg(rd, value);
                    return Ok(1);
                }
                if hw & 0xfd00 == 0xb100 || hw & 0xfd00 == 0xb900 {
                    // CBZ and CBNZ, a forward-only branch that leaves the flags
                    // alone.
                    let nonzero = hw & 0x0800 != 0;
                    let offset =
                        (u32::from((hw >> 9) & 1) << 6) | (u32::from((hw >> 3) & 0x1f) << 1);
                    let rn = usize::from(hw & 7);
                    if (self.r[rn] == 0) != nonzero {
                        self.r[15] = pc.wrapping_add(4).wrapping_add(offset);
                        return Ok(2);
                    }
                    return Ok(1);
                }
                if hw & 0xfe00 == 0xb400 {
                    // PUSH: lowest register at the lowest address, LR last.
                    let include_lr = hw & 0x0100 != 0;
                    let list = hw & 0x00ff;
                    let mut count = list.count_ones();
                    if include_lr {
                        count += 1;
                    }
                    let mut addr = self.r[13].wrapping_sub(count * 4);
                    let new_sp = addr;
                    for (i, &value) in self.r.iter().take(8).enumerate() {
                        if list & (1u16 << i) != 0 {
                            store(bus, addr, value, 4)?;
                            addr = addr.wrapping_add(4);
                        }
                    }
                    if include_lr {
                        store(bus, addr, self.r[14], 4)?;
                    }
                    self.write_reg(13, new_sp);
                    return Ok(1 + count);
                }
                if hw & 0xfe00 == 0xbc00 {
                    // POP: read the whole list before writing any register, so
                    // a popped value cannot change the address of a later one.
                    let include_pc = hw & 0x0100 != 0;
                    let list = hw & 0x00ff;
                    let mut count = list.count_ones();
                    if include_pc {
                        count += 1;
                    }
                    let mut addr = self.r[13];
                    let mut values = [0u32; 8];
                    for (i, value) in values.iter_mut().enumerate() {
                        if list & (1u16 << i) != 0 {
                            *value = load(bus, addr, 4)?;
                            addr = addr.wrapping_add(4);
                        }
                    }
                    let pc_value = if include_pc {
                        Some(load(bus, addr, 4)?)
                    } else {
                        None
                    };
                    self.write_reg(13, self.r[13].wrapping_add(count * 4));
                    for (i, &value) in values.iter().enumerate() {
                        if list & (1u16 << i) != 0 {
                            self.write_reg(i, value);
                        }
                    }
                    if let Some(value) = pc_value {
                        self.jump_or_return(bus, value)?;
                    }
                    return Ok(1 + count);
                }
                if hw & 0xff00 == 0xba00 {
                    // REV, REV16, REVSH. The `10` selector is undefined.
                    let op = (hw >> 6) & 3;
                    let rm = usize::from((hw >> 3) & 7);
                    let rd = usize::from(hw & 7);
                    let value = match op {
                        0 => self.r[rm].swap_bytes(),
                        1 => {
                            let v = self.r[rm];
                            ((v & 0x00ff_00ff) << 8) | ((v & 0xff00_ff00) >> 8)
                        }
                        3 => sx16(u32::from((self.r[rm] as u16).swap_bytes())),
                        _ => return Err(undefined),
                    };
                    self.write_reg(rd, value);
                    return Ok(1);
                }
                if hw & 0xff00 == 0xbe00 {
                    return Err(Fault::Halted);
                }
                if hw & 0xffe0 == 0xb660 && hw & 0xf == 2 {
                    // CPSID i / CPSIE i; bit 4 is the disable bit.
                    self.primask = hw & 0x10 != 0;
                    return Ok(1);
                }
                if hw & 0xff0f == 0xbf00 {
                    match (hw >> 4) & 0xf {
                        0 | 1 | 4 => return Ok(1), // NOP, YIELD, SEV
                        2 | 3 => {
                            // WFE and WFI park the core; the machine advances
                            // time and clears the flag when an interrupt comes.
                            self.waiting = true;
                            return Ok(0);
                        }
                        _ => return Err(undefined),
                    }
                }
                Err(undefined)
            }
            0xC => {
                // LDMIA and STMIA, both with writeback.
                let load_regs = hw & 0x0800 != 0;
                let rn = usize::from((hw >> 8) & 7);
                let list = hw & 0x00ff;
                let count = list.count_ones();
                let mut addr = self.r[rn];
                if load_regs {
                    let mut values = [0u32; 8];
                    for (i, value) in values.iter_mut().enumerate() {
                        if list & (1u16 << i) != 0 {
                            *value = load(bus, addr, 4)?;
                            addr = addr.wrapping_add(4);
                        }
                    }
                    // A base register named in the list takes the loaded value,
                    // which is what hardware does for the lowest-register case.
                    if list & (1u16 << rn) == 0 {
                        self.write_reg(rn, addr);
                    }
                    for (i, &value) in values.iter().enumerate() {
                        if list & (1u16 << i) != 0 {
                            self.write_reg(i, value);
                        }
                    }
                } else {
                    for (i, &value) in self.r.iter().take(8).enumerate() {
                        if list & (1u16 << i) != 0 {
                            store(bus, addr, value, 4)?;
                            addr = addr.wrapping_add(4);
                        }
                    }
                    self.write_reg(rn, addr);
                }
                Ok(1 + count)
            }
            0xD => {
                let cond = (hw >> 8) & 0xf;
                if cond < 14 {
                    if self.condition(cond) {
                        let offset = ((u32::from(hw & 0xff) as i8 as i32) << 1) as u32;
                        self.r[15] = pc.wrapping_add(4).wrapping_add(offset);
                        Ok(2)
                    } else {
                        Ok(1)
                    }
                } else if cond == 15 {
                    self.enter_exception(bus, 11)?;
                    Ok(4)
                } else {
                    Err(undefined)
                }
            }
            0xE => {
                if hw >> 11 == 0b11100 {
                    let imm = u32::from(hw & 0x7ff);
                    let offset = ((imm << 21) as i32 >> 20) as u32;
                    self.r[15] = pc.wrapping_add(4).wrapping_add(offset);
                    Ok(2)
                } else {
                    Err(undefined)
                }
            }
            _ => Err(undefined),
        }
    }

    /// The 32-bit encodings: `BL`, `MRS`, `MSR` and the three barriers.
    fn execute32(&mut self, pc: u32, hw1: u16, hw2: u16) -> Result<u32, Fault> {
        if hw1 == 0xf3ef && hw2 & 0xf000 == 0x8000 {
            let rd = usize::from((hw2 >> 8) & 0xf);
            let value = match hw2 & 0xff {
                0x00 => self.xpsr & (psr::N | psr::Z | psr::C | psr::V),
                0x05 => u32::from(self.exception),
                0x06 => self.xpsr & psr::T,
                0x07 => (self.xpsr & psr::T) | u32::from(self.exception),
                0x08 => self.msp,
                0x09 => self.psp,
                0x10 => u32::from(self.primask),
                0x14 => self.control,
                _ => return Err(Fault::Undefined { pc, opcode: hw1 }),
            };
            self.write_reg(rd, value);
            return Ok(3);
        }
        if hw1 & 0xfff0 == 0xf380 && hw2 & 0xff00 == 0x8800 {
            let rn = usize::from(hw1 & 0xf);
            let value = self.r[rn];
            match hw2 & 0xff {
                0x10 => self.primask = value & 1 != 0,
                0x14 => {
                    self.control = value & 0x3;
                    // SPSEL takes effect immediately and, outside handler mode,
                    // changes which bank `r[13]` names.
                    self.r[13] = if self.control & 2 != 0 && !self.in_handler() {
                        self.psp
                    } else {
                        self.msp
                    };
                }
                _ => return Err(Fault::Undefined { pc, opcode: hw1 }),
            }
            return Ok(3);
        }
        if hw1 == 0xf3bf {
            return match hw2 & 0xfff0 {
                0x8f40 | 0x8f50 | 0x8f60 => Ok(3),
                _ => Err(Fault::Undefined { pc, opcode: hw1 }),
            };
        }
        if hw1 & 0xf800 == 0xf000 && hw2 & 0xd000 == 0xd000 {
            // BL: the S, J1 and J2 bits complete a signed 25-bit halfword
            // offset the link instruction applies to PC + 4.
            let s = u32::from((hw1 >> 10) & 1);
            let imm10 = u32::from(hw1 & 0x3ff);
            let j1 = u32::from((hw2 >> 13) & 1);
            let j2 = u32::from((hw2 >> 11) & 1);
            let imm11 = u32::from(hw2 & 0x7ff);
            let i1 = (!(j1 ^ s)) & 1;
            let i2 = (!(j2 ^ s)) & 1;
            let imm = (s << 24) | (i1 << 23) | (i2 << 22) | (imm10 << 12) | (imm11 << 1);
            let offset = ((imm << 7) as i32 >> 7) as u32;
            self.r[14] = pc.wrapping_add(4) | 1;
            self.r[15] = pc.wrapping_add(4).wrapping_add(offset);
            return Ok(3);
        }
        Err(Fault::Undefined { pc, opcode: hw1 })
    }

    /// Read a general-purpose register, substituting `PC + 4` for `r[15]` the
    /// way a running instruction sees it.
    fn reg_or_pc(&self, index: usize, pc: u32) -> u32 {
        if index == 15 {
            pc.wrapping_add(4)
        } else {
            self.r[index]
        }
    }

    /// Write a general-purpose register, keeping the two stack banks coherent
    /// and forcing the PC into Thumb state.
    fn write_reg(&mut self, index: usize, value: u32) {
        match index {
            13 => {
                self.r[13] = value;
                self.sync_sp();
            }
            15 => self.r[15] = value & !1,
            _ => self.r[index] = value,
        }
    }

    /// Load a value into the PC, or, when the value is an `EXC_RETURN`, pop
    /// the stacked frame and resume the interrupted thread.
    ///
    /// Real firmware leaves a handler with any of `bx lr`, `pop {..., pc}` or
    /// `mov pc, lr`, so the check belongs on every path that writes the PC.
    /// Treating `0xffff_fff9` as a branch would send the next fetch into the
    /// reserved page at `0xffff_fff8` instead of returning.
    fn jump_or_return<B: Bus>(&mut self, bus: &mut B, target: u32) -> Result<(), Fault> {
        if is_exc_return(target) {
            self.return_from_exception(bus, target)
        } else {
            self.r[15] = target & !1;
            Ok(())
        }
    }

    /// Copy the active stack pointer back into whichever bank it belongs to.
    fn sync_sp(&mut self) {
        if self.control & 2 != 0 && !self.in_handler() {
            self.psp = self.r[13];
        } else {
            self.msp = self.r[13];
        }
    }

    fn flag(&self, mask: u32) -> bool {
        self.xpsr & mask != 0
    }

    fn set_flag(&mut self, mask: u32, on: bool) {
        if on {
            self.xpsr |= mask;
        } else {
            self.xpsr &= !mask;
        }
    }

    fn set_c(&mut self, on: bool) {
        self.set_flag(psr::C, on);
    }

    fn set_nz(&mut self, value: u32) {
        self.set_flag(psr::Z, value == 0);
        self.set_flag(psr::N, value & 0x8000_0000 != 0);
    }

    /// Add two operands and a carry, setting all four flags from the result.
    fn add_with_carry(&mut self, x: u32, y: u32, carry_in: bool) -> u32 {
        let (sum, carry1) = x.overflowing_add(y);
        let (result, carry2) = sum.overflowing_add(u32::from(carry_in));
        let overflow = ((x ^ result) & (y ^ result) & 0x8000_0000) != 0;
        self.set_flag(psr::C, carry1 || carry2);
        self.set_flag(psr::V, overflow);
        self.set_nz(result);
        result
    }

    /// Subtract by adding the complement with the carry set, which yields the
    /// architectural flag meanings (C set means no borrow).
    fn sub_flags(&mut self, x: u32, y: u32) -> u32 {
        self.add_with_carry(x, !y, true)
    }

    /// Apply a register-controlled shift and retire it into `rd`.
    fn write_shift(&mut self, rd: usize, value: u32, amount: u32, kind: Shift) {
        let (result, carry) = shift_reg(value, amount, kind);
        self.set_nz(result);
        if let Some(carry) = carry {
            self.set_c(carry);
        }
        self.write_reg(rd, result);
    }

    /// Whether a `B<cond>` condition holds, for condition codes 0 through 13.
    fn condition(&self, cond: u16) -> bool {
        let n = self.flag(psr::N);
        let z = self.flag(psr::Z);
        let c = self.flag(psr::C);
        let v = self.flag(psr::V);
        match cond {
            0 => z,
            1 => !z,
            2 => c,
            3 => !c,
            4 => n,
            5 => !n,
            6 => v,
            7 => !v,
            8 => c && !z,
            9 => !c || z,
            10 => n == v,
            11 => n != v,
            12 => !z && n == v,
            13 => z || n != v,
            _ => false,
        }
    }

    /// Enter `vector` as an exception: stack the thread context, set `IPSR`,
    /// leave `EXC_RETURN` in `LR` and branch to the handler.
    ///
    /// The stacked frame is the architectural one (`r0-r3, r12, lr, pc,
    /// xpsr`), aligned down to eight bytes with the alignment flag in `xPSR`
    /// bit 9 the way hardware records it.
    pub fn enter_exception<B: Bus>(&mut self, bus: &mut B, vector: u16) -> Result<(), Fault> {
        let frame = [
            self.r[0], self.r[1], self.r[2], self.r[3], self.r[12], self.r[14], self.r[15],
            self.xpsr,
        ];
        // Always MSP in handler mode, and handler mode is what this call
        // establishes.
        let mut sp = self.msp;
        if !sp.is_multiple_of(8) {
            sp &= !7;
            self.xpsr |= 1 << 9;
        }
        sp -= 32;
        for (i, word) in frame.iter().enumerate() {
            bus.write32(sp + 4 * i as u32, *word)?;
        }
        self.msp = sp;
        self.r[13] = sp;
        self.r[14] = EXC_RETURN_MSP;
        self.exception = vector;
        self.xpsr = (self.xpsr & !psr::IPSR_MASK) | u32::from(vector & psr::IPSR_MASK as u16);
        self.xpsr |= psr::T;
        let vtor = bus.read32(0xe000_ed08)? & !3;
        self.r[15] = bus.read32(vtor + u32::from(vector) * 4)? & !1;
        self.waiting = false;
        Ok(())
    }

    /// Leave an exception through `EXC_RETURN`: pop the frame and resume the
    /// thread, or return to a nested handler.
    pub fn return_from_exception<B: Bus>(
        &mut self,
        bus: &mut B,
        exc_return: u32,
    ) -> Result<(), Fault> {
        // Bit 2 says the frame is on PSP; bit 3 clear means it returns to a
        // handler rather than to the thread. v1 has no nested handlers, so a
        // frame is always popped back to thread mode.
        let use_psp = exc_return & 0x4 != 0;
        let mut sp = if use_psp { self.psp } else { self.msp };
        let mut frame = [0u32; 8];
        for (i, word) in frame.iter_mut().enumerate() {
            *word = bus.read32(sp + 4 * i as u32)?;
        }
        let aligned = sp;
        sp += 32 + if frame[7] & (1 << 9) != 0 { 4 } else { 0 };
        if use_psp {
            self.psp = sp;
        } else {
            self.msp = sp;
        }
        self.r[0] = frame[0];
        self.r[1] = frame[1];
        self.r[2] = frame[2];
        self.r[3] = frame[3];
        self.r[12] = frame[4];
        self.r[14] = frame[5];
        self.r[15] = frame[6] & !1;
        self.xpsr = (frame[7] & !psr::IPSR_MASK) | psr::T;
        self.exception = 0;
        let _ = aligned;
        self.r[13] = if self.control & 2 != 0 {
            self.psp
        } else {
            self.msp
        };
        Ok(())
    }
}

/// The four shifts a data-processing instruction can perform. The immediate
/// forms never use `Ror`, which has no immediate encoding in ARMv6-M.
#[derive(Clone, Copy)]
enum Shift {
    Lsl,
    Lsr,
    Asr,
    Ror,
}

/// Shift by an immediate, returning the result and the carry the instruction
/// should install (or `None` when the shift is defined to leave `C` alone).
fn shift_imm(value: u32, imm: u32, kind: Shift) -> (u32, Option<bool>) {
    match kind {
        Shift::Lsl => {
            if imm == 0 {
                (value, None)
            } else {
                (
                    value.wrapping_shl(imm),
                    Some((value >> (32 - imm)) & 1 != 0),
                )
            }
        }
        Shift::Lsr => {
            // An encoded shift of zero means thirty-two.
            let n = if imm == 0 { 32 } else { imm };
            if n == 32 {
                (0, Some(value & 0x8000_0000 != 0))
            } else {
                (value >> n, Some((value >> (n - 1)) & 1 != 0))
            }
        }
        Shift::Asr => {
            let n = if imm == 0 { 32 } else { imm };
            if n == 32 {
                (
                    ((value as i32) >> 31) as u32,
                    Some(value & 0x8000_0000 != 0),
                )
            } else {
                (
                    ((value as i32) >> n) as u32,
                    Some((value >> (n - 1)) & 1 != 0),
                )
            }
        }
        Shift::Ror => (value.rotate_right(imm & 31), None),
    }
}

/// Shift by a register's low byte. Shifts of zero leave the carry unchanged,
/// while shifts of thirty-two or more produce zero or the sign fill.
fn shift_reg(value: u32, amount: u32, kind: Shift) -> (u32, Option<bool>) {
    let n = amount & 0xff;
    match kind {
        Shift::Lsl => match n {
            0 => (value, None),
            1..=31 => (value << n, Some((value >> (32 - n)) & 1 != 0)),
            32 => (0, Some(value & 1 != 0)),
            _ => (0, Some(false)),
        },
        Shift::Lsr => match n {
            0 => (value, None),
            1..=31 => (value >> n, Some((value >> (n - 1)) & 1 != 0)),
            32 => (0, Some(value & 0x8000_0000 != 0)),
            _ => (0, Some(false)),
        },
        Shift::Asr => match n {
            0 => (value, None),
            1..=31 => (
                ((value as i32) >> n) as u32,
                Some((value >> (n - 1)) & 1 != 0),
            ),
            _ => (
                ((value as i32) >> 31) as u32,
                Some(value & 0x8000_0000 != 0),
            ),
        },
        Shift::Ror => {
            if n == 0 {
                (value, None)
            } else {
                (
                    value.rotate_right(n & 31),
                    Some((value >> ((n - 1) & 31)) & 1 != 0),
                )
            }
        }
    }
}

/// The `EXC_RETURN` values ARMv6-M recognises as "return from exception".
fn is_exc_return(value: u32) -> bool {
    matches!(value, 0xffff_fff1 | 0xffff_fff9 | 0xffff_fffd)
}

fn sx8(value: u32) -> u32 {
    (value as u8 as i8) as i32 as u32
}

fn sx16(value: u32) -> u32 {
    (value as u16 as i16) as i32 as u32
}

/// A load with the architectural alignment rule applied before the bus is
/// asked, so an unaligned word or halfword faults regardless of the backend.
fn load<B: Bus>(bus: &mut B, addr: u32, width: u32) -> Result<u32, Fault> {
    match width {
        1 => bus.read8(addr),
        2 if addr & 1 == 0 => bus.read16(addr),
        4 if addr & 3 == 0 => bus.read32(addr),
        _ => Err(Fault::Bus { addr, write: false }),
    }
}

fn store<B: Bus>(bus: &mut B, addr: u32, value: u32, width: u32) -> Result<(), Fault> {
    match width {
        1 => bus.write8(addr, value),
        2 if addr & 1 == 0 => bus.write16(addr, value),
        4 if addr & 3 == 0 => bus.write32(addr, value),
        _ => Err(Fault::Bus { addr, write: true }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOW_SIZE: usize = 0x0002_0000;
    const SRAM_BASE: u32 = 0x2000_0000;
    const SRAM_SIZE: usize = 0x0001_0000;

    /// Two flat regions, the low one for code and literal data and the high one
    /// for the stack, with the same alignment rules as the real bus.
    struct TestBus {
        low: Vec<u8>,
        sram: Vec<u8>,
    }

    impl TestBus {
        fn new() -> Self {
            Self {
                low: vec![0; LOW_SIZE],
                sram: vec![0; SRAM_SIZE],
            }
        }

        fn region(&self, addr: u32, len: usize) -> Option<&[u8]> {
            if (addr as usize) < LOW_SIZE {
                self.low.get(addr as usize..addr as usize + len)
            } else if (SRAM_BASE..SRAM_BASE + SRAM_SIZE as u32).contains(&addr) {
                let offset = (addr - SRAM_BASE) as usize;
                self.sram.get(offset..offset + len)
            } else {
                None
            }
        }

        fn region_mut(&mut self, addr: u32, len: usize) -> Option<&mut [u8]> {
            if (addr as usize) < LOW_SIZE {
                self.low.get_mut(addr as usize..addr as usize + len)
            } else if (SRAM_BASE..SRAM_BASE + SRAM_SIZE as u32).contains(&addr) {
                let offset = (addr - SRAM_BASE) as usize;
                self.sram.get_mut(offset..offset + len)
            } else {
                None
            }
        }

        fn place(&mut self, addr: u32, bytes: &[u8]) {
            let slice = self
                .region_mut(addr, bytes.len())
                .expect("placement inside mapped memory");
            slice.copy_from_slice(bytes);
        }
    }

    impl Bus for TestBus {
        fn read8(&mut self, addr: u32) -> Result<u32, Fault> {
            self.region(addr, 1)
                .map(|bytes| u32::from(bytes[0]))
                .ok_or(Fault::Bus { addr, write: false })
        }

        fn read16(&mut self, addr: u32) -> Result<u32, Fault> {
            if addr & 1 != 0 {
                return Err(Fault::Bus { addr, write: false });
            }
            let bytes = self
                .region(addr, 2)
                .ok_or(Fault::Bus { addr, write: false })?;
            Ok(u32::from(u16::from_le_bytes([bytes[0], bytes[1]])))
        }

        fn read32(&mut self, addr: u32) -> Result<u32, Fault> {
            // Exception entry reads VTOR from the private peripheral bus; this
            // harness puts the vector table at zero.
            if addr == 0xe000_ed08 {
                return Ok(0);
            }
            if addr & 3 != 0 {
                return Err(Fault::Bus { addr, write: false });
            }
            let bytes = self
                .region(addr, 4)
                .ok_or(Fault::Bus { addr, write: false })?;
            Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }

        fn write8(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
            self.region_mut(addr, 1)
                .map(|bytes| bytes[0] = value as u8)
                .ok_or(Fault::Bus { addr, write: true })
        }

        fn write16(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
            if addr & 1 != 0 {
                return Err(Fault::Bus { addr, write: true });
            }
            let bytes = self
                .region_mut(addr, 2)
                .ok_or(Fault::Bus { addr, write: true })?;
            bytes.copy_from_slice(&(value as u16).to_le_bytes());
            Ok(())
        }

        fn write32(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
            if addr & 3 != 0 {
                return Err(Fault::Bus { addr, write: true });
            }
            let bytes = self
                .region_mut(addr, 4)
                .ok_or(Fault::Bus { addr, write: true })?;
            bytes.copy_from_slice(&value.to_le_bytes());
            Ok(())
        }
    }

    /// The little-endian bytes of one 16-bit instruction halfword.
    fn hw(x: u16) -> [u8; 2] {
        x.to_le_bytes()
    }

    fn core_at(pc: u32) -> Core {
        let mut core = Core::new();
        core.r[15] = pc;
        core
    }

    #[test]
    fn adds_produces_a_carry() {
        let mut bus = TestBus::new();
        bus.place(0x100, &hw(0x1888)); // adds r0, r1, r2
        let mut core = core_at(0x100);
        core.r[1] = 0xffff_ffff;
        core.r[2] = 1;

        assert_eq!(core.step(&mut bus), Ok(1));
        assert_eq!(core.r[0], 0);
        assert_ne!(core.xpsr & psr::C, 0, "carry out");
        assert_ne!(core.xpsr & psr::Z, 0, "zero result");
        assert_eq!(core.r[15], 0x102);
    }

    #[test]
    fn subs_produces_a_borrow() {
        let mut bus = TestBus::new();
        bus.place(0x100, &hw(0x1a88)); // subs r0, r1, r2
        let mut core = core_at(0x100);
        core.r[1] = 0;
        core.r[2] = 1;

        assert_eq!(core.step(&mut bus), Ok(1));
        assert_eq!(core.r[0], 0xffff_ffff);
        assert_eq!(core.xpsr & psr::C, 0, "borrow clears carry");
        assert_ne!(core.xpsr & psr::N, 0, "negative result");
        assert_eq!(core.xpsr & psr::Z, 0);
    }

    #[test]
    fn ldr_literal_is_pc_relative() {
        let mut bus = TestBus::new();
        bus.place(0x100, &hw(0x4801)); // ldr r0, [pc, #4]
        bus.place(0x108, &0xdead_beefu32.to_le_bytes());
        let mut core = core_at(0x100);

        assert_eq!(core.step(&mut bus), Ok(1));
        assert_eq!(core.r[0], 0xdead_beef);
        assert_eq!(core.r[15], 0x102);
    }

    #[test]
    fn push_and_pop_round_trip() {
        let mut bus = TestBus::new();
        bus.place(0x100, &hw(0xb403)); // push {r0, r1}
        bus.place(0x102, &hw(0xbc0c)); // pop {r2, r3}
        let mut core = core_at(0x100);
        core.msp = 0x2000_1000;
        core.r[13] = core.msp;
        core.r[0] = 0x1111_1111;
        core.r[1] = 0x2222_2222;

        assert_eq!(core.step(&mut bus), Ok(3)); // one cycle plus two registers
        assert_eq!(core.r[13], 0x2000_0ff8);
        assert_eq!(core.msp, 0x2000_0ff8, "the MSP bank tracks the active SP");
        assert_eq!(core.step(&mut bus), Ok(3));
        assert_eq!(core.r[2], 0x1111_1111);
        assert_eq!(core.r[3], 0x2222_2222);
        assert_eq!(core.r[13], 0x2000_1000);
    }

    #[test]
    fn conditional_branch_taken_and_not_taken() {
        let mut bus = TestBus::new();
        bus.place(0x100, &hw(0xd002)); // beq +4
        let mut core = core_at(0x100);
        core.xpsr |= psr::Z;

        assert_eq!(core.step(&mut bus), Ok(2));
        assert_eq!(core.r[15], 0x108);

        let mut bus = TestBus::new();
        bus.place(0x100, &hw(0xd002));
        let mut core = core_at(0x100);
        core.xpsr &= !psr::Z;

        assert_eq!(core.step(&mut bus), Ok(1));
        assert_eq!(core.r[15], 0x102);
    }

    #[test]
    fn bl_branches_and_sets_the_link_register() {
        let mut bus = TestBus::new();
        bus.place(0x100, &hw(0xf000));
        bus.place(0x102, &hw(0xf87e)); // bl 0x200
        let mut core = core_at(0x100);

        assert_eq!(core.step(&mut bus), Ok(3));
        assert_eq!(core.r[15], 0x200);
        assert_eq!(core.r[14], 0x105, "return address carries the Thumb bit");
    }

    #[test]
    fn muls_is_single_cycle() {
        let mut bus = TestBus::new();
        bus.place(0x100, &hw(0x4348)); // muls r0, r1
        let mut core = core_at(0x100);
        core.r[0] = 6;
        core.r[1] = 7;

        assert_eq!(core.step(&mut bus), Ok(1));
        assert_eq!(core.r[0], 42);
        assert_eq!(core.xpsr & psr::Z, 0);
    }

    #[test]
    fn rev_reverses_bytes() {
        let mut bus = TestBus::new();
        bus.place(0x100, &hw(0xba08)); // rev r0, r1
        let mut core = core_at(0x100);
        core.r[1] = 0x1122_3344;

        assert_eq!(core.step(&mut bus), Ok(1));
        assert_eq!(core.r[0], 0x4433_2211);
    }

    #[test]
    fn cbz_branches_only_on_zero() {
        let mut bus = TestBus::new();
        bus.place(0x100, &hw(0xb110)); // cbz r0, +4
        let mut core = core_at(0x100);
        core.r[0] = 0;

        assert_eq!(core.step(&mut bus), Ok(2));
        assert_eq!(core.r[15], 0x108);

        let mut bus = TestBus::new();
        bus.place(0x100, &hw(0xb110));
        let mut core = core_at(0x100);
        core.r[0] = 1;

        assert_eq!(core.step(&mut bus), Ok(1));
        assert_eq!(core.r[15], 0x102);
    }

    #[test]
    fn undefined_encoding_is_reported() {
        let mut bus = TestBus::new();
        bus.place(0x100, &hw(0xde00)); // UDF #0
        let mut core = core_at(0x100);

        assert_eq!(
            core.step(&mut bus),
            Err(Fault::Undefined {
                pc: 0x100,
                opcode: 0xde00
            })
        );
    }

    #[test]
    fn sp_write_targets_the_selected_bank() {
        let mut bus = TestBus::new();
        bus.place(0x100, &hw(0x4685)); // mov sp, r0
        let mut core = core_at(0x100);
        core.msp = 0x2000_1000;
        core.psp = 0x2000_2000;
        core.control = 2; // SPSEL = PSP in thread mode
        core.r[13] = core.psp;
        core.r[0] = 0x2000_3000;

        assert_eq!(core.step(&mut bus), Ok(1));
        assert_eq!(core.r[13], 0x2000_3000);
        assert_eq!(core.psp, 0x2000_3000, "PSP is the active bank");
        assert_eq!(core.msp, 0x2000_1000, "MSP is untouched");
    }

    #[test]
    fn bx_lr_pops_an_exception_frame() {
        let mut bus = TestBus::new();
        let handler: u32 = 0x200;
        // Vector 11 (SVC) points at the handler; the handler returns with
        // `bx lr`, where the core left `EXC_RETURN` on entry.
        bus.place(0x2c, &handler.to_le_bytes());
        bus.place(handler, &hw(0x4770)); // bx lr

        let mut core = core_at(0x100);
        core.msp = 0x2000_1000;
        core.r[13] = core.msp;
        core.r[0] = 0x1111_1111;
        core.r[1] = 0x2222_2222;
        core.r[2] = 0x3333_3333;
        core.r[3] = 0x4444_4444;
        core.r[12] = 0x5555_5555;
        core.r[14] = 0x6666_6666;

        let pc = core.r[15];
        let sp = core.r[13];
        let xpsr = core.xpsr;

        core.enter_exception(&mut bus, 11).unwrap();
        assert_eq!(core.r[15], handler);
        assert_eq!(core.r[14], EXC_RETURN_MSP);
        assert!(core.in_handler());

        // `bx lr` with LR holding EXC_RETURN is a frame pop, not a branch into
        // the reserved 0xfffffff0 page.
        assert_eq!(core.step(&mut bus), Ok(2));
        assert_eq!(core.r[15], pc);
        assert_eq!(core.r[13], sp);
        assert_eq!(core.msp, sp);
        assert_eq!(core.xpsr, xpsr);
        assert!(!core.in_handler());
        assert_eq!(core.r[0], 0x1111_1111);
        assert_eq!(core.r[12], 0x5555_5555);
        assert_eq!(core.r[14], 0x6666_6666);
    }

    #[test]
    fn pop_pc_pops_an_exception_frame() {
        let mut bus = TestBus::new();
        let handler: u32 = 0x200;
        bus.place(0x2c, &handler.to_le_bytes());
        // The pattern gcc emits: save LR on entry, restore it into PC to
        // return, so the `EXC_RETURN` arrives through POP rather than BX.
        bus.place(handler, &hw(0xb500)); // push {lr}
        bus.place(handler + 2, &hw(0xbd00)); // pop {pc}

        let mut core = core_at(0x100);
        core.msp = 0x2000_1000;
        core.r[13] = core.msp;
        core.r[0] = 0x1111_1111;
        core.r[1] = 0x2222_2222;
        core.r[2] = 0x3333_3333;
        core.r[3] = 0x4444_4444;
        core.r[12] = 0x5555_5555;
        core.r[14] = 0x6666_6666;

        let pc = core.r[15];
        let sp = core.r[13];
        let xpsr = core.xpsr;

        core.enter_exception(&mut bus, 11).unwrap();
        assert_eq!(core.step(&mut bus), Ok(2)); // push {lr}
        assert!(core.in_handler());

        assert_eq!(core.step(&mut bus), Ok(2)); // pop {pc} is the return
        assert_eq!(core.r[15], pc);
        assert_eq!(core.r[13], sp);
        assert_eq!(core.msp, sp);
        assert_eq!(core.xpsr, xpsr);
        assert!(!core.in_handler());
        assert_eq!(core.r[0], 0x1111_1111);
        assert_eq!(core.r[14], 0x6666_6666);
    }
}
