//! The QingKe V2A core: RV32EC registers, traps and the decoder.
//!
//! RV32E is RV32I with only the sixteen low integer registers, and `C` adds the
//! compressed encodings. There is no `M`, `A`, `F` or `D` extension on this
//! part, so a multiply encoding is an illegal instruction rather than a
//! silently missing operation. The decoder is a match over the opcode fields,
//! the same shape as the ATmega and RP2040 cores, with two rules that matter
//! for an RV32E part:
//!
//! * a register field naming `x16`–`x31` is illegal (RV32E has no such
//!   register), so it faults instead of quietly wrapping;
//! * an encoding the core does not define faults with its pc and its word, so
//!   a hand-built program reports the instruction that went wrong.
//!
//! # Timing
//!
//! Every instruction is one cycle except the ones where a real pipeline
//! stalls: loads and stores take two, a taken branch and `jal`/`jalr` take two
//! (a taken branch redirects the fetch), and `mret` takes two. A compressed
//! instruction costs the same as its 32-bit form, which is the simplification
//! the small core makes in silicon too.
//!
//! # Traps
//!
//! The core owns the machine-mode CSRs (`mstatus`, `mtvec`, `mepc`, `mcause`,
//! `mtval`, `mie`, `mip`, `mscratch`) and the trap entry/return handshake. The
//! interrupt controller is outside it: [`Core::enter_interrupt`] is called by
//! the machine once the PFIC has chosen a source, and it reads the vector
//! table through the same [`Bus`] the instructions use.
//!
//! The vector table lives at the base `mtvec` names, entry 0 is the reset
//! vector and entry `n + 1` is interrupt source `n`, each entry a 32-bit
//! address. That is the layout WCH's startup code lays down (a 32-bit word per
//! entry) and it is what this model documents rather than the more usual
//! RISC-V `mtvec` *mode 1* table of instructions.

use core::fmt;

/// A core-level failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The decoder met an encoding RV32EC does not define.
    IllegalInstruction { pc: u32, word: u32 },
    /// A register field named a register RV32E does not have.
    IllegalRegister {
        pc: u32,
        field: &'static str,
        index: u8,
    },
    /// A fetch, load or store at an address the width forbids.
    Alignment { pc: u32, addr: u32, width: u32 },
    /// A fetch, load or store outside every mapped region.
    Bus { pc: u32, addr: u32, write: bool },
    /// `ecall` or `ebreak`: there is no environment to call in a simulator.
    Halted { pc: u32 },
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::IllegalInstruction { pc, word } => {
                write!(f, "illegal instruction {word:#010x} at {pc:#010x} (RV32EC)")
            }
            Fault::IllegalRegister { pc, field, index } => write!(
                f,
                "illegal {field} x{index} at {pc:#010x}: RV32E has x0-x15 only"
            ),
            Fault::Alignment { pc, addr, width } => write!(
                f,
                "{width}-byte access at unaligned {addr:#010x} (pc {pc:#010x})"
            ),
            Fault::Bus { pc, addr, write } => write!(
                f,
                "{} of {addr:#010x} has no target (pc {pc:#010x})",
                if *write { "write" } else { "read" }
            ),
            Fault::Halted { pc } => write!(f, "core halted at {pc:#010x}"),
        }
    }
}

/// What the core needs from the rest of the chip.
pub trait Bus {
    fn read8(&mut self, addr: u32) -> Result<u32, Fault>;
    fn read16(&mut self, addr: u32) -> Result<u32, Fault>;
    fn read32(&mut self, addr: u32) -> Result<u32, Fault>;
    fn write8(&mut self, addr: u32, value: u32) -> Result<(), Fault>;
    fn write16(&mut self, addr: u32, value: u32) -> Result<(), Fault>;
    fn write32(&mut self, addr: u32, value: u32) -> Result<(), Fault>;
}

/// `mstatus` bit positions.
pub mod mstatus {
    pub const MIE: u32 = 1 << 3;
    pub const MPIE: u32 = 1 << 7;
    pub const MPP_SHIFT: u32 = 11;
    pub const MPP_MASK: u32 = 0b11 << MPP_SHIFT;
    pub const VALID: u32 = MIE | MPIE | MPP_MASK;
}

/// The CSR numbers the core understands.
pub mod csr {
    pub const MSTATUS: u32 = 0x300;
    pub const MISA: u32 = 0x301;
    pub const MIE: u32 = 0x304;
    pub const MTVEC: u32 = 0x305;
    pub const MSCRATCH: u32 = 0x340;
    pub const MEPC: u32 = 0x341;
    pub const MCAUSE: u32 = 0x342;
    pub const MTVAL: u32 = 0x343;
    pub const MIP: u32 = 0x344;
    pub const MVENDORID: u32 = 0xf11;
    pub const MARCHID: u32 = 0xf12;
    pub const MIMPID: u32 = 0xf13;
    pub const MHARTID: u32 = 0xf14;
}

