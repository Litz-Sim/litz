//! RV32IMC, interpreted.
//!
//! The point of this core is not speed: it is that the same firmware image
//! that runs under esp32sim runs here, through the component boundary. Every
//! instruction the repository's fixtures emit is implemented, plus the rest of
//! RV32I and the M extension so a hand-written test program is not a puzzle.
//!
//! Compressed instructions are not decoded: `cargo` and the fixtures emit
//! 32-bit encodings, and a real firmware that needs C would need the full
//! decoder, which is exactly the kind of work a chip crate author does in
//! their own module.

/// Why the core stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trap {
    /// An instruction the core does not implement, with its encoding.
    Unimplemented(u32),
    /// A load or store outside every window the machine maps.
    Bus { addr: u32, len: u32 },
    /// `ebreak` or `ecall` with nothing installed to handle it.
    Halt(u32),
}

impl Trap {
    pub fn describe(self, pc: u32) -> String {
        match self {
            Trap::Unimplemented(word) => {
                format!("unimplemented instruction {word:#010x} at pc {pc:#010x}")
            }
            Trap::Bus { addr, len } => format!("{len}-byte access to unmapped {addr:#010x}"),
            Trap::Halt(word) => format!("{word:#010x} at pc {pc:#010x} stopped the core"),
        }
    }
}

/// What the core needs from the machine around it.
///
/// The word forms exist because peripheral registers are word-granular: a
/// `sw` to a write-one-to-set register means one write, not four byte writes
/// that each restart the register. A machine with no such registers takes the
/// byte-assembling default.
pub trait Memory {
    fn read8(&mut self, addr: u32) -> Result<u8, Trap>;
    fn write8(&mut self, addr: u32, value: u8) -> Result<(), Trap>;

    fn read32(&mut self, addr: u32) -> Result<u32, Trap> {
        let mut value = 0u32;
        for i in 0..4 {
            value |= u32::from(self.read8(addr.wrapping_add(i))?) << (8 * i);
        }
        Ok(value)
    }

    fn write32(&mut self, addr: u32, value: u32) -> Result<(), Trap> {
        for i in 0..4 {
            self.write8(addr.wrapping_add(i), (value >> (8 * i)) as u8)?;
        }
        Ok(())
    }

    /// Called once per instruction so a chip can charge time.
    fn tick(&mut self, _pc: u32) {}
}

#[derive(Clone)]
pub struct Cpu {
    pub pc: u32,
    regs: [u32; 32],
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Self {
        Self {
            pc: 0,
            regs: [0; 32],
        }
    }

    pub fn reg(&self, index: usize) -> u32 {
        if index == 0 { 0 } else { self.regs[index & 31] }
    }

    fn set(&mut self, index: usize, value: u32) {
        if index != 0 {
            self.regs[index & 31] = value;
        }
    }

    /// Execute one instruction.
    pub fn step<M: Memory>(&mut self, memory: &mut M) -> Result<(), Trap> {
        let pc = self.pc;
        memory.tick(pc);
        let word = self.fetch(memory, pc)?;
        // The compressed ISA is not decoded; catch it before the decoder
        // misreads a 16-bit instruction as a 32-bit one.
        if word & 0b11 != 0b11 {
            return Err(Trap::Unimplemented(word));
        }
        let next = self.execute(memory, pc, word)?;
        self.pc = next;
        Ok(())
    }

    fn fetch<M: Memory>(&mut self, memory: &mut M, pc: u32) -> Result<u32, Trap> {
        let b0 = memory.read8(pc)?;
        let b1 = memory.read8(pc.wrapping_add(1))?;
        let b2 = memory.read8(pc.wrapping_add(2))?;
        let b3 = memory.read8(pc.wrapping_add(3))?;
        Ok(u32::from_le_bytes([b0, b1, b2, b3]))
    }

