//! Behaviour tests, named for what the AVR is supposed to do.

use std::cell::RefCell;
use std::rc::Rc;

use litz_avr::cpu::Fault;
use litz_avr::hex::{HexError, parse_hex};
use litz_avr::{Machine, Stop};

/// Put 16-bit instruction words at a word address in flash.
fn load_words(machine: &mut Machine, word: usize, program: &[u16]) {
    let mut bytes = Vec::with_capacity(program.len() * 2);
    for instruction in program {
        bytes.extend_from_slice(&instruction.to_le_bytes());
    }
    machine.load_bin(&bytes, (word * 2) as u64).unwrap();
}

#[test]
fn adding_two_registers_sets_the_carry() {
    let mut machine = Machine::new();
    // ldi r16, 0xff ; ldi r17, 0x01 ; add r16, r17 ; rjmp .
    load_words(&mut machine, 0, &[0xef0f, 0xe011, 0x0f01, 0xcfff]);
    machine.boot();

    assert_eq!(machine.run(3), Stop::Budget);

    assert_eq!(machine.cpu().reg(16), 0x00);
    let flags = machine.cpu().sreg();
    assert!(flags.carry(), "0xff + 0x01 carries out of bit 7");
    assert!(flags.zero(), "the result is zero");
    assert!(flags.half_carry(), "the low nibble carried into bit 4");
    assert!(!flags.negative());
    assert!(!flags.overflow(), "unsigned wrap is not signed overflow");
}

#[test]
fn a_branch_over_a_nop_takes_two_cycles() {
    let mut machine = Machine::new();
    // breq .+2 (skip the nop) ; nop ; nop
    load_words(&mut machine, 0, &[0xf009, 0x0000, 0x0000]);
    machine.boot();
    machine.cpu_mut().set_sreg(0x02); // Z set

    let before = machine.cycles();
    let cycles = machine.step().expect("breq is a valid instruction");

    assert_eq!(cycles, 2, "a taken branch retires two cycles");
    assert_eq!(machine.cycles() - before, 2);
    assert_eq!(machine.pc(), 2, "the branch skips one instruction word");
}

#[test]
fn a_store_to_sram_reads_back_through_ld() {
    let mut machine = Machine::new();
    // ldi r16, 0x42 ; ldi r26, 0x00 ; ldi r27, 0x01 ; st X, r16 ; ld r17, X
    load_words(&mut machine, 0, &[0xe402, 0xe0a0, 0xe0b1, 0x930c, 0x911c]);
    machine.boot();

    assert_eq!(machine.run(5), Stop::Budget);

    assert_eq!(machine.cpu().reg(17), 0x42);
    assert_eq!(machine.sram()[0], 0x42, "0x0100 is the first SRAM byte");
}

#[test]
fn a_call_and_ret_preserve_the_return_address() {
    let mut machine = Machine::new();
    // rcall target(+2 words) ; rjmp . ; nop ; ret
    load_words(&mut machine, 0, &[0xd002, 0xcfff, 0x0000, 0x9508]);
    machine.boot();

    machine.step().expect("rcall");
    assert_eq!(machine.pc(), 3, "the call reaches the subroutine");
    assert_eq!(machine.cpu().sp(), 0x08fd, "two bytes of return address");

    machine.step().expect("ret");
    assert_eq!(
        machine.pc(),
        1,
        "ret returns to the instruction after rcall"
    );
    assert_eq!(machine.cpu().sp(), 0x08ff, "the stack is balanced again");
}

#[test]
fn an_unknown_opcode_traps_with_its_address() {
    let mut machine = Machine::new();
    load_words(&mut machine, 8, &[0xffff]);
    machine.boot();
    machine.cpu_mut().set_pc(8);

    let fault = machine.step().expect_err("0xffff is not an instruction");

    match fault {
        Fault::UnknownOpcode { addr, opcode } => {
            assert_eq!(opcode, 0xffff);
            assert_eq!(addr, 0x10, "the byte address of word 8");
        }
        other => panic!("wrong fault: {other:?}"),
    }
    assert_eq!(machine.run(1), Stop::Trap);
    assert_eq!(machine.fault(), Some(fault));
}

