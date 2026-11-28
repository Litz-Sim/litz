//! Instruction semantics: flags, decimal adjust, multiply/divide, bit
//! operations, addressing modes, jumps and the stack.

mod common;

use common::Asm;
use litz_8051::{Fault, Machine, Stop, sfr};

fn boot(program: &[u8]) -> Machine {
    let mut machine = Machine::new();
    machine.load_bin(program, 0).unwrap();
    machine.boot();
    machine
}

fn step(machine: &mut Machine, count: usize) {
    for _ in 0..count {
        machine.step().unwrap();
    }
}

#[test]
fn add_sets_carry_auxiliary_and_overflow() {
    let mut asm = Asm::new();
    asm.mov_a(0x7F);
    asm.add_a(0x01);
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 3);

    assert_eq!(machine.acc(), 0x80);
    assert!(!machine.carry(), "0x7f + 1 does not carry out of bit 7");
    assert!(
        machine.auxiliary_carry(),
        "the low nibble carried into bit 4"
    );
    assert!(machine.overflow(), "127 + 1 overflows signed 8-bit");

    // 0xff + 1 carries, zeroes and sets AC but is not signed overflow.
    let mut asm = Asm::new();
    asm.mov_a(0xFF);
    asm.add_a(0x01);
    asm.label("here");
    asm.sjmp("here");
    let mut second = boot(&asm.finish());
    step(&mut second, 2);

    assert_eq!(second.acc(), 0x00);
    assert!(second.carry());
    assert!(second.auxiliary_carry());
    assert!(!second.overflow());
}

#[test]
fn addc_takes_the_carry_in() {
    let mut asm = Asm::new();
    asm.setb_c();
    asm.mov_a(0x01);
    asm.addc_a(0x02);
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 3);

    assert_eq!(machine.acc(), 0x04);
    assert!(!machine.carry());
}

#[test]
fn subb_borrows() {
    let mut asm = Asm::new();
    asm.clr_c();
    asm.mov_a(0x00);
    asm.subb_a(0x01);
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 3);

    assert_eq!(machine.acc(), 0xFF);
    assert!(machine.carry(), "the borrow out of bit 7 is the carry");
    assert!(machine.auxiliary_carry());
    assert!(!machine.overflow(), "0 - 1 is not signed overflow");
}

#[test]
fn da_a_makes_bcd_addition_carry() {
    let mut asm = Asm::new();
    asm.mov_a(0x99);
    asm.add_a(0x01);
    asm.da_a();
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 3);

    assert_eq!(machine.acc(), 0x00);
    assert!(machine.carry(), "99 + 1 in BCD rolls over");
}

#[test]
fn mul_and_div_use_a_and_b() {
    let mut asm = Asm::new();
    asm.mov_a(12);
    asm.mov_b(10);
    asm.mul_ab();
    asm.mov_direct(0x30, 0x00); // keep a known instruction after MUL
    asm.mov_a(15);
    asm.mov_b(4);
    asm.div_ab();
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 3);

    assert_eq!(machine.acc(), 120);
    assert_eq!(machine.b(), 0);
    assert!(!machine.overflow());

    step(&mut machine, 4);
    assert_eq!(machine.acc(), 3);
    assert_eq!(machine.b(), 3);
    assert!(!machine.overflow());

    // Divide by zero sets OV and clears CY.
    let mut asm = Asm::new();
    asm.mov_a(0x55);
    asm.mov_b(0);
    asm.div_ab();
    asm.label("here");
    asm.sjmp("here");
    let mut second = boot(&asm.finish());
    step(&mut second, 3);
    assert!(second.overflow());
    assert!(!second.carry());
}

#[test]
fn inc_and_dec_leave_the_flags_alone() {
    let mut asm = Asm::new();
    asm.clr_c();
    asm.mov_a(0xFF);
    asm.add_a(0x00); // 0xff + 0: no carry
    asm.op(0x04); // INC A -> 0x00
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 4);

    assert_eq!(machine.acc(), 0x00);
    assert!(!machine.carry(), "INC does not touch the carry flag");
    assert!(!machine.overflow());
}

