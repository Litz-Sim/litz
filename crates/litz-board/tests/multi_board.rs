//! Two development boards on one diagram.
//!
//! Every test places two ESP32-C3 boards and wires them — or a part on the
//! second board — through their header pins. The point is that the boards are
//! one electrical circuit while each keeps its own chip pins and its own input
//! queue: a level board 0 drives is a level board 1's chip sees, and board 1's
//! GPIO 4 is not board 0's GPIO 4.

use litz_board::flatten::{Endpoint, Rail};
use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_core::{Drive, Level, PartState};
use litz_diagram::Circuit;

fn board_part(id: &str) -> String {
    format!(r#"{{"id":"{id}","type":"board-esp32-c3-devkitm-1","attrs":{{}}}}"#)
}

fn part(id: &str, type_key: &str, attrs: &str) -> String {
    format!(r#"{{"id":"{id}","type":"{type_key}","attrs":{attrs}}}"#)
}

fn wire(from: &str, to: &str) -> String {
    format!(r#"["{from}","{to}","",[]]"#)
}

fn flat_board(parts: &[String], connections: &[String]) -> FlatBoard {
    let json = [
        r#"{"version":1,"parts":["#,
        &parts.join(","),
        r#"],"connections":["#,
        &connections.join(","),
        r#"]}"#,
    ]
    .concat();
    let circuit = Circuit::from_json(&json).expect("the diagram parses");
    let registry = Registry::builtin();
    FlatBoard::build(&circuit, &registry).expect("the diagram flattens")
}

fn built_board(parts: &[String], connections: &[String]) -> Board {
    Board::new(flat_board(parts, connections)).expect("the boards build")
}

fn part_index(flat: &FlatBoard, id: &str) -> u16 {
    flat.parts
        .iter()
        .position(|part| part.id == id)
        .expect("the part is flat") as u16
}

/// GPIO4 of one board wired to GPIO4 of the other is one net. Either chip can
/// drive it and the other sees the level, which is the foundation of MCU-to-
/// MCU signalling; the two pins are different pads with the same number.
#[test]
fn boards_wired_gpio_to_gpio_see_each_others_outputs() {
    let mut board = built_board(
        &[board_part("a"), board_part("b")],
        &[wire("a:GPIO4", "b:GPIO4")],
    );
    assert_eq!(board.chips(), 2, "both boards are present");

    // Board 0 drives high; board 1's input queue reports that level.
    board.set_chip_output_on(0, 4, Drive::strong(Level::High), 0);
    assert_eq!(board.chip_level_on(1, 4), Some(Level::High));
    assert!(
        board
            .take_chip_inputs_on(1)
            .iter()
            .any(|&(pin, level)| pin == 4 && level == Level::High),
        "board 1 sees board 0's high on its own GPIO4"
    );

    // Release board 0 first so the next drive is the only strong source, then
    // board 1 drives low and board 0 sees it.
    board.set_chip_output_on(0, 4, Drive::HIGH_Z, 10);
    board.set_chip_output_on(1, 4, Drive::strong(Level::Low), 20);
    assert_eq!(board.chip_level_on(0, 4), Some(Level::Low));
    assert!(
        board
            .take_chip_inputs_on(0)
            .iter()
            .any(|&(pin, level)| pin == 4 && level == Level::Low),
        "board 0 sees board 1's low on its own GPIO4"
    );
    assert!(board.conflicts().is_empty(), "the two drives never met");
}

/// A part placed at top level and wired to the second board's header is on
/// that board's chip, not the first board's: the same GPIO number on board 0
/// stays untouched.
#[test]
fn a_part_on_the_second_board_is_reached_through_its_header() {
    let mut board = built_board(
        &[
            board_part("a"),
            board_part("b"),
            part("r1", "litz-resistor", r#"{"value":"220"}"#),
            part("led1", "litz-led", r#"{"color":"green"}"#),
        ],
        &[
            wire("b:GPIO8", "r1:1"),
            wire("r1:2", "led1:A"),
            wire("led1:C", "b:GND.1"),
        ],
    );

    board.set_chip_output_on(1, 8, Drive::strong(Level::High), 0);
    assert_eq!(
        board.state("led1"),
        Some(PartState::Led { lit: true }),
        "the LED is driven by board 1's GPIO8"
    );
    assert_eq!(board.part_pin_level("led1", "C"), Some(Level::Low));
    assert_eq!(board.named_pin_on("r1"), Some((1, 8)));
    assert_eq!(board.chip_level_on(1, 8), Some(Level::High));
    // Board 0 never sees GPIO8 high: its own pad is a different node.
    assert_ne!(board.chip_level_on(0, 8), Some(Level::High));
}

/// Both boards' fixed header pins are the global rails, so a part can take
/// power from one board and ground from the other.
#[test]
fn a_rail_is_shared_between_boards() {
    let mut board = built_board(
        &[
            board_part("a"),
            board_part("b"),
            part("r1", "litz-resistor", r#"{"value":"220"}"#),
            part("led1", "litz-led", r#"{"color":"green"}"#),
        ],
        &[
            // The two boards' ground and 3.3 V pins are explicitly tied.
            wire("a:GND.1", "b:GND.1"),
            wire("a:3V3", "b:3V3"),
            // Power comes from board 0, ground returns through board 1.
            wire("a:GPIO8", "r1:1"),
            wire("r1:2", "led1:A"),
            wire("led1:C", "b:GND.1"),
        ],
    );

    let flat = board.flat();
    assert!(
        flat.endpoint_net.contains_key(&Endpoint::Rail(Rail::Gnd)),
        "both boards' GND pins resolve to one global ground"
    );
    assert!(
        flat.endpoint_net.contains_key(&Endpoint::Rail(Rail::V33)),
        "both boards' 3V3 pins resolve to one global rail"
    );

    board.set_chip_output_on(0, 8, Drive::strong(Level::High), 0);
    assert_eq!(
        board.state("led1"),
        Some(PartState::Led { lit: true }),
        "current flows from board 0's GPIO through the LED to board 1's ground"
    );
}

/// Board 0 keeps the old spelling: a one-board diagram has exactly the
/// `Endpoint::Chip` endpoints it had before boards could be repeated, and
/// never a `ChipOn`.
#[test]
fn a_single_board_still_uses_endpoint_chip() {
    let flat = flat_board(
        &[
            board_part("esp"),
            part("r1", "litz-resistor", r#"{"value":"220"}"#),
        ],
        &[wire("esp:GPIO4", "r1:1"), wire("r1:2", "esp:GND.1")],
    );

    assert_eq!(flat.boards.len(), 1);
    assert!(flat.endpoint_net.contains_key(&Endpoint::Chip(4)));
    assert!(
        !flat
            .endpoint_net
            .keys()
            .any(|endpoint| matches!(endpoint, Endpoint::ChipOn { .. })),
        "board 0 never produces a ChipOn endpoint"
    );
    assert!(flat.chip_pins.contains(&4));
    assert!(
        flat.chip_pins_of
            .get(&0)
            .is_some_and(|pins| pins.contains(&4)),
        "the per-board pin set names board 0"
    );

    let board = Board::new(flat).expect("the board builds");
    assert_eq!(board.chips(), 1);
}

/// A UART part on the second board is reachable at the pin level: its TX pin
/// and board 1's GPIO20 are one flattened net, so a level driven there reaches
/// the part's pin. This is deliberately not the bus fabric — `FlatBuses` is
/// not keyed by board yet — so the test reads the wire only.
#[test]
fn a_uart_part_on_the_second_board_is_wired_at_the_pin_level() {
    let parts = [
        board_part("a"),
        board_part("b"),
        part("gps", "litz-gps-neo6m", "{}"),
    ];
    let connections = [
        wire("b:GPIO20", "gps:TX"),
        wire("b:GPIO21", "gps:RX"),
        wire("b:3V3", "gps:VCC"),
        wire("b:GND.1", "gps:GND"),
    ];
    let flat = flat_board(&parts, &connections);

    // The GPS's TX pin and board 1's GPIO20 are one net, and it is board 1's
    // pad rather than board 0's.
    let gps = part_index(&flat, "gps");
    let tx = flat.parts[gps as usize]
        .pins
        .iter()
        .position(|pin| pin == "TX")
        .expect("the GPS has a TX pin") as u16;
    let net = flat
        .endpoint_net
        .get(&Endpoint::Part { part: gps, pin: tx })
        .copied()
        .expect("the GPS TX pin is wired");
    assert!(
        flat.nets[net as usize]
            .iter()
            .any(|endpoint| matches!(endpoint, Endpoint::ChipOn { board: 1, pin: 20 })),
        "GPS TX shares a net with board 1's GPIO20"
    );
    assert!(
        !flat.nets[net as usize]
            .iter()
            .any(|endpoint| matches!(endpoint, Endpoint::Chip(20))),
        "the wire does not reach board 0's GPIO20"
    );

    let mut board = Board::new(flat).expect("the board builds");
    board.set_chip_output_on(1, 20, Drive::strong(Level::High), 0);
    assert_eq!(
        board.part_pin_level("gps", "TX"),
        Some(Level::High),
        "a level on board 1's GPIO20 reaches the GPS TX pin"
    );
    assert_eq!(board.chip_level_on(1, 20), Some(Level::High));
}
