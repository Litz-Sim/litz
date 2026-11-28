//! Shared fixtures for the 8051 tests: a tiny assembler with labels, an
//! inline 32-pin board manifest, and helpers that build a board or a bare
//! machine.

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use litz_8051::Stc89c52;
use litz_board::flatten::FlatBoard;
use litz_board::{Board, Chip, ImageKind};
use litz_catalog::Registry;
use litz_diagram::Circuit;

// -- a tiny MCS-51 assembler ----------------------------------------------

enum Kind {
    Rel,
    Abs16,
}

struct Fixup {
    at: usize,
    label: String,
    kind: Kind,
}

/// Hand-assembles an 8051 image. `org` pads with erased-flash `0xFF`, so the
/// interrupt vectors can be written in address order.
pub struct Asm {
    bytes: Vec<u8>,
    labels: BTreeMap<String, u16>,
    fixups: Vec<Fixup>,
}

impl Default for Asm {
    fn default() -> Self {
        Self::new()
    }
}

impl Asm {
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            labels: BTreeMap::new(),
            fixups: Vec::new(),
        }
    }

    pub fn org(&mut self, addr: u16) {
        let addr = usize::from(addr);
        assert!(addr >= self.bytes.len(), "org moved backwards");
        self.bytes.resize(addr, 0xFF);
    }

    pub fn label(&mut self, name: &str) {
        let addr = self.bytes.len() as u16;
        assert!(
            self.labels.insert(name.to_string(), addr).is_none(),
            "duplicate label {name}"
        );
    }

    pub fn emit(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    pub fn op(&mut self, opcode: u8) {
        self.bytes.push(opcode);
    }

    pub fn op1(&mut self, opcode: u8, a: u8) {
        self.bytes.extend_from_slice(&[opcode, a]);
    }

    pub fn op2(&mut self, opcode: u8, a: u8, b: u8) {
        self.bytes.extend_from_slice(&[opcode, a, b]);
    }

    /// An instruction with one relative operand.
    pub fn rel(&mut self, opcode: u8, label: &str) {
        self.op(opcode);
        let at = self.bytes.len();
        self.bytes.push(0);
        self.fixups.push(Fixup {
            at,
            label: label.to_string(),
            kind: Kind::Rel,
        });
    }

    /// A bit instruction with a relative operand (`JB`, `JNB`, `JBC`).
    pub fn bit_rel(&mut self, opcode: u8, bit: u8, label: &str) {
        self.op2(opcode, bit, 0);
        let at = self.bytes.len() - 1;
        self.fixups.push(Fixup {
            at,
            label: label.to_string(),
            kind: Kind::Rel,
        });
    }

    pub fn ljmp(&mut self, label: &str) {
        self.op(0x02);
        let at = self.bytes.len();
        self.bytes.extend_from_slice(&[0, 0]);
        self.fixups.push(Fixup {
            at,
            label: label.to_string(),
            kind: Kind::Abs16,
        });
    }

    pub fn lcall(&mut self, label: &str) {
        self.op(0x12);
        let at = self.bytes.len();
        self.bytes.extend_from_slice(&[0, 0]);
        self.fixups.push(Fixup {
            at,
            label: label.to_string(),
            kind: Kind::Abs16,
        });
    }

    pub fn finish(mut self) -> Vec<u8> {
        for fixup in &self.fixups {
            let target = *self
                .labels
                .get(&fixup.label)
                .unwrap_or_else(|| panic!("unknown label {}", fixup.label));
            match fixup.kind {
                Kind::Rel => {
                    let base = (fixup.at + 1) as i32;
                    let delta = i32::from(target) - base;
                    assert!(
                        (-128..=127).contains(&delta),
                        "relative branch to {} out of range: {delta}",
                        fixup.label
                    );
                    self.bytes[fixup.at] = delta as i8 as u8;
                }
                Kind::Abs16 => {
                    self.bytes[fixup.at] = (target >> 8) as u8;
                    self.bytes[fixup.at + 1] = target as u8;
                }
            }
        }
        self.bytes
    }

    // -- readable helpers for the common instructions ---------------------

    pub fn nop(&mut self) {
        self.op(0x00);
    }

    pub fn mov_a(&mut self, value: u8) {
        self.op1(0x74, value);
    }

    pub fn mov_a_direct(&mut self, addr: u8) {
        self.op1(0xE5, addr);
    }

    pub fn mov_direct_a(&mut self, addr: u8) {
        self.op1(0xF5, addr);
    }

    pub fn mov_direct(&mut self, addr: u8, value: u8) {
        self.op2(0x75, addr, value);
    }

    pub fn mov_r(&mut self, n: u8, value: u8) {
        self.op1(0x78 | (n & 7), value);
    }

    pub fn mov_direct_r(&mut self, addr: u8, n: u8) {
        self.op1(0x88 | (n & 7), addr);
    }

    pub fn mov_r_direct(&mut self, n: u8, addr: u8) {
        self.op1(0xA8 | (n & 7), addr);
    }

    pub fn mov_indirect(&mut self, n: u8, value: u8) {
        self.op1(0x76 | (n & 7), value);
    }

    pub fn mov_a_indirect(&mut self, n: u8) {
        self.op(0xE6 | (n & 7));
    }

    pub fn mov_indirect_a(&mut self, n: u8) {
        self.op(0xF6 | (n & 7));
    }

    pub fn mov_b(&mut self, value: u8) {
        self.mov_direct(0xF0, value);
    }

    pub fn mov_sbuf(&mut self, value: u8) {
        self.mov_direct(0x99, value);
    }

    pub fn mov_dptr(&mut self, value: u16) {
        self.op(0x90);
        self.bytes.push((value >> 8) as u8);
        self.bytes.push(value as u8);
    }

    pub fn setb(&mut self, bit: u8) {
        self.op1(0xD2, bit);
    }

    pub fn clr_bit(&mut self, bit: u8) {
        self.op1(0xC2, bit);
    }

    pub fn cpl_bit(&mut self, bit: u8) {
        self.op1(0xB2, bit);
    }

    pub fn setb_c(&mut self) {
        self.op(0xD3);
    }

    pub fn clr_c(&mut self) {
        self.op(0xC3);
    }

    pub fn clr_a(&mut self) {
        self.op(0xE4);
    }

    pub fn cpl_a(&mut self) {
        self.op(0xF4);
    }

    pub fn sjmp(&mut self, label: &str) {
        self.rel(0x80, label);
    }

    pub fn jz(&mut self, label: &str) {
        self.rel(0x60, label);
    }

    pub fn jnz(&mut self, label: &str) {
        self.rel(0x70, label);
    }

    pub fn jb(&mut self, bit: u8, label: &str) {
        self.bit_rel(0x20, bit, label);
    }

    pub fn jnb(&mut self, bit: u8, label: &str) {
        self.bit_rel(0x30, bit, label);
    }

    pub fn jbc(&mut self, bit: u8, label: &str) {
        self.bit_rel(0x10, bit, label);
    }

    pub fn djnz_r(&mut self, n: u8, label: &str) {
        self.rel(0xD8 | (n & 7), label);
    }

    pub fn djnz_direct(&mut self, addr: u8, label: &str) {
        self.op1(0xD5, addr);
        let at = self.bytes.len();
        self.bytes.push(0);
        self.fixups.push(Fixup {
            at,
            label: label.to_string(),
            kind: Kind::Rel,
        });
    }

    pub fn inc_direct(&mut self, addr: u8) {
        self.op1(0x05, addr);
    }

    pub fn dec_direct(&mut self, addr: u8) {
        self.op1(0x15, addr);
    }

    pub fn inc_r(&mut self, n: u8) {
        self.op(0x08 | (n & 7));
    }

    pub fn dec_r(&mut self, n: u8) {
        self.op(0x18 | (n & 7));
    }

    pub fn add_a(&mut self, value: u8) {
        self.op1(0x24, value);
    }

    pub fn addc_a(&mut self, value: u8) {
        self.op1(0x34, value);
    }

    pub fn subb_a(&mut self, value: u8) {
        self.op1(0x94, value);
    }

    pub fn anl_a(&mut self, value: u8) {
        self.op1(0x54, value);
    }

    pub fn orl_a(&mut self, value: u8) {
        self.op1(0x44, value);
    }

    pub fn xrl_a(&mut self, value: u8) {
        self.op1(0x64, value);
    }

    pub fn mul_ab(&mut self) {
        self.op(0xA4);
    }

    pub fn div_ab(&mut self) {
        self.op(0x84);
    }

    pub fn da_a(&mut self) {
        self.op(0xD4);
    }

    pub fn ret(&mut self) {
        self.op(0x22);
    }

    pub fn reti(&mut self) {
        self.op(0x32);
    }
}

