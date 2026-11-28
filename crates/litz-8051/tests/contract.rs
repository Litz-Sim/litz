//! The backend contract: a hand-assembled firmware prints and toggles a pin,
//! images load in both formats, debug reads and steps work, and two identical
//! instances produce identical output.

mod common;

use common::Asm;
use litz_8051::{DEBUG_IRAM_BASE, DEBUG_XDATA_BASE, Machine, Stc89c52, sfr};
use litz_board::{Chip, ImageKind, Stop as BoardStop};
use litz_core::PartState;

fn print_and_toggle_program() -> Vec<u8> {
    let mut asm = Asm::new();
    asm.clr_bit(0x90); // CLR P1.0
    asm.mov_direct(sfr::TMOD, 0x20);
    asm.mov_direct(sfr::TH1, 0xFD);
    asm.mov_direct(sfr::SCON, 0x40); // mode 1
    asm.setb(0x8E); // TR1
    asm.mov_sbuf(b'H');
    asm.label("h");
    asm.jnb(0x99, "h"); // JNB TI, h
    asm.clr_bit(0x99);
    asm.mov_sbuf(b'i');
    asm.label("i");
    asm.jnb(0x99, "i");
    asm.clr_bit(0x99);
    asm.setb(0x90); // SETB P1.0
    asm.label("done");
    asm.sjmp("done");
    asm.finish()
}

#[test]
fn the_chip_describes_itself_as_an_stc89c52() {
    let info = Stc89c52::new().info();
    assert_eq!(info.name, "stc89c52");
    assert_eq!(info.cpu_hz, 11_059_200);
    assert_eq!(info.flash.size, 8192);
}

#[test]
fn the_firmware_prints_a_string_and_toggles_p1_0() {
    let (mut chip, _board) = common::chip_on_shared(&common::diagram(
        &[
            r#"["mcu:GPIO8","r1:1","",[]]"#,
            r#"["r1:2","led1:A","",[]]"#,
            r#"["led1:C","mcu:GND.1","",[]]"#,
        ],
        &[
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"220"}}"#,
            r#"{"id":"led1","type":"litz-led","attrs":{"color":"green"}}"#,
        ],
    ));
    common::boot(&mut chip, &print_and_toggle_program());

    // The reset latch is high; the first instruction clears it.
    assert_eq!(chip.state("led1"), Some(PartState::Led { lit: true }));
    assert_eq!(chip.debug_step(), Some(BoardStop::Idle));
    assert_eq!(chip.state("led1"), Some(PartState::Led { lit: false }));

    let result = chip.run(120_000);
    assert_eq!(result.stop, BoardStop::Budget);
    assert_eq!(chip.take_console(), b"Hi");
    assert_eq!(
        chip.state("led1"),
        Some(PartState::Led { lit: true }),
        "the firmware set P1.0 after the two bytes"
    );
}

#[test]
fn loading_intel_hex_places_records_at_their_addresses() {
    // MOV A,#0x41 ; MOV P1,A ; SJMP .
    let program = [0x74, 0x41, 0xF5, 0x90, 0x80, 0xFE];
    let text = common::intel_hex(&program, 0);
    let mut chip = Stc89c52::new();
    chip.load_image(ImageKind::Firmware, 0, text.as_bytes())
        .unwrap();
    chip.boot().unwrap();
    chip.run(100);

    assert_eq!(chip.machine().read_sfr(sfr::P1), 0x41);
    assert_eq!(
        chip.flash_image().unwrap()[..program.len()],
        program,
        "the HEX records landed at their own addresses"
    );

    let mut chip = Stc89c52::new();
    let error = chip
        .load_image(ImageKind::Firmware, 0x10, text.as_bytes())
        .expect_err("HEX carries its own addresses");
    assert!(error.contains("addresses"), "{error}");
}

#[test]
fn a_raw_binary_lands_at_its_offset_and_flash_stays_erased_elsewhere() {
    let mut chip = Stc89c52::new();
    chip.load_image(ImageKind::Firmware, 0x100, &[0x74, 0x55])
        .unwrap();
    chip.boot().unwrap();

    let flash = chip.flash_image().expect("the 8051 has a code array");
    assert_eq!(flash.len(), 8192);
    assert_eq!(&flash[0x100..0x102], &[0x74, 0x55]);
    assert!(flash[..0x100].iter().all(|byte| *byte == 0xFF));
    assert!(flash[0x102..].iter().all(|byte| *byte == 0xFF));
}