    fn execute<M: Memory>(&mut self, memory: &mut M, pc: u32, word: u32) -> Result<u32, Trap> {
        let opcode = word & 0x7f;
        let rd = ((word >> 7) & 0x1f) as usize;
        let funct3 = (word >> 12) & 0x7;
        let rs1 = ((word >> 15) & 0x1f) as usize;
        let rs2 = ((word >> 20) & 0x1f) as usize;
        let imm_i = ((word as i32) >> 20) as u32;
        let funct7 = word >> 25;
        let next = pc.wrapping_add(4);

        match opcode {
            // LUI / AUIPC
            0x37 => {
                self.set(rd, word & 0xffff_f000);
                Ok(next)
            }
            0x17 => {
                self.set(rd, pc.wrapping_add(word & 0xffff_f000));
                Ok(next)
            }
            // JAL
            0x6f => {
                self.set(rd, next);
                Ok(pc.wrapping_add(j_imm(word)))
            }
            // JALR
            0x67 => {
                let target = self.reg(rs1).wrapping_add(imm_i) & !1;
                self.set(rd, next);
                Ok(target)
            }
            // Branches
            0x63 => {
                let a = self.reg(rs1);
                let b = self.reg(rs2);
                let taken = match funct3 {
                    0 => a == b,
                    1 => a != b,
                    4 => (a as i32) < (b as i32),
                    5 => (a as i32) >= (b as i32),
                    6 => a < b,
                    7 => a >= b,
                    _ => return Err(Trap::Unimplemented(word)),
                };
                Ok(if taken {
                    pc.wrapping_add(b_imm(word))
                } else {
                    next
                })
            }
            // Loads
            0x03 => {
                let addr = self.reg(rs1).wrapping_add(imm_i);
                let value = match funct3 {
                    0 => memory.read8(addr)? as i8 as i32 as u32,
                    1 => self.read16(memory, addr)? as i16 as i32 as u32,
                    2 => memory.read32(addr)?,
                    4 => memory.read8(addr)? as u32,
                    5 => self.read16(memory, addr)? as u32,
                    _ => return Err(Trap::Unimplemented(word)),
                };
                self.set(rd, value);
                Ok(next)
            }
            // Stores
            0x23 => {
                let addr = self.reg(rs1).wrapping_add(s_imm(word));
                let value = self.reg(rs2);
                match funct3 {
                    0 => memory.write8(addr, value as u8)?,
                    1 => {
                        memory.write8(addr, value as u8)?;
                        memory.write8(addr.wrapping_add(1), (value >> 8) as u8)?;
                    }
                    2 => memory.write32(addr, value)?,
                    _ => return Err(Trap::Unimplemented(word)),
                }
                Ok(next)
            }
            // OP-IMM
            0x13 => {
                let a = self.reg(rs1);
                let shamt = (word >> 20) & 0x1f;
                let value = match funct3 {
                    0 => a.wrapping_add(imm_i),
                    2 => u32::from((a as i32) < (imm_i as i32)),
                    3 => u32::from(a < imm_i),
                    4 => a ^ imm_i,
                    6 => a | imm_i,
                    7 => a & imm_i,
                    1 => a.wrapping_shl(shamt),
                    5 => {
                        if funct7 & 0x20 != 0 {
                            ((a as i32) >> shamt) as u32
                        } else {
                            a >> shamt
                        }
                    }
                    _ => return Err(Trap::Unimplemented(word)),
                };
                self.set(rd, value);
                Ok(next)
            }
            // OP
            0x33 => {
                let a = self.reg(rs1);
                let b = self.reg(rs2);
                let value = match (funct7, funct3) {
                    (0x00, 0) => a.wrapping_add(b),
                    (0x20, 0) => a.wrapping_sub(b),
                    (0x00, 1) => a.wrapping_shl(b & 0x1f),
                    (0x00, 2) => u32::from((a as i32) < (b as i32)),
                    (0x00, 3) => u32::from(a < b),
                    (0x00, 4) => a ^ b,
                    (0x00, 5) => a >> (b & 0x1f),
                    (0x20, 5) => ((a as i32) >> (b & 0x1f)) as u32,
                    (0x00, 6) => a | b,
                    (0x00, 7) => a & b,
                    // M extension. Division by zero is defined by the ISA as
                    // all ones for the quotient and the dividend for the
                    // remainder, not a trap.
                    (0x01, 0) => a.wrapping_mul(b),
                    (0x01, 1) => (((a as i64 * b as i64) >> 32) as i32) as u32,
                    (0x01, 2) => (((a as i64 * b as u64 as i64) >> 32) as i32) as u32,
                    (0x01, 3) => ((a as u64 * b as u64) >> 32) as u32,
                    (0x01, 4) => {
                        if b == 0 {
                            u32::MAX
                        } else {
                            ((a as i32).wrapping_div(b as i32)) as u32
                        }
                    }
                    (0x01, 5) => a.checked_div(b).unwrap_or(u32::MAX),
                    (0x01, 6) => {
                        if b == 0 {
                            a
                        } else {
                            ((a as i32).wrapping_rem(b as i32)) as u32
                        }
                    }
                    (0x01, 7) => a.checked_rem(b).unwrap_or(a),
                    _ => return Err(Trap::Unimplemented(word)),
                };
                self.set(rd, value);
                Ok(next)
            }
            // FENCE / FENCE.I: a single core has nothing to order.
            0x0f => Ok(next),
            // SYSTEM: both `ebreak` (a firmware doing its own panic) and
            // `ecall` (a request for an environment this chip does not have)
            // stop the core with the encoding in the report.
            0x73 => Err(Trap::Halt(word)),
            _ => Err(Trap::Unimplemented(word)),
        }
    }