// -- the inline board ------------------------------------------------------

/// A 32-pin development board with GPIO 0 through 31, the numbering the
/// adapter uses (`port * 8 + bit`).
pub fn board_manifest() -> String {
    let mut text = String::from(
        "[board]\n\
         id = \"board-stc89c52-test\"\n\
         name = \"STC89C52 test board\"\n\
         version = \"1.0.0\"\n\
         description = \"An STC89C52RC with every port pin on a 32-pin header\"\n\
         chip = \"stc89c52\"\n\
         flash_size = \"8KB\"\n\
         psram = false\n",
    );
    for gpio in 0..32u32 {
        text.push_str(&format!(
            "\n[[pins]]\nname = \"GPIO{gpio}\"\ngpio = {gpio}\n"
        ));
    }
    text.push_str(
        "\n[[pins]]\nname = \"GND\"\nfixed = \"gnd\"\nrepeat = 2\n\
         \n[[pins]]\nname = \"3V3\"\nfixed = \"3v3\"\n\
         \n[[pins]]\nname = \"5V\"\nfixed = \"5v\"\n",
    );
    text
}

/// The board's internal schematic: just its chip, with no onboard parts.
pub const BOARD_SCHEMATIC: &str =
    r#"{"version":1,"parts":[{"id":"chip","type":"$chip","attrs":{}}],"connections":[]}"#;

