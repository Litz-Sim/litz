//! A Pico whose board is switched off runs nothing, and switching it back on
//! starts the firmware from its reset vector with the flash it had.

use litz_board::Chip;
use litz_board::flatten::FlatBoard;
use litz_catalog::Registry;
use litz_diagram::Circuit;

const OFF_DIAGRAM: &str = r#"{"version":1,"parts":[
  {"id":"pico","type":"board-raspberry-pi-pico","attrs":{"power":false}}],
  "connections":[]}"#;

const ON_DIAGRAM: &str = r#"{"version":1,"parts":[
  {"id":"pico","type":"board-raspberry-pi-pico","attrs":{}}],
  "connections":[]}"#;

fn board(diagram: &str) -> std::rc::Rc<std::cell::RefCell<litz_board::Board>> {
    let circuit = Circuit::from_json(diagram).unwrap();
    let flat = FlatBoard::build(&circuit, &Registry::builtin()).unwrap();
    std::rc::Rc::new(std::cell::RefCell::new(
        litz_board::Board::new(flat).unwrap(),
    ))
}

fn pico(board: std::rc::Rc<std::cell::RefCell<litz_board::Board>>) -> Box<dyn Chip> {
    let mut chip = litz_rp::Rp2040::new().with_shared_board(board, 0);
    chip.load_image(
        litz_board::ImageKind::Firmware,
        0,
        &litz_fixture::pico_blink(15, 1),
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
    let board = board(OFF_DIAGRAM);
    let mut chip = pico(std::rc::Rc::clone(&board));
    chip.boot().unwrap();
    let before = chip.seconds();
    let console = run(&mut chip, 1_000_000);
    assert!(console.is_empty(), "an unpowered core prints nothing");
    assert!(
        chip.seconds() > before,
        "simulated time passes even with the board off"
    );
    assert_eq!(chip.gpio_level(15), None, "the core drives no pad");
}

#[test]
fn powering_the_board_on_boots_the_firmware_and_off_resets_it() {
    let board = board(OFF_DIAGRAM);
    let mut chip = pico(std::rc::Rc::clone(&board));
    chip.boot().unwrap();
    run(&mut chip, 1_000_000);

    let now = (chip.seconds() * 1e9) as u64;
    board.borrow_mut().set_board_power(0, true, now);
    let console = run(&mut chip, 8_000_000);
    assert!(
        console.contains("litz: pico"),
        "powering on boots from flash: {console:?}"
    );

    // Power off: the core stops and its pads are released.
    let now = (chip.seconds() * 1e9) as u64;
    board.borrow_mut().set_board_power(0, false, now);
    run(&mut chip, 1_000_000);
    assert_eq!(chip.gpio_level(15), None, "a dead core drives nothing");

    // Power on again: the firmware restarts and the banner comes back, which
    // is what flash surviving a power cycle means.
    let now = (chip.seconds() * 1e9) as u64;
    board.borrow_mut().set_board_power(0, true, now);
    let console = run(&mut chip, 8_000_000);
    assert!(
        console.contains("litz: pico"),
        "the flash kept the firmware across the power cycle: {console:?}"
    );
}

#[test]
fn a_board_that_starts_on_boots_immediately() {
    let board = board(ON_DIAGRAM);
    let mut chip = pico(std::rc::Rc::clone(&board));
    chip.boot().unwrap();
    let console = run(&mut chip, 8_000_000);
    assert!(console.contains("litz: pico"), "{console:?}");
    assert!(board.borrow().board_powered(0));
}