#[test]
fn bit_operations_hit_ram_and_sfrs() {
    let mut asm = Asm::new();
    asm.setb(0x07); // 0x20.7
    asm.clr_bit(0x00); // 0x20.0
    asm.setb(0x01); // 0x20.1
    asm.op1(0xA2, 0x01); // MOV C, 0x20.1
    asm.setb(0x90); // P1.0
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 6);

    assert_eq!(machine.read_iram(0x20), 0x82);
    assert!(machine.carry());
    assert_eq!(machine.read_sfr(sfr::P1) & 0x01, 0x01);
}

#[test]
fn register_banks_are_selected_by_psw() {
    let mut asm = Asm::new();
    asm.mov_direct(sfr::PSW, 0x08); // bank 1
    asm.mov_r(0, 0x11);
    asm.mov_direct(sfr::PSW, 0x00); // bank 0
    asm.mov_r(0, 0x22);
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 4);

    assert_eq!(machine.read_iram(0x00), 0x22);
    assert_eq!(machine.read_iram(0x08), 0x11);
    assert_eq!(machine.bank(), 0);
}

#[test]
fn the_upper_128_bytes_are_indirect_only() {
    let mut asm = Asm::new();
    asm.mov_r(0, 0x80);
    asm.mov_indirect(0, 0x42);
    asm.mov_a_indirect(0);
    asm.mov_direct_a(0x30);
    asm.mov_a_direct(sfr::P0); // reads the pin latch, not RAM at 0x80
    asm.mov_direct_a(0x31);
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 6);

    assert_eq!(machine.read_iram(0x80), 0x42, "indirect @R0 reaches 0x80");
    assert_eq!(machine.read_iram(0x30), 0x42);
    assert_eq!(
        machine.read_iram(0x31),
        0xFF,
        "direct 0x80 is P0, whose reset latch is 0xff"
    );
}

#[test]
fn a_call_pushes_a_return_address_and_ret_pops_it() {
    let mut asm = Asm::new();
    asm.lcall("sub");
    asm.mov_a(0x55);
    asm.label("done");
    asm.sjmp("done");
    asm.label("sub");
    asm.mov_a(0xAA);
    asm.emit(&[0xC0, 0xE0]); // PUSH ACC
    asm.emit(&[0xD0, 0xF0]); // POP B
    asm.ret();

    let mut machine = boot(&asm.finish());
    step(&mut machine, 8);

    assert_eq!(machine.acc(), 0x55, "the return landed after the call");
    assert_eq!(machine.b(), 0xAA, "PUSH ACC / POP B moved the value");
    assert_eq!(machine.sp(), 0x07, "the stack is balanced");
}

#[test]
fn djnz_loops_the_expected_number_of_times() {
    let mut asm = Asm::new();
    asm.mov_r(2, 3);
    asm.label("loop");
    asm.op(0x04); // INC A
    asm.djnz_r(2, "loop");
    asm.label("done");
    asm.sjmp("done");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 20);

    assert_eq!(machine.acc(), 3);
    assert_eq!(machine.register(2), 0);
}

#[test]
fn cjne_sets_carry_for_an_unsigned_less_than() {
    let mut asm = Asm::new();
    asm.clr_c();
    asm.mov_a(5);
    asm.emit(&[0xB4, 0x07, 0x00]); // CJNE A, #7, $+3
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 3);

    assert!(machine.carry(), "5 < 7 sets CY");
}

#[test]
fn jbc_clears_the_bit_and_jumps() {
    let mut asm = Asm::new();
    asm.setb(0x00);
    asm.jbc(0x00, "skip");
    asm.mov_a(0xEE);
    asm.label("skip");
    asm.sjmp("skip");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 4);

    assert_eq!(machine.read_iram(0x20) & 0x01, 0);
    assert_ne!(machine.acc(), 0xEE, "the JBC target skipped the MOV");
}

#[test]
fn movx_reaches_xdata_and_movc_reads_code() {
    let mut asm = Asm::new();
    asm.mov_dptr(0x1234);
    asm.mov_a(0x5A);
    asm.op(0xF0); // MOVX @DPTR, A
    asm.clr_a();
    asm.op(0xE0); // MOVX A, @DPTR
    asm.mov_direct_a(0x30);
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 6);

    assert_eq!(machine.read_xdata(0x1234), 0x5A);
    assert_eq!(machine.read_iram(0x30), 0x5A);

    // MOVC A, @A+DPTR reads a byte of the code array.
    let mut asm = Asm::new();
    asm.mov_dptr(0x20);
    asm.clr_a();
    asm.op(0x93); // MOVC A, @A+DPTR
    asm.mov_direct_a(0x31);
    asm.label("here");
    asm.sjmp("here");
    let mut image = asm.finish();
    image.resize(0x21, 0xFF);
    image[0x20] = 0x7E;
    let mut second = boot(&image);
    step(&mut second, 4);
    assert_eq!(second.read_iram(0x31), 0x7E);
}

