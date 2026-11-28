//! A complete 8051 disassembler for the debugger.
//!
//! Every defined opcode is decoded with its operands. Direct addresses print
//! by SFR name where the part has one (`P1`, `TCON`, `ACC`), bit operands
//! print as `P1.0`, and branch targets print as absolute code addresses, so a
//! disassembly of a running core reads the way the assembly in the firmware
//! source does. The undefined opcode `0xA5` prints as `DB 0xa5`.

/// Names of the SFRs this part implements.
pub fn sfr_name(addr: u8) -> Option<&'static str> {
    Some(match addr {
        0x80 => "P0",
        0x81 => "SP",
        0x82 => "DPL",
        0x83 => "DPH",
        0x84 => "DPL1",
        0x85 => "DPH1",
        0x87 => "PCON",
        0x88 => "TCON",
        0x89 => "TMOD",
        0x8A => "TL0",
        0x8B => "TL1",
        0x8C => "TH0",
        0x8D => "TH1",
        0x8E => "AUXR",
        0x90 => "P1",
        0x98 => "SCON",
        0x99 => "SBUF",
        0xA0 => "P2",
        0xA2 => "AUXR1",
        0xA8 => "IE",
        0xB0 => "P3",
        0xB8 => "IP",
        0xC8 => "T2CON",
        0xCA => "RCAP2L",
        0xCB => "RCAP2H",
        0xCC => "TL2",
        0xCD => "TH2",
        0xD0 => "PSW",
        0xE0 => "ACC",
        0xF0 => "B",
        _ => return None,
    })
}

fn direct_name(addr: u8) -> String {
    match sfr_name(addr) {
        Some(name) => name.to_string(),
        None => format!("{addr:#04x}"),
    }
}

fn bit_name(bit: u8) -> String {
    if bit < 0x80 {
        format!("{:#04x}.{}", 0x20 + (bit >> 3), bit & 7)
    } else {
        let addr = bit & 0xF8;
        match sfr_name(addr) {
            Some(name) => format!("{name}.{}", bit & 7),
            None => format!("{addr:#04x}.{}", bit & 7),
        }
    }
}

fn byte(code: &[u8], at: usize) -> u8 {
    code.get(at).copied().unwrap_or(0)
}

fn word(code: &[u8], at: usize) -> u16 {
    (u16::from(byte(code, at)) << 8) | u16::from(byte(code, at + 1))
}

fn target(addr: usize, len: usize, offset: u8) -> String {
    let next = addr as u16 + len as u16;
    let dest = next.wrapping_add(offset as i8 as i16 as u16);
    format!("0x{dest:04X}")
}

fn absolute(addr: usize, len: usize, low: u8, opcode: u8) -> String {
    let page = (addr as u16).wrapping_add(len as u16);
    let dest = (page & 0xF800) | (u16::from(opcode & 0xE0) << 3) | u16::from(low);
    format!("0x{dest:04X}")
}

/// Disassemble `count` instructions starting at code address `start`.
pub fn disassemble(code: &[u8], start: u32, count: usize) -> String {
    let mut out = String::new();
    let mut addr = start as usize;
    for _ in 0..count {
        if addr >= code.len() {
            break;
        }
        let (len, text) = decode(code, addr);
        let end = (addr + len).min(code.len());
        let bytes = code[addr..end]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!("{addr:04X}: {bytes:<12} {text}\n"));
        addr += len.max(1);
    }
    out
}