/// `misa` for RV32EC: MXL = 32-bit, the `E` base and the `C` extension.
pub const MISA_RV32EC: u32 = 0x4000_0114;

/// The register file and trap state.
#[derive(Clone, Debug)]
pub struct Core {
    /// `x0` through `x31`. RV32E leaves `x16`–`x31` at zero; the decoder
    /// refuses to name them, but the array is kept full so the debug surface
    /// has the 32-word shape every backend shares.
    pub regs: [u32; 32],
    pub pc: u32,
    pub mstatus: u32,
    pub mtvec: u32,
    pub mepc: u32,
    pub mcause: u32,
    pub mtval: u32,
    pub mie: u32,
    pub mip: u32,
    pub mscratch: u32,
    /// Set by `wfi`; the machine clears it when an interrupt or event arrives.
    pub waiting: bool,
    /// The priority of the handler currently executing, with the value it
    /// replaced saved so `mret` restores it. 4 is "no handler", above the
    /// two-bit priority range, so every source may pre-empt it.
    pub current_priority: u8,
    pub priority_stack: Vec<u8>,
}

impl Default for Core {
    fn default() -> Self {
        Self::new()
    }
}

impl Core {
    pub const NO_PRIORITY: u8 = 4;

    pub fn new() -> Self {
        Core {
            regs: [0; 32],
            pc: 0,
            mstatus: 0,
            mtvec: 0,
            mepc: 0,
            mcause: 0,
            mtval: 0,
            mie: 0,
            mip: 0,
            mscratch: 0,
            waiting: false,
            current_priority: Self::NO_PRIORITY,
            priority_stack: Vec::new(),
        }
    }

    pub fn reset<B: Bus>(&mut self, bus: &mut B) -> Result<(), Fault> {
        *self = Core::new();
        // The CH32V003 has no stack pointer in hardware: entry 0 of the
        // vector table is the first instruction's address and the startup code
        // sets `sp` itself.
        self.pc = bus.read32(0)? & !1;
        Ok(())
    }

    pub fn pc(&self) -> u32 {
        self.pc
    }

    pub fn reg(&self, index: usize) -> u32 {
        if index == 0 {
            0
        } else {
            self.regs.get(index).copied().unwrap_or(0)
        }
    }

    pub fn set_reg(&mut self, index: usize, value: u32) {
        if index != 0
            && let Some(slot) = self.regs.get_mut(index)
        {
            *slot = value;
        }
    }

    /// Whether a configurable interrupt may be taken.
    pub fn accepts_interrupt(&self) -> bool {
        self.mstatus & mstatus::MIE != 0
    }

    /// Enter interrupt `irq` at `priority`, saving the interrupted pc.
    ///
    /// The vector table entry for source `n` is at `mtvec + 4 * (n + 1)`; the
    /// reset vector is entry 0.
    pub fn enter_interrupt<B: Bus>(
        &mut self,
        bus: &mut B,
        irq: u16,
        priority: u8,
    ) -> Result<(), Fault> {
        self.mepc = self.pc;
        self.mcause = 0x8000_0000 | u32::from(irq);
        self.mtval = 0;
        // MPIE remembers whether interrupts were on, MIE turns them off and
        // MPP records machine mode.
        let mie = self.mstatus & mstatus::MIE != 0;
        self.mstatus = (self.mstatus & !(mstatus::MIE | mstatus::MPIE | mstatus::MPP_MASK))
            | if mie { mstatus::MPIE } else { 0 }
            | (3 << mstatus::MPP_SHIFT);
        self.waiting = false;
        let base = self.mtvec & !3;
        self.pc = bus.read32(base.wrapping_add(4 * (u32::from(irq) + 1)))? & !1;
        self.priority_stack.push(self.current_priority);
        self.current_priority = priority;
        Ok(())
    }

    /// Leave a trap through `mret`.
    fn mret(&mut self) -> u64 {
        let mpie = self.mstatus & mstatus::MPIE != 0;
        self.mstatus = (self.mstatus & !(mstatus::MIE | mstatus::MPIE | mstatus::MPP_MASK))
            | if mpie { mstatus::MIE } else { 0 }
            | mstatus::MPIE;
        self.pc = self.mepc & !1;
        self.current_priority = self.priority_stack.pop().unwrap_or(Self::NO_PRIORITY);
        self.waiting = false;
        2
    }

    fn csr_read(&self, address: u32) -> u32 {
        match address {
            csr::MSTATUS => self.mstatus & mstatus::VALID,
            csr::MISA => MISA_RV32EC,
            csr::MIE => self.mie,
            csr::MTVEC => self.mtvec,
            csr::MSCRATCH => self.mscratch,
            csr::MEPC => self.mepc,
            csr::MCAUSE => self.mcause,
            csr::MTVAL => self.mtval,
            csr::MIP => self.mip,
            csr::MVENDORID | csr::MARCHID | csr::MIMPID | csr::MHARTID => 0,
            // An unknown CSR reads as zero, the way a core ignores a CSR it
            // does not implement. Firmware that probes a vendor register
            // therefore sees "not there" instead of failing to boot.
            _ => 0,
        }
    }

