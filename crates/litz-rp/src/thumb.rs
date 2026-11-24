//! Thumb disassembly for the debugger.
//!
//! The RP2040's core executes Thumb-2, but everything a Pico firmware's hot
//! path uses is Thumb-16, and a debugger listing that names the common forms
//! and prints an unknown encoding as raw bytes is more useful than one that
//! guesses. This is the same shape `esp-soc` prints: an address, the decoded
//! instruction, one per line.
//!
//! Only code the core can fetch is decoded: the bootrom, SRAM and the
//! XIP-mapped flash array. A listing that runs into a peripheral window stops
//! rather than inventing bytes.

use crate::bus::{FLASH_BASE, FLASH_SIZE, Memory, ROM_BASE, ROM_SIZE, SRAM_BASE, SRAM_SIZE};

const REGISTERS: [&str; 16] = [
    "r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "r12", "sp", "lr",
    "pc",
];

/// One instruction at `addr`, and how many bytes it occupies.
///
/// The text is the mnemonic and operands; an encoding this table does not
/// know is printed as its halfwords so a reader can still see what the core
/// fetches.
pub fn decode(memory: &Memory, addr: u32, count: usize) -> String {
    let mut out = String::new();
    let mut pc = addr;
    for _ in 0..count {
        let Some(half) = read16(memory, pc) else {
            break;
        };
        let (text, len) = instruction(memory, pc, half);
        out.push_str(&format!("{pc:08x}: {text:<30}\n"));
        pc = pc.wrapping_add(len);
    }
    out
}

/// The bytes at `addr` if the core could fetch them.
pub fn read16(memory: &Memory, addr: u32) -> Option<u16> {
    let byte = |at: u32| -> Option<u8> {
        if (FLASH_BASE..FLASH_BASE + FLASH_SIZE as u32).contains(&at) {
            memory.flash.get((at - FLASH_BASE) as usize).copied()
        } else if (SRAM_BASE..SRAM_BASE + SRAM_SIZE as u32).contains(&at) {
            memory.sram.get((at - SRAM_BASE) as usize).copied()
        } else if (ROM_BASE..ROM_BASE + ROM_SIZE as u32).contains(&at) {
            memory.rom.get((at - ROM_BASE) as usize).copied()
        } else {
            None
        }
    };
    Some(u16::from_le_bytes([
        byte(addr)?,
        byte(addr.wrapping_add(1))?,
    ]))
}

fn reg(number: u32) -> &'static str {
    REGISTERS[(number & 0xf) as usize]
}

fn signed(value: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((value << shift) as i32) >> shift
}

fn registers(list: u16) -> String {
    let mut names = Vec::new();
    for bit in 0..8 {
        if list & (1 << bit) != 0 {
            names.push(reg(bit));
        }
    }
    format!("{{{}}}", names.join(", "))
}

/// A push/pop list: the low registers plus whichever of lr or pc the
/// instruction carries in its one spare bit.
fn register_list(h: u32, extra: Option<&'static str>) -> String {
    let mut names: Vec<&str> = (0..8u32)
        .filter(|bit| h & (1 << bit) != 0)
        .map(reg)
        .collect();
    if let Some(name) = extra {
        names.push(name);
    }
    format!("{{{}}}", names.join(", "))
}