/// Decode one instruction: its length in bytes and its assembly text.
pub fn decode(code: &[u8], addr: usize) -> (usize, String) {
    let op = byte(code, addr);
    match op {
        0x00 => (1, "NOP".into()),
        0x02 => {
            let dest = word(code, addr + 1);
            (3, format!("LJMP 0x{dest:04X}"))
        }
        0x12 => {
            let dest = word(code, addr + 1);
            (3, format!("LCALL 0x{dest:04X}"))
        }
        0x22 => (1, "RET".into()),
        0x32 => (1, "RETI".into()),
        0x03 => (1, "RR A".into()),
        0x13 => (1, "RRC A".into()),
        0x23 => (1, "RL A".into()),
        0x33 => (1, "RLC A".into()),
        0x04 => (1, "INC A".into()),
        0x05 => (2, format!("INC {}", direct_name(byte(code, addr + 1)))),
        0x06 => (1, "INC @R0".into()),
        0x07 => (1, "INC @R1".into()),
        0x08..=0x0F => (1, format!("INC R{}", op & 7)),
        0x10 => (
            3,
            format!(
                "JBC {}, {}",
                bit_name(byte(code, addr + 1)),
                target(addr, 3, byte(code, addr + 2))
            ),
        ),
        0x14 => (1, "DEC A".into()),
        0x15 => (2, format!("DEC {}", direct_name(byte(code, addr + 1)))),
        0x16 => (1, "DEC @R0".into()),
        0x17 => (1, "DEC @R1".into()),
        0x18..=0x1F => (1, format!("DEC R{}", op & 7)),
        0x20 => (
            3,
            format!(
                "JB {}, {}",
                bit_name(byte(code, addr + 1)),
                target(addr, 3, byte(code, addr + 2))
            ),
        ),
        0x24 => (2, format!("ADD A, #{}", immediate(code, addr))),
        0x25 => (2, format!("ADD A, {}", direct_name(byte(code, addr + 1)))),
        0x26 => (1, "ADD A, @R0".into()),
        0x27 => (1, "ADD A, @R1".into()),
        0x28..=0x2F => (1, format!("ADD A, R{}", op & 7)),
        0x30 => (
            3,
            format!(
                "JNB {}, {}",
                bit_name(byte(code, addr + 1)),
                target(addr, 3, byte(code, addr + 2))
            ),
        ),
        0x34 => (2, format!("ADDC A, #{}", immediate(code, addr))),
        0x35 => (2, format!("ADDC A, {}", direct_name(byte(code, addr + 1)))),
        0x36 => (1, "ADDC A, @R0".into()),
        0x37 => (1, "ADDC A, @R1".into()),
        0x38..=0x3F => (1, format!("ADDC A, R{}", op & 7)),
        0x40 => (2, format!("JC {}", target(addr, 2, byte(code, addr + 1)))),
        0x42 => (2, format!("ORL {}, A", direct_name(byte(code, addr + 1)))),
        0x43 => (
            3,
            format!(
                "ORL {}, #{}",
                direct_name(byte(code, addr + 1)),
                immediate(code, addr + 1)
            ),
        ),
        0x44 => (2, format!("ORL A, #{}", immediate(code, addr))),
        0x45 => (2, format!("ORL A, {}", direct_name(byte(code, addr + 1)))),
        0x46 => (1, "ORL A, @R0".into()),
        0x47 => (1, "ORL A, @R1".into()),
        0x48..=0x4F => (1, format!("ORL A, R{}", op & 7)),
        0x50 => (2, format!("JNC {}", target(addr, 2, byte(code, addr + 1)))),
        0x52 => (2, format!("ANL {}, A", direct_name(byte(code, addr + 1)))),
        0x53 => (
            3,
            format!(
                "ANL {}, #{}",
                direct_name(byte(code, addr + 1)),
                immediate(code, addr + 1)
            ),
        ),
        0x54 => (2, format!("ANL A, #{}", immediate(code, addr))),
        0x55 => (2, format!("ANL A, {}", direct_name(byte(code, addr + 1)))),
        0x56 => (1, "ANL A, @R0".into()),
        0x57 => (1, "ANL A, @R1".into()),
        0x58..=0x5F => (1, format!("ANL A, R{}", op & 7)),
        0x60 => (2, format!("JZ {}", target(addr, 2, byte(code, addr + 1)))),
        0x62 => (2, format!("XRL {}, A", direct_name(byte(code, addr + 1)))),
        0x63 => (
            3,
            format!(
                "XRL {}, #{}",
                direct_name(byte(code, addr + 1)),
                immediate(code, addr + 1)
            ),
        ),
        0x64 => (2, format!("XRL A, #{}", immediate(code, addr))),
        0x65 => (2, format!("XRL A, {}", direct_name(byte(code, addr + 1)))),
        0x66 => (1, "XRL A, @R0".into()),
        0x67 => (1, "XRL A, @R1".into()),
        0x68..=0x6F => (1, format!("XRL A, R{}", op & 7)),
        0x70 => (2, format!("JNZ {}", target(addr, 2, byte(code, addr + 1)))),
        0x72 => (2, format!("ORL C, {}", bit_name(byte(code, addr + 1)))),
        0x73 => (1, "JMP @A+DPTR".into()),
        0x74 => (2, format!("MOV A, #{}", immediate(code, addr))),
        0x75 => (
            3,
            format!(
                "MOV {}, #{}",
                direct_name(byte(code, addr + 1)),
                immediate(code, addr + 1)
            ),
        ),
        0x76 => (2, format!("MOV @R0, #{}", immediate(code, addr))),
        0x77 => (2, format!("MOV @R1, #{}", immediate(code, addr))),
        0x78..=0x7F => (2, format!("MOV R{}, #{}", op & 7, immediate(code, addr))),
        0x80 => (2, format!("SJMP {}", target(addr, 2, byte(code, addr + 1)))),
        0x82 => (2, format!("ANL C, {}", bit_name(byte(code, addr + 1)))),
        0x83 => (1, "MOVC A, @A+PC".into()),
        0x84 => (1, "DIV AB".into()),
        0x85 => (
            3,
            format!(
                "MOV {}, {}",
                direct_name(byte(code, addr + 2)),
                direct_name(byte(code, addr + 1))
            ),
        ),
        0x86 => (2, format!("MOV {}, @R0", direct_name(byte(code, addr + 1)))),
        0x87 => (2, format!("MOV {}, @R1", direct_name(byte(code, addr + 1)))),
        0x88..=0x8F => (
            2,
            format!("MOV {}, R{}", direct_name(byte(code, addr + 1)), op & 7),
        ),
        0x90 => {
            let dest = word(code, addr + 1);
            (3, format!("MOV DPTR, #0x{dest:04X}"))
        }
        0x92 => (2, format!("MOV {}, C", bit_name(byte(code, addr + 1)))),
        0x93 => (1, "MOVC A, @A+DPTR".into()),
        0x94 => (2, format!("SUBB A, #{}", immediate(code, addr))),
        0x95 => (2, format!("SUBB A, {}", direct_name(byte(code, addr + 1)))),
        0x96 => (1, "SUBB A, @R0".into()),
        0x97 => (1, "SUBB A, @R1".into()),
        0x98..=0x9F => (1, format!("SUBB A, R{}", op & 7)),
        0xA0 => (2, format!("ORL C, /{}", bit_name(byte(code, addr + 1)))),
        0xA2 => (2, format!("MOV C, {}", bit_name(byte(code, addr + 1)))),
        0xA3 => (1, "INC DPTR".into()),
        0xA4 => (1, "MUL AB".into()),
        0xA5 => (1, "DB 0xa5".into()),
        0xA6 => (2, format!("MOV @R0, {}", direct_name(byte(code, addr + 1)))),
        0xA7 => (2, format!("MOV @R1, {}", direct_name(byte(code, addr + 1)))),
        0xA8..=0xAF => (
            2,
            format!("MOV R{}, {}", op & 7, direct_name(byte(code, addr + 1))),
        ),
        0xB0 => (2, format!("ANL C, /{}", bit_name(byte(code, addr + 1)))),
        0xB2 => (2, format!("CPL {}", bit_name(byte(code, addr + 1)))),
        0xB3 => (1, "CPL C".into()),
        0xB4 => (
            3,
            format!(
                "CJNE A, #{}, {}",
                immediate(code, addr),
                target(addr, 3, byte(code, addr + 2))
            ),
        ),
        0xB5 => (
            3,
            format!(
                "CJNE A, {}, {}",
                direct_name(byte(code, addr + 1)),
                target(addr, 3, byte(code, addr + 2))
            ),
        ),
        0xB6 => (
            3,
            format!(
                "CJNE @R0, #{}, {}",
                immediate(code, addr + 1),
                target(addr, 3, byte(code, addr + 2))
            ),
        ),
        0xB7 => (
            3,
            format!(
                "CJNE @R1, #{}, {}",
                immediate(code, addr + 1),
                target(addr, 3, byte(code, addr + 2))
            ),
        ),
        0xB8..=0xBF => (
            3,
            format!(
                "CJNE R{}, #{}, {}",
                op & 7,
                immediate(code, addr + 1),
                target(addr, 3, byte(code, addr + 2))
            ),
        ),
        0xC0 => (2, format!("PUSH {}", direct_name(byte(code, addr + 1)))),
        0xC2 => (2, format!("CLR {}", bit_name(byte(code, addr + 1)))),
        0xC3 => (1, "CLR C".into()),
        0xC4 => (1, "SWAP A".into()),
        0xC5 => (2, format!("XCH A, {}", direct_name(byte(code, addr + 1)))),
        0xC6 => (1, "XCH A, @R0".into()),
        0xC7 => (1, "XCH A, @R1".into()),
        0xC8..=0xCF => (1, format!("XCH A, R{}", op & 7)),
        0xD0 => (2, format!("POP {}", direct_name(byte(code, addr + 1)))),
        0xD2 => (2, format!("SETB {}", bit_name(byte(code, addr + 1)))),
        0xD3 => (1, "SETB C".into()),
        0xD4 => (1, "DA A".into()),
        0xD5 => (
            3,
            format!(
                "DJNZ {}, {}",
                direct_name(byte(code, addr + 1)),
                target(addr, 3, byte(code, addr + 2))
            ),
        ),
        0xD6 => (1, "XCHD A, @R0".into()),
        0xD7 => (1, "XCHD A, @R1".into()),
        0xD8..=0xDF => (
            2,
            format!(
                "DJNZ R{}, {}",
                op & 7,
                target(addr, 2, byte(code, addr + 1))
            ),
        ),
        0xE0 => (1, "MOVX A, @DPTR".into()),
        0xE2 => (1, "MOVX A, @R0".into()),
        0xE3 => (1, "MOVX A, @R1".into()),
        0xE4 => (1, "CLR A".into()),
        0xE5 => (2, format!("MOV A, {}", direct_name(byte(code, addr + 1)))),
        0xE6 => (1, "MOV A, @R0".into()),
        0xE7 => (1, "MOV A, @R1".into()),
        0xE8..=0xEF => (1, format!("MOV A, R{}", op & 7)),
        0xF0 => (1, "MOVX @DPTR, A".into()),
        0xF2 => (1, "MOVX @R0, A".into()),
        0xF3 => (1, "MOVX @R1, A".into()),
        0xF4 => (1, "CPL A".into()),
        0xF5 => (2, format!("MOV {}, A", direct_name(byte(code, addr + 1)))),
        0xF6 => (1, "MOV @R0, A".into()),
        0xF7 => (1, "MOV @R1, A".into()),
        0xF8..=0xFF => (1, format!("MOV R{}, A", op & 7)),
        _ if op & 0x1F == 0x01 => {
            let dest = absolute(addr, 2, byte(code, addr + 1), op);
            (2, format!("AJMP {dest}"))
        }
        _ if op & 0x1F == 0x11 => {
            let dest = absolute(addr, 2, byte(code, addr + 1), op);
            (2, format!("ACALL {dest}"))
        }
        _ => (1, format!("DB {op:#04x}")),
    }
}

fn immediate(code: &[u8], addr: usize) -> String {
    format!("{:#04x}", byte(code, addr + 1))
}