#[test]
fn the_disassembler_names_sfrs_and_branch_targets() {
    let mut chip = Stc89c52::new();
    // MOV A,#0x41 ; MOV P1,A ; SJMP . ; SETB P1.0
    chip.load_image(
        ImageKind::Firmware,
        0,
        &[0x74, 0x41, 0xF5, 0x90, 0x80, 0xFE, 0xD2, 0x90],
    )
    .unwrap();
    chip.boot().unwrap();

    let text = chip.debug_disassemble(0, 4);
    assert!(text.contains("MOV A, #0x41"), "{text}");
    assert!(text.contains("MOV P1, A"), "{text}");
    assert!(text.contains("SJMP 0x0004"), "{text}");
    assert!(text.contains("SETB P1.0"), "{text}");
}

#[test]
fn the_debug_surface_reads_registers_and_all_three_memories() {
    let mut chip = Stc89c52::new();
    chip.load_image(ImageKind::Firmware, 0, &[0x12, 0x34])
        .unwrap();
    chip.boot().unwrap();

    {
        let machine = chip.machine_mut();
        machine.set_acc(0x12);
        machine.set_psw(0x08); // bank 1
        machine.set_register(0, 0x34);
        machine.set_dptr(0x5678);
        machine.write_iram(0x30, 0xAB);
        machine.write_xdata(0x1234, 0xCD);
    }

    let file = chip.debug_register_file().expect("the 8051 has registers");
    assert_eq!(file[0], 0x12, "ACC");
    assert_eq!(file[2], 0x78, "DPL");
    assert_eq!(file[3], 0x56, "DPH");
    assert_eq!(file[5], 0x08, "PSW with the bank-select bits");
    assert_eq!(file[9], 0x34, "bank 1 R0");
    assert_eq!(file[32], 0, "PC");

    assert_eq!(chip.debug_read_memory(0, 2), vec![0x12, 0x34]);
    assert_eq!(
        chip.debug_read_memory(DEBUG_IRAM_BASE + 0x30, 1),
        vec![0xAB]
    );
    assert_eq!(
        chip.debug_read_memory(DEBUG_XDATA_BASE + 0x1234, 1),
        vec![0xCD]
    );
    assert!(chip.debug_read_memory(0x3000, 1).is_empty());
}

#[test]
fn a_breakpoint_stops_before_its_instruction() {
    let mut chip = Stc89c52::new();
    // NOP ; NOP ; SJMP .
    chip.load_image(ImageKind::Firmware, 0, &[0x00, 0x00, 0x80, 0xFE])
        .unwrap();
    chip.boot().unwrap();
    chip.debug_set_breakpoints(&[2]);

    let result = chip.run(10_000);
    assert_eq!(result.stop, BoardStop::Breakpoint(2));
    assert_eq!(chip.debug_pc(), Some(2));
    assert_eq!(chip.debug_step(), Some(BoardStop::Idle));
    assert_eq!(chip.debug_pc(), Some(2), "SJMP . stays put");
}

#[test]
fn two_identical_instances_produce_identical_output() {
    fn once() -> (Vec<u8>, Vec<(u8, bool)>) {
        let mut chip = Stc89c52::new();
        chip.load_image(ImageKind::Firmware, 0, &print_and_toggle_program())
            .unwrap();
        chip.boot().unwrap();
        chip.run(120_000);
        let console = chip.take_console();
        let edges = chip.take_outputs();
        (console, edges)
    }

    let first = once();
    let second = once();
    assert_eq!(first, second);
    assert_eq!(first.0, b"Hi");
    assert!(first.1.contains(&(8, false)), "P1.0 was cleared");
    assert!(first.1.contains(&(8, true)), "P1.0 was set");
}

#[test]
fn an_undefined_opcode_reports_a_named_fault() {
    let mut chip = Stc89c52::new();
    chip.load_image(ImageKind::Firmware, 0, &[0xA5]).unwrap();
    chip.boot().unwrap();

    let result = chip.run(10);
    assert_eq!(result.stop, BoardStop::Trap);
    let fault = chip.fault().expect("a trap has a reason");
    assert!(fault.contains("unknown opcode 0xa5"), "{fault}");
    assert!(fault.contains("0x0000"), "{fault}");
}

#[test]
fn a_bare_machine_steps_the_same_instruction_stream() {
    let mut machine = Machine::new();
    machine.load_bin(&[0x74, 0x2A, 0x00], 0).unwrap();
    machine.boot();
    assert_eq!(machine.step().unwrap(), 1);
    assert_eq!(machine.acc(), 0x2A);
    assert_eq!(machine.step().unwrap(), 1);
    assert_eq!(machine.machine_cycles(), 2);
    assert_eq!(machine.cycles(), 24);
}
