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

/// The reference chip answers the same debug questions as the native ESP32
/// backend: its pc, its register file, memory, a breakpoint stop and a step.
#[test]
fn the_component_answers_the_debug_interface() {
    use litz_board::chip::Stop;

    let mut chip = booted();
    assert!(chip.debug_available());
    let entry = chip.debug_pc().expect("a pc at boot");
    assert_ne!(entry, 0, "the fixture image has an entry point");

    // The host maps the guest's named registers into the RISC-V file order and
    // puts the pc in slot 32.
    let file = chip.debug_register_file().expect("a register file");
    assert_eq!(file[32], entry);

    assert_eq!(chip.debug_read_memory(entry, 4).len(), 4);
    assert!(
        chip.debug_disassemble(entry, 2).contains("0x"),
        "the core reports raw instruction words"
    );

    // A breakpoint at the pc stops before the instruction runs.
    chip.debug_set_breakpoints(&[entry]);
    let result = chip.run(1_000);
    assert_eq!(result.cycles, 0);
    assert_eq!(result.stop, Stop::Breakpoint(entry));

    // Stepping executes the instruction at the same pc rather than
    // re-triggering the breakpoint.
    let stop = chip.debug_step().expect("a step");
    assert_eq!(stop, Stop::Budget);
    assert_ne!(chip.debug_pc().unwrap(), entry, "the pc advanced");
}

#[test]
fn a_module_that_is_not_a_component_is_refused() {
    let error = match WasmChip::from_bytes(b"\0asm\x01\0\0\0", "junk.wasm") {
        Ok(_) => panic!("a core module is not a component"),
        Err(error) => error,
    };
    assert!(error.contains("junk.wasm"), "{error}");
}