#[test]
fn the_dual_data_pointer_selects_its_own_registers() {
    let mut asm = Asm::new();
    asm.mov_direct(sfr::AUXR1, 1);
    asm.mov_dptr(0x1111);
    asm.mov_direct(sfr::AUXR1, 0);
    asm.mov_dptr(0x2222);
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 5);

    assert_eq!(machine.read_sfr(sfr::DPH), 0x22);
    assert_eq!(machine.read_sfr(sfr::DPL), 0x22);
    assert_eq!(machine.read_sfr(sfr::DPH1), 0x11);
    assert_eq!(machine.read_sfr(sfr::DPL1), 0x11);
    assert_eq!(machine.dptr(), 0x2222);
}

#[test]
fn parity_tracks_the_accumulator() {
    let mut asm = Asm::new();
    asm.mov_a(0x03);
    asm.mov_direct_a(0x30);
    asm.mov_a(0x01);
    asm.mov_direct_a(0x31);
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());

    step(&mut machine, 1);
    assert_eq!(machine.psw() & 0x01, 0, "0x03 has two set bits");
    step(&mut machine, 2);
    assert_eq!(machine.psw() & 0x01, 1, "0x01 has one set bit");
}

#[test]
fn rotate_swap_and_logic_operate_on_a() {
    let mut asm = Asm::new();
    asm.mov_a(0x81);
    asm.op(0x03); // RR A
    asm.mov_direct_a(0x30);
    asm.mov_a(0x81);
    asm.op(0x23); // RL A
    asm.mov_direct_a(0x31);
    asm.mov_a(0xF0);
    asm.anl_a(0x3C);
    asm.orl_a(0x0F);
    asm.xrl_a(0xFF);
    asm.mov_direct_a(0x32);
    asm.mov_a(0x12);
    asm.mov_direct_a(0x34);
    asm.op(0xC4); // SWAP A
    asm.mov_direct_a(0x33);
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 15);

    assert_eq!(machine.read_iram(0x30), 0xC0, "0x81 rotated right");
    assert_eq!(machine.read_iram(0x31), 0x03, "0x81 rotated left");
    assert_eq!(machine.read_iram(0x32), 0xC0);
    assert_eq!(machine.read_iram(0x33), 0x21);
}

#[test]
fn rotate_left_through_carry_moves_the_top_bit() {
    let mut asm = Asm::new();
    asm.setb_c();
    asm.mov_a(0x80);
    asm.op(0x33); // RLC A
    asm.mov_direct_a(0x30);
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 4);

    assert_eq!(machine.read_iram(0x30), 0x01);
    assert!(machine.carry(), "bit 7 moved into carry");
}

#[test]
fn xchd_swaps_only_the_low_nibbles() {
    let mut asm = Asm::new();
    asm.mov_a(0x12);
    asm.mov_r(0, 0x30);
    asm.mov_indirect(0, 0x34);
    asm.op(0xD6); // XCHD A, @R0
    asm.label("here");
    asm.sjmp("here");
    let mut machine = boot(&asm.finish());
    step(&mut machine, 4);

    assert_eq!(machine.acc(), 0x14);
    assert_eq!(machine.read_iram(0x30), 0x32);
}

#[test]
fn an_undefined_opcode_traps_with_a_named_fault() {
    let mut machine = boot(&[0xA5]);
    let fault = machine.step().expect_err("0xa5 is not an instruction");
    assert_eq!(
        fault,
        Fault::UnknownOpcode {
            addr: 0,
            opcode: 0xA5
        }
    );
    assert!(fault.to_string().contains("unknown opcode 0xa5"));
    assert_eq!(machine.run(10), Stop::Trap);
    assert_eq!(machine.fault(), Some(fault));
}
