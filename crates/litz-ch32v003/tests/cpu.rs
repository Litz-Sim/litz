//! RV32EC instruction semantics, proved with hand-assembled words.
//!
//! Every program here is laid out the way the part boots: word 0 of flash is
//! the reset vector, and the code itself starts at the address it names.

mod common;

use common::*;
use litz_ch32v003::{Machine, Stop};

/// Load `program` at `0x10` behind a reset vector, boot, and return the
/// machine ready to run.
fn boot(program: &[u32]) -> Machine {
    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(0x10, program);
    let mut machine = Machine::new();
    machine.load_flash(0, &image.bytes()).unwrap();
    machine.boot();
    machine
}

fn run(program: &[u32], budget: u64) -> Machine {
    let mut machine = boot(program);
    let _ = machine.run(budget);
    machine
}

#[test]
fn the_alu_computes_rv32e_integer_operations() {
    let machine = run(
        &[
            addi(1, 0, 5),
            addi(2, 0, -3),
            add(3, 1, 2),
            sub(4, 1, 2),
            and(5, 1, 2),
            or(6, 1, 2),
            xor(7, 1, 2),
            addi(9, 0, 2),
            sll(8, 1, 9),
            srl(10, 2, 9),
            sra(11, 2, 9),
            slt(12, 2, 1),
            sltu(13, 2, 1),
            xori(14, 1, 0xf),
            slli(15, 1, 3),
        ],
        40,
    );
    assert_eq!(machine.cpu.reg(1), 5);
    assert_eq!(machine.cpu.reg(2), (-3i32) as u32);
    assert_eq!(machine.cpu.reg(3), 2);
    assert_eq!(machine.cpu.reg(4), 8);
    assert_eq!(machine.cpu.reg(5), 5);
    assert_eq!(machine.cpu.reg(6), (-3i32) as u32);
    assert_eq!(machine.cpu.reg(7), (-8i32) as u32);
    assert_eq!(machine.cpu.reg(8), 20);
    assert_eq!(machine.cpu.reg(10), 0x3fff_ffff);
    assert_eq!(machine.cpu.reg(11), u32::MAX);
    assert_eq!(machine.cpu.reg(12), 1, "signed -3 < 5");
    assert_eq!(machine.cpu.reg(13), 0, "unsigned 0xfffffffd > 5");
    assert_eq!(machine.cpu.reg(14), 0xa);
    assert_eq!(machine.cpu.reg(15), 40);
    assert_eq!(machine.cpu.reg(0), 0);
}

#[test]
fn the_shift_immediates_and_upper_immediates_work() {
    let machine = run(
        &[
            lui(1, 0x12345),
            auipc(2, 0),
            srai(3, 1, 4),
            srli(4, 1, 4),
            slti(5, 1, 0),
            sltiu(6, 1, 1),
            ori(7, 0, 0x123),
            andi(8, 7, 0x0f0),
        ],
        20,
    );
    assert_eq!(machine.cpu.reg(1), 0x1234_5000);
    assert_eq!(machine.cpu.reg(2), 0x14, "auipc names its own address");
    assert_eq!(machine.cpu.reg(3), 0x0123_4500, "srai of a positive value");
    assert_eq!(machine.cpu.reg(4), 0x0123_4500);
    assert_eq!(machine.cpu.reg(5), 0);
    assert_eq!(machine.cpu.reg(6), 0);
    assert_eq!(machine.cpu.reg(7), 0x123);
    assert_eq!(machine.cpu.reg(8), 0x020);
}

#[test]
fn a_branch_is_taken_only_when_its_comparison_holds() {
    let cases: [(u32, i32, i32, bool); 8] = [
        (0, 1, 1, true),   // beq
        (1, 1, 2, true),   // bne
        (4, -1, 1, true),  // blt, signed
        (5, 1, 1, true),   // bge
        (6, -1, 1, false), // bltu, 0xffffffff is the larger
        (7, -1, 1, true),  // bgeu: 0xffffffff >= 1
        (4, 1, -1, false), // blt, signed, not taken
        (6, 1, -1, true),  // bltu, 1 < 0xffffffff
    ];
    for (funct3, left, right, taken) in cases {
        let branch = match funct3 {
            0 => beq(1, 2, 8),
            1 => bne(1, 2, 8),
            4 => blt(1, 2, 8),
            5 => bge(1, 2, 8),
            6 => bltu(1, 2, 8),
            7 => bgeu(1, 2, 8),
            _ => unreachable!(),
        };
        let machine = run(
            &[
                addi(1, 0, left),
                addi(2, 0, right),
                branch,
                addi(3, 0, 0x11),
                addi(4, 0, 0x55),
            ],
            10,
        );
        if taken {
            assert_eq!(machine.cpu.reg(3), 0, "funct3 {funct3} taken");
        } else {
            assert_eq!(machine.cpu.reg(3), 0x11, "funct3 {funct3} not taken");
        }
        assert_eq!(machine.cpu.reg(4), 0x55, "both paths reach the end");
    }
}

