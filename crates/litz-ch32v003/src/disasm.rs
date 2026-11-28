//! An RV32EC disassembler for the debug surface.
//!
//! The output is one line per instruction, `address  word  mnemonic`, with the
//! compressed forms decoded through the same fields the core executes. It is a
//! debugging aid rather than an assembler: illegal encodings print as
//! `.insn`/`.c.insn` with their word, so a program that trapped can be read
//! back at the pc that failed.

use crate::bus::Memory;
use crate::cpu::Bus;

fn reg(index: u32) -> String {
    format!("x{index}")
}

fn sext(value: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((value << shift) as i32) >> shift
}

fn hex(value: i64) -> String {
    if value < 0 {
        format!("-{:#x}", -value)
    } else {
        format!("{value:#x}")
    }
}

/// Disassemble `count` instructions starting at `addr`.
pub fn disassemble(mem: &mut Memory, addr: u32, count: usize) -> String {
    let mut text = String::new();
    let mut at = addr;
    for _ in 0..count {
        let Ok(low) = mem.read16(at) else {
            text.push_str(&format!("{at:08x}  <unreadable>\n"));
            break;
        };
        let low = low as u16;
        if low & 0b11 != 0b11 {
            text.push_str(&format!("{at:08x}  {low:04x}      {}\n", compressed(low)));
            at = at.wrapping_add(2);
        } else {
            let Ok(high) = mem.read16(at.wrapping_add(2)) else {
                text.push_str(&format!("{at:08x}  {low:04x}      <truncated>\n"));
                break;
            };
            let word = u32::from(low) | (u32::from(high as u16) << 16);
            text.push_str(&format!("{at:08x}  {word:08x}  {}\n", full(word)));
            at = at.wrapping_add(4);
        }
    }
    text
}

/// The four shift-in/out operations, for the compressed we print.
fn name_of(index: u32) -> String {
    reg(index & 0x1f)
}

fn branch(word: u32) -> String {
    let funct3 = (word >> 12) & 7;
    let rs1 = name_of(word >> 15);
    let rs2 = name_of(word >> 20);
    let imm = sext(
        (((word >> 31) & 1) << 12)
            | (((word >> 7) & 1) << 11)
            | (((word >> 25) & 0x3f) << 5)
            | (((word >> 8) & 0xf) << 1),
        13,
    );
    let mnemonic = match funct3 {
        0 => "beq",
        1 => "bne",
        4 => "blt",
        5 => "bge",
        6 => "bltu",
        7 => "bgeu",
        _ => ".insn",
    };
    format!("{mnemonic} {rs1}, {rs2}, {}", hex(i64::from(imm)))
}

fn load(word: u32, funct3: u32) -> String {
    let rd = name_of(word >> 7);
    let rs1 = name_of(word >> 15);
    let imm = sext(word >> 20, 12);
    let mnemonic = match funct3 {
        0 => "lb",
        1 => "lh",
        2 => "lw",
        4 => "lbu",
        5 => "lhu",
        _ => ".insn",
    };
    format!("{mnemonic} {rd}, {}({rs1})", hex(i64::from(imm)))
}

fn store(word: u32, funct3: u32) -> String {
    let rs2 = name_of(word >> 20);
    let rs1 = name_of(word >> 15);
    let imm = sext((((word >> 25) & 0x7f) << 5) | ((word >> 7) & 0x1f), 12);
    let mnemonic = match funct3 {
        0 => "sb",
        1 => "sh",
        2 => "sw",
        _ => ".insn",
    };
    format!("{mnemonic} {rs2}, {}({rs1})", hex(i64::from(imm)))
}