    fn csr_write(&mut self, address: u32, value: u32) {
        match address {
            csr::MSTATUS => self.mstatus = value & mstatus::VALID,
            csr::MIE => self.mie = value,
            csr::MTVEC => self.mtvec = value,
            csr::MSCRATCH => self.mscratch = value,
            csr::MEPC => self.mepc = value,
            csr::MCAUSE => self.mcause = value,
            csr::MTVAL => self.mtval = value,
            csr::MIP => self.mip = value,
            // `misa`, the ID registers and anything unknown are read-only.
            _ => {}
        }
    }

    /// Execute one instruction and return the cycles it consumed.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> Result<u64, Fault> {
        let pc = self.pc;
        if pc & 1 != 0 {
            return Err(Fault::Alignment {
                pc,
                addr: pc,
                width: 2,
            });
        }
        let low = bus.read16(pc)? as u16;
        if low & 0b11 != 0b11 {
            self.pc = pc.wrapping_add(2);
            return self.execute_compressed(bus, pc, low);
        }
        let high = bus.read16(pc.wrapping_add(2))? as u16;
        let word = u32::from(low) | (u32::from(high) << 16);
        self.pc = pc.wrapping_add(4);
        self.execute32(bus, pc, word)
    }

    fn check_reg(&self, pc: u32, field: &'static str, index: usize) -> Result<(), Fault> {
        if index >= 16 {
            Err(Fault::IllegalRegister {
                pc,
                field,
                index: index as u8,
            })
        } else {
            Ok(())
        }
    }

    fn execute32<B: Bus>(&mut self, bus: &mut B, pc: u32, word: u32) -> Result<u64, Fault> {
        let illegal = Fault::IllegalInstruction { pc, word };
        let opcode = word & 0x7f;
        let rd = ((word >> 7) & 0x1f) as usize;
        let funct3 = (word >> 12) & 7;
        let rs1 = ((word >> 15) & 0x1f) as usize;
        let rs2 = ((word >> 20) & 0x1f) as usize;
        let funct7 = word >> 25;
        let imm_i = sext(word >> 20, 12);
        let imm_s = sext(((word >> 25) << 5) | ((word >> 7) & 0x1f), 12);
        let imm_b = sext(
            (((word >> 31) & 1) << 12)
                | (((word >> 7) & 1) << 11)
                | (((word >> 25) & 0x3f) << 5)
                | (((word >> 8) & 0xf) << 1),
            13,
        );
        let imm_j = sext(
            (((word >> 31) & 1) << 20)
                | (((word >> 12) & 0xff) << 12)
                | (((word >> 20) & 1) << 11)
                | (((word >> 21) & 0x3ff) << 1),
            21,
        );
        let imm_u = word & 0xffff_f000;

        match opcode {
            // LUI / AUIPC
            0b011_0111 => {
                self.check_reg(pc, "rd", rd)?;
                self.set_reg(rd, imm_u);
                Ok(1)
            }
            0b001_0111 => {
                self.check_reg(pc, "rd", rd)?;
                self.set_reg(rd, pc.wrapping_add(imm_u));
                Ok(1)
            }
            // JAL
            0b110_1111 => {
                self.check_reg(pc, "rd", rd)?;
                self.set_reg(rd, pc.wrapping_add(4));
                self.pc = pc.wrapping_add(imm_j) & !1;
                Ok(2)
            }
            // JALR
            0b110_0111 => {
                self.check_reg(pc, "rd", rd)?;
                self.check_reg(pc, "rs1", rs1)?;
                if funct3 != 0 {
                    return Err(illegal);
                }
                let target = self.reg(rs1).wrapping_add(imm_i) & !1;
                self.set_reg(rd, pc.wrapping_add(4));
                self.pc = target;
                Ok(2)
            }
            // Conditional branches
            0b110_0011 => {
                self.check_reg(pc, "rs1", rs1)?;
                self.check_reg(pc, "rs2", rs2)?;
                let a = self.reg(rs1);
                let b = self.reg(rs2);
                let taken = match funct3 {
                    0b000 => a == b,
                    0b001 => a != b,
                    0b100 => (a as i32) < (b as i32),
                    0b101 => (a as i32) >= (b as i32),
                    0b110 => a < b,
                    0b111 => a >= b,
                    _ => return Err(illegal),
                };
                if taken {
                    self.pc = pc.wrapping_add(imm_b) & !1;
                    Ok(2)
                } else {
                    Ok(1)
                }
            }
            // Loads
            0b000_0011 => {
                self.check_reg(pc, "rd", rd)?;
                self.check_reg(pc, "rs1", rs1)?;
                let addr = self.reg(rs1).wrapping_add(imm_i);
                let value = match funct3 {
                    0b000 => load(bus, pc, addr, 1).map(sx8)?,
                    0b001 => load(bus, pc, addr, 2).map(sx16)?,
                    0b010 => load(bus, pc, addr, 4)?,
                    0b100 => load(bus, pc, addr, 1)?,
                    0b101 => load(bus, pc, addr, 2)?,
                    _ => return Err(illegal),
                };
                self.set_reg(rd, value);
                Ok(2)
            }
            // Stores
            0b010_0011 => {
                self.check_reg(pc, "rs1", rs1)?;
                self.check_reg(pc, "rs2", rs2)?;
                let addr = self.reg(rs1).wrapping_add(imm_s);
                let value = self.reg(rs2);
                match funct3 {
                    0b000 => bus.write8(addr, value).map_err(|_| Fault::Bus {
                        pc,
                        addr,
                        write: true,
                    })?,
                    0b001 => store(bus, pc, addr, value, 2)?,
                    0b010 => store(bus, pc, addr, value, 4)?,
                    _ => return Err(illegal),
                }
                Ok(2)
            }
            // OP-IMM
            0b001_0011 => {
                self.check_reg(pc, "rd", rd)?;
                self.check_reg(pc, "rs1", rs1)?;
                let a = self.reg(rs1);
                let value = match funct3 {
                    0b000 => a.wrapping_add(imm_i),
                    0b010 => ((a as i32) < (imm_i as i32)) as u32,
                    0b011 => (a < imm_i) as u32,
                    0b100 => a ^ imm_i,
                    0b110 => a | imm_i,
                    0b111 => a & imm_i,
                    0b001 => {
                        if funct7 != 0 {
                            return Err(illegal);
                        }
                        a << ((word >> 20) & 0x1f)
                    }
                    0b101 => {
                        let shamt = (word >> 20) & 0x1f;
                        match funct7 {
                            0x00 => a >> shamt,
                            0x20 => ((a as i32) >> shamt) as u32,
                            _ => return Err(illegal),
                        }
                    }
                    _ => unreachable!(),
                };
                self.set_reg(rd, value);
                Ok(1)
            }
            // OP: no M extension, so funct7 0000001 is illegal by construction.
            0b011_0011 => {
                self.check_reg(pc, "rd", rd)?;
                self.check_reg(pc, "rs1", rs1)?;
                self.check_reg(pc, "rs2", rs2)?;
                let a = self.reg(rs1);
                let b = self.reg(rs2);
                let value = match (funct7, funct3) {
                    (0x00, 0b000) => a.wrapping_add(b),
                    (0x20, 0b000) => a.wrapping_sub(b),
                    (0x00, 0b001) => a << (b & 0x1f),
                    (0x00, 0b010) => ((a as i32) < (b as i32)) as u32,
                    (0x00, 0b011) => (a < b) as u32,
                    (0x00, 0b100) => a ^ b,
                    (0x00, 0b101) => a >> (b & 0x1f),
                    (0x20, 0b101) => ((a as i32) >> (b & 0x1f)) as u32,
                    (0x00, 0b110) => a | b,
                    (0x00, 0b111) => a & b,
                    _ => return Err(illegal),
                };
                self.set_reg(rd, value);
                Ok(1)
            }
            // FENCE and FENCE.I are ordering hints with no modelled effect.
            0b000_1111 => match funct3 {
                0b000 | 0b001 => Ok(1),
                _ => Err(illegal),
            },
            // SYSTEM
            0b111_0011 => self.execute_system(pc, word, funct3, rd, rs1, rs2),
            _ => Err(illegal),
        }
    }

    fn execute_system(
        &mut self,
        pc: u32,
        word: u32,
        funct3: u32,
        rd: usize,
        rs1: usize,
        _rs2: usize,
    ) -> Result<u64, Fault> {
        let illegal = Fault::IllegalInstruction { pc, word };
        let immediate = word >> 20;
        match funct3 {
            0b000 => match immediate {
                0x000 | 0x001 => Err(Fault::Halted { pc }),
                0x302 => Ok(self.mret()),
                0x002 | 0x105 => {
                    // `wfe` and `wfi`: this part has no event register, so
                    // both park until the machine sees an interrupt or event.
                    self.waiting = true;
                    Ok(0)
                }
                _ => Err(illegal),
            },
            0b001 | 0b010 | 0b011 | 0b101 | 0b110 | 0b111 => {
                self.check_reg(pc, "rd", rd)?;
                if funct3 < 0b100 {
                    self.check_reg(pc, "rs1", rs1)?;
                }
                let address = immediate;
                let old = self.csr_read(address);
                let source = if funct3 < 0b100 {
                    self.reg(rs1)
                } else {
                    rs1 as u32
                };
                let write = match funct3 & 0b11 {
                    0b01 => true,
                    0b10 => rs1 != 0,
                    0b11 => rs1 != 0,
                    _ => false,
                };
                if write {
                    let value = match funct3 & 0b11 {
                        0b01 => source,
                        0b10 => old | source,
                        0b11 => old & !source,
                        _ => old,
                    };
                    self.csr_write(address, value);
                }
                self.set_reg(rd, old);
                Ok(1)
            }
            _ => Err(illegal),
        }
    }

    fn execute_compressed<B: Bus>(
        &mut self,
        bus: &mut B,
        pc: u32,
        word: u16,
    ) -> Result<u64, Fault> {
        let illegal = Fault::IllegalInstruction {
            pc,
            word: u32::from(word),
        };
        let quadrant = word & 0b11;
        let funct3 = (word >> 13) & 7;
        match quadrant {
            0b00 => {
                match funct3 {
                    // C.ADDI4SPN
                    0b000 => {
                        let imm = (u32::from((word >> 7) & 0xf) << 6)
                            | (u32::from((word >> 11) & 0x3) << 4)
                            | (u32::from((word >> 5) & 1) << 3)
                            | (u32::from((word >> 6) & 1) << 2);
                        if imm == 0 {
                            return Err(illegal);
                        }
                        let rd = 8 + usize::from((word >> 2) & 0x7);
                        self.set_reg(rd, self.reg(2).wrapping_add(imm));
                        Ok(1)
                    }
                    // C.LW
                    0b010 => {
                        let rd = 8 + usize::from((word >> 2) & 0x7);
                        let rs1 = 8 + usize::from((word >> 7) & 0x7);
                        let offset = (u32::from((word >> 10) & 0x7) << 3)
                            | (u32::from((word >> 6) & 1) << 2)
                            | (u32::from((word >> 5) & 1) << 6);
                        let addr = self.reg(rs1).wrapping_add(offset);
                        let value = load(bus, pc, addr, 4)?;
                        self.set_reg(rd, value);
                        Ok(2)
                    }
                    // C.SW
                    0b110 => {
                        let rs2 = 8 + usize::from((word >> 2) & 0x7);
                        let rs1 = 8 + usize::from((word >> 7) & 0x7);
                        let offset = (u32::from((word >> 10) & 0x7) << 3)
                            | (u32::from((word >> 6) & 1) << 2)
                            | (u32::from((word >> 5) & 1) << 6);
                        let addr = self.reg(rs1).wrapping_add(offset);
                        store(bus, pc, addr, self.reg(rs2), 4)?;
                        Ok(2)
                    }
                    // C.FLD/C.FLW and C.FSD/C.FSW need an FPU this core lacks.
                    0b011 | 0b111 => Err(illegal),
                    _ => Err(illegal),
                }
            }
            0b01 => match funct3 {
                // C.ADDI / C.NOP
                0b000 => {
                    let rd = usize::from((word >> 7) & 0x1f);
                    let imm = sext(u32::from(((word >> 12) & 1) << 5 | ((word >> 2) & 0x1f)), 6);
                    if rd != 0 {
                        self.set_reg(rd, self.reg(rd).wrapping_add(imm));
                    }
                    Ok(1)
                }
                // C.JAL (RV32)
                0b001 => {
                    let offset = jump_offset(word);
                    self.set_reg(1, pc.wrapping_add(2));
                    self.pc = pc.wrapping_add(offset) & !1;
                    Ok(2)
                }
                // C.LI
                0b010 => {
                    let rd = usize::from((word >> 7) & 0x1f);
                    let imm = sext(u32::from(((word >> 12) & 1) << 5 | ((word >> 2) & 0x1f)), 6);
                    if rd != 0 {
                        self.set_reg(rd, imm);
                    }
                    Ok(1)
                }
                // C.ADDI16SP / C.LUI
                0b011 => {
                    let rd = usize::from((word >> 7) & 0x1f);
                    if rd == 2 {
                        let raw = (u32::from((word >> 12) & 1) << 9)
                            | (u32::from((word >> 3) & 0x3) << 7)
                            | (u32::from((word >> 5) & 1) << 6)
                            | (u32::from((word >> 2) & 1) << 5)
                            | (u32::from((word >> 6) & 1) << 4);
                        let imm = sext(raw, 10);
                        if imm == 0 {
                            return Err(illegal);
                        }
                        self.set_reg(2, self.reg(2).wrapping_add(imm));
                        Ok(1)
                    } else if rd != 0 {
                        let raw =
                            (u32::from((word >> 12) & 1) << 5) | u32::from((word >> 2) & 0x1f);
                        let imm = sext(raw << 12, 18);
                        self.set_reg(rd, imm);
                        Ok(1)
                    } else {
                        Err(illegal)
                    }
                }
                // C.SRLI/C.SRAI/C.ANDI/C.SUB...
                0b100 => {
                    let rd = 8 + usize::from((word >> 7) & 0x7);
                    let funct2 = (word >> 10) & 3;
                    let shamt = u32::from((word >> 2) & 0x1f);
                    match funct2 {
                        0b00 | 0b01 => {
                            if word & 0x1000 != 0 {
                                // RV32 has no room for shamt[5].
                                return Err(illegal);
                            }
                            let value = if funct2 == 0 {
                                self.reg(rd) >> shamt
                            } else {
                                ((self.reg(rd) as i32) >> shamt) as u32
                            };
                            self.set_reg(rd, value);
                            Ok(1)
                        }
                        0b10 => {
                            let imm =
                                sext(u32::from(((word >> 12) & 1) << 5 | ((word >> 2) & 0x1f)), 6);
                            self.set_reg(rd, self.reg(rd) & imm);
                            Ok(1)
                        }
                        _ => {
                            if word & 0x1000 != 0 {
                                return Err(illegal);
                            }
                            let rs2 = 8 + usize::from((word >> 2) & 0x7);
                            let value = match (word >> 5) & 3 {
                                0b00 => self.reg(rd).wrapping_sub(self.reg(rs2)),
                                0b01 => self.reg(rd) ^ self.reg(rs2),
                                0b10 => self.reg(rd) | self.reg(rs2),
                                _ => self.reg(rd) & self.reg(rs2),
                            };
                            self.set_reg(rd, value);
                            Ok(1)
                        }
                    }
                }
                // C.J
                0b101 => {
                    let offset = jump_offset(word);
                    self.pc = pc.wrapping_add(offset) & !1;
                    Ok(2)
                }
                // C.BEQZ / C.BNEZ
                0b110 | 0b111 => {
                    let rs1 = 8 + usize::from((word >> 7) & 0x7);
                    let offset = branch_offset(word);
                    let zero = self.reg(rs1) == 0;
                    let taken = if funct3 == 0b110 { zero } else { !zero };
                    if taken {
                        self.pc = pc.wrapping_add(offset) & !1;
                        Ok(2)
                    } else {
                        Ok(1)
                    }
                }
                _ => Err(illegal),
            },
            0b10 => match funct3 {
                // C.SLLI
                0b000 => {
                    let rd = usize::from((word >> 7) & 0x1f);
                    if word & 0x1000 != 0 {
                        return Err(illegal);
                    }
                    let shamt = u32::from((word >> 2) & 0x1f);
                    if rd != 0 {
                        self.set_reg(rd, self.reg(rd) << shamt);
                    }
                    Ok(1)
                }
                // C.FLDSP / C.FLWSP: no FPU.
                0b001 | 0b011 => Err(illegal),
                // C.LWSP
                0b010 => {
                    let rd = usize::from((word >> 7) & 0x1f);
                    if rd == 0 {
                        return Err(illegal);
                    }
                    let offset = (u32::from((word >> 12) & 1) << 5)
                        | (u32::from((word >> 4) & 0x7) << 2)
                        | (u32::from((word >> 2) & 0x3) << 6);
                    let addr = self.reg(2).wrapping_add(offset);
                    let value = load(bus, pc, addr, 4)?;
                    self.set_reg(rd, value);
                    Ok(2)
                }
                // C.JR / C.MV / C.EBREAK / C.JALR / C.ADD
                0b100 => {
                    let rd = usize::from((word >> 7) & 0x1f);
                    let rs2 = usize::from((word >> 2) & 0x1f);
                    match (word >> 12) & 0xf {
                        0b1000 => {
                            if rd == 0 {
                                Err(illegal)
                            } else if rs2 == 0 {
                                self.pc = self.reg(rd) & !1;
                                Ok(2)
                            } else {
                                self.set_reg(rd, self.reg(rs2));
                                Ok(1)
                            }
                        }
                        0b1001 => {
                            if rd == 0 {
                                if rs2 == 0 {
                                    Err(Fault::Halted { pc })
                                } else {
                                    Err(illegal)
                                }
                            } else if rs2 == 0 {
                                let target = self.reg(rd) & !1;
                                self.set_reg(1, pc.wrapping_add(2));
                                self.pc = target;
                                Ok(2)
                            } else {
                                self.set_reg(rd, self.reg(rd).wrapping_add(self.reg(rs2)));
                                Ok(1)
                            }
                        }
                        _ => Err(illegal),
                    }
                }
                // C.SWSP
                0b110 => {
                    let rs2 = usize::from((word >> 2) & 0x1f);
                    let offset =
                        (u32::from((word >> 9) & 0xf) << 2) | (u32::from((word >> 7) & 0x3) << 6);
                    let addr = self.reg(2).wrapping_add(offset);
                    store(bus, pc, addr, self.reg(rs2), 4)?;
                    Ok(2)
                }
                // C.FSWSP: no FPU.
                0b111 => Err(illegal),
                _ => Err(illegal),
            },
            _ => Err(illegal),
        }
    }
}

