//! A tiny hand assembler shared by the integration tests.
//!
//! It encodes the RV32E instructions and the compressed forms the tests use,
//! plus an [`Image`] that places words at byte offsets so a test can lay out a
//! vector table, a reset routine and a handler explicitly.

#![allow(dead_code)]

/// An image builder: little-endian words and halfwords at byte offsets.
pub struct Image {
    bytes: Vec<u8>,
}

impl Image {
    pub fn new() -> Self {
        Image {
            bytes: vec![0xff; 16 * 1024],
        }
    }

    pub fn word(&mut self, offset: usize, value: u32) -> &mut Self {
        self.bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        self
    }

    pub fn half(&mut self, offset: usize, value: u16) -> &mut Self {
        self.bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        self
    }

    pub fn place(&mut self, offset: usize, words: &[u32]) -> &mut Self {
        for (index, word) in words.iter().enumerate() {
            self.word(offset + index * 4, *word);
        }
        self
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

impl Default for Image {
    fn default() -> Self {
        Self::new()
    }
}

// -- 32-bit encoders -------------------------------------------------------

fn i_type(opcode: u32, funct3: u32, rd: u32, rs1: u32, imm: i32) -> u32 {
    ((imm as u32 & 0xfff) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
}

pub fn addi(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(0x13, 0, rd, rs1, imm)
}

pub fn slti(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(0x13, 2, rd, rs1, imm)
}

pub fn sltiu(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(0x13, 3, rd, rs1, imm)
}

pub fn xori(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(0x13, 4, rd, rs1, imm)
}

pub fn ori(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(0x13, 6, rd, rs1, imm)
}

pub fn andi(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(0x13, 7, rd, rs1, imm)
}

pub fn slli(rd: u32, rs1: u32, shamt: u32) -> u32 {
    i_type(0x13, 1, rd, rs1, shamt as i32)
}

pub fn srli(rd: u32, rs1: u32, shamt: u32) -> u32 {
    i_type(0x13, 5, rd, rs1, shamt as i32)
}

pub fn srai(rd: u32, rs1: u32, shamt: u32) -> u32 {
    ((0x20 << 5) << 20) | ((shamt & 0x1f) << 20) | (rs1 << 15) | (5 << 12) | (rd << 7) | 0x13
}

pub fn lui(rd: u32, imm20: u32) -> u32 {
    ((imm20 & 0xfffff) << 12) | (rd << 7) | 0x37
}

pub fn auipc(rd: u32, imm20: u32) -> u32 {
    ((imm20 & 0xfffff) << 12) | (rd << 7) | 0x17
}

pub fn jal(rd: u32, offset: i32) -> u32 {
    let imm = offset as u32 & 0x001f_ffff;
    (((imm >> 20) & 1) << 31)
        | (((imm >> 1) & 0x3ff) << 21)
        | (((imm >> 11) & 1) << 20)
        | (((imm >> 12) & 0xff) << 12)
        | (rd << 7)
        | 0x6f
}

pub fn jalr(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(0x67, 0, rd, rs1, imm)
}

fn b_type(funct3: u32, rs1: u32, rs2: u32, offset: i32) -> u32 {
    let imm = offset as u32 & 0x1fff;
    (((imm >> 12) & 1) << 31)
        | (((imm >> 5) & 0x3f) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (funct3 << 12)
        | (((imm >> 1) & 0xf) << 8)
        | (((imm >> 11) & 1) << 7)
        | 0x63
}

pub fn beq(rs1: u32, rs2: u32, offset: i32) -> u32 {
    b_type(0, rs1, rs2, offset)
}

pub fn bne(rs1: u32, rs2: u32, offset: i32) -> u32 {
    b_type(1, rs1, rs2, offset)
}

pub fn blt(rs1: u32, rs2: u32, offset: i32) -> u32 {
    b_type(4, rs1, rs2, offset)
}

pub fn bge(rs1: u32, rs2: u32, offset: i32) -> u32 {
    b_type(5, rs1, rs2, offset)
}

pub fn bltu(rs1: u32, rs2: u32, offset: i32) -> u32 {
    b_type(6, rs1, rs2, offset)
}

pub fn bgeu(rs1: u32, rs2: u32, offset: i32) -> u32 {
    b_type(7, rs1, rs2, offset)
}

fn s_type(funct3: u32, rs2: u32, rs1: u32, imm: i32) -> u32 {
    let imm = imm as u32 & 0xfff;
    (((imm >> 5) & 0x7f) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (funct3 << 12)
        | ((imm & 0x1f) << 7)
        | 0x23
}

pub fn sb(rs2: u32, rs1: u32, imm: i32) -> u32 {
    s_type(0, rs2, rs1, imm)
}

pub fn sh(rs2: u32, rs1: u32, imm: i32) -> u32 {
    s_type(1, rs2, rs1, imm)
}

pub fn sw(rs2: u32, rs1: u32, imm: i32) -> u32 {
    s_type(2, rs2, rs1, imm)
}

pub fn lb(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(0x03, 0, rd, rs1, imm)
}

pub fn lh(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(0x03, 1, rd, rs1, imm)
}

pub fn lw(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(0x03, 2, rd, rs1, imm)
}

pub fn lbu(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(0x03, 4, rd, rs1, imm)
}

pub fn lhu(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(0x03, 5, rd, rs1, imm)
}

fn r_type(funct7: u32, funct3: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
    (funct7 << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | 0x33
}

pub fn add(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(0, 0, rd, rs1, rs2)
}

pub fn sub(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(0x20, 0, rd, rs1, rs2)
}

pub fn sll(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(0, 1, rd, rs1, rs2)
}

pub fn slt(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(0, 2, rd, rs1, rs2)
}

pub fn sltu(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(0, 3, rd, rs1, rs2)
}

pub fn xor(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(0, 4, rd, rs1, rs2)
}

pub fn srl(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(0, 5, rd, rs1, rs2)
}

pub fn sra(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(0x20, 5, rd, rs1, rs2)
}

pub fn or(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(0, 6, rd, rs1, rs2)
}

pub fn and(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(0, 7, rd, rs1, rs2)
}

/// An illegal encoding: `mul rd, rs1, rs2`.
pub fn mul(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(1, 0, rd, rs1, rs2)
}

pub fn csrrw(rd: u32, csr: u32, rs1: u32) -> u32 {
    (csr << 20) | (rs1 << 15) | (1 << 12) | (rd << 7) | 0x73
}

pub fn csrrs(rd: u32, csr: u32, rs1: u32) -> u32 {
    (csr << 20) | (rs1 << 15) | (2 << 12) | (rd << 7) | 0x73
}

pub fn csrrc(rd: u32, csr: u32, rs1: u32) -> u32 {
    (csr << 20) | (rs1 << 15) | (3 << 12) | (rd << 7) | 0x73
}

pub const MRET: u32 = 0x3020_0073;
pub const WFI: u32 = 0x1050_0073;
pub const WFE: u32 = 0x0020_0073;
pub const ECALL: u32 = 0x0000_0073;
pub const EBREAK: u32 = 0x0010_0073;
pub const NOP: u32 = 0x0000_0013;
/// `jal x0, 0`: an endless loop in place.
pub const SELF: u32 = 0x0000_006f;

// -- compressed encoders ---------------------------------------------------

fn c_imm6(imm: i32) -> u16 {
    let value = imm as u32 & 0x3f;
    (((value >> 5) & 1) << 12) as u16 | ((value & 0x1f) << 2) as u16
}

pub fn c_addi(rd: u32, imm: i32) -> u16 {
    0x0001 | ((rd as u16) << 7) | c_imm6(imm)
}

pub fn c_li(rd: u32, imm: i32) -> u16 {
    0x4001 | ((rd as u16) << 7) | c_imm6(imm)
}

pub fn c_slli(rd: u32, shamt: u32) -> u16 {
    0x0002 | ((rd as u16) << 7) | ((shamt as u16 & 0x1f) << 2)
}

pub fn c_mv(rd: u32, rs2: u32) -> u16 {
    0x8002 | ((rd as u16) << 7) | ((rs2 as u16) << 2)
}

pub fn c_add(rd: u32, rs2: u32) -> u16 {
    0x9002 | ((rd as u16) << 7) | ((rs2 as u16) << 2)
}

pub fn c_j(offset: i32) -> u16 {
    let imm = offset as u32 & 0xfff;
    let mut value = 0xa001u16;
    value |= (((imm >> 11) & 1) as u16) << 12;
    value |= (((imm >> 4) & 1) as u16) << 11;
    value |= (((imm >> 8) & 0x3) as u16) << 9;
    value |= (((imm >> 10) & 1) as u16) << 8;
    value |= (((imm >> 6) & 1) as u16) << 7;
    value |= (((imm >> 7) & 1) as u16) << 6;
    value |= (((imm >> 1) & 0x7) as u16) << 3;
    value |= (((imm >> 5) & 1) as u16) << 2;
    value
}

pub fn c_beqz(rs1: u32, offset: i32) -> u16 {
    c_branch(6, rs1, offset)
}

pub fn c_bnez(rs1: u32, offset: i32) -> u16 {
    c_branch(7, rs1, offset)
}

fn c_branch(funct3: u16, rs1: u32, offset: i32) -> u16 {
    assert!((8..16).contains(&rs1));
    let imm = offset as u32 & 0x1ff;
    let mut value = (funct3 << 13) | 0x0001;
    value |= (((rs1 - 8) as u16) & 7) << 7;
    value |= (((imm >> 8) & 1) as u16) << 12;
    value |= (((imm >> 3) & 0x3) as u16) << 10;
    value |= (((imm >> 6) & 0x3) as u16) << 5;
    value |= (((imm >> 1) & 0x3) as u16) << 3;
    value |= (((imm >> 5) & 1) as u16) << 2;
    value
}

pub fn c_lw(rd: u32, rs1: u32, offset: u32) -> u16 {
    assert!((8..16).contains(&rd) && (8..16).contains(&rs1));
    0x4000
        | (((rs1 - 8) as u16 & 7) << 7)
        | (((rd - 8) as u16 & 7) << 2)
        | ((((offset >> 3) & 7) as u16) << 10)
        | ((((offset >> 2) & 1) as u16) << 6)
        | ((((offset >> 6) & 1) as u16) << 5)
}

pub fn c_sw(rs2: u32, rs1: u32, offset: u32) -> u16 {
    assert!((8..16).contains(&rs2) && (8..16).contains(&rs1));
    0xc000
        | (((rs1 - 8) as u16 & 7) << 7)
        | (((rs2 - 8) as u16 & 7) << 2)
        | ((((offset >> 3) & 7) as u16) << 10)
        | ((((offset >> 2) & 1) as u16) << 6)
        | ((((offset >> 6) & 1) as u16) << 5)
}

/// `lui rd, hi` followed by `addi rd, rd, lo`: a 32-bit constant.
pub fn li32(rd: u32, value: u32) -> [u32; 2] {
    let hi = (value.wrapping_add(0x800)) >> 12;
    let lo = (value & 0xfff) as i32;
    let lo = if lo >= 0x800 { lo - 0x1000 } else { lo };
    [lui(rd, hi & 0xfffff), addi(rd, rd, lo)]
}