#[test]
fn jal_and_jalr_link_and_jump() {
    let machine = run(&[jal(1, 8), addi(2, 0, 1), addi(3, 0, 2), SELF], 10);
    assert_eq!(machine.cpu.reg(1), 0x14, "the link is the next word");
    assert_eq!(machine.cpu.reg(2), 0, "the delay slot is skipped");
    assert_eq!(machine.cpu.reg(3), 2);
    assert_eq!(machine.cpu.pc(), 0x1c);

    let machine = run(
        &[
            auipc(1, 0),
            addi(1, 1, 0x10),
            jalr(2, 1, 0),
            addi(3, 0, 0xaa),
            addi(4, 0, 0x55),
            SELF,
        ],
        10,
    );
    assert_eq!(machine.cpu.reg(2), 0x1c, "jalr links past itself");
    assert_eq!(machine.cpu.reg(3), 0, "the skipped word did not run");
    assert_eq!(machine.cpu.reg(4), 0x55);
}

#[test]
fn loads_and_stores_move_bytes_halfwords_and_words() {
    // li x1, 0x20000000 ; li x2, 0x89abcdef ; sw ; lw ; sb/lb/lbu ; sh/lh/lhu
    let [hi1, lo1] = li32(1, 0x2000_0000);
    let [hi2, lo2] = li32(2, 0x89ab_cdef);
    let machine = run(
        &[
            hi1,
            lo1,
            hi2,
            lo2,
            sw(2, 1, 0),
            lw(3, 1, 0),
            addi(4, 0, 0x80u32 as i32),
            sb(4, 1, 4),
            lb(5, 1, 4),
            lbu(6, 1, 4),
            li32(7, 0x8001)[0],
            li32(7, 0x8001)[1],
            sh(7, 1, 6),
            lh(8, 1, 6),
            lhu(9, 1, 6),
        ],
        40,
    );
    assert_eq!(machine.cpu.reg(3), 0x89ab_cdef);
    assert_eq!(machine.cpu.reg(5), 0xffff_ff80, "lb sign extends");
    assert_eq!(machine.cpu.reg(6), 0x80, "lbu zero extends");
    assert_eq!(machine.cpu.reg(8), 0xffff_8001, "lh sign extends");
    assert_eq!(machine.cpu.reg(9), 0x8001, "lhu zero extends");
    let sram = machine.sram();
    assert_eq!(&sram[0..4], &0x89ab_cdefu32.to_le_bytes());
    assert_eq!(sram[4], 0x80);
    assert_eq!(&sram[6..8], &0x8001u16.to_le_bytes());
}

