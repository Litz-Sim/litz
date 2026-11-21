//! A potentiometer reaching the chip as a voltage, end to end: the board
//! derives the wiper voltage from the divider, the adapter programs the ADC
//! pad, and firmware that samples it reacts.
//!
//! The firmware is `litz_fixture::pot_led` — hand-assembled RV32IMC that
//! converts ADC1 channel 0 in a loop and lights GPIO8 past half scale — so the
//! whole path is exercised without ESP-IDF or a toolchain.

#![cfg(feature = "esp32c3")]

use litz_board::{Board as LitzBoard, FlatBoard};
use litz_catalog::Registry;
use litz_core::{Level, PartState};
use litz_diagram::Circuit;

const DIAGRAM: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
    {"id": "pot1", "type": "litz-potentiometer", "attrs": {"value": "0"}},
    {"id": "r1", "type": "litz-resistor", "attrs": {"value": "220"}},
    {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
  ],
  "connections": [
    ["esp:GND.1", "pot1:1", "", []],
    ["pot1:2", "esp:GPIO0", "", []],
    ["pot1:3", "esp:3V3", "", []],
    ["esp:GPIO8", "r1:1", "", []],
    ["r1:2", "led1:A", "", []],
    ["led1:C", "esp:GND.1", "", []]
  ]
}"#;

fn boot() -> litz_esp::backend::C3 {
    let registry = Registry::builtin();
    let circuit = Circuit::from_json(DIAGRAM).unwrap();
    let board = LitzBoard::new(FlatBoard::build(&circuit, &registry).unwrap()).unwrap();
    let mut mcu =
        litz_esp::backend::C3::new([0x24, 0x0a, 0xc4, 0x12, 0x34, 0x56], 4 << 20).with_board(board);
    let firmware = litz_fixture::pot_led();
    mcu.write_flash(0x1_0000, &firmware).unwrap();
    mcu.boot_app(0x1_0000).expect("boot_app");
    mcu
}

/// Run long enough for the polling loop to convert and act at least twice.
fn settle(mcu: &mut litz_esp::backend::C3) {
    for _ in 0..8 {
        let _ = mcu.run(1_000_000);
    }
}

#[test]
fn the_knob_position_reaches_the_adc() {
    let mut mcu = boot();
    // The diagram starts the knob at 0 % with pin 1 on ground and pin 3 on
    // 3.3 V, so the wiper is at 0 V and the LED stays dark.
    assert_eq!(mcu.state("pot1"), Some(PartState::Knob { permille: 0 }));
    assert_eq!(mcu.analog_inputs(), vec![(0, 0.0)]);
    settle(&mut mcu);
    assert_eq!(mcu.state("led1"), Some(PartState::Led { lit: false }));

    assert!(mcu.set_control("pot1", 1000));
    assert_eq!(mcu.analog_inputs(), vec![(0, 3.3)]);
    settle(&mut mcu);
    assert_eq!(mcu.state("led1"), Some(PartState::Led { lit: true }));

    assert!(mcu.set_control("pot1", 0));
    settle(&mut mcu);
    assert_eq!(mcu.state("led1"), Some(PartState::Led { lit: false }));
    assert!(mcu.conflicts().is_empty(), "{:?}", mcu.conflicts());
}

#[test]
fn the_wiper_is_a_voltage_not_a_logic_level() {
    let mut mcu = boot();
    mcu.set_control("pot1", 500);
    let [(pin, volts)] = mcu.analog_inputs()[..] else {
        panic!("one analog pad")
    };
    assert_eq!(pin, 0);
    assert!((volts - 1.65).abs() < 0.01, "{volts}");
    // Half way is not a level either: digital readers of the pad see a
    // floating net, because 1.65 V is neither a clean high nor a clean low.
    assert_eq!(mcu.part_pin_level("pot1", Some("2")), Some(Level::HighZ));
}
