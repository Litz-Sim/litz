//! GPIO to and from a flattened board, plus a scheduled host level.
//!
//! The board is the catalog's Seeed XIAO RP2040 used as a numbered header:
//! its `D4` row is GPIO 6 (PC6) and its `D5` row is GPIO 7 (PC7), the pads the
//! diagrams below wire parts to.

mod common;

use common::*;
use litz_board::{Board, Chip, FlatBoard, ImageKind, Input};
use litz_catalog::Registry;
use litz_ch32v003::Ch32v003;
use litz_core::{Level, PartState};
use litz_diagram::Circuit;

const GPIOC: u32 = 0x4001_1000;
const SRAM: u32 = 0x2000_0000;

fn flat_board(diagram: &str) -> Board {
    let circuit = Circuit::from_json(diagram).unwrap();
    let registry = Registry::builtin();
    Board::new(FlatBoard::build(&circuit, &registry).unwrap()).unwrap()
}

fn diagram(connections: &[&str], parts: &[&str]) -> String {
    [
        r#"{"version":1,"parts":[{"id":"xiao","type":"board-seeed-xiao-rp2040","attrs":{}}"#,
        if parts.is_empty() { "" } else { "," },
        &parts.join(","),
        r#"],"connections":["#,
        &connections.join(","),
        "]}",
    ]
    .concat()
}

fn boot(chip: &mut Ch32v003, program: &[u32]) {
    let mut image = Image::new();
    image.word(0, 0x10);
    image.place(0x10, program);
    chip.load_image(ImageKind::Firmware, 0, &image.bytes())
        .unwrap();
    chip.boot().unwrap();
}

#[test]
fn a_gpio_program_lights_the_boards_led() {
    let mut chip = Ch32v003::new().with_board(flat_board(&diagram(
        &[
            r#"["xiao:D4","r1:1","",[]]"#,
            r#"["r1:2","led1:A","",[]]"#,
            r#"["led1:C","xiao:GND","",[]]"#,
        ],
        &[
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"220"}}"#,
            r#"{"id":"led1","type":"litz-led","attrs":{"color":"green"}}"#,
        ],
    )));

    // GPIOC.CFGLR = pin 6 push-pull output, then BSHR sets PC6.
    let [hi1, lo1] = li32(1, GPIOC);
    let [hi2, lo2] = li32(2, 0x4244_4444);
    boot(
        &mut chip,
        &[
            hi1,
            lo1,
            hi2,
            lo2,
            sw(2, 1, 0),
            addi(3, 0, 0x40),
            sw(3, 1, 0x10),
            SELF,
        ],
    );
    assert_eq!(chip.state("led1"), Some(PartState::Led { lit: false }));

    let result = chip.run(100);
    assert_eq!(result.stop, litz_board::Stop::Budget, "{:?}", chip.fault());
    assert_eq!(
        chip.state("led1"),
        Some(PartState::Led { lit: true }),
        "PC6/D4 drives the LED through its resistor"
    );
    assert!(chip.conflicts().is_empty(), "{:?}", chip.conflicts());
}

#[test]
fn a_button_press_reaches_the_gpio_input_latch() {
    let mut chip = Ch32v003::new().with_board(flat_board(&diagram(
        &[
            r#"["xiao:3V3","r1:1","",[]]"#,
            r#"["r1:2","xiao:D4","",[]]"#,
            r#"["xiao:D4","btn1:1.l","",[]]"#,
            r#"["btn1:2.l","xiao:GND","",[]]"#,
        ],
        &[
            r#"{"id":"r1","type":"litz-resistor","attrs":{"value":"10000"}}"#,
            r#"{"id":"btn1","type":"litz-pushbutton","attrs":{}}"#,
        ],
    )));

    // loop: lw x4, GPIOC.INDR ; sw x4, 0(x2) ; jal x0, loop
    let [hi1, lo1] = li32(1, GPIOC);
    let [hi2, lo2] = li32(2, SRAM);
    boot(
        &mut chip,
        &[hi1, lo1, hi2, lo2, lw(4, 1, 0x08), sw(4, 2, 0), jal(0, -8)],
    );

    assert!(chip.press("btn1", true));
    chip.run(50);
    let pressed = chip.machine().sram()[0];
    assert_eq!(pressed & 0x40, 0, "PC6 is pulled to ground while pressed");

    assert!(chip.press("btn1", false));
    chip.run(50);
    let released = chip.machine().sram()[0];
    assert_ne!(released & 0x40, 0, "the pull-up returns PC6 high");
}

#[test]
fn a_scheduled_host_level_reaches_the_input_register() {
    let [hi1, lo1] = li32(1, GPIOC);
    let [hi2, lo2] = li32(2, SRAM);
    let mut chip = Ch32v003::new();
    boot(
        &mut chip,
        &[hi1, lo1, hi2, lo2, lw(4, 1, 0x08), sw(4, 2, 0), SELF],
    );
    chip.schedule(vec![Input::Gpio {
        at_ns: 0,
        pin: 6,
        level: Level::High,
    }])
    .unwrap();

    chip.run(30);
    assert_ne!(
        chip.machine().sram()[0] & 0x40,
        0,
        "the scheduled level is visible in GPIOC.INDR"
    );
}
