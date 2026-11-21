//! The committed 74HC595 component, placed on a board and driven by chip pins.
//!
//! This is the end-to-end proof that the `litz:part` contract, the guest and
//! the catalog manifest agree: the manifest's pin indices are alphabetical,
//! the guest's constants are the same order, and the board sees `Q0`..`Q7`
//! follow a byte clocked in through `DS`.
//!
//! The host has no chip attached, so the GPIOs are driven straight through
//! `Board::set_chip_output`, which is exactly what a backend does.

use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_core::{Drive, Level};
use litz_diagram::Circuit;

const DIAGRAM: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
    {"id": "sr1", "type": "litz-74hc595", "attrs": {}}
  ],
  "connections": [
    ["esp:GPIO8",  "sr1:DS",   "", []],
    ["esp:GPIO9",  "sr1:SHCP", "", []],
    ["esp:GPIO10", "sr1:STCP", "", []],
    ["esp:GPIO0",  "sr1:Q0",   "", []],
    ["esp:GPIO1",  "sr1:Q1",   "", []],
    ["esp:GPIO2",  "sr1:Q2",   "", []],
    ["esp:GPIO3",  "sr1:Q3",   "", []],
    ["esp:GPIO4",  "sr1:Q4",   "", []],
    ["esp:GPIO5",  "sr1:Q5",   "", []],
    ["esp:GPIO6",  "sr1:Q6",   "", []],
    ["esp:GPIO7",  "sr1:Q7",   "", []],
    ["esp:GPIO18", "sr1:OE",   "", []],
    ["esp:GPIO19", "sr1:MR",   "", []]
  ]
}"#;

const DS: u8 = 8;
const SHCP: u8 = 9;
const STCP: u8 = 10;

fn board() -> Board {
    let circuit = Circuit::from_json(DIAGRAM).unwrap();
    let registry = Registry::builtin();
    Board::new(FlatBoard::build(&circuit, &registry).unwrap()).unwrap()
}

fn drive(board: &mut Board, gpio: u8, level: Level) {
    board.set_chip_output(gpio, Drive::strong(level), 0);
}

fn clock(board: &mut Board, gpio: u8) {
    drive(board, gpio, Level::High);
    drive(board, gpio, Level::Low);
}

/// Shift one bit in on `SHCP`'s rising edge.
fn shift_bit(board: &mut Board, bit: bool) {
    drive(board, DS, if bit { Level::High } else { Level::Low });
    clock(board, SHCP);
}

/// Send a byte most-significant bit first, then latch it. The last bit
/// clocked in lands on `Q0`, so this leaves bit `i` on `Qi`.
fn send(board: &mut Board, byte: u8) {
    for bit in (0..8).rev() {
        shift_bit(board, byte & (1 << bit) != 0);
    }
    clock(board, STCP);
}

fn outputs(board: &Board) -> u8 {
    let mut mask = 0;
    for bit in 0..8 {
        let pin = format!("Q{bit}");
        if board.part_pin_level("sr1", &pin) == Some(Level::High) {
            mask |= 1 << bit;
        }
    }
    mask
}

#[test]
fn the_outputs_follow_the_byte_clocked_through_ds() {
    let mut board = board();
    assert_eq!(outputs(&board), 0, "the latch starts clear");

    for byte in [0x00, 0x01, 0x80, 0xa5, 0x5a, 0xff] {
        send(&mut board, byte);
        assert_eq!(outputs(&board), byte, "after shifting {byte:#04x}");
        assert!(board.conflicts().is_empty(), "{:?}", board.conflicts());
    }
}

#[test]
fn the_latch_holds_until_the_next_storage_clock() {
    let mut board = board();
    send(&mut board, 0x3c);
    assert_eq!(outputs(&board), 0x3c);

    // Clock a new byte in without raising STCP: the outputs keep the old one.
    for bit in (0..8).rev() {
        shift_bit(&mut board, 0xc3 & (1 << bit) != 0);
    }
    assert_eq!(outputs(&board), 0x3c);
    clock(&mut board, STCP);
    assert_eq!(outputs(&board), 0xc3);
}

#[test]
fn output_enable_and_master_reset_do_what_the_datasheet_says() {
    let mut board = board();
    send(&mut board, 0xaa);
    assert_eq!(outputs(&board), 0xaa);

    // OE is active low: high releases every output, low drives again.
    drive(&mut board, SR_OE, Level::High);
    assert_eq!(
        board.part_pin_level("sr1", "Q0"),
        Some(Level::HighZ),
        "OE high leaves the outputs floating"
    );
    drive(&mut board, SR_OE, Level::Low);
    assert_eq!(outputs(&board), 0xaa);

    // MR is active low and clears the shift register, not the latch. Clocking
    // while it is held does nothing; releasing it lets the next bit in.
    drive(&mut board, SR_MR, Level::Low);
    shift_bit(&mut board, false);
    drive(&mut board, SR_MR, Level::High);
    shift_bit(&mut board, true);
    clock(&mut board, STCP);
    assert_eq!(outputs(&board), 0x01);
}

// OE and MR sit on their own header pins so the test can hold them.
const SR_OE: u8 = 18;
const SR_MR: u8 = 19;