fn instruction(memory: &Memory, addr: u32, h: u16) -> (String, u32) {
    let h = u32::from(h);
    match h >> 12 {
        0x0 | 0x1 => match (h >> 11) & 3 {
            // 000 11: add/sub, register or three-bit immediate.
            3 => {
                let sub = h & 0x0200 != 0;
                let immediate = h & 0x0400 != 0;
                let name = if sub { "sub" } else { "add" };
                let operand = if immediate {
                    format!("#{}", (h >> 6) & 7)
                } else {
                    reg((h >> 6) & 7).to_string()
                };
                (
                    format!("{name} {}, {}, {operand}", reg(h & 7), reg((h >> 3) & 7)),
                    2,
                )
            }
            // 000 00 through 000 10: a shift by an immediate, the only place
            // Thumb-16 has one.
            op => {
                let name = ["lsl", "lsr", "asr"][op as usize];
                (
                    format!(
                        "{name} {}, {}, #{}",
                        reg(h & 7),
                        reg((h >> 3) & 7),
                        (h >> 6) & 0x1f
                    ),
                    2,
                )
            }
        },
        // 001 00: movs rd, #imm8; 001 01: cmp rd, #imm8.
        0x2 => {
            let rd = reg((h >> 8) & 7);
            let imm = h & 0xff;
            if h & 0x0800 == 0 {
                (format!("movs {rd}, #{imm}"), 2)
            } else {
                (format!("cmp {rd}, #{imm}"), 2)
            }
        }
        // 001 10 adds, 001 11 subs, both with an eight-bit immediate.
        0x3 => {
            let name = if h & 0x0800 != 0 { "subs" } else { "adds" };
            (format!("{name} {}, #{}", reg((h >> 8) & 7), h & 0xff), 2)
        }
        0x4 => {
            if h & 0x0c00 == 0x0400 {
                // High-register operation, BX or BLX.
                let op = (h >> 8) & 3;
                let target = reg((h >> 3) & 0xf);
                let source = reg(h & 7);
                match op {
                    0 => (format!("add {target}, {source}"), 2),
                    1 => (format!("cmp {target}, {source}"), 2),
                    2 => (format!("mov {target}, {source}"), 2),
                    _ => {
                        let name = if h & 0x0080 != 0 { "blx" } else { "bx" };
                        (format!("{name} {target}"), 2)
                    }
                }
            } else if h & 0x0c00 == 0x0800 {
                // LDR (literal).
                let offset = (h & 0xff) * 4;
                let at = (addr + 4) & !3;
                (
                    format!(
                        "ldr {}, [pc, #{}] ; {:#010x}",
                        reg((h >> 8) & 7),
                        offset,
                        at.wrapping_add(offset)
                    ),
                    2,
                )
            } else {
                let name = [
                    "and", "eor", "lsl", "lsr", "asr", "adc", "sbc", "ror", "tst", "neg", "cmp",
                    "cmn", "orr", "mul", "bic", "mvn",
                ][((h >> 6) & 0xf) as usize];
                let op = (h >> 6) & 0xf;
                let text = match op {
                    13 => format!("mul {}, {}, {}", reg(h & 7), reg((h >> 3) & 7), reg(h & 7)),
                    9 => format!("neg {}, {}", reg(h & 7), reg((h >> 3) & 7)),
                    8 | 10 | 11 => format!("{name} {}, {}", reg(h & 7), reg((h >> 3) & 7)),
                    _ => format!("{name} {}, {}", reg(h & 7), reg((h >> 3) & 7)),
                };
                (text, 2)
            }
        }
        0x5 => {
            let name = [
                "str", "strh", "strb", "ldrsb", "ldr", "ldrh", "ldrb", "ldrsh",
            ][((h >> 9) & 7) as usize];
            (
                format!(
                    "{name} {}, [{}, {}]",
                    reg(h & 7),
                    reg((h >> 3) & 7),
                    reg((h >> 6) & 7)
                ),
                2,
            )
        }
        0x6..=0x8 => {
            let name = match h >> 12 {
                0x6 => "ldr",
                0x7 => "ldrb",
                _ => "ldrh",
            };
            let name = if h & 0x0800 != 0 {
                match name {
                    "ldr" => "ldr",
                    "ldrb" => "ldrb",
                    _ => "ldrh",
                }
            } else {
                match name {
                    "ldr" => "str",
                    "ldrb" => "strb",
                    _ => "strh",
                }
            };
            let scale = match h >> 12 {
                0x6 => 4,
                0x8 => 2,
                _ => 1,
            };
            let offset = ((h >> 6) & 0x1f) * scale;
            (
                format!(
                    "{name} {}, [{}, #{}]",
                    reg(h & 7),
                    reg((h >> 3) & 7),
                    offset
                ),
                2,
            )
        }
        0x9 => {
            let name = if h & 0x0800 != 0 { "ldr" } else { "str" };
            (
                format!("{name} {}, [sp, #{}]", reg((h >> 8) & 7), (h & 0xff) * 4),
                2,
            )
        }
        0xa => (
            format!(
                "adr {}, {:#010x}",
                reg((h >> 8) & 7),
                ((addr + 4) & !3).wrapping_add((h & 0xff) * 4)
            ),
            2,
        ),
        0xb => instruction_b(addr, h),
        0xc => {
            let load = h & 0x0800 != 0;
            let name = if load { "ldmia" } else { "stmia" };
            let base = reg((h >> 8) & 7);
            let writeback = if h & (1 << ((h >> 8) & 7)) == 0 {
                "!"
            } else {
                ""
            };
            (
                format!("{name} {base}{writeback}, {}", registers(h as u16)),
                2,
            )
        }
        0xd => {
            let condition = (h >> 8) & 0xf;
            let offset = signed(h & 0xff, 8) * 2;
            let target = (addr as i32 + 4 + offset) as u32;
            match condition {
                0xe => (format!("udf #{}", h & 0xff), 2),
                0xf => (format!("svc #{}", h & 0xff), 2),
                _ => (
                    format!("b{} {:#010x}", CONDITIONS[condition as usize], target),
                    2,
                ),
            }
        }
        0xe => {
            let offset = signed(h & 0x7ff, 11) * 2;
            let target = (addr as i32 + 4 + offset) as u32;
            (format!("b {target:#010x}"), 2)
        }
        _ => instruction_32(memory, addr, h),
    }
}