/// Sign-extend the low `bits` of `value`.
fn sext(value: u32, bits: u32) -> u32 {
    let shift = 32 - bits;
    (((value << shift) as i32) >> shift) as u32
}

fn sx8(value: u32) -> u32 {
    (value as u8 as i8) as i32 as u32
}

fn sx16(value: u32) -> u32 {
    (value as u16 as i16) as i32 as u32
}

/// The 12-bit signed jump displacement of a `C.J`/`C.JAL`.
fn jump_offset(word: u16) -> u32 {
    let raw = (u32::from((word >> 12) & 1) << 11)
        | (u32::from((word >> 11) & 1) << 4)
        | (u32::from((word >> 9) & 0x3) << 8)
        | (u32::from((word >> 8) & 1) << 10)
        | (u32::from((word >> 7) & 1) << 6)
        | (u32::from((word >> 6) & 1) << 7)
        | (u32::from((word >> 3) & 0x7) << 1)
        | (u32::from((word >> 2) & 1) << 5);
    sext(raw, 12)
}

/// The 9-bit signed displacement of a `C.BEQZ`/`C.BNEZ`.
fn branch_offset(word: u16) -> u32 {
    let raw = (u32::from((word >> 12) & 1) << 8)
        | (u32::from((word >> 10) & 0x3) << 3)
        | (u32::from((word >> 5) & 0x3) << 6)
        | (u32::from((word >> 3) & 0x3) << 1)
        | (u32::from((word >> 2) & 1) << 5);
    sext(raw, 9)
}

