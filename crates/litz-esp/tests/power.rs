//! A C3 whose board is switched off runs nothing, and switching it back on
//! starts the firmware from its reset vector with the flash it had.

#![cfg(feature = "esp32c3")]

use litz_board::Chip;
use litz_board::flatten::FlatBoard;
use litz_catalog::Registry;
use litz_diagram::Circuit;

const MAC: [u8; 6] = [0x24, 0x0a, 0xc4, 0x00, 0x02, 0x20];
const FLASH: usize = 4 << 20;
const APP: u64 = 0x1_0000;

fn board(power: bool) -> std::rc::Rc<std::cell::RefCell<litz_board::Board>> {
    let diagram = format!(
        r#"{{"version":1,"parts":[
          {{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{{"power":{power}}}}}],
          "connections":[]}}"#
    );
    let circuit = Circuit::from_json(&diagram).unwrap();
    let flat = FlatBoard::build(&circuit, &Registry::builtin()).unwrap();
    std::rc::Rc::new(std::cell::RefCell::new(
        litz_board::Board::new(flat).unwrap(),
    ))
}

fn c3(board: std::rc::Rc<std::cell::RefCell<litz_board::Board>>) -> Box<dyn Chip> {
    let mut chip = litz_esp::backend::C3::new(MAC, FLASH).with_shared_board(board, 0);
    chip.load_image(
        litz_board::ImageKind::Firmware,
        APP,
        &litz_fixture::blinky(16_000_000),
    )
    .unwrap();
    Box::new(chip)
}

fn run(chip: &mut Box<dyn Chip>, budget: u64) -> String {
    let _ = chip.run(budget);
    String::from_utf8_lossy(&chip.take_console()).into_owned()
}

#[test]
fn an_off_board_runs_nothing_and_the_clock_still_moves() {
    let board = board(false);
    let mut chip = c3(std::rc::Rc::clone(&board));
    chip.boot().unwrap();
    let before = chip.seconds();
    let console = run(&mut chip, 1_000_000);
    assert!(console.is_empty(), "an unpowered core prints nothing");
    assert!(
        chip.seconds() > before,
        "simulated time passes even with the board off"
    );
}

#[test]
fn powering_the_board_on_boots_the_firmware_and_off_resets_it() {
    let board = board(false);
    let mut chip = c3(std::rc::Rc::clone(&board));
    chip.boot().unwrap();
    run(&mut chip, 1_000_000);

    let now = (chip.seconds() * 1e9) as u64;
    board.borrow_mut().set_board_power(0, true, now);
    let console = run(&mut chip, 4_000_000);
    assert!(
        console.contains("litz: blinky"),
        "powering on boots from flash: {console:?}"
    );

    // Power off: the core stops.
    let now = (chip.seconds() * 1e9) as u64;
    board.borrow_mut().set_board_power(0, false, now);
    let console = run(&mut chip, 1_000_000);
    assert!(
        console.is_empty(),
        "a dead core prints nothing: {console:?}"
    );

    // Power on again: the firmware restarts from the same flash.
    let now = (chip.seconds() * 1e9) as u64;
    board.borrow_mut().set_board_power(0, true, now);
    let console = run(&mut chip, 4_000_000);
    assert!(
        console.contains("litz: blinky"),
        "the flash kept the firmware across the power cycle: {console:?}"
    );
}

#[test]
fn a_board_that_starts_on_boots_immediately() {
    let board = board(true);
    let mut chip = c3(std::rc::Rc::clone(&board));
    chip.boot().unwrap();
    let console = run(&mut chip, 4_000_000);
    assert!(console.contains("litz: blinky"), "{console:?}");
    assert!(board.borrow().board_powered(0));
}

/// Power loss releases the pads. An LED wired through a resistor goes dark
/// rather than staying latched on the level the pad last held: the dead pad
/// bleeds its net low, which is what makes the level reach the LED at all.
#[test]
fn switching_the_board_off_darkens_a_led_on_a_pin() {
    let diagram = r#"{"version":1,"parts":[
      {"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}},
      {"id":"r1","type":"litz-resistor","attrs":{"value":"220"}},
      {"id":"led1","type":"litz-led","attrs":{"color":"green"}}
    ],"connections":[
      ["esp:GPIO8","r1:1","",[]],
      ["r1:2","led1:A","",[]],
      ["led1:C","esp:GND.1","",[]]
    ]}"#;
    let circuit = Circuit::from_json(diagram).unwrap();
    let flat = FlatBoard::build(&circuit, &Registry::builtin()).unwrap();
    let board = std::rc::Rc::new(std::cell::RefCell::new(
        litz_board::Board::new(flat).unwrap(),
    ));
    let mut chip = c3(std::rc::Rc::clone(&board));
    chip.boot().unwrap();

    // The firmware drives the LED high soon after it starts.
    let mut lit = false;
    for _ in 0..20 {
        chip.run(1_000_000);
        if board.borrow().part_pin_level("led1", "A") == Some(litz_core::Level::High) {
            lit = true;
            break;
        }
    }
    assert!(lit, "the firmware lights the LED");

    let now = (chip.seconds() * 1e9) as u64;
    board.borrow_mut().set_board_power(0, false, now);
    chip.run(1_000_000);
    assert_eq!(
        board.borrow().part_pin_level("led1", "A"),
        Some(litz_core::Level::Low),
        "a switched-off board leaves the LED dark"
    );
}