const CONDITIONS: [&str; 14] = [
    "eq", "ne", "cs", "cc", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le",
];

fn instruction_b(addr: u32, h: u32) -> (String, u32) {
    match h & 0xff00 {
        // add/sub sp, #imm7
        0xb000 => {
            let name = if h & 0x0080 != 0 { "sub" } else { "add" };
            (format!("{name} sp, #{}", (h & 0x7f) * 4), 2)
        }
        // cbz/cbnz: 1011 0i0i iiiiinnn
        0xb100 | 0xb300 | 0xb900 | 0xbb00 => {
            let nonzero = h & 0x0800 != 0;
            let offset = ((h >> 3) & 0x1f) * 2 + ((h >> 9) & 0x1) * 2;
            let target = (addr as i32 + 4 + offset as i32) as u32;
            (
                format!(
                    "{} {}, {target:#010x}",
                    if nonzero { "cbnz" } else { "cbz" },
                    reg(h & 7)
                ),
                2,
            )
        }
        // sxth/sxtb/uxth/uxtb
        0xb200 => {
            let index = ((h >> 6) & 3) as usize;
            let name = ["sxth", "sxtb", "uxth", "uxtb"][index];
            (format!("{name} {}, {}", reg(h & 7), reg((h >> 3) & 7)), 2)
        }
        // PUSH takes 0xb400-0xb5ff, so its mask drops the lr bit.
        _ if h & 0xfe00 == 0xb400 => (
            format!(
                "push {}",
                register_list(h, (h & 0x0100 != 0).then_some("lr"))
            ),
            2,
        ),
        // POP likewise, with pc instead of lr.
        _ if h & 0xfe00 == 0xbc00 => (
            format!(
                "pop {}",
                register_list(h, (h & 0x0100 != 0).then_some("pc"))
            ),
            2,
        ),
        // nop and the other architectural hints
        0xbf00 => (
            if h & 0xf == 0 {
                "nop".to_string()
            } else {
                format!("hint #{}", h & 0xf)
            },
            2,
        ),
        // rev / rev16 / revsh
        0xba00 => {
            let name = match h & 0x00c0 {
                0x00 => "rev",
                0x40 => "rev16",
                _ => "revsh",
            };
            (format!("{name} {}, {}", reg(h & 7), reg((h >> 3) & 7)), 2)
        }
        _ => (format!(".short {:#06x}", h & 0xffff), 2),
    }
}