fn load<B: Bus>(bus: &mut B, pc: u32, addr: u32, width: u32) -> Result<u32, Fault> {
    match width {
        1 => bus.read8(addr).map_err(|_| Fault::Bus {
            pc,
            addr,
            write: false,
        }),
        2 if addr & 1 == 0 => bus.read16(addr).map_err(|_| Fault::Bus {
            pc,
            addr,
            write: false,
        }),
        4 if addr & 3 == 0 => bus.read32(addr).map_err(|_| Fault::Bus {
            pc,
            addr,
            write: false,
        }),
        _ => Err(Fault::Alignment { pc, addr, width }),
    }
}

fn store<B: Bus>(bus: &mut B, pc: u32, addr: u32, value: u32, width: u32) -> Result<(), Fault> {
    match width {
        1 => bus.write8(addr, value).map_err(|_| Fault::Bus {
            pc,
            addr,
            write: true,
        }),
        2 if addr & 1 == 0 => bus.write16(addr, value).map_err(|_| Fault::Bus {
            pc,
            addr,
            write: true,
        }),
        4 if addr & 3 == 0 => bus.write32(addr, value).map_err(|_| Fault::Bus {
            pc,
            addr,
            write: true,
        }),
        _ => Err(Fault::Alignment { pc, addr, width }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: u32 = 0x0000_0000;
    const SRAM: u32 = 0x2000_0000;

    struct TestBus {
        code: Vec<u8>,
        sram: Vec<u8>,
    }

    impl TestBus {
        fn new(code: &[u32]) -> Self {
            let mut bytes = vec![0u8; 0x1000];
            for (index, word) in code.iter().enumerate() {
                bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
            }
            TestBus {
                code: bytes,
                sram: vec![0; 0x100],
            }
        }

        fn region(&self, addr: u32, len: usize) -> Option<&[u8]> {
            if (addr as usize) < self.code.len() {
                self.code.get(addr as usize..addr as usize + len)
            } else if (SRAM..SRAM + 0x100).contains(&addr) {
                let offset = (addr - SRAM) as usize;
                self.sram.get(offset..offset + len)
            } else {
                None
            }
        }

        fn region_mut(&mut self, addr: u32, len: usize) -> Option<&mut [u8]> {
            if (addr as usize) < self.code.len() {
                self.code.get_mut(addr as usize..addr as usize + len)
            } else if (SRAM..SRAM + 0x100).contains(&addr) {
                let offset = (addr - SRAM) as usize;
                self.sram.get_mut(offset..offset + len)
            } else {
                None
            }
        }
    }

    impl Bus for TestBus {
        fn read8(&mut self, addr: u32) -> Result<u32, Fault> {
            self.region(addr, 1)
                .map(|b| u32::from(b[0]))
                .ok_or(Fault::Bus {
                    pc: 0,
                    addr,
                    write: false,
                })
        }
        fn read16(&mut self, addr: u32) -> Result<u32, Fault> {
            if addr & 1 != 0 {
                return Err(Fault::Alignment {
                    pc: 0,
                    addr,
                    width: 2,
                });
            }
            let b = self.region(addr, 2).ok_or(Fault::Bus {
                pc: 0,
                addr,
                write: false,
            })?;
            Ok(u32::from(u16::from_le_bytes([b[0], b[1]])))
        }
        fn read32(&mut self, addr: u32) -> Result<u32, Fault> {
            if addr & 3 != 0 {
                return Err(Fault::Alignment {
                    pc: 0,
                    addr,
                    width: 4,
                });
            }
            let b = self.region(addr, 4).ok_or(Fault::Bus {
                pc: 0,
                addr,
                write: false,
            })?;
            Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        }
        fn write8(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
            self.region_mut(addr, 1)
                .map(|b| b[0] = value as u8)
                .ok_or(Fault::Bus {
                    pc: 0,
                    addr,
                    write: true,
                })
        }
        fn write16(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
            if addr & 1 != 0 {
                return Err(Fault::Alignment {
                    pc: 0,
                    addr,
                    width: 2,
                });
            }
            let b = self.region_mut(addr, 2).ok_or(Fault::Bus {
                pc: 0,
                addr,
                write: true,
            })?;
            b.copy_from_slice(&(value as u16).to_le_bytes());
            Ok(())
        }
        fn write32(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
            if addr & 3 != 0 {
                return Err(Fault::Alignment {
                    pc: 0,
                    addr,
                    width: 4,
                });
            }
            let b = self.region_mut(addr, 4).ok_or(Fault::Bus {
                pc: 0,
                addr,
                write: true,
            })?;
            b.copy_from_slice(&value.to_le_bytes());
            Ok(())
        }
    }

    fn core_at(pc: u32) -> Core {
        let mut core = Core::new();
        core.pc = pc;
        core
    }

    #[test]
    fn addi_sets_a_register_and_advances() {
        // addi x1, x0, 5
        let mut bus = TestBus::new(&[0x0050_0093]);
        let mut core = core_at(CODE);
        assert_eq!(core.step(&mut bus), Ok(1));
        assert_eq!(core.reg(1), 5);
        assert_eq!(core.pc, 4);
    }

    #[test]
    fn a_branch_taken_costs_two_cycles() {
        // beq x0, x0, +8 ; addi x1, x0, 1 ; addi x2, x0, 2
        let mut bus = TestBus::new(&[0x0000_0463, 0x0010_0093, 0x0020_0113]);
        let mut core = core_at(CODE);
        assert_eq!(core.step(&mut bus), Ok(2));
        assert_eq!(core.pc, 8);
        assert_eq!(core.reg(1), 0, "the skipped instruction did not run");
    }

    #[test]
    fn loads_and_stores_use_sram() {
        // lui x1, 0x20000 ; sw x2, 0(x1) ; lw x3, 0(x1)
        let mut bus = TestBus::new(&[0x2000_00b7, 0x0020_a023, 0x0000_a183]);
        let mut core = core_at(CODE);
        core.set_reg(2, 0xdead_beef);
        assert_eq!(core.step(&mut bus), Ok(1));
        assert_eq!(core.step(&mut bus), Ok(2));
        assert_eq!(core.step(&mut bus), Ok(2));
        assert_eq!(core.reg(3), 0xdead_beef);
    }

    #[test]
    fn a_multiply_encoding_is_illegal_without_m() {
        // mul x1, x2, x3: funct7 = 0000001
        let mut bus = TestBus::new(&[0x0231_00b3]);
        let mut core = core_at(CODE);
        assert_eq!(
            core.step(&mut bus),
            Err(Fault::IllegalInstruction {
                pc: CODE,
                word: 0x0231_00b3
            })
        );
    }

    #[test]
    fn a_high_register_field_is_illegal_on_rv32e() {
        // addi x16, x0, 1
        let mut bus = TestBus::new(&[0x0010_0813]);
        let mut core = core_at(CODE);
        assert_eq!(
            core.step(&mut bus),
            Err(Fault::IllegalRegister {
                pc: CODE,
                field: "rd",
                index: 16
            })
        );
    }

    #[test]
    fn compressed_addi_and_jump_work() {
        // c.addi x1, 1 ; c.j +4 ; c.addi x2, 1
        let mut bus = TestBus::new(&[]);
        bus.write16(CODE, 0x0085).unwrap(); // c.addi x1, 1
        bus.write16(CODE + 2, 0xa011).unwrap(); // c.j +4 -> CODE+6
        bus.write16(CODE + 4, 0x0105).unwrap(); // c.addi x2, 1
        let mut core = core_at(CODE);
        assert_eq!(core.step(&mut bus), Ok(1));
        assert_eq!(core.reg(1), 1);
        assert_eq!(core.step(&mut bus), Ok(2));
        assert_eq!(core.pc, 6);
    }

    #[test]
    fn mret_restores_mie_and_the_saved_pc() {
        let mut core = Core::new();
        core.mepc = 0x200;
        core.mstatus = mstatus::MPIE;
        core.current_priority = 1;
        core.priority_stack.push(Core::NO_PRIORITY);
        // mret
        let mut bus = TestBus::new(&[0x3020_0073]);
        core.pc = CODE;
        assert_eq!(core.step(&mut bus), Ok(2));
        assert_eq!(core.pc, 0x200);
        assert_ne!(core.mstatus & mstatus::MIE, 0);
        assert_eq!(core.current_priority, Core::NO_PRIORITY);
    }
}
