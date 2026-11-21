//! A real component, loaded and driven through the contract.
//!
//! The module is the one the catalog ships for `board-litz-rv32`, and the
//! firmware is the one `litz-fixture` generates — so this test fails if the
//! WIT, the guest and the host drift apart in any direction.

#![cfg(not(target_arch = "wasm32"))]

use litz_board::chip::{Chip, ImageKind};
use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_diagram::Circuit;
use litz_mcu_wasm::WasmChip;

/// The component the catalog carries for the reference board.
const MODULE: &[u8] = include_bytes!("../../litz-catalog/builtin/boards/litz-rv32.wasm");

const DIAGRAM: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "rv", "type": "board-litz-rv32", "attrs": {}},
    {"id": "r1", "type": "litz-resistor", "attrs": {"value": "220"}},
    {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
  ],
  "connections": [
    ["rv:GPIO8", "r1:1", "", []],
    ["r1:2", "led1:A", "", []],
    ["led1:C", "rv:GND.1", "", []]
  ]
}"#;

fn booted() -> WasmChip {
    let registry = Registry::builtin();
    let circuit = Circuit::from_json(DIAGRAM).unwrap();
    let board = Board::new(FlatBoard::build(&circuit, &registry).unwrap()).unwrap();
    let mut chip = WasmChip::from_bytes(MODULE, "litz-rv32.wasm")
        .unwrap()
        .with_board(board);
    let flash = chip.info().flash;
    // A short half-period keeps the test to a fraction of a second of host
    // time while still producing several edges.
    let firmware = litz_fixture::blinky(2_000_000);
    chip.load_image(ImageKind::Firmware, flash.app, &firmware)
        .expect("firmware loads");
    chip.boot().expect("boot");
    chip
}

#[test]
fn the_component_reports_the_chip_it_is() {
    let chip = WasmChip::from_bytes(MODULE, "litz-rv32.wasm").unwrap();
    let info = chip.info();
    assert_eq!(info.name, "litz-rv32");
    assert_eq!(info.cpu_hz, 160_000_000);
    assert_eq!(info.flash.app, 0x1_0000);
}

#[test]
fn the_repository_firmware_runs_on_the_component() {
    let mut chip = booted();
    let mut serial = String::new();
    for _ in 0..40 {
        let _ = chip.run(1_000_000);
        serial.push_str(&String::from_utf8_lossy(&chip.take_console()));
        if serial.matches('+').count() >= 2 {
            break;
        }
    }
    assert!(serial.contains("litz: blinky"), "serial was {serial:?}");
    assert!(
        serial.contains('+'),
        "the LED should have been driven: {serial:?}"
    );
    assert!(chip.conflicts().is_empty(), "{:?}", chip.conflicts());
    assert!(chip.fault().is_none(), "{:?}", chip.fault());
}

#[test]
fn the_board_sees_the_pin_the_firmware_drives() {
    let mut chip = booted();
    let mut saw_lit = false;
    for _ in 0..40 {
        let _ = chip.run(1_000_000);
        let _ = chip.take_console();
        if chip.state("led1") == Some(litz_core::PartState::Led { lit: true }) {
            saw_lit = true;
            break;
        }
    }
    assert!(saw_lit, "the LED on the board never lit");
}

#[test]
fn a_module_that_is_not_a_component_is_refused() {
    let error = match WasmChip::from_bytes(b"\0asm\x01\0\0\0", "junk.wasm") {
        Ok(_) => panic!("a core module is not a component"),
        Err(error) => error,
    };
    assert!(error.contains("junk.wasm"), "{error}");
}