fn instruction_32(memory: &Memory, addr: u32, first: u32) -> (String, u32) {
    let Some(second) = read16(memory, addr.wrapping_add(2)) else {
        return (format!(".short {:#06x}", first & 0xffff), 2);
    };
    let second = u32::from(second);
    let s = (first >> 10) & 1;
    let j1 = (second >> 13) & 1;
    let j2 = (second >> 11) & 1;
    if first & 0xf800 == 0xf000 && second & 0xd000 == 0xd000 {
        // BL: S in bit 10 of the first half, I1/I2 reconstructed from J1/J2.
        let i1 = (!(j1 ^ s)) & 1;
        let i2 = (!(j2 ^ s)) & 1;
        let offset =
            (s << 24) | (i1 << 23) | (i2 << 22) | ((first & 0x3ff) << 12) | ((second & 0x7ff) << 1);
        let target = (addr as i32 + 4 + signed(offset, 25)) as u32;
        return (format!("bl {target:#010x}"), 4);
    }
    if first & 0xf800 == 0xf000 && second & 0xd000 == 0xc000 {
        let i1 = (!(j1 ^ s)) & 1;
        let i2 = (!(j2 ^ s)) & 1;
        let offset =
            (s << 24) | (i1 << 23) | (i2 << 22) | ((first & 0x3ff) << 12) | ((second & 0x7ff) << 1);
        let target = ((addr as i32 + 4) & !3) + signed(offset, 25);
        return (format!("blx {:#010x}", target as u32), 4);
    }
    (format!(".word {:#06x}{:04x}", first & 0xffff, second), 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_with(code: &[u8]) -> Memory {
        let mut memory = Memory::new();
        memory.flash[..code.len()].copy_from_slice(code);
        memory
    }

    #[test]
    fn common_thumb_instructions_decode() {
        // movs r0, #1; adds r0, #2; b .+4; push {lr}; pop {pc}; bx lr
        let memory = memory_with(&[
            0x01, 0x20, 0x02, 0x30, 0x00, 0xe0, 0x00, 0xb5, 0x00, 0xbd, 0x70, 0x47,
        ]);
        let text = decode(&memory, FLASH_BASE, 6);
        assert!(text.contains("movs r0, #1"), "{text}");
        assert!(text.contains("adds r0, #2"), "{text}");
        assert!(text.contains("b 0x10000008"), "{text}");
        assert!(text.contains("push {lr}"), "{text}");
        assert!(text.contains("pop {pc}"), "{text}");
        assert!(text.contains("bx lr"), "{text}");
    }

    #[test]
    fn a_thirty_two_bit_branch_takes_four_bytes() {
        // bl 0x10000010 from 0x10000000: S=0, J1=J2=1 (so I1=I2=0) and
        // imm11 = 6, which the core doubles into an offset of 0x0c.
        let memory = memory_with(&[0x00, 0xf0, 0x06, 0xf8, 0x00, 0xbf]);
        let text = decode(&memory, FLASH_BASE, 2);
        assert!(text.starts_with("10000000: bl 0x10000010"), "{text}");
        assert!(text.contains("10000004: nop"), "{text}");
    }

    #[test]
    fn a_listing_stops_at_a_peripheral_window() {
        let memory = Memory::new();
        let text = decode(&memory, 0x4000_0000, 4);
        assert!(text.is_empty(), "a peripheral is not code: {text:?}");
    }
}