/// The 32-bit encoding.
pub fn full(word: u32) -> String {
    let opcode = word & 0x7f;
    let rd = (word >> 7) & 0x1f;
    let funct3 = (word >> 12) & 7;
    let rs1 = (word >> 15) & 0x1f;
    let rs2 = (word >> 20) & 0x1f;
    let funct7 = word >> 25;
    match opcode {
        0b011_0111 => format!("lui {}, {:#x}", name_of(rd), word >> 12),
        0b001_0111 => format!("auipc {}, {:#x}", name_of(rd), word >> 12),
        0b110_1111 => {
            let imm = sext(
                (((word >> 31) & 1) << 20)
                    | (((word >> 12) & 0xff) << 12)
                    | (((word >> 20) & 1) << 11)
                    | (((word >> 21) & 0x3ff) << 1),
                21,
            );
            format!("jal {}, {}", name_of(rd), hex(i64::from(imm)))
        }
        0b110_0111 => {
            let imm = sext(word >> 20, 12);
            format!(
                "jalr {}, {}({})",
                name_of(rd),
                hex(i64::from(imm)),
                name_of(rs1)
            )
        }
        0b110_0011 => branch(word),
        0b000_0011 => load(word, funct3),
        0b010_0011 => store(word, funct3),
        0b001_0011 => {
            let a = name_of(rs1);
            let d = name_of(rd);
            let imm = sext(word >> 20, 12);
            match funct3 {
                0 => format!("addi {d}, {a}, {}", hex(i64::from(imm))),
                2 => format!("slti {d}, {a}, {}", hex(i64::from(imm))),
                3 => format!("sltiu {d}, {a}, {}", hex(i64::from(imm))),
                4 => format!("xori {d}, {a}, {}", hex(i64::from(imm))),
                6 => format!("ori {d}, {a}, {}", hex(i64::from(imm))),
                7 => format!("andi {d}, {a}, {}", hex(i64::from(imm))),
                1 => format!("slli {d}, {a}, {}", (word >> 20) & 0x1f),
                5 => {
                    let shamt = (word >> 20) & 0x1f;
                    if funct7 == 0x20 {
                        format!("srai {d}, {a}, {shamt}")
                    } else {
                        format!("srli {d}, {a}, {shamt}")
                    }
                }
                _ => format!(".insn {word:#010x}"),
            }
        }
        0b011_0011 => {
            let a = name_of(rs1);
            let b = name_of(rs2);
            let d = name_of(rd);
            let name = match (funct7, funct3) {
                (0x00, 0) => "add",
                (0x20, 0) => "sub",
                (0x00, 1) => "sll",
                (0x00, 2) => "slt",
                (0x00, 3) => "sltu",
                (0x00, 4) => "xor",
                (0x00, 5) => "srl",
                (0x20, 5) => "sra",
                (0x00, 6) => "or",
                (0x00, 7) => "and",
                _ => return format!(".insn {word:#010x}"),
            };
            format!("{name} {d}, {a}, {b}")
        }
        0b000_1111 => match funct3 {
            0 => "fence".into(),
            1 => "fence.i".into(),
            _ => format!(".insn {word:#010x}"),
        },
        0b111_0011 => {
            let csr = word >> 20;
            let d = name_of(rd);
            let a = name_of(rs1);
            match funct3 {
                0 => match csr {
                    0x000 => "ecall".into(),
                    0x001 => "ebreak".into(),
                    0x302 => "mret".into(),
                    0x105 => "wfi".into(),
                    _ => format!(".insn {word:#010x}"),
                },
                1 => format!("csrrw {d}, {csr:#x}, {a}"),
                2 => format!("csrrs {d}, {csr:#x}, {a}"),
                3 => format!("csrrc {d}, {csr:#x}, {a}"),
                5 => format!("csrrwi {d}, {csr:#x}, {rs1}"),
                6 => format!("csrrsi {d}, {csr:#x}, {rs1}"),
                7 => format!("csrrci {d}, {csr:#x}, {rs1}"),
                _ => format!(".insn {word:#010x}"),
            }
        }
        _ => format!(".insn {word:#010x}"),
    }
}