#[test]
fn the_compressed_encodings_execute() {
    let mut image = Image::new();
    image.word(0, 0x10);
    image.half(0x10, c_li(8, 5));
    image.half(0x12, c_addi(8, 3));
    image.half(0x14, c_slli(8, 2));
    image.half(0x16, c_mv(9, 8));
    image.half(0x18, c_add(9, 8));
    image.word(0x1a, lui(8, 0x20000));
    image.half(0x1e, c_li(9, 7));
    image.half(0x20, c_sw(9, 8, 0));
    image.half(0x22, c_lw(10, 8, 0));
    image.half(0x24, c_addi(10, 1));
    image.half(0x26, c_li(11, 0));
    image.half(0x28, c_beqz(11, 4));
    image.half(0x2a, c_li(12, 1));
    image.half(0x2c, c_li(12, 2));
    image.half(0x2e, c_bnez(12, 4));
    image.half(0x30, c_li(13, 1));
    image.half(0x32, c_li(13, 3));
    image.half(0x34, 0x6001 | ((14u16) << 7) | (0x1f << 2)); // c.lui x14, 0x1f000
    image.half(0x36, c_j(0));

    let mut machine = Machine::new();
    machine.load_flash(0, &image.bytes()).unwrap();
    machine.boot();
    let stop = machine.run(60);
    assert_eq!(stop, Stop::Budget);
    assert_eq!(machine.cpu.reg(8), 0x2000_0000);
    assert_eq!(machine.cpu.reg(9), 7);
    assert_eq!(machine.cpu.reg(10), 8, "loaded 7 and added one");
    assert_eq!(machine.cpu.reg(12), 2, "c.beqz skipped the 1");
    assert_eq!(machine.cpu.reg(13), 3, "c.bnez skipped the 1");
    assert_eq!(machine.cpu.reg(14), 0x1f_000, "c.lui shifts by 12");
    assert_eq!(&machine.sram()[0..4], &7u32.to_le_bytes());
}

#[test]
fn an_illegal_register_field_traps() {
    let mut machine = boot(&[addi(16, 0, 1)]);
    assert_eq!(machine.run(10), Stop::Trap);
    let fault = machine.fault().unwrap().to_string();
    assert!(fault.contains("x16"), "{fault}");
    assert!(fault.contains("RV32E"), "{fault}");
}

#[test]
fn an_unknown_encoding_traps_and_names_the_word() {
    let mut machine = boot(&[mul(1, 2, 3)]);
    assert_eq!(machine.run(10), Stop::Trap);
    let fault = machine.fault().unwrap().to_string();
    assert!(fault.contains("illegal instruction"), "{fault}");
    assert!(fault.contains("0x023100b3"), "{fault}");
    assert!(fault.contains("0x00000010"), "{fault}");
}

#[test]
fn ecall_and_ebreak_halt_the_core() {
    let mut machine = boot(&[ECALL]);
    assert_eq!(machine.run(10), Stop::Halted);
    let mut machine = boot(&[EBREAK]);
    assert_eq!(machine.run(10), Stop::Halted);
}

#[test]
fn an_unaligned_load_traps() {
    let [hi, lo] = li32(1, 0x2000_0000);
    let mut machine = boot(&[hi, lo, lw(2, 1, 1)]);
    assert_eq!(machine.run(10), Stop::Trap);
    let fault = machine.fault().unwrap().to_string();
    assert!(fault.contains("unaligned"), "{fault}");
}

#[test]
fn a_bus_access_outside_every_block_traps() {
    let [hi, lo] = li32(1, 0x9000_0000);
    let mut machine = boot(&[hi, lo, lw(2, 1, 0)]);
    assert_eq!(machine.run(10), Stop::Trap);
    let fault = machine.fault().unwrap().to_string();
    assert!(fault.contains("no target"), "{fault}");
}

#[test]
fn the_csr_file_round_trips_and_misa_reports_rv32ec() {
    let machine = run(
        &[
            addi(2, 0, 0x5a),
            csrrw(1, 0x340, 2),
            csrrs(3, 0x340, 0),
            csrrs(4, 0x301, 0),
            csrrs(5, 0x305, 0),
        ],
        20,
    );
    assert_eq!(machine.cpu.reg(1), 0, "mscratch started clear");
    assert_eq!(machine.cpu.reg(3), 0x5a, "csrrs read it back");
    assert_eq!(machine.cpu.reg(4), 0x4000_0114, "misa says RV32EC");
    assert_eq!(machine.cpu.reg(5), 0, "mtvec resets to the table at zero");
}

#[test]
fn instruction_costs_are_visible_in_cycles() {
    let mut machine = boot(&[addi(1, 0, 1)]);
    assert_eq!(machine.run(1), Stop::Budget);
    assert_eq!(machine.cycles(), 1);

    let [hi, lo] = li32(1, 0x2000_0000);
    let mut machine = boot(&[hi, lo, lw(2, 1, 0)]);
    machine.run(4); // lui + addi are one cycle each; the load is two
    assert_eq!(machine.cycles(), 4);

    let mut machine = boot(&[beq(0, 0, 8), NOP]);
    machine.run(1);
    assert_eq!(machine.cycles(), 2, "a taken branch costs two cycles");
}
