//! The STC89C52RC driving a flattened Litz board: port edges reach parts, a
//! part's level reaches a pin, and the board's supply controls the core.
//!
//! The board is the inline manifest from `common`, whose 32 header pins are
//! numbered with the adapter's convention: GPIO = port * 8 + bit.

mod common;

use common::Asm;
use litz_8051::{Stc89c52, sfr};
use litz_board::{Chip, Stop as BoardStop};
use litz_core::{Level, PartState};

fn blink_program() -> Vec<u8> {
    let mut asm = Asm::new();
    asm.clr_bit(0x90); // CLR P1.0
    asm.setb(0x90); // SETB P1.0
    asm.label("here");
    asm.sjmp("here");
    asm.finish()
}

fn print_program() -> Vec<u8> {
    let mut asm = Asm::new();
    asm.label("main");
    asm.mov_direct(sfr::TMOD, 0x20);
    asm.mov_direct(sfr::TH1, 0xFD);
    asm.mov_direct(sfr::SCON, 0x40);
    asm.setb(0x8E); // TR1
    asm.mov_sbuf(b'R');
    asm.label("poll");
    asm.jnb(0x99, "poll"); // SCON.1 = TI
    asm.clr_bit(0x99);
    asm.setb(0x90); // P1.0
    asm.label("done");
    asm.sjmp("done");
    asm.finish()
}

#[test]
fn a_port_edge_lights_and_clears_the_led() {
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
    common::boot(&mut chip, &blink_program());

    // The reset latches are high, so the board was seeded lit.
    assert_eq!(chip.state("led1"), Some(PartState::Led { lit: true }));
    assert!(chip.conflicts().is_empty(), "{:?}", chip.conflicts());

    assert_eq!(chip.debug_step(), Some(BoardStop::Idle)); // CLR P1.0
    assert_eq!(chip.state("led1"), Some(PartState::Led { lit: false }));
    assert_eq!(chip.gpio_level(8), Some(Level::Low));

    assert_eq!(chip.debug_step(), Some(BoardStop::Idle)); // SETB P1.0
    assert_eq!(chip.state("led1"), Some(PartState::Led { lit: true }));
    assert_eq!(chip.gpio_level(8), Some(Level::High));
}

#[test]
fn a_button_on_int0_reaches_the_firmware() {
    let (mut chip, _board) = common::chip_on_shared(&common::diagram(
        &[
            r#"["mcu:3V3","r1:1","",[]]"#,
            r#"["r1:2","mcu:GPIO26","",[]]"#,
            r#"["mcu:GPIO26","btn1:1.l","",[]]"#,
            r#"["btn1:2.l","mcu:GND.1","",[]]"#,
        ],
        &[
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"10000"}}"#,
            r#"{"id":"btn1","type":"litz-pushbutton","attrs":{}}"#,
        ],
    ));

    // loop: JB P3.2, loop ; MOV A,#1 ; MOV 0x30,A ; done: SJMP done
    let mut asm = Asm::new();
    asm.label("loop");
    asm.jb(0xB2, "loop");
    asm.mov_a(1);
    asm.mov_direct_a(0x30);
    asm.label("done");
    asm.sjmp("done");
    common::boot(&mut chip, &asm.finish());

    chip.run(200);
    assert_eq!(
        chip.machine().read_iram(0x30),
        0,
        "the pull-up holds P3.2 high"
    );

    assert!(chip.press("btn1", true));
    chip.run(400);
    assert_eq!(
        chip.machine().read_iram(0x30),
        1,
        "the button pulled P3.2 low and the firmware stored its marker"
    );
    assert_eq!(
        chip.state("btn1"),
        Some(PartState::Button { pressed: true })
    );
}

#[test]
fn a_high_latch_reads_a_low_external_driver() {
    let (mut chip, _board) = common::chip_on_shared(&common::diagram(
        &[
            r#"["mcu:3V3","r1:1","",[]]"#,
            r#"["r1:2","mcu:GPIO9","",[]]"#,
            r#"["mcu:GPIO9","btn1:1.l","",[]]"#,
            r#"["btn1:2.l","mcu:GND.1","",[]]"#,
        ],
        &[
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"10000"}}"#,
            r#"{"id":"btn1","type":"litz-pushbutton","attrs":{}}"#,
        ],
    ));

    // loop: MOV A,P1 ; MOV 0x30,A ; SJMP loop
    let mut asm = Asm::new();
    asm.label("loop");
    asm.mov_a_direct(sfr::P1);
    asm.mov_direct_a(0x30);
    asm.sjmp("loop");
    common::boot(&mut chip, &asm.finish());

    chip.run(100);
    assert_eq!(
        chip.machine().read_iram(0x30),
        0xFF,
        "with P1.1's pull-up high, all latched ones read back"
    );

    chip.press("btn1", true);
    chip.run(200);
    assert_eq!(
        chip.machine().read_iram(0x30),
        0xFD,
        "the pin's high latch ANDed with the external low reads zero"
    );
}

#[test]
fn powering_the_board_off_stops_the_core_and_back_on_reboots_it() {
    let (mut chip, board) = common::chip_on_shared(&common::diagram(&[], &[]));
    common::boot(&mut chip, &print_program());

    chip.run(80_000);
    assert_eq!(chip.take_console(), b"R", "the firmware printed its banner");

    let now = (chip.seconds() * 1e9) as u64;
    board.borrow_mut().set_board_power(0, false, now);
    let before = chip.seconds();
    let result = chip.run(10_000);
    assert_eq!(result.stop, BoardStop::Idle);
    assert!(
        chip.take_console().is_empty(),
        "an unpowered core prints nothing"
    );
    assert!(
        chip.seconds() > before,
        "simulated time still advances with the board off"
    );

    let now = (chip.seconds() * 1e9) as u64;
    board.borrow_mut().set_board_power(0, true, now);
    chip.run(80_000);
    assert_eq!(
        chip.take_console(),
        b"R",
        "powering on boots from the code array's reset vector"
    );
}

#[test]
fn an_8051_has_no_board_parts_of_its_own() {
    let mut chip = Stc89c52::new();
    assert!(!chip.press("btn1", true));
    assert!(!chip.set_control("pot1", 10));
    assert_eq!(chip.state("led1"), None);
    assert!(chip.conflicts().is_empty());
}
