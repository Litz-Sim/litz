//! RV32IMC instruction encoders, one function per instruction shape.
//!
//! Only the instructions the fixture programs emit are here. Every encoder
//! returns the 32-bit word the ISA defines, ready to be written little-endian
//! into a code segment. The RISC-V unprivileged specification is the
//! authority for every constant in this module, not the assembler the values
//! were checked against.

/// Integer register x0. Reads as zero, writes are discarded.
pub const ZERO: u32 = 0;
/// Integer register x2, the stack pointer.
pub const SP: u32 = 2;
/// Integer register x5, temporary 0.
pub const T0: u32 = 5;
/// Integer register x6, temporary 1.
pub const T1: u32 = 6;
/// Integer register x7, temporary 2.
pub const T2: u32 = 7;
/// Integer register x10, argument/return 0.
pub const A0: u32 = 10;
/// Integer register x11, argument/return 1.
pub const A1: u32 = 11;
/// Integer register x28, temporary 3.
pub const T3: u32 = 28;

/// A fourth temporary: the button program keeps its last known state here.
pub const T4: u32 = 29;

/// Opcode for `OP-IMM` (addi and friends).
const OP_IMM: u32 = 0x13;
/// Opcode for `LUI`.
const OP_LUI: u32 = 0x37;
/// Opcode for `AUIPC`.
const OP_AUIPC: u32 = 0x17;
/// Opcode for `LOAD`.
const OP_LOAD: u32 = 0x03;
/// Opcode for `STORE`.
const OP_STORE: u32 = 0x23;
/// Opcode for `BRANCH`.
const OP_BRANCH: u32 = 0x63;
/// Opcode for `JAL`.
const OP_JAL: u32 = 0x6f;
/// Opcode for `SYSTEM`.
const OP_SYSTEM: u32 = 0x73;
/// `funct3` selecting a word access in `LOAD`/`STORE`.
const SIZE_WORD: u32 = 0b010;

/// Panics unless `r` names an integer register.
fn check_reg(r: u32) {
    assert!(r < 32, "x{r} is not an RV32 register (x0..x31)");
}

/// Panics unless `imm` fits the signed 12-bit immediate field.
fn check_imm12(imm: i32) {
    assert!(
        (-2048..=2047).contains(&imm),
        "immediate {imm} does not fit 12 signed bits"
    );
}

/// Panics unless `offset` is an even byte offset that fits the branch field.
fn check_branch_offset(offset: i32) {
    assert!(
        offset % 2 == 0,
        "branch offset {offset} is not 2-byte aligned"
    );
    assert!(
        (-4096..=4094).contains(&offset),
        "branch offset {offset} does not fit 13 signed bits"
    );
}

/// `lui rd, imm20`: load `imm20 << 12` into `rd`.
pub fn lui(rd: u32, imm20: u32) -> u32 {
    check_reg(rd);
    assert!(
        imm20 < (1 << 20),
        "lui immediate {imm20:#x} does not fit 20 bits"
    );
    (imm20 << 12) | (rd << 7) | OP_LUI
}

/// `auipc rd, imm20`: add `imm20 << 12` to the address of this instruction.
pub fn auipc(rd: u32, imm20: u32) -> u32 {
    check_reg(rd);
    assert!(
        imm20 < (1 << 20),
        "auipc immediate {imm20:#x} does not fit 20 bits"
    );
    (imm20 << 12) | (rd << 7) | OP_AUIPC
}

/// `addi rd, rs1, imm12`: `rd = rs1 + sign_extend(imm12)`.
pub fn addi(rd: u32, rs1: u32, imm12: i32) -> u32 {
    check_reg(rd);
    check_reg(rs1);
    check_imm12(imm12);
    ((imm12 as u32 & 0xfff) << 20) | (rs1 << 15) | (rd << 7) | OP_IMM
}

/// `sw rs2, imm12(rs1)`: store the low 32 bits of `rs2` at `rs1 + imm12`.
/// `andi rd, rs1, imm` — the register form bitwise-ANDs; only the immediate
/// one is needed here, for a single-bit test.
pub fn andi(rd: u32, rs1: u32, imm12: i32) -> u32 {
    assert!(
        (-2048..=2047).contains(&imm12),
        "andi immediate out of range"
    );
    ((imm12 as u32 & 0xfff) << 20) | (rs1 << 15) | (7 << 12) | (rd << 7) | 0x13
}