/// The 16-bit encoding.
pub fn compressed(word: u16) -> String {
    let quadrant = word & 0b11;
    let funct3 = (word >> 13) & 7;
    match quadrant {
        0b00 => match funct3 {
            0 => {
                let rd = 8 + ((word >> 2) & 7);
                let imm = ((word as u32 >> 7) & 0xf) << 6
                    | ((word as u32 >> 11) & 0x3) << 4
                    | ((word as u32 >> 5) & 1) << 3
                    | ((word as u32 >> 6) & 1) << 2;
                format!("c.addi4spn x{rd}, sp, {imm:#x}")
            }
            2 => {
                let rd = 8 + ((word >> 2) & 7);
                let rs1 = 8 + ((word >> 7) & 7);
                let offset = ((word as u32 >> 10) & 7) << 3
                    | ((word as u32 >> 6) & 1) << 2
                    | ((word as u32 >> 5) & 1) << 6;
                format!("c.lw x{rd}, {offset:#x}(x{rs1})")
            }
            6 => {
                let rs2 = 8 + ((word >> 2) & 7);
                let rs1 = 8 + ((word >> 7) & 7);
                let offset = ((word as u32 >> 10) & 7) << 3
                    | ((word as u32 >> 6) & 1) << 2
                    | ((word as u32 >> 5) & 1) << 6;
                format!("c.sw x{rs2}, {offset:#x}(x{rs1})")
            }
            _ => format!(".c.insn {word:04x}"),
        },
        0b01 => match funct3 {
            0 => {
                let rd = (word >> 7) & 0x1f;
                let imm = sext(u32::from(((word >> 12) & 1) << 5 | ((word >> 2) & 0x1f)), 6);
                if rd == 0 {
                    if imm == 0 {
                        "c.nop".into()
                    } else {
                        format!("c.nop {imm}")
                    }
                } else {
                    format!("c.addi x{rd}, {imm}")
                }
            }
            1 => format!("c.jal {}", hex(i64::from(jump_offset(word)))),
            2 => {
                let rd = (word >> 7) & 0x1f;
                let imm = sext(u32::from(((word >> 12) & 1) << 5 | ((word >> 2) & 0x1f)), 6);
                format!("c.li x{rd}, {imm}")
            }
            3 => {
                let rd = (word >> 7) & 0x1f;
                if rd == 2 {
                    let raw = (u32::from((word >> 12) & 1) << 9)
                        | (u32::from((word >> 3) & 0x3) << 7)
                        | (u32::from((word >> 5) & 1) << 6)
                        | (u32::from((word >> 2) & 1) << 5)
                        | (u32::from((word >> 6) & 1) << 4);
                    format!("c.addi16sp {}", hex(i64::from(sext(raw, 10))))
                } else {
                    let raw = (u32::from((word >> 12) & 1) << 5) | u32::from((word >> 2) & 0x1f);
                    format!("c.lui x{rd}, {:#x}", sext(raw << 12, 18) >> 12)
                }
            }
            4 => {
                let rd = 8 + ((word >> 7) & 7);
                let shamt = (word >> 2) & 0x1f;
                match (word >> 10) & 3 {
                    0 => format!("c.srli x{rd}, {shamt}"),
                    1 => format!("c.srai x{rd}, {shamt}"),
                    2 => {
                        let imm =
                            sext(u32::from(((word >> 12) & 1) << 5 | ((word >> 2) & 0x1f)), 6);
                        format!("c.andi x{rd}, {imm}")
                    }
                    _ => {
                        let rs2 = 8 + ((word >> 2) & 7);
                        let name = match (word >> 5) & 3 {
                            0 => "c.sub",
                            1 => "c.xor",
                            2 => "c.or",
                            _ => "c.and",
                        };
                        format!("{name} x{rd}, x{rs2}")
                    }
                }
            }
            5 => format!("c.j {}", hex(i64::from(jump_offset(word)))),
            6 | 7 => {
                let rs1 = 8 + ((word >> 7) & 7);
                let name = if funct3 == 6 { "c.beqz" } else { "c.bnez" };
                format!("{name} x{rs1}, {}", hex(i64::from(branch_offset(word))))
            }
            _ => format!(".c.insn {word:04x}"),
        },
        0b10 => match funct3 {
            0 => {
                let rd = (word >> 7) & 0x1f;
                format!("c.slli x{rd}, {}", (word >> 2) & 0x1f)
            }
            2 => {
                let rd = (word >> 7) & 0x1f;
                let offset = (u32::from((word >> 12) & 1) << 5)
                    | (u32::from((word >> 4) & 0x7) << 2)
                    | (u32::from((word >> 2) & 0x3) << 6);
                format!("c.lwsp x{rd}, {offset:#x}(sp)")
            }
            4 => {
                let rd = (word >> 7) & 0x1f;
                let rs2 = (word >> 2) & 0x1f;
                match (word >> 12) & 0xf {
                    0b1000 if rs2 == 0 => format!("c.jr x{rd}"),
                    0b1000 => format!("c.mv x{rd}, x{rs2}"),
                    0b1001 if rd == 0 && rs2 == 0 => "c.ebreak".into(),
                    0b1001 if rs2 == 0 => format!("c.jalr x{rd}"),
                    0b1001 => format!("c.add x{rd}, x{rs2}"),
                    _ => format!(".c.insn {word:04x}"),
                }
            }
            6 => {
                let rs2 = (word >> 2) & 0x1f;
                let offset =
                    (u32::from((word >> 9) & 0xf) << 2) | (u32::from((word >> 7) & 0x3) << 6);
                format!("c.swsp x{rs2}, {offset:#x}(sp)")
            }
            _ => format!(".c.insn {word:04x}"),
        },
        _ => format!(".c.insn {word:04x}"),
    }
}

fn jump_offset(word: u16) -> i32 {
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

fn branch_offset(word: u16) -> i32 {
    let raw = (u32::from((word >> 12) & 1) << 8)
        | (u32::from((word >> 10) & 0x3) << 3)
        | (u32::from((word >> 5) & 0x3) << 6)
        | (u32::from((word >> 3) & 0x3) << 1)
        | (u32::from((word >> 2) & 1) << 5);
    sext(raw, 9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_32_bit_forms_print() {
        assert_eq!(full(0x0050_0093), "addi x1, x0, 0x5");
        assert_eq!(full(0x0000_006f), "jal x0, 0x0");
        assert_eq!(full(0x3020_0073), "mret");
    }

    #[test]
    fn the_compressed_forms_print() {
        assert_eq!(compressed(0x0085), "c.addi x1, 1");
        assert_eq!(compressed(0xa011), "c.j 0x4");
    }
}
