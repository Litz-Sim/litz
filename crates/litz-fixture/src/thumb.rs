//! A Thumb-1 assembler, enough to hand-build RP2040 firmware.
//!
//! ARMv6-M is a small instruction set: sixteen registers, two-address data
//! processing, no `movw`/`movt`. That last point decides the shape of this
//! module — every 32-bit constant (`0x4003_4000`, a pin mask, a delay count)
//! has to come out of a literal pool, so the assembler keeps a pool and patches
//! the `ldr rd, [pc, #imm8]` instructions that read it once the base address is
//! known.
//!
//! Labels are word indices into the instruction stream, and branches are
//! resolved in [`Thumb::link`] for the same reason: the fixture is assembled
//! once, at the address it will run from.

/// Condition codes for [`Thumb::bcond`].
pub mod cond {
    pub const EQ: u8 = 0;
    pub const NE: u8 = 1;
    pub const CS: u8 = 2;
    pub const CC: u8 = 3;
    pub const MI: u8 = 4;
    pub const PL: u8 = 5;
    pub const VS: u8 = 6;
    pub const VC: u8 = 7;
    pub const HI: u8 = 8;
    pub const LS: u8 = 9;
    pub const GE: u8 = 10;
    pub const LT: u8 = 11;
    pub const GT: u8 = 12;
    pub const LE: u8 = 13;
}

/// Link register number, for [`Thumb::bx`].
pub const LR: u8 = 14;

enum Target {
    Address(usize),
    /// A label that is defined later; filled in by [`Thumb::bind`].
    Pending,
}

struct Branch {
    at: usize,
    target: Target,
    cond: Option<u8>,
}

enum Literal {
    Constant(u32),
    /// An offset into the data blob returned by [`Thumb::data`].
    Data(usize),
}

/// A two-pass Thumb-1 assembler.
///
/// ```text
/// let mut t = Thumb::new();
/// t.movs(0, 1);                  // movs r0, #1
/// let stop = t.here();
/// t.subs(0, 1);                  // subs r0, #1
/// t.bcond(cond::NE, stop);       // bne stop
/// let bytes = t.link(0x1000_0100);
/// ```
#[derive(Default)]
pub struct Thumb {
    words: Vec<u16>,
    branches: Vec<Branch>,
    literals: Vec<(usize, Literal)>,
    data: Vec<u8>,
}

impl Thumb {
    pub fn new() -> Self {
        Self::default()
    }

    /// The label of the instruction the next encoder call will emit.
    pub fn here(&self) -> usize {
        self.words.len()
    }

    /// How many halfwords have been emitted so far.
    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    fn emit(&mut self, word: u16) -> usize {
        self.words.push(word);
        self.words.len() - 1
    }

    /// `movs rd, #imm8`.
    pub fn movs(&mut self, rd: u8, imm: u8) {
        self.emit(0x2000 | u16::from(rd) << 8 | u16::from(imm));
    }

    /// `adds rd, #imm8`.
    pub fn adds(&mut self, rd: u8, imm: u8) {
        self.emit(0x3000 | u16::from(rd) << 8 | u16::from(imm));
    }

    /// `subs rd, #imm8`.
    pub fn subs(&mut self, rd: u8, imm: u8) {
        self.emit(0x3800 | u16::from(rd) << 8 | u16::from(imm));
    }

    /// `cmp rn, #imm8`.
    pub fn cmp(&mut self, rn: u8, imm: u8) {
        self.emit(0x2800 | u16::from(rn) << 8 | u16::from(imm));
    }

    /// `tst rn, rm`, which sets Z when every bit of `rm` is clear in `rn`.
    pub fn tst(&mut self, rn: u8, rm: u8) {
        self.emit(0x4200 | u16::from(rm) << 3 | u16::from(rn));
    }

    /// `ldr rd, [pc, #imm]` reading a value from the pool.
    pub fn ldr_const(&mut self, rd: u8, value: u32) {
        let at = self.emit(0x4800 | u16::from(rd) << 8);
        self.literals.push((at, Literal::Constant(value)));
    }