/// `srli rd, rs1, shamt` — logical shift right, three-operand shift.
pub fn srli(rd: u32, rs1: u32, shamt: u32) -> u32 {
    check_reg(rd);
    check_reg(rs1);
    assert!(shamt < 32, "shift amount out of range");
    (shamt << 20) | (rs1 << 15) | (5 << 12) | (rd << 7) | 0x13
}

pub fn sw(rs2: u32, rs1: u32, imm12: i32) -> u32 {
    check_reg(rs2);
    check_reg(rs1);
    check_imm12(imm12);
    let imm = imm12 as u32 & 0xfff;
    ((imm >> 5) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (SIZE_WORD << 12)
        | ((imm & 0x1f) << 7)
        | OP_STORE
}

/// `lw rd, imm12(rs1)`: load the word at `rs1 + imm12` into `rd`.
pub fn lw(rd: u32, rs1: u32, imm12: i32) -> u32 {
    check_reg(rd);
    check_reg(rs1);
    check_imm12(imm12);
    ((imm12 as u32 & 0xfff) << 20) | (rs1 << 15) | (SIZE_WORD << 12) | (rd << 7) | OP_LOAD
}

/// Encode a B-type branch with the given `funct3`.
fn branch(funct3: u32, rs1: u32, rs2: u32, byte_offset: i32) -> u32 {
    check_reg(rs1);
    check_reg(rs2);
    check_branch_offset(byte_offset);
    // B-type scatters the immediate: imm[12]|imm[10:5] in bits 31:25,
    // imm[4:1] in bits 11:8 and imm[11] in bit 7. Bit 0 of the offset is
    // always zero because RV32I instructions are 2-byte aligned.
    let imm = byte_offset as u32;
    (((imm >> 12) & 1) << 31)
        | (((imm >> 5) & 0x3f) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (funct3 << 12)
        | (((imm >> 1) & 0xf) << 8)
        | (((imm >> 11) & 1) << 7)
        | OP_BRANCH
}

/// `beq rs1, rs2, byte_offset`: branch when the registers are equal.
pub fn beq(rs1: u32, rs2: u32, byte_offset: i32) -> u32 {
    branch(0b000, rs1, rs2, byte_offset)
}

/// `bne rs1, rs2, byte_offset`: branch when the registers differ.
pub fn bne(rs1: u32, rs2: u32, byte_offset: i32) -> u32 {
    branch(0b001, rs1, rs2, byte_offset)
}

/// `jal rd, byte_offset`: jump and link. `jal x0, 0` jumps to itself forever.
pub fn jal(rd: u32, byte_offset: i32) -> u32 {
    check_reg(rd);
    assert!(
        byte_offset % 2 == 0,
        "jal offset {byte_offset} is not 2-byte aligned"
    );
    assert!(
        (-1_048_576..=1_048_574).contains(&byte_offset),
        "jal offset {byte_offset} does not fit 21 signed bits"
    );
    // J-type: imm[20]|imm[10:1]|imm[11]|imm[19:12] in bits 31:12.
    let imm = byte_offset as u32;
    (((imm >> 20) & 1) << 31)
        | (((imm >> 1) & 0x3ff) << 21)
        | (((imm >> 11) & 1) << 20)
        | (((imm >> 12) & 0xff) << 12)
        | (rd << 7)
        | OP_JAL
}

/// `ebreak`: break to the debugger (a trap on hardware without one).
pub fn ebreak() -> u32 {
    // SYSTEM opcode, funct3 000, rs1 = x0, rd = x0, imm = 1.
    (1 << 20) | OP_SYSTEM
}

/// `li rd, value`, assembled from at most two instructions.
///
/// Values in the signed 12-bit range become one `addi rd, x0, value`. Anything
/// else becomes `lui` plus an optional `addi` carrying the sign-extended low
/// 12 bits. The `+ 0x800` before shifting is what makes the lower half's sign
/// bit carry into the upper half, so `lui` and `addi` always reconstruct
/// `value` exactly, including the `0xffff_xxxx` range where the naive split
/// would choose an `imm20` that no longer fits twenty bits.
///
/// A trailing `addi` with a zero immediate is omitted; the result is therefore
/// one or two words, in execution order.
pub fn li(rd: u32, value: u32) -> Vec<u32> {
    check_reg(rd);
    let upper = value.wrapping_add(0x800) >> 12;
    let low = value & 0xfff;
    let low = if low >= 0x800 {
        low as i32 - 0x1000
    } else {
        low as i32
    };
    if upper == 0 {
        vec![addi(rd, ZERO, low)]
    } else if low == 0 {
        vec![lui(rd, upper)]
    } else {
        vec![lui(rd, upper), addi(rd, rd, low)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `andi` is the I-type form of the bitwise AND; the button program uses
    /// it to test one pin bit.
    #[test]
    fn andi_takes_a_twelve_bit_immediate() {
        assert_eq!(andi(8, 8, 1), 0x0014_7413);
        assert_eq!(andi(5, 0, 0x3ff), 0x3ff0_7293);
    }

    /// Hand-checked against the RISC-V unprivileged ISA, RV32I chapter.
    #[test]
    fn the_documented_encodings_match_the_isa() {
        assert_eq!(addi(1, 0, 1), 0x0010_0093);
        assert_eq!(jal(0, 0), 0x0000_006f);
        assert_eq!(lui(5, 0x60004), 0x6000_42b7);
        assert_eq!(bne(1, 0, -4), 0xfe00_9ee3);
    }

    /// The brief supplied `0x0062_a223` for `sw x6, 0x24(x5)`. That word is
    /// `sw x6, 4(x5)`: its imm[11:5] field (bits 31:25) is zero, so only the
    /// low five immediate bits are set. The ISA puts imm[11:5] in bits 31:25,
    /// and 0x24 has bit 5 set, so the correct word carries 0x0200_0000 as
    /// well. The encoder follows the ISA.
    #[test]
    fn the_store_constant_in_the_brief_was_for_a_different_offset() {
        assert_eq!(sw(6, 5, 0x24), 0x0262_a223);
        assert_eq!(sw(6, 5, 0x04), 0x0062_a223);
    }

    #[test]
    fn the_other_shapes_are_correct() {
        assert_eq!(auipc(5, 0x60004), 0x6000_4297);
        assert_eq!(lw(6, 5, 0x24), 0x0242_a303);
        assert_eq!(beq(1, 0, -4), 0xfe00_8ee3);
        assert_eq!(ebreak(), 0x0010_0073);
    }

    /// `bne x7, x0, -4` is the delay-loop back edge. Decoding it by hand:
    ///
    /// * bits 6:0 = 0x63, opcode BRANCH;
    /// * bits 14:12 = 001, funct3 BNE;
    /// * bits 19:15 = 00111, rs1 = x7;
    /// * bits 24:20 = 00000, rs2 = x0;
    /// * bit 31 = imm[12] = 1;
    /// * bits 30:25 = imm[10:5] = 111111;
    /// * bits 11:8 = imm[4:1] = 1110;
    /// * bit 7 = imm[11] = 1;
    ///
    /// so imm[12:1] = 0b1111_1111_1110, a 12-bit two's-complement -2, and the
    /// low zero bit makes the byte offset -4. That is exactly one instruction
    /// back, from the `bne` to the `addi` that precedes it.
    #[test]
    fn the_delay_loop_back_edge_is_minus_four() {
        assert_eq!(bne(T2, ZERO, -4), 0xfe03_9ee3);
    }

    #[test]
    fn li_uses_one_or_two_words() {
        assert_eq!(li(5, 1), vec![addi(5, ZERO, 1)]);
        assert_eq!(li(5, 0x7ff), vec![addi(5, ZERO, 0x7ff)]);
        assert_eq!(li(5, -1i32 as u32), vec![addi(5, ZERO, -1)]);
        assert_eq!(li(5, 0x1234), vec![lui(5, 1), addi(5, 5, 0x234)]);
        assert_eq!(li(5, 0x6000_9000), vec![lui(5, 0x60009)]);
        assert_eq!(li(5, 0x3fcc_0000), vec![lui(5, 0x3fcc0)]);
    }

    /// Every `li` split must reconstruct the value: `lui << 12` plus the
    /// sign-extended low half. Sweeping interesting boundaries catches the
    /// carry in `wrapping_add(0x800)`.
    #[test]
    fn li_reconstructs_every_boundary_value() {
        for value in [
            0u32,
            1,
            0x7ff,
            0x800,
            0xfff,
            0x1000,
            0x7fff_f800,
            0x8000_0000,
            0xffff_f7ff,
            0xffff_f800,
            0xffff_ffff,
        ] {
            let words = li(5, value);
            let mut got = 0u32;
            for word in words {
                match word & 0x7f {
                    OP_LUI => got = word & 0xffff_f000,
                    OP_IMM => {
                        let imm = ((word >> 20) & 0xfff) as i32;
                        let imm = (imm << 20) >> 20; // sign-extend the 12-bit field
                        got = got.wrapping_add(imm as u32);
                    }
                    _ => panic!("li emitted {word:#010x}"),
                }
            }
            assert_eq!(got, value, "li({value:#x}) decoded to {got:#x}");
        }
    }
}
