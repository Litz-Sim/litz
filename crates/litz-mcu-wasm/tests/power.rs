//! A custom chip on a board that is switched off runs nothing, and the first
//! run after power arrives boots the guest from its reset state.

#![cfg(not(target_arch = "wasm32"))]

use std::cell::RefCell;
use std::rc::Rc;

use litz_board::chip::{Chip, ImageKind};
use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_diagram::Circuit;
use litz_mcu_wasm::WasmChip;

const MODULE: &[u8] = include_bytes!("fixtures/board-v2/board-v2.wasm");

fn board(power: bool) -> Rc<RefCell<Board>> {
    let diagram = format!(
        r#"{{"version":1,"parts":[
          {{"id":"mcu","type":"board-v2-fixture","attrs":{{"power":{power}}}}}],
          "connections":[]}}"#
    );
    let mut registry = Registry::builtin();
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    registry.load_dir(std::path::Path::new(dir)).unwrap();
    let flat = FlatBoard::build(&Circuit::from_json(&diagram).unwrap(), &registry).unwrap();
    Rc::new(RefCell::new(Board::new(flat).unwrap()))
}

fn program() -> Vec<u8> {
    let mut bytes = vec![1, b'h', 1, b'i', 1, b'\n'];
    bytes.push(0);
    bytes
}

fn chip(board: Rc<RefCell<Board>>) -> Box<dyn Chip> {
    let mut chip = WasmChip::from_bytes_with_abi(MODULE, "board-v2.wasm", "litz-mcu-2")
        .expect("the component is a 0.2 guest")
        .with_shared_board(board, 0);
    chip.load_image(ImageKind::Firmware, 0, &program()).unwrap();
    Box::new(chip)
}

fn run(chip: &mut Box<dyn Chip>, budget: u64) -> String {
    let _ = chip.run(budget);
    String::from_utf8_lossy(&chip.take_console()).into_owned()
}

#[test]
fn powering_a_custom_chip_on_boots_it_and_off_stops_it() {
    let board = board(false);
    let mut chip = chip(Rc::clone(&board));
    chip.boot().unwrap();
    let console = run(&mut chip, 1_000_000);
    assert!(console.is_empty(), "an unpowered guest prints nothing");

    let now = (chip.seconds() * 1e9) as u64;
    board.borrow_mut().set_board_power(0, true, now);
    let console = run(&mut chip, 1_000_000);
    assert_eq!(console, "hi\n", "powering on boots the guest");

    let now = (chip.seconds() * 1e9) as u64;
    board.borrow_mut().set_board_power(0, false, now);
    let console = run(&mut chip, 1_000_000);
    assert!(console.is_empty(), "a switched-off guest prints nothing");

    let now = (chip.seconds() * 1e9) as u64;
    board.borrow_mut().set_board_power(0, true, now);
    let console = run(&mut chip, 1_000_000);
    assert_eq!(console, "hi\n", "power-on resets and boots the guest again");
}