    /// `ldr rd, =` the address of a blob registered by [`Thumb::data`].
    pub fn ldr_data(&mut self, rd: u8, data: usize) {
        let at = self.emit(0x4800 | u16::from(rd) << 8);
        self.literals.push((at, Literal::Data(data)));
    }

    /// `str rt, [rn, #offset]`; the offset is in bytes and a multiple of four.
    pub fn str(&mut self, rt: u8, rn: u8, offset: u16) {
        self.emit(0x6000 | (offset / 4) << 6 | u16::from(rn) << 3 | u16::from(rt));
    }

    /// `ldr rt, [rn, #offset]`; the offset is in bytes and a multiple of four.
    pub fn ldr(&mut self, rt: u8, rn: u8, offset: u16) {
        self.emit(0x6800 | (offset / 4) << 6 | u16::from(rn) << 3 | u16::from(rt));
    }

    /// `strb rt, [rn, #offset]`.
    pub fn strb(&mut self, rt: u8, rn: u8, offset: u16) {
        self.emit(0x7000 | offset << 6 | u16::from(rn) << 3 | u16::from(rt));
    }

    /// `ldrb rt, [rn, #offset]`.
    pub fn ldrb(&mut self, rt: u8, rn: u8, offset: u16) {
        self.emit(0x7800 | offset << 6 | u16::from(rn) << 3 | u16::from(rt));
    }

    /// `push {reglist, lr}`; `list` names r0-r7 only, bit 8 is always LR.
    pub fn push(&mut self, list: u16) {
        self.emit(0xb400 | 0x0100 | list & 0x00ff);
    }

    /// `pop {reglist, pc}`; `list` names r0-r7 only, bit 8 is always PC.
    pub fn pop(&mut self, list: u16) {
        self.emit(0xbc00 | 0x0100 | list & 0x00ff);
    }

    /// `bx rm`.
    pub fn bx(&mut self, rm: u8) {
        self.emit(0x4700 | u16::from(rm) << 3);
    }

    /// A `b` to an already-known label.
    pub fn b(&mut self, target: usize) {
        let at = self.emit(0xe000);
        self.branches.push(Branch {
            at,
            target: Target::Address(target),
            cond: None,
        });
    }

    /// A `b` whose target is defined later; returns the id for [`Thumb::bind`].
    pub fn b_forward(&mut self) -> usize {
        let at = self.emit(0xe000);
        self.branches.push(Branch {
            at,
            target: Target::Pending,
            cond: None,
        });
        self.branches.len() - 1
    }

    /// A conditional branch to an already-known label.
    pub fn bcond(&mut self, cond: u8, target: usize) {
        let at = self.emit(0xd000 | u16::from(cond) << 8);
        self.branches.push(Branch {
            at,
            target: Target::Address(target),
            cond: Some(cond),
        });
    }

    /// A conditional branch whose target is defined later.
    pub fn bcond_forward(&mut self, cond: u8) -> usize {
        let at = self.emit(0xd000 | u16::from(cond) << 8);
        self.branches.push(Branch {
            at,
            target: Target::Pending,
            cond: Some(cond),
        });
        self.branches.len() - 1
    }

    /// A `bl` to an already-known label.
    pub fn bl(&mut self, target: usize) {
        let at = self.emit(0xf000);
        self.emit(0xf800);
        self.branches.push(Branch {
            at,
            target: Target::Address(target),
            cond: None,
        });
        // `bl` occupies two halfwords; `link` tells it from `b` by the opcode.
        debug_assert_eq!(self.words[at], 0xf000);
    }

    /// A `bl` whose target is defined later.
    pub fn bl_forward(&mut self) -> usize {
        let at = self.emit(0xf000);
        self.emit(0xf800);
        self.branches.push(Branch {
            at,
            target: Target::Pending,
            cond: None,
        });
        debug_assert_eq!(self.words[at], 0xf000);
        self.branches.len() - 1
    }

    /// Give a pending branch its target.
    pub fn bind(&mut self, branch: usize, target: usize) {
        self.branches[branch].target = Target::Address(target);
    }