    fn read16<M: Memory>(&mut self, memory: &mut M, addr: u32) -> Result<u16, Trap> {
        let lo = memory.read8(addr)?;
        let hi = memory.read8(addr.wrapping_add(1))?;
        Ok(u16::from_le_bytes([lo, hi]))
    }
}

/// J-type immediate, sign-extended.
fn j_imm(word: u32) -> u32 {
    let imm20 = (word >> 31) & 1;
    let imm10_1 = (word >> 21) & 0x3ff;
    let imm11 = (word >> 20) & 1;
    let imm19_12 = (word >> 12) & 0xff;
    let imm = (imm20 << 20) | (imm19_12 << 12) | (imm11 << 11) | (imm10_1 << 1);
    ((imm as i32) << 11 >> 11) as u32
}

/// B-type immediate, sign-extended.
fn b_imm(word: u32) -> u32 {
    let imm12 = (word >> 31) & 1;
    let imm10_5 = (word >> 25) & 0x3f;
    let imm4_1 = (word >> 8) & 0xf;
    let imm11 = (word >> 7) & 1;
    let imm = (imm12 << 12) | (imm11 << 11) | (imm10_5 << 5) | (imm4_1 << 1);
    ((imm as i32) << 19 >> 19) as u32
}

/// S-type immediate, sign-extended.
fn s_imm(word: u32) -> u32 {
    let imm11_5 = (word >> 25) & 0x7f;
    let imm4_0 = (word >> 7) & 0x1f;
    let imm = (imm11_5 << 5) | imm4_0;
    ((imm as i32) << 20 >> 20) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Flat memory for instruction tests, with the same trap on an unmapped
    /// address a real machine would produce.
    struct Ram {
        bytes: BTreeMap<u32, u8>,
    }

    impl Ram {
        fn with(pairs: &[(u32, u32)]) -> Self {
            let mut bytes = BTreeMap::new();
            for (addr, word) in pairs {
                for (i, byte) in word.to_le_bytes().iter().enumerate() {
                    bytes.insert(addr + i as u32, *byte);
                }
            }
            Self { bytes }
        }
    }

    impl Memory for Ram {
        fn read8(&mut self, addr: u32) -> Result<u8, Trap> {
            self.bytes
                .get(&addr)
                .copied()
                .ok_or(Trap::Bus { addr, len: 1 })
        }
        fn write8(&mut self, addr: u32, value: u8) -> Result<(), Trap> {
            self.bytes.insert(addr, value);
            Ok(())
        }
    }

    fn run(words: &[(u32, u32)], steps: usize) -> Cpu {
        let mut cpu = Cpu::new();
        cpu.pc = words[0].0;
        let mut ram = Ram::with(words);
        for _ in 0..steps {
            if cpu.step(&mut ram).is_err() {
                break;
            }
        }
        cpu
    }

    #[test]
    fn li_costs_two_instructions() {
        // lui x7, 0x7A1 ; addi x7, x7, 0x200
        let cpu = run(&[(0x1000, 0x007A_13B7), (0x1004, 0x2003_8393)], 2);
        assert_eq!(cpu.reg(7), 0x7A1_200);
    }

    #[test]
    fn a_taken_branch_moves_the_pc_and_a_loop_terminates() {
        // addi x7, x0, 3 ; addi x7, x7, -1 ; bne x7, x0, -4
        let cpu = run(
            &[
                (0x1000, 0x0030_0393),
                (0x1004, 0xfff3_8393),
                (0x1008, 0xfe03_9ee3),
            ],
            7,
        );
        assert_eq!(cpu.reg(7), 0);
        assert_eq!(cpu.pc, 0x100C);
    }

    #[test]
    fn loads_and_stores_round_trip() {
        // sw x7, 0x40(x5) ; lw x8, 0x40(x5)
        let mut cpu = Cpu::new();
        cpu.pc = 0x1000;
        cpu.set(5, 0x2000);
        cpu.set(7, 0xdead_beef);
        let mut ram = Ram::with(&[(0x1000, 0x0472_A023), (0x1004, 0x0402_A403)]);
        cpu.step(&mut ram).unwrap();
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.reg(8), 0xdead_beef);
    }

    #[test]
    fn an_unmapped_access_is_a_trap_not_a_zero() {
        let mut cpu = Cpu::new();
        cpu.pc = 0x1000;
        let mut ram = Ram::with(&[(0x1000, 0x0000_A403)]); // lw x8, 0(x1)
        assert_eq!(cpu.step(&mut ram), Err(Trap::Bus { addr: 0, len: 1 }));
    }
}