#[test]
fn a_blink_program_toggles_pb5() {
    let mut machine = Machine::new();
    // sbi DDRB,5 ; sbi PORTB,5 ; cbi PORTB,5 ; rjmp back to the second sbi
    load_words(&mut machine, 0, &[0x9a25, 0x9a2d, 0x982d, 0xcffd]);

    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    machine.set_gpio_callback(Box::new(move |pin, level| {
        sink.borrow_mut().push((pin, level));
    }));
    machine.boot();

    machine.run(40);

    let edges = seen.borrow().clone();
    assert!(
        edges.contains(&(13, true)),
        "PB5 is Arduino D13 and goes high: {edges:?}"
    );
    assert!(
        edges.contains(&(13, false)),
        "PB5 returns low on the next pass: {edges:?}"
    );
    let recorded = machine.take_outputs();
    assert!(recorded.contains(&(13, true)));
    assert!(recorded.contains(&(13, false)));
    assert_eq!(
        machine.peripherals().gpio.ddr(litz_avr::PORT_B) & 0x20,
        0x20,
        "PB5 was made an output"
    );
}

#[test]
fn a_byte_written_to_udr0_reaches_the_console() {
    let mut machine = Machine::new();
    // ldi r16, 'A' ; sts 0xc6, r16
    load_words(&mut machine, 0, &[0xe401, 0x9300]);
    machine.load_bin(&[0xc6, 0x00], 4).unwrap();
    machine.boot();

    assert_eq!(machine.run(3), Stop::Budget);

    assert_eq!(machine.take_console(), b"A".to_vec());
}

#[test]
fn an_hex_record_with_a_bad_checksum_is_refused() {
    // Same record as the valid `:0100000000ff`, with the checksum flipped.
    let error = parse_hex(":0100000000fe\n").expect_err("the checksum is wrong");

    assert!(matches!(error, HexError::BadChecksum { .. }), "{error:?}");

    let mut machine = Machine::new();
    assert!(machine.load_hex(":0100000000fe\n").is_err());
    assert_eq!(
        machine.flash()[0],
        0xff,
        "a refused image writes nothing; erased flash reads 0xff"
    );
}

#[test]
fn a_valid_hex_record_loads_and_runs() {
    // ldi r16, 0xff ; ldi r17, 1 ; add r16, r17 ; rjmp .
    let hex = "\
:080000000FEF11E0010FFFCF2B\n\
:00000001FF\n";
    let mut machine = Machine::new();
    machine.load_hex(hex).expect("the image is valid");
    machine.boot();
    machine.run(3);
    assert!(machine.cpu().sreg().carry());
}

#[test]
fn a_multiply_writes_r1_r0_and_flags() {
    let mut machine = Machine::new();
    // ldi r16, 3 ; ldi r17, 4 ; mul r16, r17
    load_words(&mut machine, 0, &[0xe003, 0xe014, 0x9f01]);
    machine.boot();
    machine.run(3);

    assert_eq!(machine.cpu().reg(0), 12);
    assert_eq!(machine.cpu().reg(1), 0);
    assert!(!machine.cpu().sreg().carry());
    assert!(!machine.cpu().sreg().zero());
}

#[test]
fn adiw_takes_a_pair_to_the_signed_limit() {
    let mut machine = Machine::new();
    // ldi r24, 0xff ; ldi r25, 0x7f ; adiw r24, 1
    load_words(&mut machine, 0, &[0xef8f, 0xe79f, 0x9601]);
    machine.boot();
    machine.run(3);

    let low = u16::from(machine.cpu().reg(24));
    let high = u16::from(machine.cpu().reg(25));
    assert_eq!(low | (high << 8), 0x8000);
    assert!(machine.cpu().sreg().negative());
    assert!(
        machine.cpu().sreg().overflow(),
        "0x7fff + 1 overflows a signed 16-bit value"
    );
    assert!(!machine.cpu().sreg().carry());
}