    /// Register a blob of bytes (a string, a table) and get its offset, for
    /// [`Thumb::ldr_data`]. Blobs are laid out after the literal pool.
    pub fn data(&mut self, bytes: &[u8]) -> usize {
        let at = self.data.len();
        self.data.extend_from_slice(bytes);
        at
    }

    /// Resolve every branch and literal, then return the code, pool and data as
    /// the bytes that run from `base`.
    ///
    /// # Panics
    ///
    /// Panics on an unbound label, on a branch out of range, or on a literal
    /// more than 1020 bytes past its reader. A fixture that trips this is a bug
    /// in the fixture, not in the simulator, so failing loudly is right.
    pub fn link(&mut self, base: u32) -> Vec<u8> {
        for branch in &self.branches {
            let Target::Address(target) = branch.target else {
                panic!("branch at halfword {} has no label", branch.at);
            };
            // PC is the address of the branch plus four.
            let offset = (target as i64 - branch.at as i64 - 2) * 2;
            let at = branch.at;
            match branch.cond {
                None if self.words[at] == 0xf000 => {
                    let (first, second) = bl_halves(offset);
                    self.words[at] = first;
                    self.words[at + 1] = second;
                }
                None => {
                    assert!(
                        (-2048..=2046).contains(&offset) && offset % 2 == 0,
                        "b at halfword {at} reaches {offset} bytes, past the 11-bit range"
                    );
                    self.words[at] = 0xe000 | ((offset as u16 >> 1) & 0x07ff);
                }
                Some(cond) => {
                    assert!(
                        (-256..=254).contains(&offset) && offset % 2 == 0,
                        "bcc at halfword {at} reaches {offset} bytes, past the 8-bit range"
                    );
                    self.words[at] =
                        0xd000 | u16::from(cond) << 8 | ((offset as u16 >> 1) & 0x00ff);
                }
            }
        }

        // The pool goes after the code, on a word boundary, and at least one
        // halfword clear of it: `ldr` reads from the instruction address plus
        // four, so a pool that touched the last instruction would be behind
        // the reader's PC.
        // A reader at halfword `r` sees PC = base + 2r + 4 rounded down to a
        // word, so the pool has to start no earlier than halfword `r + 2` and
        // on a word boundary.
        let last_reader = self.literals.iter().map(|(at, _)| *at).max();
        let mut pool_at = self.words.len().max(last_reader.map_or(0, |r| r + 2));
        if pool_at % 2 == 1 {
            pool_at += 1;
        }
        while self.words.len() < pool_at {
            self.emit(0xbf00); // nop
        }
        let pool_base = base as i64 + (pool_at as i64) * 2;
        let data_base = pool_base + (self.literals.len() as i64) * 4;
        for (index, (reader, literal)) in self.literals.iter().enumerate() {
            let value = match literal {
                Literal::Constant(value) => *value,
                Literal::Data(offset) => (data_base + *offset as i64) as u32,
            };
            let reader = *reader;
            // `ldr` reads from (instruction address + 4) with bits 1:0 clear.
            let pc = (base as i64 + reader as i64 * 2 + 4) & !3;
            let delta = pool_base + index as i64 * 4 - pc;
            assert_eq!(delta % 4, 0, "a literal pool word is not word-aligned");
            let imm = delta / 4;
            assert!(
                (0..=255).contains(&imm),
                "literal at halfword {reader} is {imm} words away, past ldr's 8-bit reach"
            );
            self.words[reader] |= imm as u16;
            self.words.push(value as u16);
            self.words.push((value >> 16) as u16);
        }

        let mut out = Vec::with_capacity(self.words.len() * 2 + self.data.len());
        for word in &self.words {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(&self.data);
        out
    }
}

/// Encode the two halfwords of a `bl` at a byte offset, per the ARMv6-M manual:
/// `S:I1:I2:imm10:imm11:0`, with `J1 = !(I1 ^ S)` and `J2 = !(I2 ^ S)`.
fn bl_halves(offset: i64) -> (u16, u16) {
    assert!(
        (-(1 << 24)..(1 << 24)).contains(&offset) && offset % 2 == 0,
        "bl reaches {offset} bytes, past the 25-bit range"
    );
    let value = offset as u32;
    let s = (value >> 24) & 1;
    let i1 = (value >> 23) & 1;
    let i2 = (value >> 22) & 1;
    let imm10 = (value >> 12) & 0x03ff;
    let imm11 = (value >> 1) & 0x07ff;
    let j1 = !(i1 ^ s) & 1;
    let j2 = !(i2 ^ s) & 1;
    (
        0xf000 | (s as u16) << 10 | imm10 as u16,
        0xf800 | (j1 as u16) << 13 | (j2 as u16) << 11 | imm11 as u16,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_simple_encodings_match_the_manual() {
        let mut t = Thumb::new();
        t.movs(0, 1);
        t.subs(7, 1);
        t.cmp(2, 0);
        t.tst(0, 3);
        t.str(7, 5, 0x10);
        t.ldr(0, 4, 0x18);
        t.strb(2, 4, 0);
        t.ldrb(2, 1, 3);
        t.bx(LR);
        assert_eq!(
            t.words,
            vec![
                0x2001, // movs r0, #1
                0x3f01, // subs r7, #1
                0x2a00, // cmp r2, #0
                0x4218, // tst r0, r3
                0x612f, // str r7, [r5, #0x10]
                0x69a0, // ldr r0, [r4, #0x18]
                0x7022, // strb r2, [r4]
                0x78ca, // ldrb r2, [r1, #3]
                0x4770, // bx lr
            ]
        );
    }

    #[test]
    fn a_literal_lands_in_the_pool_and_the_reader_points_at_it() {
        let mut t = Thumb::new();
        t.ldr_const(4, 0x4003_4000);
        t.link(0x1000_0100);
        // The reader's PC is 0x104 and the pool starts there, so imm8 is zero.
        assert_eq!(t.words[0], 0x4c00);
        assert_eq!(t.words[2], 0x4000);
        assert_eq!(t.words[3], 0x4003);
    }

    #[test]
    fn a_literal_skips_the_padding_back_to_the_pool() {
        let mut t = Thumb::new();
        t.movs(0, 0);
        t.movs(0, 0);
        t.movs(0, 0);
        t.ldr_const(0, 7);
        t.movs(0, 0);
        t.link(0x1000_0100);
        // Five halfwords of code put the pool at 0x10c; the reader at 0x106
        // has PC 0x108, one word short of it.
        assert_eq!(t.words[3], 0x4801);
        assert_eq!(&t.words[6..8], &[7, 0]);
    }

    #[test]
    fn back_and_forward_branches_resolve_in_halfwords() {
        let mut t = Thumb::new();
        let loop_head = t.here();
        t.subs(0, 1);
        t.bcond(cond::NE, loop_head);
        let skip = t.bcond_forward(cond::EQ);
        t.movs(0, 1);
        let after = t.here();
        t.bind(skip, after);
        t.link(0x1000_0100);
        // bne back to the first instruction: PC 6, target 0, -3 halfwords.
        assert_eq!(t.words[1], 0xd1fd);
        // beq to the instruction after `movs`: PC already points there.
        assert_eq!(t.words[2], 0xd000);
    }

    #[test]
    fn a_branch_link_uses_the_split_immediate() {
        let mut t = Thumb::new();
        let call = t.bl_forward();
        t.movs(0, 0);
        let callee = t.here();
        t.bind(call, callee);
        t.link(0x1000_0100);
        assert_eq!(t.words[0], 0xf000);
        // Two bytes of reach: S=0, J1=J2=1, imm11=1.
        assert_eq!(t.words[1], 0xf801);
    }

    #[test]
    fn data_sits_after_the_pool_and_its_address_is_loaded() {
        let mut t = Thumb::new();
        let blob = t.data(b"hi\0");
        t.ldr_data(0, blob);
        let bytes = t.link(0x1000_0100);
        // Reader at 0x100 with PC 0x104; the pool is at 0x104 and the data at
        // 0x108, one word past it.
        assert_eq!(t.words[0], 0x4800);
        assert_eq!(&t.words[2..4], &[0x0108, 0x1000]);
        assert_eq!(&bytes[8..11], b"hi\0");
    }
}