pub fn registry() -> Registry {
    let mut registry = Registry::builtin();
    let id = registry.insert_text(&board_manifest()).unwrap();
    registry.set_schematic(&id, BOARD_SCHEMATIC);
    registry
}

pub fn board(diagram: &str) -> Board {
    let circuit = Circuit::from_json(diagram).unwrap();
    let registry = registry();
    Board::new(FlatBoard::build(&circuit, &registry).unwrap()).unwrap()
}

pub fn shared_board(diagram: &str) -> Rc<RefCell<Board>> {
    Rc::new(RefCell::new(board(diagram)))
}

pub fn chip_on(diagram: &str) -> Stc89c52 {
    Stc89c52::new().with_board(board(diagram))
}

pub fn chip_on_shared(diagram: &str) -> (Stc89c52, Rc<RefCell<Board>>) {
    let board = shared_board(diagram);
    let chip = Stc89c52::new().with_shared_board(Rc::clone(&board), 0);
    (chip, board)
}

/// A diagram with the board instance `mcu` and any extra parts.
pub fn diagram(connections: &[&str], parts: &[&str]) -> String {
    let mut text = String::from(
        r#"{"version":1,"parts":[{"id":"mcu","type":"board-stc89c52-test","attrs":{}}"#,
    );
    if !parts.is_empty() {
        text.push(',');
        text.push_str(&parts.join(","));
    }
    text.push_str(r#"],"connections":["#);
    text.push_str(&connections.join(","));
    text.push_str("]}");
    text
}

// -- loading helpers -------------------------------------------------------

pub fn boot(chip: &mut Stc89c52, image: &[u8]) {
    chip.load_image(ImageKind::Firmware, 0, image).unwrap();
    chip.boot().unwrap();
}

pub fn boot_at(chip: &mut Stc89c52, image: &[u8], offset: u64) {
    chip.load_image(ImageKind::Firmware, offset, image).unwrap();
    chip.boot().unwrap();
}

/// Build an Intel HEX text for `bytes` at `base`.
pub fn intel_hex(bytes: &[u8], base: u16) -> String {
    let mut text = String::new();
    for (chunk_index, chunk) in bytes.chunks(16).enumerate() {
        let address = usize::from(base) + chunk_index * 16;
        let mut record = vec![chunk.len() as u8, (address >> 8) as u8, address as u8, 0x00];
        record.extend_from_slice(chunk);
        let sum = record.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte));
        record.push(0u8.wrapping_sub(sum));
        text.push(':');
        for byte in record {
            text.push_str(&format!("{byte:02X}"));
        }
        text.push('\n');
    }
    text.push_str(":00000001FF\n");
    text
}