#[test]
fn sbc_chains_the_zero_flag_across_bytes() {
    let mut machine = Machine::new();
    // sbc r25, r17
    load_words(&mut machine, 0, &[0x0b91]);
    machine.boot();
    machine.cpu_mut().set_reg(17, 0);
    machine.cpu_mut().set_reg(25, 0);

    machine.cpu_mut().set_sreg(0x02); // previous zero flag set
    machine.step().unwrap();
    assert!(
        machine.cpu().sreg().zero(),
        "a zero byte keeps the chained zero flag"
    );

    machine.cpu_mut().set_pc(0);
    machine.cpu_mut().set_sreg(0x00); // previous zero flag clear
    machine.step().unwrap();
    assert!(
        !machine.cpu().sreg().zero(),
        "a zero result clears Z when the chain was already broken"
    );
}

#[test]
fn pre_and_post_increment_move_the_pointer() {
    let mut machine = Machine::new();
    // ldi r26, 0 ; ldi r27, 1 ; ldi r16, 0xaa ; st X+, r16 ; ld r17, -X
    load_words(&mut machine, 0, &[0xe0a0, 0xe0b1, 0xea0a, 0x930d, 0x911e]);
    machine.boot();
    machine.run(6);

    assert_eq!(machine.cpu().reg(16), 0xaa);
    assert_eq!(machine.cpu().reg(17), 0xaa);
    assert_eq!(machine.cpu().reg(26), 0x00);
    assert_eq!(machine.cpu().reg(27), 0x01);
}

#[test]
fn an_out_of_range_sram_access_traps() {
    let mut machine = Machine::new();
    // ldi r26, 0xff ; ldi r27, 0x09 ; ld r16, X
    load_words(&mut machine, 0, &[0xefaf, 0xe0b9, 0x910c]);
    machine.boot();

    assert_eq!(machine.run(3), Stop::Trap);
    match machine.fault() {
        Some(Fault::DataOutOfBounds { addr }) => assert_eq!(addr, 0x09ff),
        other => panic!("wrong fault: {other:?}"),
    }
}

#[test]
fn a_scheduled_gpio_input_reaches_pind() {
    use litz_avr::Atmega328p;
    use litz_board::{Chip, ImageKind, Input};

    let mut chip = Atmega328p::new();
    // in r16, PIND ; rjmp .
    let mut image = Vec::new();
    for word in [0xb109u16, 0xcfff] {
        image.extend_from_slice(&word.to_le_bytes());
    }
    chip.load_image(ImageKind::Firmware, 0, &image).unwrap();
    chip.boot().unwrap();
    chip.schedule(vec![Input::Gpio {
        at_ns: 0,
        pin: 2,
        level: litz_core::Level::High,
    }])
    .unwrap();

    chip.run(4);

    assert_ne!(
        chip.machine().cpu().reg(16) & 0x04,
        0,
        "PD2 read high through PIN D"
    );
}

#[test]
fn timer0_overflow_sets_tov0() {
    let mut machine = Machine::new();
    // rjmp . forever; Timer0 at /1, starting from 0xfe.
    load_words(&mut machine, 0, &[0xcfff]);
    machine.boot();
    {
        let periph = machine.peripherals_mut();
        periph.timer0.tccr_b = 0x01; // no prescale
        periph.timer0.count = 0xfe;
    }

    machine.run(4);

    assert!(
        machine.peripherals().timer0.tifr & 0x01 != 0,
        "TOV0 is set after the counter wraps"
    );
    // Writing a one clears it.
    machine.peripherals_mut().timer0.tifr &= !0x01;
    assert_eq!(machine.peripherals().timer0.tifr & 0x01, 0);
}
